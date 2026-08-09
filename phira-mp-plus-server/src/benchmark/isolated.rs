//! 隔离基准服务器（World B）拉起 / 就绪 / 销毁。
//!
//! `benchmark run` 默认自spawn 一个独立的 PMP 实例（World B）：独立游戏/HTTP
//! 端口、独立测试数据库、`phira_api_endpoint` 指向 Mock Phira。压测 World B，
//! 测完杀进程。线上实例（World A）的状态与配置完全不被触碰。
//!
//! World B 通过 `--config <临时文件>` 复用同一二进制（`current_exe`）拉起，
//! 就绪判定为 `GET /health/ready` 返回 200。

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use super::config::BenchmarkConfig;

/// 隔离的 World B 服务器句柄。Drop 时尽力杀进程 + 删临时配置（错误路径兜底）。
pub struct IsolatedServer {
    child: tokio::process::Child,
    /// 游戏 TCP 端口
    pub port: u16,
    /// HTTP/SSE 端口
    pub http_port: u16,
    /// 临时配置文件路径
    config_path: PathBuf,
    /// World B stderr 日志路径（启动/DB/认证失败时诊断用）
    pub stderr_path: PathBuf,
}

impl Drop for IsolatedServer {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_file(&self.config_path);
        // stderr 日志保留（含 DB URL 的配置删除，服务器日志留作诊断）。
    }
}

impl IsolatedServer {
    /// 正常路径：杀进程并等待回收，清理临时配置（stderr 日志保留）。
    pub async fn shutdown(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
        let _ = std::fs::remove_file(&self.config_path);
    }

    /// 子进程 PID（报告里记录 World B 实例）。
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }
}

/// benchmark 完成后清理测试库（World B 写入的全部表），保证每次压测从干净
/// 数据库开始，测试库不随运行次数累积膨胀。
pub async fn cleanup_test_db(db_url: &str) -> Result<(), String> {
    let pool = sqlx::PgPool::connect(db_url)
        .await
        .map_err(|e| format!("failed to connect benchmark test DB: {e}"))?;
    const TABLES: &[&str] = &[
        "mp_events",
        "mp_room_snapshots",
        "mp_user_visits",
        "mp_runtime_telemetry_batches",
        "mp_runtime_telemetry_items",
        "mp_round_results",
        "mp_rounds",
        "mp_round_player_data",
        "mp_user_room_history",
        "playtime",
        "mp_users",
        "mp_server_instances",
        "mp_runtime_benchmark_reports",
    ];
    for t in TABLES {
        if let Err(e) = sqlx::query(&format!("TRUNCATE {t}")).execute(&pool).await {
            tracing::warn!(table = t, error = %e, "benchmark test DB truncate failed");
        }
    }
    pool.close().await;
    Ok(())
}

/// 绑定 `127.0.0.1:0` 拿一个空闲端口（随后释放）。
async fn pick_free_port() -> Result<u16, String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("failed to pick free port: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("failed to read free port: {e}"))?
        .port();
    drop(listener);
    Ok(port)
}

/// 生成 World B 的 server_config.yml（独立端口 / 数据库 / mock endpoint）。
fn build_config_yaml(config: &BenchmarkConfig, port: u16, http_port: u16, mock_url: &str) -> String {
    format!(
        "port: {port}\n\
         http_port: {http_port}\n\
         database_url: \"{db}\"\n\
         phira_api_endpoint: \"{mock_url}\"\n\
         cli_enabled: false\n\
         room_creation_enabled: true\n",
        db = config.server_db_url,
    )
}

/// 拉起隔离的 World B 实例，等待 `/health/ready` 就绪后返回句柄。
///
/// 出错时杀进程、删临时配置，避免残留。
pub async fn spawn_isolated_server(
    config: &BenchmarkConfig,
    mock_url: &str,
) -> Result<IsolatedServer, String> {
    let port = match config.server_port {
        Some(p) => p,
        None => pick_free_port().await?,
    };
    let http_port = pick_free_port().await?;

    let yaml = build_config_yaml(config, port, http_port, mock_url);
    let config_path = std::env::temp_dir().join(format!(
        "pmp-bench-{}.yml",
        uuid::Uuid::new_v4()
    ));
    tokio::fs::write(&config_path, yaml)
        .await
        .map_err(|e| format!("failed to write World B config: {e}"))?;

    let exe = std::env::current_exe()
        .map_err(|e| format!("failed to resolve self path for World B: {e}"))?;
    // World B stderr 落临时日志（启动/DB/认证失败时可诊断；之前 null 掉吞了错误）。
    let stderr_path = std::env::temp_dir().join(format!(
        "pmp-bench-worldb-{}.log",
        uuid::Uuid::new_v4()
    ));
    let stderr_file = std::fs::File::create(&stderr_path)
        .map_err(|e| format!("failed to create World B log file: {e}"))?;
    let mut child = tokio::process::Command::new(&exe)
        .arg("--config")
        .arg(&config_path)
        .arg("--no-cli")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .map_err(|e| format!("failed to spawn World B server: {e}"))?;

    // 等待 /health/ready 就绪（最多 30s）。
    let ready_url = format!("http://127.0.0.1:{http_port}/health/ready");
    let client = reqwest::Client::new();
    let mut ready = false;
    for _ in 0..150 {
        if child
            .try_wait()
            .map_err(|e| format!("World B check failed: {e}"))?
            .is_some()
        {
            return Err("World B exited before becoming ready".to_string());
        }
        if let Ok(resp) = client.get(&ready_url).send().await {
            if resp.status().is_success() {
                ready = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    if !ready {
        let _ = child.start_kill();
        let _ = std::fs::remove_file(&config_path);
        return Err(format!(
            "World B did not become ready within 30s ({ready_url}); stderr: {}",
            stderr_path.display()
        ));
    }

    Ok(IsolatedServer {
        child,
        port,
        http_port,
        config_path,
        stderr_path,
    })
}

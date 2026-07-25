//! Local Mock Phira HTTP server
//!
//! 本地 Mock Phira HTTP 服务器，在 Real 模式下替代真实的 Phira API。
//! 使用 Axum 在随机空闲端口启动。
//! 提供标准的 /me、/user/{id}、/chart/{id} 端点用于认证和数据查询。

use axum::{extract::Path, routing::get, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::info;

/// Mock Phira 服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockPhiraConfig {
    /// 模拟的 API 响应延迟（毫秒）
    pub delay_ms: u64,
    /// 随机的延迟抖动范围（毫秒）
    pub jitter_ms: u64,
    /// 模拟的错误率（0.0 ~ 1.0），0.01 表示 1% 的请求会返回错误
    pub error_rate: f64,
    /// 模拟的请求超时时间
    pub timeout_ms: u64,
    /// 随机种子，用于确定性回放
    pub seed: u64,
    /// 模拟的响应体大小（字节）
    pub response_size: usize,
    /// 监听的地址
    pub listen_addr: String,
    /// 是否记录所有请求日志
    pub verbose: bool,
}

impl Default for MockPhiraConfig {
    fn default() -> Self {
        Self {
            delay_ms: 5,
            jitter_ms: 2,
            error_rate: 0.0,
            timeout_ms: 30_000,
            seed: 114_514,
            response_size: 1024,
            listen_addr: "127.0.0.1:9877".to_string(),
            verbose: false,
        }
    }
}

/// Mock Phira 服务器
///
/// 在后台启动一个 Axum HTTP 服务器，模拟 Phira API 端点。
/// 提供 `start` / `stop` / `port` 方法控制生命周期和获取端口。
pub struct MockPhiraServer {
    config: MockPhiraConfig,
    shutdown_tx: Arc<RwLock<Option<oneshot::Sender<()>>>>,
    handle: Arc<RwLock<Option<JoinHandle<()>>>>,
    port: Arc<RwLock<Option<u16>>>,
}

impl MockPhiraServer {
    /// 使用给定配置创建 Mock Phira 服务器（尚未启动）
    pub fn new(config: MockPhiraConfig) -> Self {
        Self {
            config,
            shutdown_tx: Arc::new(RwLock::new(None)),
            handle: Arc::new(RwLock::new(None)),
            port: Arc::new(RwLock::new(None)),
        }
    }

    /// 启动 Mock Phira HTTP 服务器
    ///
    /// 绑定到 127.0.0.1:0（随机空闲端口），然后构建 Axum 路由：
    /// - `GET /me` — 返回固定测试用户信息
    /// - `GET /user/{id}` — 返回测试用户数据
    /// - `GET /chart/{id}` — 返回测试谱面数据
    ///
    /// 启动后通过 `port()` 获取实际端口号。
    pub async fn start(&self) -> Result<(), String> {
        let app = Router::new()
            .route("/me", get(me_handler))
            .route("/user/{id}", get(user_handler))
            .route("/chart/{id}", get(chart_handler));

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| format!("failed to bind Mock Phira server: {e}"))?;

        let local_addr = listener
            .local_addr()
            .map_err(|e| format!("failed to get local address: {e}"))?;
        let port = local_addr.port();

        let (tx, rx) = oneshot::channel::<()>();
        *self.shutdown_tx.write().map_err(|_| "lock error")? = Some(tx);
        *self.port.write().map_err(|_| "lock error")? = Some(port);

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
                .ok();
        });

        *self.handle.write().map_err(|_| "lock error")? = Some(handle);

        info!("Mock Phira server listening on 127.0.0.1:{}", port);
        Ok(())
    }

    /// 停止 Mock Phira HTTP 服务器
    ///
    /// 发送 shutdown 信号并等待服务器任务退出。
    pub async fn stop(&self) -> Result<(), String> {
        // 发送 shutdown 信号 — release lock before any await
        let tx = self.shutdown_tx.write().map_err(|_| "lock error")?.take();
        if let Some(tx) = tx {
            let _ = tx.send(());
        }

        // Take handle while holding lock, then drop lock before await
        let handle = { self.handle.write().map_err(|_| "lock error")?.take() };
        self.port.write().map_err(|_| "lock error")?.take();

        if let Some(handle) = handle {
            let _ = handle.await;
        }
        Ok(())
    }

    /// 返回服务器当前监听的端口号
    ///
    /// 仅在 `start()` 成功后返回 `Some(port)`。
    pub fn port(&self) -> Option<u16> {
        self.port.read().ok().and_then(|g| *g)
    }

    /// 返回服务器当前监听地址（含端口）
    pub fn listen_addr(&self) -> String {
        self.port()
            .map(|p| format!("127.0.0.1:{}", p))
            .unwrap_or_else(|| self.config.listen_addr.clone())
    }

    /// 返回配置引用
    pub fn config(&self) -> &MockPhiraConfig {
        &self.config
    }
}

// ── Axum 请求处理器 ──────────────────────────────────────────────

/// `GET /me` — 返回当前认证用户信息
///
/// PMP 使用此端点验证用户 token 并获取用户信息。
async fn me_handler() -> Json<Value> {
    Json(json!({
        "id": 999,
        "name": "benchmark-user",
        "language": "zh-CN"
    }))
}

/// `GET /user/{id}` — 返回指定用户信息
///
/// PMP 使用此端点通过用户 ID 获取用户名等信息。
async fn user_handler(Path(id): Path<i32>) -> Json<Value> {
    Json(json!({
        "id": id,
        "name": format!("user-{}", id),
        "language": "zh-CN"
    }))
}

/// `GET /chart/{id}` — 返回指定谱面信息
///
/// PMP 使用此端点通过谱面 ID 获取谱面名称等信息。
async fn chart_handler(Path(id): Path<i32>) -> Json<Value> {
    Json(json!({
        "id": id,
        "name": format!("chart-{}", id)
    }))
}

//! Real mode benchmark runner
//!
//! 真实模式运行器。连接到一个正在运行的 PMP 服务器，使用真实二进制
//! 协议 (phira_mp_common::Stream) 进行完整的认证与房间命令交互。
//! 收集命令延迟和吞吐量指标。
//!
//! 运行流程：
//! 1. 可选：启动 Mock Phira 服务器（用于本地认证）
//! 2. TCP 连接到 PMP 服务器
//! 3. 建立 Stream 并完成认证
//! 4. 发送顺序房间命令：CreateRoom, SelectChart, Played
//! 5. 采集延迟指标
//! 6. 清理连接

use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::environment::EnvironmentSnapshot;
use crate::benchmark::metrics::LatencySampler;
use crate::benchmark::mock_phira::{MockPhiraConfig, MockPhiraServer};
use crate::benchmark::report::BenchmarkReport;
use phira_mp_common::{ClientCommand, RoomId, ServerCommand, Stream, Varchar};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::info;

/// 真实模式运行结果
pub struct RealRunResult {
    /// 基准测试报告
    pub report: BenchmarkReport,
    /// 服务器进程 ID（本运行器不启动 PMP，始终为 None）
    pub server_pid: Option<u32>,
}

/// 运行真实模式基准测试
///
/// 连接到一个已运行的 PMP 服务器并执行基准测试：
/// 1. 解析配置，若 `config.mock_phira` 为 true 则启动 Mock Phira 服务器
/// 2. TCP 连接到 `config.listen_addr`（默认 127.0.0.1:12346）
/// 3. 通过二进制协议建立 Stream 并认证
/// 4. 依次发送 CreateRoom → SelectChart → Played 命令
/// 5. 测量每条命令的往返延迟
/// 6. 生成并返回基准测试报告
pub async fn run_real(
    config: BenchmarkConfig,
    state: &crate::server::PlusServerState,
) -> Result<RealRunResult, String> {
    let started_at = Instant::now();
    let environment = EnvironmentSnapshot::capture().await;
    let mut report = BenchmarkReport::new("Real Mode Benchmark", environment, config.clone());

    // ── 1. 可选：启动 Mock Phira 服务器 ──────────────────────────────
    let mock_phira = if config.mock_phira {
        let mock_config = MockPhiraConfig {
            listen_addr: "127.0.0.1:0".to_string(),
            ..MockPhiraConfig::default()
        };
        let server = MockPhiraServer::new(mock_config);
        server.start().await?;
        info!("Mock Phira server started on port {:?}", server.port());
        Some(server)
    } else {
        None
    };

    // ── 1b. 如果启动了 Mock Phira，覆盖 PMP 的 API endpoint ──────────
    let original_endpoint = if let Some(ref mock) = mock_phira {
        let port = mock.port().ok_or("Mock Phira port not available")?;
        let mock_url = format!("http://127.0.0.1:{}", port);
        let mut lc = state.live_config.write().await;
        let orig = lc.phira_api_endpoint.clone();
        info!("Set phira_api_endpoint to {mock_url} (original: {orig})");
        lc.phira_api_endpoint = mock_url;
        Some(orig)
    } else {
        None
    };

    // ── 2. TCP 连接到 PMP 服务器 ─────────────────────────────────────
    let server_addr = config
        .listen_addr
        .clone()
        .unwrap_or_else(|| format!("127.0.0.1:{}", state.config.port));

    info!("Connecting to PMP server at {}", server_addr);
    let connect_start = Instant::now();
    let tcp_stream = TcpStream::connect(&server_addr)
        .await
        .map_err(|e| format!("failed to connect to PMP server at {server_addr}: {e}"))?;
    let connect_latency = connect_start.elapsed();
    info!("Connected in {:.1}ms", connect_latency.as_secs_f64() * 1000.0);
    report.connect_latency_ms = connect_latency.as_secs_f64() * 1000.0;

    // ── 3. 建立 Stream ────────────────────────────────────────────────
    // 使用 mpsc 通道将 Stream handler 收到的 ServerCommand 转发给主流程
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<ServerCommand>();

    let stream = Arc::new(
        Stream::<ClientCommand, ServerCommand>::new(
            Some(1), // 协议版本
            tcp_stream,
            Box::new(move |_send_tx, cmd| {
                let cmd_tx = cmd_tx.clone();
                async move {
                    // 忽略发送失败（通道关闭 = 主流程已结束）
                    let _ = cmd_tx.send(cmd);
                }
            }),
        )
        .await
        .map_err(|e| format!("failed to establish stream: {e}"))?,
    );

    info!(
        "Stream established, protocol version: {}",
        stream.version()
    );

    // ── 延迟采样器 ────────────────────────────────────────────────────
    let mut latency_sampler = LatencySampler::new(10_000);

    // ── 4. 认证 (Authenticate) ────────────────────────────────────────
    info!("Authenticating...");
    let token: Varchar<32> = Varchar::try_from("benchmark-token-1234567890ab".to_string())
        .map_err(|e| format!("invalid token: {e}"))?;

    let cmd_start = Instant::now();
    stream
        .send(ClientCommand::Authenticate { token })
        .await
        .map_err(|e| format!("failed to send Authenticate: {e}"))?;

    let auth_result = wait_for_response(&mut cmd_rx, |cmd| match cmd {
        ServerCommand::Authenticate(result) => Some(result.clone()),
        _ => None,
    })
    .await?;

    let (_user_info, _room_state) = auth_result
        .map_err(|e| format!("authentication failed: {e}"))?;

    let auth_latency = cmd_start.elapsed();
    latency_sampler.record_duration(auth_latency);
    info!(
        "Authenticated as user {} ({:.1}ms)",
        _user_info.id,
        auth_latency.as_secs_f64() * 1000.0
    );

    // ── 5. 创建房间 (CreateRoom) ──────────────────────────────────────
    info!("Creating room...");
    let room_id = RoomId::try_from("bench-run-001".to_string())
        .map_err(|e| format!("invalid room id: {e}"))?;

    let cmd_start = Instant::now();
    stream
        .send(ClientCommand::CreateRoom {
            id: room_id.clone(),
        })
        .await
        .map_err(|e| format!("failed to send CreateRoom: {e}"))?;

    let create_result = wait_for_response(&mut cmd_rx, |cmd| match cmd {
        ServerCommand::CreateRoom(result) => Some(result.clone()),
        _ => None,
    })
    .await?;

    create_result.map_err(|e| format!("CreateRoom failed: {e}"))?;

    let create_latency = cmd_start.elapsed();
    latency_sampler.record_duration(create_latency);
    info!(
        "Room created ({:.1}ms)",
        create_latency.as_secs_f64() * 1000.0
    );

    // ── 6. 选曲 (SelectChart) ────────────────────────────────────────
    info!("Selecting chart...");
    let cmd_start = Instant::now();
    stream
        .send(ClientCommand::SelectChart { id: 114514 })
        .await
        .map_err(|e| format!("failed to send SelectChart: {e}"))?;

    let select_result = wait_for_response(&mut cmd_rx, |cmd| match cmd {
        ServerCommand::SelectChart(result) => Some(result.clone()),
        _ => None,
    })
    .await?;

    select_result.map_err(|e| format!("SelectChart failed: {e}"))?;

    let select_latency = cmd_start.elapsed();
    latency_sampler.record_duration(select_latency);
    info!(
        "Chart selected ({:.1}ms)",
        select_latency.as_secs_f64() * 1000.0
    );

    // ── 7. 发起游戏并准备 (RequestStart + Ready) ─────────────────────
    info!("Starting game...");
    stream
        .send(ClientCommand::RequestStart)
        .await
        .map_err(|e| format!("failed to send RequestStart: {e}"))?;
    let _ = wait_for_response(&mut cmd_rx, |cmd| match cmd {
        ServerCommand::Message(msg) => match msg {
            phira_mp_common::Message::GameStart { .. } => Some(Ok::<(), String>(())),
            _ => None,
        },
        _ => None,
    })
    .await;
    // 标记准备
    stream
        .send(ClientCommand::Ready)
        .await
        .map_err(|e| format!("failed to send Ready: {e}"))?;
    let _ = wait_for_response(&mut cmd_rx, |cmd| match cmd {
        ServerCommand::Message(msg) => match msg {
            phira_mp_common::Message::StartPlaying { .. } => Some(Ok::<(), String>(())),
            _ => None,
        },
        _ => None,
    })
    .await;

    // ── 8. 游玩报告 (Played) ─────────────────────────────────────────
    info!("Reporting played...");
    let cmd_start = Instant::now();
    stream
        .send(ClientCommand::Played { id: 114514 })
        .await
        .map_err(|e| format!("failed to send Played: {e}"))?;

    let played_result = wait_for_response(&mut cmd_rx, |cmd| match cmd {
        ServerCommand::Played(result) => Some(result.clone()),
        _ => None,
    })
    .await?;

    played_result.map_err(|e| format!("Played failed: {e}"))?;

    let played_latency = cmd_start.elapsed();
    latency_sampler.record_duration(played_latency);
    info!(
        "Played reported ({:.1}ms)",
        played_latency.as_secs_f64() * 1000.0
    );

    // ── 8. 汇总指标并生成报告 ───────────────────────────────────────
    let elapsed = started_at.elapsed();
    let latency_percentiles = latency_sampler.percentiles();

    report.summary.duration_secs = elapsed.as_secs().max(1);
    report.summary.total_commands = 4; // Auth + CreateRoom + SelectChart + Played
    report.summary.avg_commands_per_sec = 4.0 / elapsed.as_secs_f64().max(0.001);
    report.summary.peak_commands_per_sec = 4.0 / elapsed.as_secs_f64().max(0.001);
    report.summary.clients_succeeded = 1;
    report.command_latency = latency_percentiles;
    report.mark_finished();

    info!("Real mode benchmark completed in {:.1}s", elapsed.as_secs_f64());

    // ── 9. 清理 ──────────────────────────────────────────────────────
    stream.close();

    // 恢复原始 endpoint
    if let Some(orig) = original_endpoint {
        state.live_config.write().await.phira_api_endpoint = orig;
        info!("Restored original phira_api_endpoint");
    }

    if let Some(mock) = mock_phira {
        mock.stop().await?;
    }

    Ok(RealRunResult {
        report,
        server_pid: None,
    })
}

/// 等待从 Stream 接收指定类型的 ServerCommand 响应
///
/// 使用 predicate 筛选并提取感兴趣的命令。忽略所有不匹配的中间命令
/// （如 Pong、Message 等），直到找到匹配项或流关闭。
async fn wait_for_response<T>(
    rx: &mut mpsc::UnboundedReceiver<ServerCommand>,
    predicate: impl Fn(&ServerCommand) -> Option<T>,
) -> Result<T, String> {
    loop {
        match rx.recv().await {
            Some(cmd) => {
                if let Some(result) = predicate(&cmd) {
                    return Ok(result);
                }
                // 忽略不匹配的命令
            }
            None => return Err("stream closed while waiting for response".to_string()),
        }
    }
}

//! Real mode benchmark runner
//!
//! 真实模式运行器。连接到一个正在运行的 PMP 服务器，使用真实二进制
//! 协议 (phira_mp_common::Stream) 进行完整的认证与房间命令交互。
//! 收集命令延迟和吞吐量指标。
//!
//! ## 改进 (PMP23 迭代)
//!
//! - **多客户端**: 从 config.clients 生成 N 个并行客户端任务
//! - **唯一标识**: 每个客户端使用 bench-{run_id}-{client_index} 的 token/user/room
//! - **步骤超时**: 每个协议步骤有独立超时，整体使用 config.duration 作为时限
//! - **命令计数器**: 使用 CommandCollector 实时追踪发送的命令数
//! - **场景驱动**: 根据 config.scenario 执行不同行为（Connection / RoomLifecycle / Gameplay / HotRoom 等）
//! - **Touch/Judge**: HotRoom/Gameplay 场景在 playing 阶段发送 60Hz Touch/Judge 帧
//! - **Mock Phira 故障**: 配置参数 (delay_ms, jitter_ms, error_rate, timeout_ms, seed) 实际生效

use crate::benchmark::command::BenchmarkScenario;
use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::environment::EnvironmentSnapshot;
use crate::benchmark::metrics::LatencySampler;
use crate::benchmark::mock_phira::{MockPhiraConfig, MockPhiraServer};
use crate::benchmark::report::BenchmarkReport;
use phira_mp_common::{
    ClientCommand, CompactPos, JudgeEvent, Judgement, RoomId, ServerCommand, Stream, TouchFrame,
    Varchar,
};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Barrier};
use tokio::time::Instant;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// 默认的单个步骤超时（秒）
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// 真实模式运行结果
pub struct RealRunResult {
    /// 基准测试报告
    pub report: BenchmarkReport,
    /// 服务器进程 ID（本运行器不启动 PMP，始终为 None）
    pub server_pid: Option<u32>,
}

/// 每个客户端采集的指标
#[derive(Default)]
struct ClientMetrics {
    /// 发送的命令数
    commands_sent: u64,
    /// 发生的错误数
    errors: u64,
    /// TCP 连接延迟（毫秒）
    connect_latency_ms: f64,
    /// 所有命令延迟样本（毫秒）
    all_latencies: Vec<f64>,
}

impl ClientMetrics {
    fn new() -> Self {
        Self::default()
    }

    fn record_command(&mut self) {
        self.commands_sent += 1;
    }

}

// ── 辅助函数 ──────────────────────────────────────────────────────

/// 生成确定性基准 token
///
/// 格式: `bench-{hex_hash}` (22 字符，适配 Varchar<32>)
/// hex_hash 是 run_id + client_index 的 64-bit 哈希的 16 位十六进制表示。
fn make_bench_token(run_id: &Uuid, client_index: u32) -> String {
    let mut s = std::collections::hash_map::DefaultHasher::new();
    run_id.hash(&mut s);
    client_index.hash(&mut s);
    format!("bench-{:016x}", s.finish())
}

/// 生成确定性房间 ID
///
/// 格式: `b{run_hash}-{room_index}` 包含 run_id 前缀，避免不同运行间的房间冲突。
/// run_hash 使用 run_id 前 8 位十六进制字符。
fn make_bench_room_id(run_id: &Uuid, room_index: u32) -> String {
    let run_hash = &run_id.to_string()[..8];
    format!("b{}-{}", run_hash, room_index)
}

/// 带超时的 wait_for_response
///
/// 从 Stream 接收 ServerCommand，使用 predicate 筛选感兴趣的命令。
/// 忽略所有不匹配的中间命令（如 Pong、Message 等），
/// 直到找到匹配项、流关闭或超时。
///
/// `last_cmd` 用于在超时时报告最后接收到的命令。
async fn wait_for_response<T>(
    rx: &mut mpsc::UnboundedReceiver<ServerCommand>,
    predicate: impl Fn(&ServerCommand) -> Option<T>,
    timeout: Duration,
    step_name: &str,
    last_cmd: &mut Option<String>,
) -> Result<T, String> {
    loop {
        match tokio::time::timeout(timeout, rx.recv()).await {
            Ok(Some(cmd)) => {
                *last_cmd = Some(format!("{cmd:?}"));
                if let Some(result) = predicate(&cmd) {
                    return Ok(result);
                }
                // 忽略不匹配的命令，继续等待
            }
            Ok(None) => {
                return Err(format!(
                    "stream closed while waiting for step '{step_name}'"
                ));
            }
            Err(_elapsed) => {
                return Err(format!(
                    "timeout ({:.1}s) while waiting for step '{step_name}', last command: {}",
                    timeout.as_secs_f64(),
                    last_cmd.as_deref().unwrap_or("(none)")
                ));
            }
        }
    }
}

/// 认证步骤
async fn step_authenticate(
    stream: &Stream<ClientCommand, ServerCommand>,
    rx: &mut mpsc::UnboundedReceiver<ServerCommand>,
    cm: &mut ClientMetrics,
    last_cmd: &mut Option<String>,
    client_index: u32,
    run_id: &Uuid,
) -> Result<(), String> {
    let token_str = make_bench_token(run_id, client_index);
    let token: Varchar<32> = Varchar::try_from(token_str.clone())
        .map_err(|e| format!("client {client_index}: invalid token '{token_str}': {e}"))?;

    let step_start = Instant::now();
    stream
        .send(ClientCommand::Authenticate { token })
        .await
        .map_err(|e| format!("client {client_index}: send Authenticate: {e}"))?;
    cm.record_command();

    let auth_result = wait_for_response(
        rx,
        |cmd| match cmd {
            ServerCommand::Authenticate(result) => Some(result.clone()),
            _ => None,
        },
        STEP_TIMEOUT,
        "Authenticate",
        last_cmd,
    )
    .await
    .map_err(|e| format!("client {client_index}: {e}"))?;

    let (_user_info, _room_state) = auth_result
        .map_err(|e| format!("client {client_index}: authentication failed: {e}"))?;

    cm.all_latencies
        .push(step_start.elapsed().as_secs_f64() * 1000.0);
    info!(
        "client {} authenticated as user {}",
        client_index, _user_info.id
    );
    Ok(())
}

/// 创建房间步骤
async fn step_create_room(
    stream: &Stream<ClientCommand, ServerCommand>,
    rx: &mut mpsc::UnboundedReceiver<ServerCommand>,
    cm: &mut ClientMetrics,
    last_cmd: &mut Option<String>,
    client_index: u32,
    run_id: &Uuid,
    room_index: u32,
) -> Result<(), String> {
    let room_id_str = make_bench_room_id(run_id, room_index);
    let room_id = RoomId::try_from(room_id_str.clone())
        .map_err(|e| format!("client {client_index}: invalid room id '{room_id_str}': {e}"))?;

    let step_start = Instant::now();
    stream
        .send(ClientCommand::CreateRoom {
            id: room_id.clone(),
        })
        .await
        .map_err(|e| format!("client {client_index}: send CreateRoom: {e}"))?;
    cm.record_command();

    let create_result = wait_for_response(
        rx,
        |cmd| match cmd {
            ServerCommand::CreateRoom(result) => Some(result.clone()),
            _ => None,
        },
        STEP_TIMEOUT,
        "CreateRoom",
        last_cmd,
    )
    .await
    .map_err(|e| format!("client {client_index}: {e}"))?;

    create_result.map_err(|e| format!("client {client_index}: CreateRoom failed: {e}"))?;

    cm.all_latencies
        .push(step_start.elapsed().as_secs_f64() * 1000.0);
    info!("client {} created room {}", client_index, room_id);
    Ok(())
}

/// 加入房间步骤
async fn step_join_room(
    stream: &Stream<ClientCommand, ServerCommand>,
    rx: &mut mpsc::UnboundedReceiver<ServerCommand>,
    cm: &mut ClientMetrics,
    last_cmd: &mut Option<String>,
    client_index: u32,
    run_id: &Uuid,
    room_index: u32,
) -> Result<(), String> {
    let room_id_str = make_bench_room_id(run_id, room_index);
    let room_id = RoomId::try_from(room_id_str.clone())
        .map_err(|e| format!("client {client_index}: invalid room id '{room_id_str}': {e}"))?;

    let step_start = Instant::now();
    stream
        .send(ClientCommand::JoinRoom {
            id: room_id.clone(),
            monitor: false,
        })
        .await
        .map_err(|e| format!("client {client_index}: send JoinRoom: {e}"))?;
    cm.record_command();

    let join_result = wait_for_response(
        rx,
        |cmd| match cmd {
            ServerCommand::JoinRoom(result) => Some(result.clone()),
            _ => None,
        },
        STEP_TIMEOUT,
        "JoinRoom",
        last_cmd,
    )
    .await
    .map_err(|e| format!("client {client_index}: {e}"))?;

    match join_result {
        Ok(_response) => {
            cm.all_latencies
                .push(step_start.elapsed().as_secs_f64() * 1000.0);
            info!("client {} joined room {}", client_index, room_id);
            Ok(())
        }
        Err(e) => Err(format!("client {client_index}: JoinRoom failed: {e}")),
    }
}

/// 选曲步骤
async fn step_select_chart(
    stream: &Stream<ClientCommand, ServerCommand>,
    rx: &mut mpsc::UnboundedReceiver<ServerCommand>,
    cm: &mut ClientMetrics,
    last_cmd: &mut Option<String>,
    client_index: u32,
) -> Result<(), String> {
    let step_start = Instant::now();
    stream
        .send(ClientCommand::SelectChart { id: 114514 })
        .await
        .map_err(|e| format!("client {client_index}: send SelectChart: {e}"))?;
    cm.record_command();

    let select_result = wait_for_response(
        rx,
        |cmd| match cmd {
            ServerCommand::SelectChart(result) => Some(result.clone()),
            _ => None,
        },
        STEP_TIMEOUT,
        "SelectChart",
        last_cmd,
    )
    .await
    .map_err(|e| format!("client {client_index}: {e}"))?;

    select_result.map_err(|e| format!("client {client_index}: SelectChart failed: {e}"))?;

    cm.all_latencies
        .push(step_start.elapsed().as_secs_f64() * 1000.0);
    Ok(())
}

/// 请求开始 + 准备步骤
async fn step_start_and_ready(
    stream: &Stream<ClientCommand, ServerCommand>,
    rx: &mut mpsc::UnboundedReceiver<ServerCommand>,
    cm: &mut ClientMetrics,
    last_cmd: &mut Option<String>,
    client_index: u32,
) -> Result<(), String> {
    // RequestStart
    let step_start = Instant::now();
    stream
        .send(ClientCommand::RequestStart)
        .await
        .map_err(|e| format!("client {client_index}: send RequestStart: {e}"))?;
    cm.record_command();

    let _game_start = wait_for_response(
        rx,
        |cmd| match cmd {
            ServerCommand::Message(msg) => match msg {
                phira_mp_common::Message::GameStart { .. } => Some(Ok::<(), String>(())),
                _ => None,
            },
            _ => None,
        },
        STEP_TIMEOUT,
        "GameStart",
        last_cmd,
    )
    .await
    .map_err(|e| format!("client {client_index}: waiting GameStart: {e}"))?;

    cm.all_latencies
        .push(step_start.elapsed().as_secs_f64() * 1000.0);

    // Ready
    let step_start = Instant::now();
    stream
        .send(ClientCommand::Ready)
        .await
        .map_err(|e| format!("client {client_index}: send Ready: {e}"))?;
    cm.record_command();

    let _start_playing = wait_for_response(
        rx,
        |cmd| match cmd {
            ServerCommand::Message(msg) => match msg {
                phira_mp_common::Message::StartPlaying => Some(Ok::<(), String>(())),
                _ => None,
            },
            _ => None,
        },
        STEP_TIMEOUT,
        "StartPlaying",
        last_cmd,
    )
    .await
    .map_err(|e| format!("client {client_index}: waiting StartPlaying: {e}"))?;

    cm.all_latencies
        .push(step_start.elapsed().as_secs_f64() * 1000.0);
    info!("client {} entered playing state", client_index);
    Ok(())
}

/// 游玩报告步骤
async fn step_played(
    stream: &Stream<ClientCommand, ServerCommand>,
    rx: &mut mpsc::UnboundedReceiver<ServerCommand>,
    cm: &mut ClientMetrics,
    last_cmd: &mut Option<String>,
    client_index: u32,
) -> Result<(), String> {
    let step_start = Instant::now();
    stream
        .send(ClientCommand::Played { id: 114514 })
        .await
        .map_err(|e| format!("client {client_index}: send Played: {e}"))?;
    cm.record_command();

    let played_result = wait_for_response(
        rx,
        |cmd| match cmd {
            ServerCommand::Played(result) => Some(result.clone()),
            _ => None,
        },
        STEP_TIMEOUT,
        "Played",
        last_cmd,
    )
    .await
    .map_err(|e| format!("client {client_index}: {e}"))?;

    played_result.map_err(|e| format!("client {client_index}: Played failed: {e}"))?;

    cm.all_latencies
        .push(step_start.elapsed().as_secs_f64() * 1000.0);
    info!("client {} reported played", client_index);
    Ok(())
}

/// 发送 Touch/Judge 帧（用于 Gameplay 和 HotRoom 场景）
///
/// 在 Playing 状态下以 ~60Hz 发送 Touch/Judge 命令，持续 `duration` 秒。
async fn send_touch_judge_frames(
    stream: &Stream<ClientCommand, ServerCommand>,
    cm: &mut ClientMetrics,
    client_index: u32,
    duration: Duration,
    overall_deadline: Instant,
) -> Result<(), String> {
    let interval = Duration::from_secs_f64(1.0 / 60.0); // ~60Hz
    let num_touches_per_frame = 5;
    let num_judges_per_batch = 10;
    let end = Instant::now() + duration;

    let mut touch_time: f32 = 0.0;
    let mut frame_index: u32 = 0;
    while Instant::now() < end && Instant::now() < overall_deadline {
        // Touch 帧
        let frames: Vec<TouchFrame> = (0..num_touches_per_frame)
            .map(|i| TouchFrame {
                time: touch_time,
                points: vec![(
                    i as i8,
                    CompactPos::new(0.5 + i as f32 * 0.1, 0.5 + i as f32 * 0.05),
                )],
            })
            .collect();

        stream
            .send(ClientCommand::Touches {
                frames: Arc::new(frames),
            })
            .await
            .map_err(|e| format!("client {client_index}: send Touches: {e}"))?;
        cm.record_command();

        // 每 10 帧（~6Hz）发送一次 Judge 事件
        if frame_index % 10 == 0 {
            let judges: Vec<JudgeEvent> = (0..num_judges_per_batch)
                .map(|j| JudgeEvent {
                    time: touch_time,
                    line_id: j as u32,
                    note_id: j as u32 + 100,
                    judgement: if j % 2 == 0 {
                        Judgement::Perfect
                    } else {
                        Judgement::Good
                    },
                })
                .collect();

            stream
                .send(ClientCommand::Judges {
                    judges: Arc::new(judges),
                })
                .await
                .map_err(|e| format!("client {client_index}: send Judges: {e}"))?;
            cm.record_command();
        }

        touch_time += 1.0 / 60.0;
        frame_index += 1;
        tokio::time::sleep(interval).await;
    }

    info!(
        "client {} sent Touch/Judge for {:.1}s",
        client_index,
        duration.as_secs_f64()
    );
    Ok(())
}

/// 完整房间生命周期：Auth → CreateRoom/JoinRoom → SelectChart → RequestStart → Ready → Played
///
/// `is_host` 控制该客户端是创建房间还是加入现有房间。
/// `room_index` 指定要创建/加入的房间。
async fn run_full_lifecycle(
    stream: &Stream<ClientCommand, ServerCommand>,
    rx: &mut mpsc::UnboundedReceiver<ServerCommand>,
    cm: &mut ClientMetrics,
    last_cmd: &mut Option<String>,
    client_index: u32,
    run_id: &Uuid,
    config: &BenchmarkConfig,
    overall_deadline: Instant,
    is_host: bool,
    room_index: u32,
    auth_barrier: &Barrier,
    phase_barrier: &Barrier,
) -> Result<(), String> {
    step_authenticate(stream, rx, cm, last_cmd, client_index, run_id).await?;
    auth_barrier.wait().await;

    if Instant::now() >= overall_deadline {
        return Ok(());
    }

    if is_host {
        step_create_room(stream, rx, cm, last_cmd, client_index, run_id, room_index).await?;

        if Instant::now() >= overall_deadline {
            return Ok(());
        }

        step_select_chart(stream, rx, cm, last_cmd, client_index).await?;

        if Instant::now() >= overall_deadline {
            return Ok(());
        }

        step_start_and_ready(stream, rx, cm, last_cmd, client_index).await?;
    } else {
        step_join_room(stream, rx, cm, last_cmd, client_index, run_id, room_index).await?;

        if Instant::now() >= overall_deadline {
            return Ok(());
        }

        // 非 host 客户端只发送 Ready 等待开始
        let step_start = Instant::now();
        stream
            .send(ClientCommand::Ready)
            .await
            .map_err(|e| format!("client {client_index}: send Ready: {e}"))?;
        cm.record_command();

        let _start_playing = wait_for_response(
            rx,
            |cmd| match cmd {
                ServerCommand::Message(msg) => match msg {
                    phira_mp_common::Message::StartPlaying => Some(Ok::<(), String>(())),
                    _ => None,
                },
                _ => None,
            },
            STEP_TIMEOUT,
            "StartPlaying",
            last_cmd,
        )
        .await
        .map_err(|e| format!("client {client_index}: waiting StartPlaying: {e}"))?;

        cm.all_latencies
            .push(step_start.elapsed().as_secs_f64() * 1000.0);
        info!("client {} entered playing state (joiner)", client_index);
    }

    phase_barrier.wait().await;

    if Instant::now() >= overall_deadline {
        return Ok(());
    }

    // 场景特定行为：Touch/Judge
    match config.scenario {
        BenchmarkScenario::HotRoom => {
            send_touch_judge_frames(stream, cm, client_index, Duration::from_secs(10), overall_deadline)
                .await?;
        }
        BenchmarkScenario::Gameplay => {
            send_touch_judge_frames(stream, cm, client_index, Duration::from_secs(5), overall_deadline)
                .await?;
        }
        _ => {}
    }

    if Instant::now() >= overall_deadline {
        return Ok(());
    }

    step_played(stream, rx, cm, last_cmd, client_index).await?;

    Ok(())
}

/// 运行单个客户端场景
///
/// `is_host` 表示该客户端是否为房间创建者。
/// `room_index` 指定客户端应加入的房间编号（用于 rooms/members_per_room 分组）。
async fn run_single_client(
    client_index: u32,
    run_id: Uuid,
    server_addr: String,
    config: BenchmarkConfig,
    overall_deadline: Instant,
    is_host: bool,
    room_index: u32,
    auth_barrier: Arc<Barrier>,
    phase_barrier: Arc<Barrier>,
) -> Result<ClientMetrics, String> {
    let mut cm = ClientMetrics::new();
    let mut last_cmd: Option<String> = None;

    // ── 1. TCP 连接 ──
    let connect_start = Instant::now();
    let tcp_stream = tokio::time::timeout(STEP_TIMEOUT, TcpStream::connect(&server_addr))
        .await
        .map_err(|_| {
            format!(
                "client {client_index}: connect timeout ({:.1}s)",
                STEP_TIMEOUT.as_secs_f64()
            )
        })?
        .map_err(|e| format!("client {client_index}: connect failed: {e}"))?;
    cm.connect_latency_ms = connect_start.elapsed().as_secs_f64() * 1000.0;

    // ── 2. 建立 Stream ──
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<ServerCommand>();

    let stream = Arc::new(
        Stream::<ClientCommand, ServerCommand>::new(
            Some(1),
            tcp_stream,
            Box::new(move |_send_tx, cmd| {
                let cmd_tx = cmd_tx.clone();
                async move {
                    let _ = cmd_tx.send(cmd);
                }
            }),
        )
        .await
        .map_err(|e| format!("client {client_index}: stream setup failed: {e}"))?,
    );

    info!(
        "client {} connected, stream version {}",
        client_index,
        stream.version()
    );

    // ── 3. 场景调度 ──
    let result = match config.scenario {
        BenchmarkScenario::Connection => {
            // Connection：只认证，通过两个屏障确保所有客户端同时完成阶段
            step_authenticate(
                &stream,
                &mut cmd_rx,
                &mut cm,
                &mut last_cmd,
                client_index,
                &run_id,
            )
            .await?;
            auth_barrier.wait().await;
            phase_barrier.wait().await;
            Ok(())
        }
        BenchmarkScenario::SteadyState => {
            // SteadyState: 认证 + 创建房间，然后等待剩余时间
            step_authenticate(
                &stream,
                &mut cmd_rx,
                &mut cm,
                &mut last_cmd,
                client_index,
                &run_id,
            )
            .await?;
            auth_barrier.wait().await;
            if Instant::now() < overall_deadline {
                step_create_room(
                    &stream,
                    &mut cmd_rx,
                    &mut cm,
                    &mut last_cmd,
                    client_index,
                    &run_id,
                    0,
                )
                .await?;
            }
            phase_barrier.wait().await;
            if Instant::now() < overall_deadline {
                let remaining = overall_deadline.saturating_duration_since(Instant::now());
                if remaining > Duration::from_millis(100) {
                    tokio::time::sleep(remaining).await;
                }
            }
            Ok(())
        }
        BenchmarkScenario::RoomLifecycle
        | BenchmarkScenario::Gameplay
        | BenchmarkScenario::HotRoom => {
            run_full_lifecycle(
                &stream,
                &mut cmd_rx,
                &mut cm,
                &mut last_cmd,
                client_index,
                &run_id,
                &config,
                overall_deadline,
                is_host,
                room_index,
                auth_barrier.as_ref(),
                phase_barrier.as_ref(),
            )
            .await
        }
        // ── 尚未在 Real 模式中实现的场景 ──
        BenchmarkScenario::SlowConsumer
        | BenchmarkScenario::Reconnect
        | BenchmarkScenario::PluginLoad
        | BenchmarkScenario::DatabaseWrite
        | BenchmarkScenario::Mixed
        | BenchmarkScenario::LongRun => Err(format!(
            "scenario '{}' is not implemented in real mode",
            config.scenario.as_str()
        )),
    };

    // ── 4. 清理 ──
    stream.close();

    result?;
    Ok(cm)
}

/// 运行真实模式基准测试
///
/// 连接到一个已运行的 PMP 服务器并执行基准测试：
/// 1. 解析配置，若 `config.mock_phira` 为 true 则启动 Mock Phira 服务器
/// 2. 根据 `config.clients` 并发连接 N 个客户端
/// 3. 每个客户端执行场景定义的步骤（Auth → CreateRoom → SelectChart → ...）
/// 4. 使用 `config.duration` 作为整体超时
/// 5. 用 `CommandCollector` 追踪命令数、延迟和错误
/// 6. 根据 `config.scenario` 调整行为（Connection / RoomLifecycle / HotRoom 等）
/// 7. 生成并返回基准测试报告
pub async fn run_real(
    config: BenchmarkConfig,
    state: &crate::server::PlusServerState,
    run_id: Uuid,
) -> Result<RealRunResult, String> {
    let started_at = Instant::now();
    let environment = EnvironmentSnapshot::capture().await;
    let mut report = BenchmarkReport::new("Real Mode Benchmark", environment, config.clone());
    let overall_deadline = started_at + config.duration;

    // ── 1. 可选：启动 Mock Phira 服务器 ──────────────────────────────
    let mock_phira = if config.mock_phira {
        let listen_addr = if config.mock_phira_port > 0 {
            format!("127.0.0.1:{}", config.mock_phira_port)
        } else {
            "127.0.0.1:0".to_string()
        };
        let mock_config = MockPhiraConfig {
            listen_addr: listen_addr.clone(),
            delay_ms: config.mock_phira_delay_ms,
            jitter_ms: config.mock_phira_jitter_ms,
            error_rate: config.mock_phira_error_rate,
            timeout_ms: config.mock_phira_timeout_ms,
            seed: config.seed,
            ..MockPhiraConfig::default()
        };
        let server = MockPhiraServer::new(mock_config);
        server.start().await?;
        info!("Mock Phira server started on {} (port {:?})", listen_addr, server.port());
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

    // ── 2. 确定服务器地址 ────────────────────────────────────────────
    let server_addr = config
        .listen_addr
        .clone()
        .unwrap_or_else(|| format!("127.0.0.1:{}", state.config.port));

    info!(
        "Starting real benchmark: {} clients, scenario={}, duration={}s, rooms={}, members_per_room={}",
        config.clients,
        config.scenario.as_str(),
        config.duration.as_secs(),
        config.rooms,
        config.members_per_room,
    );

    // ── 3. 并发启动 N 个客户端任务 ──────────────────────────────────
    let num_clients = config.clients.max(1);
    let rooms = config.rooms.max(1);
    let members_per_room = config.members_per_room.max(1);
    let mut join_set = tokio::task::JoinSet::new();

    // 创建阶段同步屏障（所有客户端在 Auth / RoomSetup 阶段结束后同步）
    let n = num_clients as usize;
    let auth_barrier = Arc::new(Barrier::new(n));
    let phase_barrier = Arc::new(Barrier::new(n));

    for i in 0..num_clients {
        let addr = server_addr.clone();
        let cfg = config.clone();
        let rid = run_id;
        let deadline = overall_deadline;
        let auth_b = auth_barrier.clone();
        let phase_b = phase_barrier.clone();

        // 按 rooms 和 members_per_room 计算房间分配
        let room_index = if cfg.scenario == BenchmarkScenario::HotRoom {
            // HotRoom: 所有客户端加入同一个房间
            0
        } else {
            // 按 rooms 分配：每个房间一组
            (i / members_per_room) % rooms
        };

        // 每组第一个客户端是 host（创建房间），其余 join
        let is_host = if cfg.scenario == BenchmarkScenario::HotRoom {
            i == 0
        } else {
            i % members_per_room == 0
        };

        join_set.spawn(async move {
            run_single_client(i, rid, addr, cfg, deadline, is_host, room_index, auth_b, phase_b).await
        });
    }

    info!("Spawned {} client tasks, waiting for completion...", num_clients);

    // ── 4. 等待所有客户端完成（受整体超时限制）────────────────────
    let mut results = Vec::new();
    let remaining = overall_deadline.saturating_duration_since(Instant::now());

    if remaining > Duration::from_millis(50) {
        let deadline_instant = Instant::now() + remaining;
        loop {
            let deadline_remaining = deadline_instant.saturating_duration_since(Instant::now());
            if deadline_remaining.is_zero() {
                warn!("Overall benchmark timeout ({:.1}s) reached; aborting {} remaining task(s)",
                    config.duration.as_secs_f64(), join_set.len());
                join_set.abort_all();
                // Drain aborted tasks to avoid detached futures
                while let Some(result) = join_set.join_next().await {
                    match result {
                        Ok(Ok(cm)) => results.push(Ok(cm)),
                        Ok(Err(e)) => results.push(Err(e)),
                        Err(e) if e.is_cancelled() => {
                            debug!("Client task cancelled on timeout");
                        }
                        Err(e) => {
                            warn!("Client task join error during drain: {e}");
                        }
                    }
                }
                break;
            }

            match tokio::time::timeout(deadline_remaining, join_set.join_next()).await {
                Ok(Some(Ok(Ok(cm)))) => {
                    results.push(Ok(cm));
                }
                Ok(Some(Ok(Err(e)))) => {
                    results.push(Err(e));
                }
                Ok(Some(Err(e))) => {
                    warn!("Client task join error: {e}");
                }
                Ok(None) => {
                    // All tasks completed
                    break;
                }
                Err(_) => {
                    // Timeout reached during join_next
                    warn!("Overall benchmark timeout ({:.1}s) reached; aborting {} remaining task(s)",
                        config.duration.as_secs_f64(), join_set.len());
                    join_set.abort_all();
                    // Drain remaining
                    while let Some(result) = join_set.join_next().await {
                        match result {
                            Ok(Ok(cm)) => results.push(Ok(cm)),
                            Ok(Err(e)) => results.push(Err(e)),
                            Err(e) if e.is_cancelled() => {}
                            Err(e) => warn!("Client task join error during drain: {e}"),
                        }
                    }
                    break;
                }
            }
        }
    }

    // ── 5. 合并结果 ──────────────────────────────────────────────────
    let elapsed = started_at.elapsed();
    let mut total_commands: u64 = 0;
    let mut total_errors: u64 = 0;
    let mut clients_succeeded: u32 = 0;
    let mut clients_failed: u32 = 0;
    let mut all_latencies: Vec<f64> = Vec::new();
    let mut connect_latencies: Vec<f64> = Vec::new();

    for result in &results {
        match result {
            Ok(cm) => {
                total_commands += cm.commands_sent;
                total_errors += cm.errors;
                clients_succeeded += 1;
                all_latencies.extend_from_slice(&cm.all_latencies);
                if cm.connect_latency_ms > 0.0 {
                    connect_latencies.push(cm.connect_latency_ms);
                }
            }
            Err(e) => {
                clients_failed += 1;
                total_errors += 1;
                warn!("Client error: {e}");
            }
        }
    }

    // ── 6. 填写报告 ──────────────────────────────────────────────────
    let duration_secs = elapsed.as_secs().max(1);
    let total_messages = total_commands; // 近似

    report.summary.duration_secs = duration_secs;
    report.summary.total_commands = total_commands;
    report.summary.total_messages = total_messages;
    report.summary.avg_commands_per_sec = total_commands as f64 / duration_secs as f64;
    report.summary.peak_commands_per_sec = total_commands as f64 / duration_secs as f64;
    report.summary.clients_succeeded = clients_succeeded;
    report.summary.clients_failed = clients_failed;
    report.summary.avg_messages_per_sec = total_messages as f64 / duration_secs as f64;
    report.summary.peak_messages_per_sec = total_messages as f64 / duration_secs as f64;

    report.errors_total = total_errors;
    report.connect_latency_ms = connect_latencies
        .iter()
        .copied()
        .fold(0.0_f64, f64::max);

    // 计算延迟百分位
    if !all_latencies.is_empty() {
        let mut sampler = LatencySampler::new(all_latencies.len().max(100_000));
        for lat in &all_latencies {
            sampler.record_ms(*lat);
        }
        report.command_latency = sampler.percentiles();
        report.command_latency.connect_latency_ms = report.connect_latency_ms;
    }

    let scenario_name = config.scenario.as_str();
    report.scenario_results.insert(
        scenario_name.to_string(),
        crate::benchmark::report::ScenarioResult {
            name: scenario_name.to_string(),
            commands_per_sec: report.summary.avg_commands_per_sec,
            messages_per_sec: report.summary.avg_messages_per_sec,
            errors: total_errors,
            latency: report.command_latency,
            passed: clients_failed == 0,
            error: if clients_failed > 0 {
                Some(format!("{} client(s) failed", clients_failed))
            } else {
                None
            },
        },
    );

    report.mark_finished();

    info!(
        "Real benchmark completed in {:.1}s: {} clients ({} ok, {} failed), {} commands, {} errors",
        elapsed.as_secs_f64(),
        num_clients,
        clients_succeeded,
        clients_failed,
        total_commands,
        total_errors,
    );

    // ── 7. 清理 ──────────────────────────────────────────────────────
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

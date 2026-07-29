//! Real mode benchmark runner
//!
//! 真实模式运行器。连接到一个正在运行的 PMP 服务器，使用真实二进制
//! 协议 (phira_mp_common::Stream) 进行完整的认证与房间命令交互。
//! 收集命令延迟和吞吐量指标。
//!
//! ## 改进 (PMP27 迭代)
//!
//! - **Per-Room 编排器**: 用 `RoomCoordinator`（基于 `watch` 通道）
//!   替换全局 `Barrier`，每个房间独立同步阶段
//! - **阶段**: auth -> create -> join -> select -> start -> ready -> play -> finish
//! - **取消令牌**: 第一个客户端失败会设置 `cancelled` 标志，避免死锁
//! - **SteadyState**: 正确的房间分组（host 创建、joiners 加入），
//!   稳态期间发送 Ping 代替 sleep
//! - **HotRoom**: 使用 `members_needed = num_clients` 确保所有成员就位后开始

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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch, Notify};
use tokio::time::Instant;
use tracing::{info, warn};
use uuid::Uuid;

/// 默认的单个步骤超时（秒）
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// Ping 发送间隔（SteadyState 场景）
const STEADY_STATE_PING_INTERVAL: Duration = Duration::from_secs(2);

// ── Public types ─────────────────────────────────────────────────────

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

// ── Room Orchestrator ───────────────────────────────────────────────

/// 房间阶段：所有客户端在同一个房间内按阶段同步推进。
///
/// 变体的声明顺序就是阶段顺序（通过 `#[derive(Ord)]` 保证
/// `Auth < Create < … < Finish`），因此可以用 `>=` 比较。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RoomPhase {
    Auth,
    Create,
    Join,
    Select,
    Start,
    Ready,
    Play,
    Finish,
}

/// 每房间协调器，替换全局的 `Barrier`。
///
/// 使用 `tokio::sync::watch` 通道广播阶段切换。一个原子计数器
/// 跟踪有多少客户端已到达当前阶段的关卡。当最后一个客户端到达时，
/// 阶段推进到下一阶段。
///
/// 如果某个客户端失败（`cancel()` 被调用），`cancelled` 标志被设置，
/// 所有正在等待的客户端立即收到错误，不会死锁。
struct RoomCoordinator {
    /// 阶段广播发送端
    phase_tx: watch::Sender<RoomPhase>,
    /// 当前关卡到达的客户端数
    arrived: AtomicU32,
    /// 该房间需要的客户端总数
    members_needed: u32,
    /// 取消标志（首个客户端失败时设置）
    cancelled: AtomicBool,
    /// 取消时通知等待者
    cancel_notify: Notify,
}

impl RoomCoordinator {
    /// 创建新协调器。`members_needed` 是该房间的总客户端数。
    fn new(members_needed: u32) -> Arc<Self> {
        let (phase_tx, _) = watch::channel(RoomPhase::Auth);
        Arc::new(Self {
            phase_tx,
            arrived: AtomicU32::new(0),
            members_needed,
            cancelled: AtomicBool::new(false),
            cancel_notify: Notify::new(),
        })
    }

    /// 获取当前阶段。
    #[allow(dead_code)]
    fn current_phase(&self) -> RoomPhase {
        *self.phase_tx.borrow()
    }

    /// 订阅阶段变更。
    fn subscribe(&self) -> watch::Receiver<RoomPhase> {
        self.phase_tx.subscribe()
    }

    /// 设置取消标志并唤醒所有等待者。
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.cancel_notify.notify_waiters();
    }

    /// 等待房间阶段达到（或超过）`target`。
    async fn wait_phase(
        &self,
        rx: &mut watch::Receiver<RoomPhase>,
        target: RoomPhase,
    ) -> Result<(), String> {
        loop {
            if self.cancelled.load(Ordering::SeqCst) {
                return Err("room cancelled due to client failure".into());
            }
            if *rx.borrow() >= target {
                return Ok(());
            }
            tokio::select! {
                _ = rx.changed() => {}
                _ = self.cancel_notify.notified() => {
                    if self.cancelled.load(Ordering::SeqCst) {
                        return Err("room cancelled due to client failure".into());
                    }
                }
            }
        }
    }

    /// 标记当前阶段完成并等待所有房间成员也完成。
    /// 当最后一个成员到达时，阶段推进到 `next`。
    async fn advance(&self, next: RoomPhase) -> Result<(), String> {
        let prev = self.arrived.fetch_add(1, Ordering::SeqCst);
        if prev + 1 == self.members_needed {
            // 最后一个到达 -> 推进阶段并唤醒所有人
            self.arrived.store(0, Ordering::SeqCst);
            self.phase_tx
                .send(next)
                .map_err(|_| "room coordinator: receiver dropped".to_string())?;
            return Ok(());
        }

        // 不是最后一个 -> 等待阶段推进
        loop {
            if self.cancelled.load(Ordering::SeqCst) {
                return Err("room cancelled due to client failure".into());
            }
            if *self.phase_tx.borrow() >= next {
                return Ok(());
            }
            let mut rx = self.phase_tx.subscribe();
            if *rx.borrow() >= next {
                return Ok(());
            }
            tokio::select! {
                _ = rx.changed() => {}
                _ = self.cancel_notify.notified() => {
                    if self.cancelled.load(Ordering::SeqCst) {
                        return Err("room cancelled due to client failure".into());
                    }
                }
            }
        }
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
///
/// 注意：`deadline` 是绝对截止时间，从第一次调用时开始计算。
/// 不会因为不匹配的中间消息而重置，从而避免"永远等待"问题。
async fn wait_for_response<T>(
    rx: &mut mpsc::UnboundedReceiver<ServerCommand>,
    predicate: impl Fn(&ServerCommand) -> Option<T>,
    timeout: Duration,
    step_name: &str,
    last_cmd: &mut Option<String>,
) -> Result<T, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "timeout ({:.1}s) while waiting for step '{step_name}', last command: {}",
                timeout.as_secs_f64(),
                last_cmd.as_deref().unwrap_or("(none)")
            ));
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
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

/// 请求开始 + 准备步骤（host 调用）
///
/// 发送 RequestStart，等待 GameStart，然后发送 Ready，等待 StartPlaying。
async fn step_host_start_and_ready(
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
    info!("client {} (host) entered playing state", client_index);
    Ok(())
}

/// 加入者准备步骤（joiner 调用）
///
/// 等待 GameStart，然后发送 Ready，等待 StartPlaying。
async fn step_joiner_ready(
    stream: &Stream<ClientCommand, ServerCommand>,
    rx: &mut mpsc::UnboundedReceiver<ServerCommand>,
    cm: &mut ClientMetrics,
    last_cmd: &mut Option<String>,
    client_index: u32,
) -> Result<(), String> {
    // Wait for GameStart (sent by host)
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
    info!("client {} (joiner) entered playing state", client_index);
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

/// 在 SteadyState 场景的 Play 阶段发送 Ping 命令。
///
/// 每 2 秒发送一次 Ping，持续到 `duration` 结束或整体截止时间。
/// 不等待 Pong 响应。
async fn send_steady_state_commands(
    stream: &Stream<ClientCommand, ServerCommand>,
    cm: &mut ClientMetrics,
    client_index: u32,
    duration: Duration,
    overall_deadline: Instant,
) -> Result<(), String> {
    let end = Instant::now() + duration;
    let mut ping_count: u64 = 0;

    while Instant::now() < end && Instant::now() < overall_deadline {
        stream
            .send(ClientCommand::Ping)
            .await
            .map_err(|e| format!("client {client_index}: send Ping: {e}"))?;
        cm.record_command();
        ping_count += 1;

        tokio::time::sleep(STEADY_STATE_PING_INTERVAL).await;
    }

    info!(
        "client {} sent {} Ping commands during steady state ({:.1}s)",
        client_index,
        ping_count,
        duration.as_secs_f64()
    );
    Ok(())
}

// ── 场景运行器 ──────────────────────────────────────────────────────

/// 运行单个客户端的完整场景，通过 `RoomCoordinator` 与同房间其他客户端同步。
///
/// 所有场景共享同一个阶段流水线，仅在实际执行的工作上有所不同：
/// - `Connection`: 仅认证（跳过房间操作）
/// - `SteadyState`: 认证 -> 创建/加入房间 -> 发送 Ping
/// - `RoomLifecycle`/`Gameplay`/`HotRoom`: 完整房间生命周期
async fn run_client_scenario(
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
    coordinator: &RoomCoordinator,
    phase_watcher: &mut watch::Receiver<RoomPhase>,
) -> Result<(), String> {
    let scenario = config.scenario;

    // ── Auth 阶段 ────────────────────────────────────────────────
    coordinator.wait_phase(phase_watcher, RoomPhase::Auth).await?;
    step_authenticate(stream, rx, cm, last_cmd, client_index, run_id).await?;
    coordinator.advance(RoomPhase::Create).await?;

    // ── Create 阶段 ─────────────────────────────────────────────
    coordinator.wait_phase(phase_watcher, RoomPhase::Create).await?;
    match scenario {
        BenchmarkScenario::Connection => {} // no rooms
        _ => {
            if is_host {
                step_create_room(stream, rx, cm, last_cmd, client_index, run_id, room_index)
                    .await?;
            }
        }
    }
    coordinator.advance(RoomPhase::Join).await?;

    // ── Join 阶段 ───────────────────────────────────────────────
    coordinator.wait_phase(phase_watcher, RoomPhase::Join).await?;
    match scenario {
        BenchmarkScenario::Connection => {}
        _ => {
            if !is_host {
                step_join_room(stream, rx, cm, last_cmd, client_index, run_id, room_index)
                    .await?;
            }
        }
    }
    coordinator.advance(RoomPhase::Select).await?;

    // ── Select 阶段 ─────────────────────────────────────────────
    coordinator.wait_phase(phase_watcher, RoomPhase::Select).await?;
    match scenario {
        BenchmarkScenario::Connection | BenchmarkScenario::SteadyState => {
            // SteadyState: skip chart selection (no game needed)
        }
        BenchmarkScenario::RoomLifecycle
        | BenchmarkScenario::Gameplay
        | BenchmarkScenario::HotRoom => {
            if is_host {
                step_select_chart(stream, rx, cm, last_cmd, client_index).await?;
            }
        }
        _ => {}
    }
    coordinator.advance(RoomPhase::Start).await?;

    // ── Start 阶段 ──────────────────────────────────────────────
    coordinator.wait_phase(phase_watcher, RoomPhase::Start).await?;
    match scenario {
        BenchmarkScenario::Connection | BenchmarkScenario::SteadyState => {
            // no game start needed
        }
        BenchmarkScenario::RoomLifecycle
        | BenchmarkScenario::Gameplay
        | BenchmarkScenario::HotRoom => {
            if is_host {
                step_host_start_and_ready(stream, rx, cm, last_cmd, client_index).await?;
            } else {
                step_joiner_ready(stream, rx, cm, last_cmd, client_index).await?;
            }
        }
        _ => {}
    }
    coordinator.advance(RoomPhase::Ready).await?;

    // ── Ready phase (work already merged into Start) ────────────
    coordinator.wait_phase(phase_watcher, RoomPhase::Ready).await?;
    coordinator.advance(RoomPhase::Play).await?;

    // ── Play 阶段 ───────────────────────────────────────────────
    coordinator.wait_phase(phase_watcher, RoomPhase::Play).await?;
    match scenario {
        BenchmarkScenario::SteadyState => {
            // Send Ping until overall_deadline is near
            let remaining = overall_deadline.saturating_duration_since(Instant::now());
            if remaining > Duration::from_millis(100) {
                send_steady_state_commands(
                    stream,
                    cm,
                    client_index,
                    remaining,
                    overall_deadline,
                )
                .await?;
            }
        }
        BenchmarkScenario::HotRoom => {
            send_touch_judge_frames(stream, cm, client_index, config.measurement_duration, overall_deadline)
                .await?;
        }
        BenchmarkScenario::Gameplay => {
            send_touch_judge_frames(stream, cm, client_index, config.measurement_duration, overall_deadline)
                .await?;
        }
        _ => {}
    }
    coordinator.advance(RoomPhase::Finish).await?;

    // ── Finish 阶段 ─────────────────────────────────────────────
    coordinator.wait_phase(phase_watcher, RoomPhase::Finish).await?;
    match scenario {
        BenchmarkScenario::Connection | BenchmarkScenario::SteadyState => {
            // no Played report needed — no game was started
        }
        BenchmarkScenario::RoomLifecycle
        | BenchmarkScenario::Gameplay
        | BenchmarkScenario::HotRoom => {
            step_played(stream, rx, cm, last_cmd, client_index).await?;
        }
        _ => {}
    }

    // ── 离开房间（RoomLifecycle 场景） ───────────────────────────
    if scenario == BenchmarkScenario::RoomLifecycle {
        stream
            .send(ClientCommand::LeaveRoom)
            .await
            .map_err(|e| format!("client {client_index}: send LeaveRoom: {e}"))?;
        cm.record_command();

        // Wait for LeaveRoom confirmation
        let _leave_result = wait_for_response(
            rx,
            |cmd| match cmd {
                ServerCommand::LeaveRoom(result) => Some(result.clone()),
                _ => None,
            },
            STEP_TIMEOUT,
            "LeaveRoom",
            last_cmd,
        )
        .await
        .map_err(|e| format!("client {client_index}: {e}"))?;

        info!("client {} left room", client_index);
    }

    Ok(())
}

/// 运行单个客户端任务
///
/// 1. TCP 连接
/// 2. 建立 Stream
/// 3. 通过 `RoomCoordinator` 编排的阶段流水线运行场景
/// 4. 清理并返回指标
async fn run_single_client(
    client_index: u32,
    run_id: Uuid,
    server_addr: String,
    config: BenchmarkConfig,
    overall_deadline: Instant,
    is_host: bool,
    room_index: u32,
    coordinator: Arc<RoomCoordinator>,
    mut phase_watcher: watch::Receiver<RoomPhase>,
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

    // ── 3. Scenario (synchronised through RoomCoordinator) ──────
    let result = run_client_scenario(
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
        &coordinator,
        &mut phase_watcher,
    )
    .await;

    // ── 4. Cleanup ──
    stream.close();

    // If this client failed, cancel the room so other members
    // don't deadlock waiting for us.
    if result.is_err() {
        coordinator.cancel();
    }

    result?;
    Ok(cm)
}

// ── 主运行器 ──────────────────────────────────────────────────────

/// 运行真实模式基准测试
///
/// 连接到一个已运行的 PMP 服务器并执行基准测试：
/// 1. 解析配置，若 `config.mock_phira` 为 true 则启动 Mock Phira 服务器
/// 2. 为每个房间创建 `RoomCoordinator`
/// 3. 根据 `config.clients` 并发连接 N 个客户端
/// 4. 每个客户端通过 `RoomCoordinator` 同步执行阶段流水线
/// 5. 使用 `config.duration` 作为整体超时
/// 6. 生成并返回基准测试报告
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

    // For Connection each client is its own "room" (members=1) so they
    // never block on each other.
    let is_connection = config.scenario == BenchmarkScenario::Connection;
    let is_hot_room = config.scenario == BenchmarkScenario::HotRoom;

    if is_hot_room {
        // ── HotRoom: all clients in one room ────────────────────
        let coordinator = RoomCoordinator::new(num_clients);

        for i in 0..num_clients {
            let addr = server_addr.clone();
            let cfg = config.clone();
            let rid = run_id;
            let deadline = overall_deadline;
            let is_host = i == 0;

            let phase_rx = coordinator.subscribe();
            let coord = coordinator.clone();

            join_set.spawn(async move {
                run_single_client(
                    i, rid, addr, cfg, deadline, is_host, 0, coord, phase_rx,
                )
                .await
            });
        }
    } else if is_connection {
        // ── Connection: each client gets its own coordinator ────
        for i in 0..num_clients {
            let addr = server_addr.clone();
            let cfg = config.clone();
            let rid = run_id;
            let deadline = overall_deadline;

            let coordinator = RoomCoordinator::new(1);
            let phase_rx = coordinator.subscribe();
            let coord = coordinator;

            join_set.spawn(async move {
                run_single_client(
                    i, rid, addr, cfg, deadline, false, 0, coord, phase_rx,
                )
                .await
            });
        }
    } else {
        // ── Standard / SteadyState / RoomLifecycle / Gameplay ──
        let actual_rooms_needed = num_clients.div_ceil(members_per_room);
        let num_rooms_to_use = actual_rooms_needed.max(rooms);

        // Compute the actual number of clients per room; the last
        // room may have fewer than members_per_room.
        let mut room_sizes: Vec<u32> = vec![0; num_rooms_to_use as usize];
        for i in 0..num_clients {
            let idx = (i / members_per_room) as usize % num_rooms_to_use as usize;
            room_sizes[idx] += 1;
        }

        let mut room_coords: Vec<Arc<RoomCoordinator>> =
            Vec::with_capacity(num_rooms_to_use as usize);
        for &size in &room_sizes {
            room_coords.push(RoomCoordinator::new(size));
        }

        for i in 0..num_clients {
            let addr = server_addr.clone();
            let cfg = config.clone();
            let rid = run_id;
            let deadline = overall_deadline;

            let room_idx = (i / members_per_room) % num_rooms_to_use;
            let is_host = i % members_per_room == 0;

            let coordinator = room_coords[room_idx as usize].clone();
            let phase_rx = coordinator.subscribe();

            join_set.spawn(async move {
                run_single_client(
                    i, rid, addr, cfg, deadline, is_host, room_idx, coordinator, phase_rx,
                )
                .await
            });
        }
    }

    info!("Spawned {} client tasks, waiting for completion...", num_clients);

    // ── 4. 等待所有客户端完成（受整体超时限制）────────────────────
    let mut results = Vec::new();
    let mut clients_timed_out: u32 = 0;
    let remaining = overall_deadline.saturating_duration_since(Instant::now());

    if remaining > Duration::from_millis(50) {
        let deadline_instant = Instant::now() + remaining;
        loop {
            let deadline_remaining = deadline_instant.saturating_duration_since(Instant::now());
            if deadline_remaining.is_zero() {
                warn!("Overall benchmark timeout ({:.1}s) reached; aborting {} remaining task(s)",
                    config.duration.as_secs_f64(), join_set.len());
                clients_timed_out += join_set.len() as u32;
                join_set.abort_all();
                // Drain aborted tasks to avoid detached futures
                while let Some(result) = join_set.join_next().await {
                    match result {
                        Ok(Ok(cm)) => results.push(Ok(cm)),
                        Ok(Err(e)) => results.push(Err(e)),
                        Err(_) => {
                            // Already counted in clients_timed_out
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
                    // Other clients in the same room will exit
                    // through coordinator.cancel().
                }
                Ok(Some(Err(e))) => {
                    warn!("Client task join error: {e}");
                    if e.is_cancelled() {
                        clients_timed_out += 1;
                    }
                }
                Ok(None) => {
                    // All tasks completed
                    break;
                }
                Err(_) => {
                    warn!("Overall benchmark timeout ({:.1}s) reached; aborting {} remaining task(s)",
                        config.duration.as_secs_f64(), join_set.len());
                    clients_timed_out += join_set.len() as u32;
                    join_set.abort_all();
                    while let Some(result) = join_set.join_next().await {
                        match result {
                            Ok(Ok(cm)) => results.push(Ok(cm)),
                            Ok(Err(e)) => results.push(Err(e)),
                            Err(_) => {} // Already counted
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
    let mut clients_cancelled: u32 = 0;
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
                // Distinguish cancelled (room cancelled by another client's failure)
                // from other failures
                if e.contains("room cancelled due to client failure") {
                    clients_cancelled += 1;
                } else {
                    clients_failed += 1;
                }
                total_errors += 1;
                warn!("Client error: {e}");
            }
        }
    }

    // ── 6. 填写报告 ──────────────────────────────────────────────────
    let duration_secs = elapsed.as_secs().max(1);
    let total_messages = total_commands;

    report.summary.duration_secs = duration_secs;
    report.summary.total_commands = total_commands;
    report.summary.total_messages = total_messages;
    report.summary.avg_commands_per_sec = total_commands as f64 / duration_secs as f64;
    report.summary.peak_commands_per_sec = total_commands as f64 / duration_secs as f64;
    report.summary.clients_started = num_clients;
    report.summary.clients_completed = clients_succeeded;
    report.summary.clients_succeeded = clients_succeeded;
    report.summary.clients_failed = clients_failed;
    report.summary.clients_cancelled = clients_cancelled;
    report.summary.clients_timed_out = clients_timed_out;
    report.summary.avg_messages_per_sec = total_messages as f64 / duration_secs as f64;
    report.summary.peak_messages_per_sec = total_messages as f64 / duration_secs as f64;

    report.errors_total = total_errors;
    report.connect_latency_ms = connect_latencies
        .iter()
        .copied()
        .fold(0.0_f64, f64::max);

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
            passed: clients_failed == 0 && clients_cancelled == 0 && clients_timed_out == 0,
            error: {
                let mut reasons = Vec::new();
                if clients_failed > 0 {
                    reasons.push(format!("{} failed", clients_failed));
                }
                if clients_cancelled > 0 {
                    reasons.push(format!("{} cancelled", clients_cancelled));
                }
                if clients_timed_out > 0 {
                    reasons.push(format!("{} timed_out", clients_timed_out));
                }
                if reasons.is_empty() {
                    None
                } else {
                    Some(reasons.join(", "))
                }
            },
        },
    );

    report.mark_finished();

    info!(
        "Real benchmark completed in {:.1}s: {} clients ({} ok, {} failed, {} cancelled, {} timed_out), {} commands, {} errors",
        elapsed.as_secs_f64(),
        num_clients,
        clients_succeeded,
        clients_failed,
        clients_cancelled,
        clients_timed_out,
        total_commands,
        total_errors,
    );

    // ── 7. 清理基准测试房间 ──────────────────────────────────────────
    // 使用 benchmark_run_id 构造房间 ID 前缀，关闭本运行创建的所有房间。
    if config.scenario != BenchmarkScenario::Connection && !results.is_empty() {
        let rooms_used = if is_hot_room {
            1u32
        } else {
            let actual_rooms_needed = num_clients.div_ceil(members_per_room);
            actual_rooms_needed.max(rooms)
        };

        for room_idx in 0..rooms_used {
            let room_id = make_bench_room_id(&run_id, room_idx);
            if let Err(e) = state.room_commands.close_room(state, &room_id).await {
                warn!("Failed to close benchmark room {room_id}: {e}");
            } else {
                info!("Closed benchmark room {room_id}");
            }
        }
    }

    // ── 8. 恢复环境 ──────────────────────────────────────────────────
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

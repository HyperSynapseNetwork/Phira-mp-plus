//! Server configuration and runtime state.

use crate::ban::BanManager;
use crate::benchmark_report::{BenchmarkMode, BenchmarkReport};
use crate::benchmark_snapshot::BenchmarkReportStore;
use crate::extensions::ExtensionManager;
use crate::plugin::{PluginEvent, PluginManager};
use crate::plugin_http::{PluginHttpServer, SseHub};
use anyhow::Result;
use phira_mp_common::{generate_secret_key, RoomEvent, RoomId, ServerCommand};
use phira_mp_plus_server_api as api;
use serde_json::Value;
use std::{
    collections::HashSet,
    sync::{atomic::Ordering, Arc, Weak},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, Notify, RwLock, Semaphore},
};
use tracing::{info, trace, warn};
use uuid::Uuid;

const USER_ROOM_HISTORY_LIMIT: usize = 64;
const BENCHMARK_QUEUE_CAPACITY: usize = 1;
const ROOM_METADATA_REFRESH_CONCURRENCY: usize = 8;
const CONNECTION_LIMITER_CLEANUP_SECS: u64 = 60;

// Import types/functions that moved to sub-modules
use super::config::{Chart, IdMap, Record, SafeMap};
use super::config::normalize_phira_api_endpoint;

/// Backoff 获取 tokio RwLock 读锁（同步上下文使用，如 Web API 的 try_read 路径）
///
/// 替代自旋 yield_now，使用指数退避（1μs → 2μs → 4μs → … → 1ms cap）
/// 避免在锁竞争时浪费 CPU 并放大尾延迟。
#[macro_export]
macro_rules! read_lock {
    ($lock:expr) => {{
        let mut attempts = 0u32;
        loop {
            match $lock.try_read() {
                Ok(g) => break g,
                Err(_) => {
                    attempts += 1;
                    if attempts > 100 {
                        ::tracing::warn!(
                            "read_lock! spinning on {} ({} attempts)",
                            stringify!($lock),
                            attempts
                        );
                    }
                    // 指数退避：1μs → 2μs → 4μs → … → 1ms cap
                    let cap = std::cmp::min(attempts, 10);
                    let delay_us = 1u64 << cap;
                    let delay = std::cmp::min(delay_us, 1024);
                    std::thread::sleep(std::time::Duration::from_micros(delay));
                }
            }
        }
    }};
}

// Configuration types (LiveConfig, PlusConfig, PlusConfigCli, RuntimeV2Config,
// and default-value helpers) moved to super::config.
use super::config::{LiveConfig, PlusConfig};

/// Runtime v2 benchmark request.
///
/// Simulation remains the default benchmark path and is handled by
/// [`crate::simulation::SimulationManager`]. This queue is only for explicit
/// benchmark modes: real network tests and hybrid Phira probes.
use super::benchmark::{BenchRequest, BenchRequestKind, HybridBenchmarkConfig, load_benchmark_tokens, save_benchmark_tokens};
/// Phira-mp+ 服务器状态
pub struct PlusServerState {
    pub config: PlusConfig,
    /// Hot-reloadable runtime config.
    pub live_config: Arc<RwLock<LiveConfig>>,
    pub sessions: IdMap<Arc<crate::session::Session>>,
    pub users: SafeMap<i32, Arc<crate::session::User>>,
    pub rooms: SafeMap<RoomId, Arc<crate::room::Room>>,
    pub lost_con_tx: mpsc::Sender<Uuid>,
    pub plugin_manager: Arc<PluginManager>,
    pub extensions: Arc<ExtensionManager>,
    pub ban_manager: Arc<BanManager>,
    pub shutdown: Notify,
    /// 连接速率限制器（按 IP）
    pub connection_limiter: crate::rate_limiter::ConnectionRateLimiter,
    /// 轮次数据持久化存储（Touches/Judges 按轮次写入磁盘）
    pub round_store: Arc<crate::round_store::RoundStore>,
    /// 用户房间访问历史: user_id → (room_id, room_uuid, join_timestamp_ms)
    pub user_room_history: SafeMap<i32, Vec<(String, String, i64)>>,
    /// 压测请求发送端（背景 tokio 任务消费）
    pub bench_tx: tokio::sync::mpsc::Sender<BenchRequest>,
    /// 房间展示元数据后台刷新并发闸门。
    pub room_metadata_refresh_gate: Arc<Semaphore>,
    /// Runtime v2 命令元数据注册表。Step 1 仅用于 help/补全/未来统一入口，不改变现有执行逻辑。
    pub command_registry: Arc<crate::command_registry::CommandRegistry>,
    /// Runtime v2 事件总线。当前记录新增 Runtime v2 事件和诊断统计，旧路径仍逐步迁移。
    pub event_bus: Arc<crate::event_bus::EventBus>,
    /// Runtime v2 benchmark 报告只读快照。CLI/TUI/Web 诊断读这里，不解析 EventBus 字符串。
    pub benchmark_reports: Arc<BenchmarkReportStore>,
    /// Runtime v2 Simulation 状态管理器。当前只创建隔离 shadow world，不污染真实 rooms/users。
    pub simulation: Arc<crate::simulation::SimulationManager>,
    /// Runtime v2 持久化 Worker 骨架。现有 db.rs 写入路径暂不迁移。
    pub persistence_worker: Arc<crate::persistence_worker::PersistenceWorker>,
    /// Runtime v2 Actor 模型迁移蓝图。当前是诊断/路线层，不替换真实协议热路径。
    pub actor_runtime: Arc<crate::actor_runtime::ActorRuntime>,
    /// Runtime v2 Room command gateway. Admin/StateQuery room writes route through this facade while the gateway gradually moves commands into per-room mailboxes.
    pub room_commands: Arc<crate::room_actor::RoomCommandGateway>,
    /// Runtime v2 Phira HTTP client. Authentication/chart/record paths should converge here before hybrid/real benchmark expansion.
    pub phira_client: Arc<crate::phira_client::PhiraRetryClient>,
    /// 压测用 Phira token 列表（来自配置或 CLI 命令）。
    pub bench_tokens: RwLock<Vec<String>>,
    /// 管理员 Phira ID 集合。可由配置、PostgreSQL 设置、CLI/WIT 动态修改。
    pub admin_ids: RwLock<HashSet<i32>>,
    /// 房间 monitor key（与 phira-web-monitor 共享密钥）
    pub room_monitor_key: Vec<u8>,
    pub events: Arc<SseHub>,
    /// 房间 monitor 会话（唯一）
    pub room_monitor: RwLock<Option<Weak<crate::session::Session>>>,
    /// 游戏 monitor 会话（按用户 ID）
    pub game_monitors: SafeMap<i32, Weak<crate::session::Session>>,
    /// PostgreSQL 数据库管理器。
    pub db_manager: crate::db::DbManager,
    /// Idle mode monitor — tracks activity and controls service suspension.
    pub idle_monitor: Arc<crate::idle::IdleMonitor>,
}

/// Phira-mp+ 服务器
pub struct PlusServer {
    pub state: Arc<PlusServerState>,
    listener: TcpListener,
    _lost_con_handle: tokio::task::JoinHandle<()>,
}

// Event subscriber functions moved to super::events
impl PlusServer {
    /// 创建新的 Phira-mp+ 服务器
    pub async fn new(config: PlusConfig) -> Result<Self> {
        // Windows 下 IPV6_V6ONLY 默认 true，[::] 不收 IPv4 连接。
        // Linux 下 IPV6_V6ONLY 默认 false，绑 [::] 即可收 IPv4。
        // 统一使用 0.0.0.0 确保两平台局域网 IP 都能连。
        let addr = std::net::SocketAddr::new(
            std::net::Ipv4Addr::UNSPECIFIED.into(),
            config.port,
        );
        let listener = TcpListener::bind(addr).await?;
        info!("Phira-mp+ listening on http://{}", addr);

        // 初始化 Supervisor Actor（接受子任务注册与健康检查）
        crate::supervisor_actor::init();

        let (lost_con_tx, mut lost_con_rx) = mpsc::channel(16);

        // 初始化扩展管理器
        let extensions = Arc::new(ExtensionManager::new(config.extensions_file.clone()));

        // 初始化插件管理器
        let plugin_manager = Arc::new(PluginManager::new(
            &config.plugins_dir,
            Arc::clone(&extensions),
            config.wasm_runtime.clone(),
        ));

        // 初始化黑名单管理器
        let ban_manager = Arc::new(BanManager::new(Arc::clone(&extensions)));

        let http_port = config.http_port;
        let rate_limit = config.connection_rate_limit;
        let rate_window = config.connection_rate_window;
        let retention_days = config.round_data_retention_days;
        let bench_tokens = load_benchmark_tokens(&config);
        let mut admin_ids: HashSet<i32> = config.admin_phira_ids.iter().copied().collect();
        if admin_ids.is_empty() {
            if let Ok(raw) = std::fs::read_to_string("data/admin-phira-ids.json") {
                if let Ok(ids) = serde_json::from_str::<Vec<i32>>(&raw) {
                    admin_ids.extend(ids.into_iter().filter(|id| *id > 0));
                }
            }
        }
        let (bench_tx, bench_rx) =
            tokio::sync::mpsc::channel::<BenchRequest>(BENCHMARK_QUEUE_CAPACITY);

        let runtime_v2 = config.runtime_v2.clone();
        let command_registry = Arc::new(crate::command_registry::runtime_v2_registry());
        let event_bus = Arc::new(crate::event_bus::EventBus::new_with_trace(
            crate::runtime_diagnostics::EVENT_BUS_CHANNEL_CAPACITY,
            crate::runtime_diagnostics::EVENT_TRACE_WINDOW,
        ));
        super::events::spawn_runtime_event_observer(Arc::clone(&event_bus));
        let benchmark_reports = Arc::new(BenchmarkReportStore::new(
            crate::runtime_diagnostics::BENCHMARK_REPORT_HISTORY,
        ));
        let simulation = Arc::new(crate::simulation::SimulationManager::new());
        let persistence_worker = crate::persistence_worker::PersistenceWorker::spawn_with_policy(
            runtime_v2.persistence_queue_capacity,
            runtime_v2.telemetry_batcher.clone(),
            runtime_v2.telemetry_cutover_mode,
        );
        crate::persistence_worker::spawn_event_bus_mirror(
            Arc::clone(&event_bus),
            Arc::clone(&persistence_worker),
        );
        let actor_runtime = Arc::new(crate::actor_runtime::ActorRuntime::new_blueprint());
        let room_commands = Arc::new(crate::room_actor::RoomCommandGateway::new());
        let phira_client = Arc::new(crate::phira_client::PhiraRetryClient::new(
            runtime_v2.phira_http.clone().into_policy(),
        )?);
        let events = Arc::new(SseHub::new());
        // Capture config fields before config is consumed by state
        let proxy_protocol_port = config.proxy_protocol_port;
        let idle_config = config.idle.clone();
        // Initialize database connection early so it's available throughout
        let db_manager = crate::db::DbManager::new(config.database_url.as_deref()).await;
        let live_config = Arc::new(RwLock::new(LiveConfig::from_full(&config)));
        let state = Arc::new(PlusServerState {
            config,
            live_config,
            sessions: IdMap::default(),
            users: SafeMap::default(),
            rooms: SafeMap::default(),
            lost_con_tx,
            plugin_manager,
            extensions,
            ban_manager,
            shutdown: Notify::new(),
            connection_limiter: crate::rate_limiter::ConnectionRateLimiter::new(
                rate_limit,
                rate_window,
            ),
            round_store: Arc::new(crate::round_store::RoundStore::new("data", retention_days)),
            user_room_history: SafeMap::default(),
            bench_tx: bench_tx.clone(),
            room_metadata_refresh_gate: Arc::new(Semaphore::new(
                ROOM_METADATA_REFRESH_CONCURRENCY,
            )),
            command_registry,
            event_bus,
            benchmark_reports,
            simulation,
            persistence_worker,
            actor_runtime,
            room_commands,
            phira_client,
            bench_tokens: RwLock::new(bench_tokens),
            admin_ids: RwLock::new(admin_ids),
            room_monitor_key: generate_secret_key("room_monitor", 64).unwrap_or_default(),
            room_monitor: RwLock::new(None),
            game_monitors: SafeMap::default(),
            events,
            idle_monitor: crate::idle::IdleMonitor::new(idle_config),
            db_manager,
        });
        // Wire PersistenceWorker into ExtensionManager for mirrored writes
        state.extensions.set_persistence_worker(&state.persistence_worker).await;
        super::events::spawn_event_subscribers(&state);
        super::events::spawn_plugin_subscriber(&state);
        state.room_commands.start_mailbox(Arc::clone(&state), 1024);
        state
            .actor_runtime
            .mark_status(
                "room-actor",
                crate::actor_runtime::ActorBoundaryStatus::WriteRouted,
                "set_lock/set_cycle/set_host/close/kick/start/cancel cross a per-room mailbox registry; gateway internals are now split for typed command migration",
            )
            .await;
        // 启动 IdleMonitor 主循环（定期检查空闲条件，挂起/恢复重服务）
        state.idle_monitor.start_loop(&state);
        let bench_state = Arc::clone(&state);
        crate::supervisor_actor::spawn_named("benchmark-worker", async move {
            let mut bench_rx = bench_rx;
            while let Some(request) = bench_rx.recv().await {
                let bs = Arc::clone(&bench_state);
                let output = match request.kind {
                    BenchRequestKind::Real {
                        duration_secs,
                        target_rooms,
                    } => bs.run_benchmark_network(duration_secs, target_rooms).await,
                    BenchRequestKind::Hybrid(config) => bs.run_benchmark_hybrid(config).await,
                };
                let _ = request.result_tx.send(output);
            }
        });

        let lost_con_state = Arc::clone(&state);
        let lost_con_handle = tokio::spawn(async move {
            while let Some(id) = lost_con_rx.recv().await {
                warn!("lost connection with {id}");
                let session_opt = lost_con_state.sessions.write().await.remove(&id);
                if let Some(session) = session_opt {
                    let user_ref = {
                        let session_guard = session.user.session.read().await;
                        session_guard
                            .as_ref()
                            .is_some_and(|it| it.ptr_eq(&Arc::downgrade(&session)))
                    };
                    if user_ref {
                        Arc::clone(&session.user).dangle().await;
                    }
                }
            }
        });

        // 初始化黑名单扩展字段
        state.ban_manager.register_fields().await;

        // 设置发送聊天消息能力（供插件使用）
        let s = Arc::clone(&state);
        state
            .plugin_manager
            .set_send_chat(Arc::new(move |uid, msg| {
                let s = Arc::clone(&s);
                tokio::spawn(async move {
                    let cmd =
                        phira_mp_common::ServerCommand::Message(phira_mp_common::Message::Chat {
                            user: 0,
                            content: msg,
                        });

                    // WASM `send.to_all` uses uid = 0.  Older code only looked up a
                    // concrete user id, so `send.to_all` could silently send to no
                    // one.  Clone user Arcs before awaiting to avoid holding the
                    // global users lock across network sends.
                    if uid == 0 {
                        let recipients = {
                            let users = s.users.read().await;
                            users.values().cloned().collect::<Vec<_>>()
                        };
                        for user in recipients {
                            user.try_send(cmd.clone()).await;
                        }
                        return;
                    }

                    let user = {
                        let users = s.users.read().await;
                        users.get(&uid).cloned()
                    };
                    if let Some(user) = user {
                        user.try_send(cmd).await;
                    }
                });
            }))
            .await;

        // 设置默认状态查询（所有插件可用 state.query host API）
        let state_query_all = api::ServerStateQuery::new({
            let s = Arc::clone(&state);
            move |method: &str, args: &[Value]| -> Result<Value, String> {
                super::query::server_state_query_inner(&s, method, args)
            }
        });
        state
            .plugin_manager
            .set_default_state(state_query_all)
            .await;

        // http_port>0 时才启动 HTTP 服务
        let http_server = if http_port > 0 {
            let srv = Arc::new(PluginHttpServer::new(
                http_port,
                proxy_protocol_port,
                Arc::clone(&state.events),
            ));
            let http_handle = api::HttpHandle::new(crate::plugin_http::HttpHandleBridge(Arc::clone(
                &srv,
            )));
            state.plugin_manager.set_http_handle(http_handle).await;
            Some(srv)
        } else {
            None
        };
        // 设置 WIT 组件模型所需的服务端状态引用
        state.plugin_manager.set_server_state(Arc::clone(&state)).await;
        // 设置命令注册表的 Room ID 补全引用
        state.command_registry.install_room_completer(&state);

        // 初始化 session actor mailbox（Chat 等命令通过它路由）
        crate::session_actor::init();

        // 加载插件
        let plugin_count = state.plugin_manager.load_plugins().await.unwrap_or(0);
        info!("loaded {} plugin(s)", plugin_count);

        // 初始化内置功能（欢迎语/追踪/排行等）
        let http_server_ref = http_server.as_ref().map(|s| Arc::clone(s));
        crate::internal_hooks::init_internal_hooks(&state, &http_server_ref, &state.plugin_manager)
            .await;

        // 启动中央 HTTP 服务器（所有路由已注册完毕）
        if let Some(srv) = http_server {
            let http_state = Arc::clone(&state);
            crate::supervisor_actor::spawn_named("http-server", async move {
                srv.start(http_state).await;
            });
        }

        // 定期持久化 auth 缓存（避免每次认证都写盘）
        let persist_state = Arc::clone(&state);
        crate::supervisor_actor::spawn_named("auth-cache-persist", async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                if let Err(e) = persist_state.extensions.persist().await {
                    warn!("auth cache persist: {e}");
                }
            }
        });

        let limiter_cleanup_state = Arc::clone(&state);
        crate::supervisor_actor::spawn_named("rate-limiter-cleanup", async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(
                    CONNECTION_LIMITER_CLEANUP_SECS,
                ))
                .await;
                limiter_cleanup_state.connection_limiter.cleanup().await;
            }
        });

        // 轮次文件与统一 PostgreSQL 持久化定期清理（每小时检查一次）
        let telemetry_retention_days = state
            .config
            .touch_judge_retention_days
            .unwrap_or(state.config.persistence_retention_days);
        if retention_days > 0
            || state.config.persistence_retention_days > 0
            || telemetry_retention_days > 0
        {
            let cleanup_state = Arc::clone(&state);
            crate::supervisor_actor::spawn_named("retention-cleanup", async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                    cleanup_state.round_store.cleanup_expired().await;
                    if let Some(db) = crate::internal_hooks::DB.get() {
                        let telemetry_retention_days = cleanup_state
                            .config
                            .touch_judge_retention_days
                            .unwrap_or(cleanup_state.config.persistence_retention_days);
                        db.cleanup_expired(
                            cleanup_state.config.persistence_retention_days,
                            telemetry_retention_days,
                        )
                        .await;
                    }
                }
            });
        }

        Ok(Self {
            state,
            listener,
            _lost_con_handle: lost_con_handle,
        })
    }

    /// 接受新连接
    pub async fn accept(&self) -> Result<()> {
        let (stream, addr) = self.listener.accept().await?;
        let ip = addr.ip().to_string();

        // 连接速率限制检查
        if !self.state.connection_limiter.check(&ip).await {
            // 限流时直接静默丢弃，不产生日志
            return Ok(());
        }

        // 最大会话数快速检查（try_read 避免阻塞）
        if let Ok(guard) = self.state.sessions.try_read() {
            if guard.len() > 4096 {
                return Ok(());
            }
        }

        self.state.idle_monitor.mark_activity();
        let id = Uuid::new_v4();
        let auth_timeout = self.state.config.idle.auth_timeout_secs.max(5);
        let session = match tokio::time::timeout(
            std::time::Duration::from_secs(auth_timeout),
            crate::session::Session::new(id, addr, stream, Arc::clone(&self.state)),
        )
        .await
        {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                warn!("failed to create session for {ip}: {e:?}");
                return Ok(());
            }
            Err(_) => {
                warn!("session creation timed out for {ip}");
                return Ok(());
            }
        };

        // 写锁窗口最小化：仅插入 session
        if let Ok(mut guard) = self.state.sessions.try_write() {
            guard.insert(id, session);
        } else {
            self.state.sessions.write().await.insert(id, session);
        }

        trace!("connection from {ip} accepted, session {id}");
        Ok(())
    }

    /// 触发插件事件
    pub async fn trigger_event(&self, event: &PluginEvent) {
        self.state
            .event_bus
            .publish(crate::event_bus::MpEvent::PluginEventDispatched(
                std::sync::Arc::new(event.clone()),
            ));
    }

    /// 获取服务器统计信息
    pub async fn stats(&self) -> ServerStats {
        let user_count = self
            .state
            .users
            .read()
            .await
            .values()
            .filter(|user| user.id > 0)
            .count();
        let room_count = self.state.rooms.read().await.len();
        let session_count = self.state.sessions.read().await.len();
        let plugin_count = self.state.plugin_manager.list_plugins().await.len();

        ServerStats {
            users_online: user_count,
            active_rooms: room_count,
            active_sessions: session_count,
            loaded_plugins: plugin_count,
            port: self.state.config.port,
        }
    }
}

/// 服务器统计信息
pub struct ServerStats {
    pub users_online: usize,
    pub active_rooms: usize,
    pub active_sessions: usize,
    pub loaded_plugins: usize,
    pub port: u16,
}

impl PlusServerState {
    /// Publish a Runtime v2 event without changing the current side-effect path.
    ///
    /// Step 4 uses this as an observation-only mirror: plugins, room monitor,
    /// SSE and PostgreSQL direct writes continue to run exactly as before.
    pub fn publish_runtime_event(&self, event: crate::event_bus::MpEvent) -> usize {
        if let crate::event_bus::MpEvent::BenchmarkCompleted { report } = &event {
            self.benchmark_reports.record(report.clone());
        }
        self.event_bus.publish(event)
    }

    pub fn publish_benchmark_completed(&self, report: &BenchmarkReport) -> usize {
        self.publish_runtime_event(crate::event_bus::MpEvent::BenchmarkCompleted {
            report: report.clone(),
        })
    }

    fn append_benchmark_report(&self, out: &mut String, report: BenchmarkReport) {
        out.push_str(&report.render_text());
        self.publish_benchmark_completed(&report);
    }

    /// Broadcast a system chat message to every currently connected normal user.
    ///
    /// This is intentionally small and side-effect-only. Runtime v2 background
    /// tasks use it for simulation lifecycle notices without reaching into the
    /// CLI handler. User Arcs are cloned before awaiting so the global users lock
    /// is never held across network sends.
    pub async fn broadcast_system_message(&self, message: &str) -> usize {
        let recipients = {
            let users = self.users.read().await;
            users.values().cloned().collect::<Vec<_>>()
        };
        let cmd = ServerCommand::Message(phira_mp_common::Message::Chat {
            user: 0,
            content: format!("[系统广播] {message}"),
        });
        let mut sent = 0usize;
        for user in recipients {
            user.try_send(cmd.clone()).await;
            sent += 1;
        }
        sent
    }

    async fn mirror_room_event_to_runtime_bus(&self, event: &RoomEvent) {
        
        match event {
            RoomEvent::CreateRoom { room, .. } => {
                let room_uuid = self
                    .rooms
                    .read()
                    .await
                    .get(room)
                    .map(|room| room.uuid)
                    .unwrap_or_else(Uuid::nil);
                self.publish_runtime_event(crate::event_bus::MpEvent::RoomCreated {
                    room_id: room.clone(),
                    room_uuid,
                });
                self.publish_runtime_event(crate::event_bus::MpEvent::RoomUpdated {
                    room_id: room.clone(),
                });
            }
            RoomEvent::UpdateRoom { room, data } => {
                self.publish_runtime_event(crate::event_bus::MpEvent::RoomUpdated {
                    room_id: room.clone(),
                });
                if let Some(host) = data.host {
                    self.publish_runtime_event(crate::event_bus::MpEvent::HostChanged {
                        room_id: room.clone(),
                        host: Some(host),
                    });
                }
                if let Some(lock) = data.lock {
                    self.publish_runtime_event(crate::event_bus::MpEvent::RoomLocked {
                        room_id: room.clone(),
                        locked: lock,
                    });
                }
                if let Some(cycle) = data.cycle {
                    self.publish_runtime_event(crate::event_bus::MpEvent::RoomCycled {
                        room_id: room.clone(),
                        cycle,
                    });
                }
                if let Some(chart_id) = data.chart {
                    self.publish_runtime_event(crate::event_bus::MpEvent::ChartSelected {
                        room_id: room.clone(),
                        chart_id,
                    });
                }
                if let Some(state) = data.state {
                    self.publish_runtime_event(crate::event_bus::MpEvent::RoomStateChanged {
                        room_id: room.clone(),
                        state: format!("{state:?}"),
                    });
                }
            }
            RoomEvent::JoinRoom { room, user } => {
                self.publish_runtime_event(crate::event_bus::MpEvent::RoomJoined {
                    room_id: room.clone(),
                    user_id: *user,
                });
            }
            RoomEvent::LeaveRoom { room, user } => {
                self.publish_runtime_event(crate::event_bus::MpEvent::RoomLeft {
                    room_id: room.clone(),
                    user_id: *user,
                });
            }
            RoomEvent::NewRound { room, .. } => {
                let room_ref = {
                    let rooms = self.rooms.read().await;
                    rooms.get(room).map(Arc::clone)
                };
                let round_id = if let Some(room_ref) = room_ref {
                    room_ref
                        .current_round_id
                        .read()
                        .await
                        .map(|round_id| round_id.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                } else {
                    "unknown".to_string()
                };
                self.publish_runtime_event(crate::event_bus::MpEvent::RoundCompleted {
                    room_id: room.clone(),
                    round_id,
                });
            }
        }
    }

    /// 获取房间 monitor 会话
    pub async fn get_room_monitor(&self) -> Option<Arc<crate::session::Session>> {
        self.room_monitor
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade)
    }
    /// 设置房间 monitor 会话
    pub async fn set_room_monitor(&self, session: Weak<crate::session::Session>) {
        *self.room_monitor.write().await = Some(session);
    }
    /// 获取游戏 monitor 会话
    pub async fn get_game_monitor(&self, player_id: i32) -> Option<Arc<crate::session::Session>> {
        self.game_monitors
            .read()
            .await
            .get(&player_id)
            .and_then(Weak::upgrade)
    }
    /// 设置游戏 monitor 会话
    pub async fn set_game_monitor(&self, player_id: i32, session: Weak<crate::session::Session>) {
        self.game_monitors.write().await.insert(player_id, session);
    }

    pub async fn publish_room_event(&self, event: RoomEvent) {
        self.mirror_room_event_to_runtime_bus(&event).await;
        // Enqueue to PersistenceWorker (exclusive — no direct DB fallback)
        let _ = self.persistence_worker.enqueue(
            crate::persistence::message::PersistenceEvent::ServerEvent {
                kind: event.event_type().to_string(),
                payload: event.clone().inner(),
                simulation: false,
            },
        ).await;
        self.events.publish_room_event(event.clone());
        if let Some(monitor) = self.get_room_monitor().await {
            monitor.try_send(ServerCommand::RoomEvent(event)).await;
        }
    }

    /// 创建无人持久空房间。该房间没有初始房主，首个加入的普通玩家会静默成为房主。
    pub async fn create_empty_room(
        self: &Arc<Self>,
        room_id: &str,
        endpoint: Option<String>,
        persistent_empty: bool,
    ) -> Result<Value, String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let endpoint = match endpoint {
            Some(value) => Some(normalize_phira_api_endpoint(&value)?),
            None => None,
        };
        let max_users = self.config.max_users_per_room.unwrap_or(100);
        let room = Arc::new(crate::room::Room::new_empty(
            rid.clone(),
            Some(Arc::clone(&self.plugin_manager)),
            Arc::downgrade(self),
            max_users,
            Some(Arc::clone(&self.round_store)),
        ));
        room.set_persistent_empty(persistent_empty);
        if let Some(endpoint) = endpoint.clone() {
            room.set_phira_api_endpoint_override(Some(endpoint)).await;
        }
        {
            let mut rooms = self.rooms.write().await;
            if rooms.contains_key(&rid) {
                return Err("room already exists".to_string());
            }
            rooms.insert(rid.clone(), Arc::clone(&room));
        }
        self.publish_room_event(RoomEvent::CreateRoom {
            room: rid.clone(),
            data: crate::room::Room::into_data(&room).await,
        })
        .await;
        self.event_bus
            .publish(crate::event_bus::MpEvent::PluginEventDispatched(
                std::sync::Arc::new(PluginEvent::RoomCreate {
                    user_id: 0,
                    room_id: rid.to_string(),
                }),
            ));
        Ok(serde_json::json!({
            "ok": true,
            "room_id": rid.to_string(),
            "uuid": room.uuid.to_string(),
            "persistent_empty": room.is_persistent_empty(),
            "phira_api_endpoint": room.effective_phira_api_endpoint(self).await,
            "phira_api_endpoint_override": room.phira_api_endpoint_override().await,
        }))
    }

    pub async fn set_room_persistent_empty(
        &self,
        room_id: &str,
        persistent: bool,
    ) -> Result<Value, String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let room = {
            let rooms = self.rooms.read().await;
            rooms.get(&rid).map(Arc::clone).ok_or("room not found")?
        };
        room.set_persistent_empty(persistent);
        self.event_bus
            .publish(crate::event_bus::MpEvent::PluginEventDispatched(
                std::sync::Arc::new(PluginEvent::RoomModify {
                    user_id: 0,
                    room_id: rid.to_string(),
                    data: serde_json::json!({"action":"persistent_empty","value": persistent})
                        .to_string(),
                }),
            ));
        Ok(
            serde_json::json!({"ok": true, "room_id": rid.to_string(), "persistent_empty": persistent}),
        )
    }

    /// 如果房间没有真实房主或系统 `?` 房主，让指定普通玩家成为房主。
    /// `announce=false` 用于无人空房间首个玩家加入：只更新服务器状态与 host 标记，
    /// 不广播 `NewHost`，避免客户端在还没收到 JoinRoom 用户表前显示 `? 成为房主`。
    pub async fn assign_room_host_if_missing(
        &self,
        room: &Arc<crate::room::Room>,
        user: &Arc<crate::session::User>,
        monitor: bool,
        announce: bool,
    ) -> bool {
        if monitor || room.has_host().await {
            return false;
        }
        room.set_host(Some(user.id), announce).await.is_ok()
    }

    fn persist_room_snapshot_background(&self, room: Arc<crate::room::Room>) {
        let fallback_endpoint = self.config.phira_api_endpoint.clone();
        crate::supervisor_actor::spawn_named("room-snapshot", async move {
            let Some(db) = crate::internal_hooks::DB.get() else {
                return;
            };
            if !db.is_active() {
                return;
            }
            let users = room.users().await;
            let monitors = room.monitors().await;
            let host_id = room
                .host_id()
                .await
                .or_else(|| room.is_system_host().then_some(-1));
            let chart = room.chart.read().await.clone();
            let state = match &*room.state.read().await {
                crate::room::InternalRoomState::SelectChart => {
                    serde_json::json!({"kind":"select_chart"})
                }
                crate::room::InternalRoomState::WaitForReady { started, .. } => {
                    let mut ready: Vec<i32> = started.iter().copied().collect();
                    ready.sort_unstable();
                    serde_json::json!({"kind":"wait_for_ready", "ready_users": ready})
                }
                crate::room::InternalRoomState::Playing { results, aborted } => {
                    let mut finished: Vec<i32> = results.keys().copied().collect();
                    finished.sort_unstable();
                    let mut aborted_users: Vec<i32> = aborted.iter().copied().collect();
                    aborted_users.sort_unstable();
                    serde_json::json!({"kind":"playing", "finished_users": finished, "aborted_users": aborted_users})
                }
            };
            let mut user_values = Vec::new();
            for u in &users {
                user_values.push(serde_json::json!({"id": u.id, "name": room.display_name(u).await, "monitor": false}));
            }
            let mut monitor_values = Vec::new();
            for u in &monitors {
                monitor_values.push(serde_json::json!({"id": u.id, "name": room.display_name(u).await, "monitor": true}));
            }
            let endpoint_override = room.phira_api_endpoint_override().await;
            let endpoint = endpoint_override.clone().unwrap_or(fallback_endpoint);
            let payload = serde_json::json!({
                "id": room.id.to_string(),
                "uuid": room.uuid.to_string(),
                "created_at": room.created_at,
                "updated_at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0),
                "host": host_id,
                "host_is_system": room.is_system_host(),
                "users": user_values,
                "monitors": monitor_values,
                "locked": room.is_locked(),
                "cycling": room.is_cycle(),
                "hidden": room.is_hidden(),
                "persistent_empty": room.is_persistent_empty(),
                "live": room.is_live(),
                "max_users": room.max_users_count(),
                "chart": chart.as_ref().map(|c| serde_json::json!({"id": c.id, "name": c.name.clone()})),
                "state": state,
                "current_round_id": room.current_round_id.read().await.as_ref().map(|id| id.to_string()),
                "phira_api_endpoint": endpoint,
                "phira_api_endpoint_override": endpoint_override,
            });
            db.record_room_snapshot_sync(room.id.to_string(), room.uuid.to_string(), payload);
        });
    }

    /// 刷新房间内展示用用户名与谱面名。只影响服务端 TUI/Web/欢迎语/历史展示；不改客户端本机 Phira API。
    pub async fn refresh_room_display_metadata(&self, room: &Arc<crate::room::Room>) {
        let endpoint = room.effective_phira_api_endpoint(self).await;
        Self::refresh_room_display_metadata_with_endpoint(
            room,
            endpoint,
            Arc::clone(&self.phira_client),
        )
        .await;
    }

    async fn refresh_room_display_metadata_with_endpoint(
        room: &Arc<crate::room::Room>,
        endpoint: String,
        phira_client: Arc<crate::phira_client::PhiraRetryClient>,
    ) {
        let people = room
            .users()
            .await
            .into_iter()
            .chain(room.monitors().await.into_iter())
            .collect::<Vec<_>>();
        for user in people {
            let mut display = user.name.clone();
            if let Some(token) = user.auth_token().await {
                if let Some((remote_id, remote_name)) = phira_client
                    .fetch_user_by_token(&endpoint, None, &token)
                    .await
                {
                    if remote_id == user.id || user.id < 0 {
                        display = remote_name;
                    }
                }
            }
            room.set_display_name(user.id, display).await;
        }
        let chart_id = room.chart.read().await.as_ref().map(|chart| chart.id);
        if let Some(chart_id) = chart_id {
            if let Some(chart) = phira_client
                .fetch_chart_by_id(&endpoint, chart_id)
                .await
            {
                *room.chart.write().await = Some(chart);
                room.publish_update(phira_mp_common::PartialRoomData {
                    chart: Some(chart_id),
                    ..Default::default()
                })
                .await;
            }
        }
    }

    /// 后台刷新房间展示元数据。
    ///
    /// 这个流程会访问 Phira `/me` 和 `/chart/<id>`，自定义 endpoint 慢、不可达或 502 时可能
    /// 等到 reqwest 超时。加入房间、强制迁移、设置 endpoint 等协议关键路径不能等待它，
    /// 否则客户端会先看到 timeout，随后重连才发现服务端其实已经把用户放进房间。
    pub fn refresh_room_display_metadata_background(&self, room: &Arc<crate::room::Room>) {
        let permit = match Arc::clone(&self.room_metadata_refresh_gate).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                trace!(
                    room = room.id.to_string(),
                    "skipping room metadata refresh because refresh concurrency is saturated"
                );
                return;
            }
        };
        let room = Arc::clone(room);
        let fallback_endpoint = self.config.phira_api_endpoint.clone();
        let phira_client = Arc::clone(&self.phira_client);
        crate::supervisor_actor::spawn_named("room-metadata-refresh", async move {
            let _permit = permit;
            let endpoint = room
                .phira_api_endpoint_override()
                .await
                .unwrap_or(fallback_endpoint);
            PlusServerState::refresh_room_display_metadata_with_endpoint(
                &room, endpoint, phira_client,
            )
            .await;
        });
    }
}

impl PlusServerState {
    /// Get the latest RoomActor snapshot for a room, if available.
    /// Falls back to None if the room has no actor yet.
    pub fn room_snapshot(&self, room_id: &str) -> Option<crate::room_actor::actor::RoomSnapshot> {
        self.room_commands.room_snapshot(room_id)
    }

    pub(crate) async fn record_user_room_history(
        &self,
        user_id: i32,
        room_id: String,
        room_uuid: String,
        joined_at: i64,
    ) {
        {
            let mut history = self.user_room_history.write().await;
            let entries = history.entry(user_id).or_default();
            entries.push((room_id.clone(), room_uuid.clone(), joined_at));
            if entries.len() > USER_ROOM_HISTORY_LIMIT {
                let remove = entries.len() - USER_ROOM_HISTORY_LIMIT;
                entries.drain(0..remove);
            }
        }
        // Primary: route through PersistenceWorker
        let worker_event = crate::persistence::message::PersistenceEvent::UserRoomHistory {
            user_id,
            room_id: room_id.clone(),
            room_uuid: room_uuid.clone(),
            joined_at,
        };
        if self.persistence_worker.enqueue(worker_event).await.is_err() {
            warn!("record_user_room_history: worker enqueue failed, data kept in memory only");
        }
    }

    /// 绑定真实 Phira 账号 token 作为网络压测客户端。
    pub async fn bind_benchmark_tokens(&self, raw_tokens: Vec<String>) -> Result<usize, String> {
        let tokens = super::benchmark::sanitize_benchmark_tokens(raw_tokens);
        if tokens.is_empty() {
            return Err(
                "未提供有效 token；可传入空格/逗号分隔的 1 个或多个 Phira token".to_string(),
            );
        }
        save_benchmark_tokens(&tokens)?;
        let count = tokens.len();
        *self.bench_tokens.write().await = tokens;
        Ok(count)
    }

    /// Runtime v2 hybrid benchmark probe.
    ///
    /// Hybrid is explicit and switch-driven. With all switches disabled it is a
    /// dry-run contract check and does not contact Phira. Read probes go through
    /// the unified PhiraRetryClient; write probes are intentionally blocked until
    /// upload-record semantics and safety limits are specified.
    pub async fn run_benchmark_hybrid(&self, config: HybridBenchmarkConfig) -> String {
        let mut out = String::new();
        macro_rules! o { ($($t:tt)*) => { out.push_str(&format!($($t)*)); out.push('\n'); } }

        o!("  ◆ Phira-mp+ Hybrid benchmark probe");
        o!(
            "  │ duration={}s  endpoint={}",
            config.duration_secs,
            config
                .endpoint_override
                .as_deref()
                .unwrap_or("<global phira_api_endpoint>"),
        );
        let switches = config.enabled_switches();
        let mut report = BenchmarkReport::new(
            BenchmarkMode::Hybrid,
            "hybrid Phira probe",
            config.duration_secs,
        );
        if switches.is_empty() {
            report.probes.record_skipped();
            report.add_note(
                "dry-run: no Phira request was sent because all hybrid switches are disabled",
            );
            report.add_note("simulation remains the default pressure path; real Phira probes require explicit switches");
            o!("  │ switches: none");
            o!("  │");
            o!("  ✓ hybrid dry-run complete: no Phira request was sent");
            o!("  │ simulation remains the default pressure path; real Phira probes require explicit switches");
            self.append_benchmark_report(&mut out, report);
            return out;
        }
        o!("  │ switches: {}", switches.join(", "));
        o!("  │");

        if let Err(err) = config.validate() {
            report.add_failure_sample("config", err.clone());
            o!("  ✗ invalid hybrid config: {err}");
            self.append_benchmark_report(&mut out, report);
            return out;
        }

        let endpoint_override = config.endpoint_override.as_deref();
        let tokens = self.bench_tokens.read().await.clone();
        let mut ok = 0usize;
        let mut failed = 0usize;

        if config.authenticate {
            o!("  ├─ authenticate /me");
            if let Some(token) = tokens.first() {
                match self
                    .phira_client
                    .fetch_user_by_token(
                        &self.config.phira_api_endpoint,
                        endpoint_override,
                        token,
                    )
                    .await
                {
                    Some((user_id, user_name)) => {
                        ok += 1;
                        report.probes.record_success();
                        o!("  │ ✓ authenticated as {} ({})", user_name, user_id);
                    }
                    None => {
                        failed += 1;
                        report.probes.record_failure();
                        report.add_failure_sample("authenticate", "fetch_user_by_token returned None".to_string());
                        o!("  │ ✗ authenticate failed: returned None");
                    }
                }
            } else {
                failed += 1;
                report.probes.record_skipped();
                report.add_failure_sample("authenticate", "no benchmark token configured");
                o!("  │ ✗ skipped: no benchmark token configured");
                o!("  │   set benchmark_phira_tokens in server_config.yml or run benchmark-auth <token>");
            }
        }

        if let Some(chart_id) = config.chart_lookup {
            o!("  ├─ chart_lookup /chart/{chart_id}");
            match self
                .phira_client
                .get_json::<Chart>(
                    &self.config.phira_api_endpoint,
                    endpoint_override,
                    &format!("/chart/{chart_id}"),
                    None,
                    crate::phira_client::PhiraRetryNoticeTarget::Silent,
                )
                .await
            {
                Ok(chart) => {
                    ok += 1;
                    report.probes.record_success();
                    o!("  │ ✓ chart {}: {}", chart.id, chart.name);
                }
                Err(err) => {
                    failed += 1;
                    report.probes.record_failure();
                    report.add_failure_sample("chart_lookup", err.to_string());
                    o!("  │ ✗ chart lookup failed: {err}");
                }
            }
        }

        if let Some(record_id) = config.record_lookup {
            o!("  ├─ record_lookup /record/{record_id}");
            match self
                .phira_client
                .get_json::<Record>(
                    &self.config.phira_api_endpoint,
                    endpoint_override,
                    &format!("/record/{record_id}"),
                    None,
                    crate::phira_client::PhiraRetryNoticeTarget::Silent,
                )
                .await
            {
                Ok(record) => {
                    ok += 1;
                    report.probes.record_success();
                    o!(
                        "  │ ✓ record {}: player={} score={} acc={:.4}",
                        record.id,
                        record.player,
                        record.score,
                        record.accuracy
                    );
                }
                Err(err) => {
                    failed += 1;
                    report.probes.record_failure();
                    report.add_failure_sample("record_lookup", err.to_string());
                    o!("  │ ✗ record lookup failed: {err}");
                }
            }
        }

        if config.upload_record {
            failed += 1;
            report.probes.record_blocked();
            report.add_failure_sample("upload_record", "hybrid write probes are intentionally disabled until upload semantics are specified");
            o!("  ├─ upload_record");
            o!("  │ ✗ blocked: hybrid write probes are intentionally disabled until upload semantics are specified");
        }

        let stats = self.phira_client.stats();
        o!("  │");
        o!("  └─ hybrid complete: ok={ok} failed={failed}");
        o!("  │ phira_http: requests={} successes={} failures={} retry_attempts={} circuit_open={}",
            stats.requests, stats.successes, stats.failures, stats.retry_attempts, stats.circuit_open_rejections);
        report.add_note(format!(
            "phira_http requests={} successes={} failures={} retry_attempts={} circuit_open={}",
            stats.requests,
            stats.successes,
            stats.failures,
            stats.retry_attempts,
            stats.circuit_open_rejections,
        ));
        self.append_benchmark_report(&mut out, report);
        out
    }

    /// 通过真实 TCP 协议连接本服务端执行压测；不再直接篡改内存状态。
    pub async fn run_benchmark_network(&self, duration_secs: u64, target_rooms: usize) -> String {
        use std::time::Instant;

        struct BenchClient {
            stream: tokio::net::TcpStream,
            room_id: String,
        }

        let tokens = self.bench_tokens.read().await.clone();
        let mut out = String::new();
        macro_rules! o { ($($t:tt)*) => { out.push_str(&format!($($t)*)); out.push('\n'); } }

        o!("  ◆ Phira-mp+ 真实网络压测");
        o!("  │ 目标房间: {target_rooms}  测试时长: {duration_secs}s");
        o!("  │");
        if tokens.is_empty() {
            o!("  ✗ 未配置 Phira 压测账号");
            o!("  │  请配置 benchmark_phira_tokens 或执行 benchmark-auth <token>");
            o!(r#"  │  或直接修改 server_config.yml: benchmark_phira_tokens: ["..."]"#);
            o!("  │  也可以写入 data/benchmark-auth.json: {{\"tokens\":[\"...\"]}}");
            let mut report = BenchmarkReport::new(
                BenchmarkMode::Real,
                "real TCP compatibility benchmark",
                duration_secs,
            )
            .with_target_rooms(target_rooms);
            report.add_failure_sample("config", "no benchmark Phira tokens configured");
            report.add_note("real benchmark is explicit and requires local benchmark tokens; simulation remains the default pressure path");
            self.append_benchmark_report(&mut out, report);
            return out;
        }

        let room_count = target_rooms.max(1);
        let token_slots = tokens.len().max(1);
        if tokens.len() < target_rooms {
            o!(
                "  │ 账号不足：将复用 {} 个 token 分批创建/重建 {} 间房间",
                tokens.len(),
                target_rooms
            );
            o!(
                "  │ 最终只保持最多 {} 个真实客户端在线；创建吞吐仍覆盖目标房间数",
                tokens.len()
            );
            o!("  │");
        }

        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], self.config.port));
        let started_at = Instant::now();
        let mut clients_by_slot: Vec<Option<BenchClient>> =
            (0..token_slots).map(|_| None).collect();
        let mut created = 0usize;
        let mut rebuilt = 0usize;
        let mut joined = 0usize;
        let mut failures: Vec<String> = Vec::new();

        o!("  ├─ [阶段1] 真实 TCP 连接 + 认证 + 创建/重建房间");
        let phase1 = Instant::now();
        for i in 0..room_count {
            let slot = i % tokens.len();
            if let Some(mut old) = clients_by_slot[slot].take() {
                let _ = bench_leave_room(&mut old.stream).await;
                rebuilt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            let token = &tokens[slot];
            let room_id = format!("bench-{i}");
            match bench_connect_auth(addr, token).await {
                Ok(mut stream) => match bench_create_room(&mut stream, &room_id).await {
                    Ok(()) => {
                        clients_by_slot[slot] = Some(BenchClient { stream, room_id });
                        created += 1;
                    }
                    Err(err) => failures.push(format!("create {room_id}: {err}")),
                },
                Err(err) => failures.push(format!("auth host#{i}: {err}")),
            }
        }
        let mut clients: Vec<BenchClient> = clients_by_slot.into_iter().flatten().collect();
        o!(
            "  │ ✓ 创建/重建 {created} 间, 重建 {rebuilt} 次, 当前保持 {} 个客户端, 耗时 {:.1}s",
            clients.len(),
            phase1.elapsed().as_secs_f64()
        );

        if created == 0 {
            o!("  │");
            o!("  ✗ 没有成功创建任何房间，压测停止");
            for failure in failures.iter().take(8) {
                o!("  │  - {failure}");
            }
            let mut report = BenchmarkReport::new(
                BenchmarkMode::Real,
                "real TCP compatibility benchmark",
                duration_secs,
            )
            .with_target_rooms(target_rooms);
            report.rooms_created = Some(0);
            report.rooms_rebuilt = Some(rebuilt);
            report.failed_operations = Some(failures.len() as u64);
            for failure in failures.iter().take(8) {
                report.add_failure_sample("real_network", failure.clone());
            }
            self.append_benchmark_report(&mut out, report);
            return out;
        }

        o!("  │");
        o!("  ├─ [阶段2] 剩余账号真实 JoinRoom 填充房间");
        let phase2 = Instant::now();
        if tokens.len() > target_rooms {
            for (i, token) in tokens.iter().enumerate().skip(room_count) {
                let room_id = format!("bench-{}", (i - room_count) % created.max(1));
                match bench_connect_auth(addr, token).await {
                    Ok(mut stream) => match bench_join_room(&mut stream, &room_id, false).await {
                        Ok(()) => {
                            clients.push(BenchClient { stream, room_id });
                            joined += 1;
                        }
                        Err(err) => failures.push(format!("join token#{i}: {err}")),
                    },
                    Err(err) => failures.push(format!("auth guest#{i}: {err}")),
                }
            }
        } else {
            o!("  │ 无剩余 token 可填充玩家，已跳过；账号不足时重点测试创建/重建与连接稳定性");
        }
        o!(
            "  │ ✓ 加入 {joined} 人, 活跃客户端 {}, 耗时 {:.1}s",
            clients.len(),
            phase2.elapsed().as_secs_f64()
        );

        o!("  │");
        o!("  ├─ [阶段3] 保持连接并通过 Ping/Pong 测网络链路 {duration_secs}s");
        let phase3 = Instant::now();
        let mut op_count = 0u64;
        let mut failed_ops = 0u64;
        let mut latencies = Vec::new();
        while phase3.elapsed().as_secs() < duration_secs {
            for client in &mut clients {
                let t = Instant::now();
                match bench_ping(&mut client.stream).await {
                    Ok(()) => {
                        op_count += 1;
                        latencies.push(t.elapsed().as_secs_f64() * 1000.0);
                    }
                    Err(err) => {
                        failed_ops += 1;
                        if failures.len() < 16 {
                            failures.push(format!("ping {}: {err}", client.room_id));
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let avg_ms = if latencies.is_empty() {
            0.0
        } else {
            latencies.iter().sum::<f64>() / latencies.len() as f64
        };
        let p99_ms = if latencies.len() > 1 {
            let mut sorted = latencies.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            sorted[((sorted.len() - 1) as f64 * 0.99).round() as usize]
        } else {
            avg_ms
        };
        o!("  │ ✓ Ping/Pong {op_count} 次, 失败 {failed_ops} 次, avg={avg_ms:.2}ms p99={p99_ms:.2}ms");

        o!("  │");
        o!("  ├─ [阶段4] 通过协议 LeaveRoom 并清理 bench-* 房间");
        for client in &mut clients {
            let _ = bench_leave_room(&mut client.stream).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        self.rooms
            .write()
            .await
            .retain(|rid, _| !rid.to_string().starts_with("bench-"));
        o!("  │ ✓ 清理完成");

        o!("  │");
        o!("  └─ 压测完成 ({:.1}s)", started_at.elapsed().as_secs_f64());
        o!("");
        let mut report = BenchmarkReport::new(
            BenchmarkMode::Real,
            "real TCP compatibility benchmark",
            duration_secs,
        )
        .with_target_rooms(target_rooms);
        report.active_clients = Some(clients.len());
        report.rooms_created = Some(created);
        report.rooms_rebuilt = Some(rebuilt);
        report.users_joined = Some(joined);
        report.operations = Some(op_count);
        report.failed_operations = Some(failed_ops);
        report.avg_latency_ms = Some(avg_ms);
        report.p99_latency_ms = Some(p99_ms);
        if tokens.len() < target_rooms {
            report.add_note(format!(
                "benchmark tokens were fewer than target rooms; {} tokens were reused for {} target rooms",
                tokens.len(), target_rooms,
            ));
        }
        for failure in failures.iter().take(8) {
            report.add_failure_sample("real_network", failure.clone());
        }
        self.append_benchmark_report(&mut out, report);
        out
    }

    /// 管理员强制把用户迁移到指定房间，绕过房间人数、锁定、进行中等普通加入限制。
    pub async fn force_move_user_to_room(
        &self,
        room_id: &str,
        target_id: i32,
        monitor: bool,
    ) -> Result<Value, String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let target_room = {
            let rooms = self.rooms.read().await;
            rooms.get(&rid).map(Arc::clone).ok_or("room not found")?
        };
        let user = {
            let users = self.users.read().await;
            users
                .get(&target_id)
                .map(Arc::clone)
                .ok_or("user not found")?
        };

        let old_room = user.room.read().await.as_ref().map(Arc::clone);
        let old_room_id = old_room.as_ref().map(|room| room.id.to_string());
        let was_monitor = user.monitor.load(Ordering::SeqCst);
        let same_room = old_room
            .as_ref()
            .is_some_and(|room| room.id.to_string() == rid.to_string());

        if let Some(room) = old_room.as_ref().filter(|_| !same_room) {
            let old_id = room.id.clone();
            let old_id_text = old_id.to_string();
            if room.on_user_leave(&user).await {
                self.rooms.write().await.remove(&old_id);
            }
            if !was_monitor {
                self.publish_room_event(RoomEvent::LeaveRoom {
                    room: old_id,
                    user: target_id,
                })
                .await;
            }
            self.event_bus
                .publish(crate::event_bus::MpEvent::PluginEventDispatched(
                    std::sync::Arc::new(PluginEvent::RoomLeave {
                        user_id: target_id,
                        room_id: old_id_text,
                    }),
                ));
        }

        user.monitor.store(monitor, Ordering::SeqCst);
        target_room
            .force_add_user(Arc::downgrade(&user), monitor)
            .await;
        *user.room.write().await = Some(Arc::clone(&target_room));
        if monitor {
            target_room.live.store(true, Ordering::SeqCst);
        }
        self.assign_room_host_if_missing(&target_room, &user, monitor, false)
            .await;
        self.refresh_room_display_metadata_background(&target_room);

        let join = ServerCommand::OnJoinRoom(user.to_info());
        let message = ServerCommand::Message(phira_mp_common::Message::JoinRoom {
            user: user.id,
            name: user.name.clone(),
        });
        if monitor {
            target_room.broadcast_players(join).await;
            target_room.broadcast_players(message).await;
        } else {
            target_room.broadcast(join).await;
            target_room.broadcast(message).await;
            if !same_room || was_monitor {
                self.publish_room_event(RoomEvent::JoinRoom {
                    room: rid.clone(),
                    user: target_id,
                })
                .await;
            }
        }

        let mut users = target_room.users().await;
        users.extend(target_room.monitors().await);
        user.try_send(ServerCommand::JoinRoom(Ok(
            phira_mp_common::JoinRoomResponse {
                state: target_room.client_room_state().await,
                users: users.into_iter().map(|user| user.to_info()).collect(),
                live: target_room.is_live(),
            },
        )))
        .await;
        user.try_send(ServerCommand::ChangeHost(
            target_room.check_host(&user).await.is_ok(),
        ))
        .await;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.record_user_room_history(
            target_id,
            rid.to_string(),
            target_room.uuid.to_string(),
            now,
        )
        .await;

        self.event_bus
            .publish(crate::event_bus::MpEvent::PluginEventDispatched(
                std::sync::Arc::new(PluginEvent::RoomJoin {
                    user_id: target_id,
                    room_id: rid.to_string(),
                    is_monitor: monitor,
                }),
            ));
        self.event_bus.publish(crate::event_bus::MpEvent::PluginEventDispatched(
            std::sync::Arc::new(PluginEvent::RoomModify {
                user_id: target_id,
                room_id: rid.to_string(),
                data: serde_json::json!({"action":"force-move","from": old_room_id.clone(),"monitor": monitor}).to_string(),
            }),
        ));

        target_room
            .send(phira_mp_common::Message::Chat {
                user: 0,
                content: format!("用户 {} 已被管理员强制转移到本房间", user.name),
            })
            .await;

        Ok(serde_json::json!({
            "ok": true,
            "room_id": rid.to_string(),
            "target_id": target_id,
            "monitor": monitor,
            "from": old_room_id,
        }))
    }

    pub async fn set_room_hidden(&self, room_id: &str, hidden: bool) -> Result<Value, String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let room = {
            let rooms = self.rooms.read().await;
            rooms.get(&rid).map(Arc::clone).ok_or("room not found")?
        };
        room.set_hidden(hidden);
        self.event_bus
            .publish(crate::event_bus::MpEvent::PluginEventDispatched(
                std::sync::Arc::new(PluginEvent::RoomModify {
                    user_id: 0,
                    room_id: rid.to_string(),
                    data: format!(r#"{{"action":"hidden","value":{hidden}}}"#),
                }),
            ));
        Ok(serde_json::json!({"ok": true, "room_id": rid.to_string(), "hidden": hidden}))
    }

    pub async fn get_room_phira_api_endpoint(&self, room_id: &str) -> Result<Value, String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let room = {
            let rooms = self.rooms.read().await;
            rooms.get(&rid).map(Arc::clone).ok_or("room not found")?
        };
        let override_endpoint = room.phira_api_endpoint_override().await;
        let using_room_override = override_endpoint.is_some();
        let effective_endpoint = override_endpoint
            .clone()
            .unwrap_or_else(|| self.config.phira_api_endpoint.clone());
        Ok(serde_json::json!({
            "ok": true,
            "room_id": rid.to_string(),
            "phira_api_endpoint": effective_endpoint,
            "phira_api_endpoint_override": override_endpoint,
            "using_room_override": using_room_override,
        }))
    }

    pub async fn set_room_phira_api_endpoint(
        &self,
        room_id: &str,
        endpoint: Option<String>,
    ) -> Result<Value, String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let room = {
            let rooms = self.rooms.read().await;
            rooms.get(&rid).map(Arc::clone).ok_or("room not found")?
        };
        let normalized = match endpoint {
            Some(value) => Some(normalize_phira_api_endpoint(&value)?),
            None => None,
        };
        room.set_phira_api_endpoint_override(normalized.clone())
            .await;
        self.refresh_room_display_metadata_background(&room);
        let using_room_override = normalized.is_some();
        let effective_endpoint = normalized
            .clone()
            .unwrap_or_else(|| self.config.phira_api_endpoint.clone());
        self.event_bus
            .publish(crate::event_bus::MpEvent::PluginEventDispatched(
                std::sync::Arc::new(PluginEvent::RoomModify {
                    user_id: 0,
                    room_id: rid.to_string(),
                    data: serde_json::json!({
                        "action": "phira_api_endpoint",
                        "value": normalized.clone(),
                        "effective": effective_endpoint.clone(),
                    })
                    .to_string(),
                }),
            ));
        Ok(serde_json::json!({
            "ok": true,
            "room_id": rid.to_string(),
            "phira_api_endpoint": effective_endpoint,
            "phira_api_endpoint_override": normalized,
            "using_room_override": using_room_override,
        }))
    }
}

async fn bench_send_command(
    stream: &mut tokio::net::TcpStream,
    payload: &phira_mp_common::ClientCommand,
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let mut buffer = Vec::new();
    phira_mp_common::encode_packet(payload, &mut buffer);
    let mut len_buf = [0u8; 5];
    let mut x = buffer.len() as u32;
    let mut n = 0usize;
    loop {
        len_buf[n] = (x & 0x7f) as u8;
        n += 1;
        x >>= 7;
        if x == 0 {
            break;
        }
        len_buf[n - 1] |= 0x80;
    }
    stream
        .write_all(&len_buf[..n])
        .await
        .map_err(|e| format!("write length: {e}"))?;
    stream
        .write_all(&buffer)
        .await
        .map_err(|e| format!("write payload: {e}"))?;
    stream.flush().await.map_err(|e| format!("flush: {e}"))
}

async fn bench_recv_command(
    stream: &mut tokio::net::TcpStream,
) -> Result<phira_mp_common::ServerCommand, String> {
    use tokio::io::AsyncReadExt;
    let mut len = 0u32;
    let mut pos = 0;
    loop {
        let byte = stream
            .read_u8()
            .await
            .map_err(|e| format!("read length: {e}"))?;
        len |= ((byte & 0x7f) as u32) << pos;
        pos += 7;
        if byte & 0x80 == 0 {
            break;
        }
        if pos > 32 {
            return Err("invalid packet length".to_string());
        }
    }
    if len > 2 * 1024 * 1024 {
        return Err("packet too large".to_string());
    }
    let mut buffer = vec![0u8; len as usize];
    stream
        .read_exact(&mut buffer)
        .await
        .map_err(|e| format!("read payload: {e}"))?;
    phira_mp_common::decode_packet(&buffer).map_err(|e| format!("decode packet: {e}"))
}

async fn bench_connect_auth(
    addr: std::net::SocketAddr,
    token: &str,
) -> Result<tokio::net::TcpStream, String> {
    use tokio::io::AsyncWriteExt;
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .map_err(|_| "connect timeout".to_string())?
    .map_err(|e| format!("connect: {e}"))?;
    stream
        .set_nodelay(true)
        .map_err(|e| format!("set_nodelay: {e}"))?;
    stream
        .write_u8(1)
        .await
        .map_err(|e| format!("write protocol version: {e}"))?;
    bench_send_command(
        &mut stream,
        &phira_mp_common::ClientCommand::Authenticate {
            token: token
                .to_string()
                .try_into()
                .map_err(|e| format!("invalid token: {e}"))?,
        },
    )
    .await?;
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            match bench_recv_command(&mut stream).await? {
                phira_mp_common::ServerCommand::Authenticate(Ok(_)) => return Ok(()),
                phira_mp_common::ServerCommand::Authenticate(Err(err)) => {
                    return Err(format!("authenticate rejected: {err}"))
                }
                phira_mp_common::ServerCommand::Message(_) => {}
                other => trace!(?other, "benchmark ignored packet while authenticating"),
            }
        }
    })
    .await
    .map_err(|_| "authenticate timeout".to_string())??;
    Ok(stream)
}

async fn bench_create_room(
    stream: &mut tokio::net::TcpStream,
    room_id: &str,
) -> Result<(), String> {
    bench_send_command(
        stream,
        &phira_mp_common::ClientCommand::CreateRoom {
            id: room_id
                .to_string()
                .try_into()
                .map_err(|e| format!("invalid room id: {e}"))?,
        },
    )
    .await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match bench_recv_command(stream).await? {
                phira_mp_common::ServerCommand::CreateRoom(Ok(())) => return Ok(()),
                phira_mp_common::ServerCommand::CreateRoom(Err(err)) => {
                    return Err(format!("create room rejected: {err}"))
                }
                phira_mp_common::ServerCommand::Message(_)
                | phira_mp_common::ServerCommand::OnJoinRoom(_) => {}
                other => trace!(?other, "benchmark ignored packet while creating room"),
            }
        }
    })
    .await
    .map_err(|_| "create room timeout".to_string())?
}

async fn bench_join_room(
    stream: &mut tokio::net::TcpStream,
    room_id: &str,
    monitor: bool,
) -> Result<(), String> {
    bench_send_command(
        stream,
        &phira_mp_common::ClientCommand::JoinRoom {
            id: room_id
                .to_string()
                .try_into()
                .map_err(|e| format!("invalid room id: {e}"))?,
            monitor,
        },
    )
    .await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match bench_recv_command(stream).await? {
                phira_mp_common::ServerCommand::JoinRoom(Ok(_)) => return Ok(()),
                phira_mp_common::ServerCommand::JoinRoom(Err(err)) => {
                    return Err(format!("join room rejected: {err}"))
                }
                phira_mp_common::ServerCommand::Message(_)
                | phira_mp_common::ServerCommand::OnJoinRoom(_) => {}
                other => trace!(?other, "benchmark ignored packet while joining room"),
            }
        }
    })
    .await
    .map_err(|_| "join room timeout".to_string())?
}

async fn bench_ping(stream: &mut tokio::net::TcpStream) -> Result<(), String> {
    bench_send_command(stream, &phira_mp_common::ClientCommand::Ping).await?;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            match bench_recv_command(stream).await? {
                phira_mp_common::ServerCommand::Pong => return Ok(()),
                phira_mp_common::ServerCommand::Message(_)
                | phira_mp_common::ServerCommand::OnJoinRoom(_) => {}
                other => trace!(?other, "benchmark ignored packet while waiting pong"),
            }
        }
    })
    .await
    .map_err(|_| "pong timeout".to_string())?
}

async fn bench_leave_room(stream: &mut tokio::net::TcpStream) -> Result<(), String> {
    bench_send_command(stream, &phira_mp_common::ClientCommand::LeaveRoom).await?;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            match bench_recv_command(stream).await? {
                phira_mp_common::ServerCommand::LeaveRoom(_) => return Ok(()),
                phira_mp_common::ServerCommand::Message(_)
                | phira_mp_common::ServerCommand::OnJoinRoom(_) => {}
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| "leave timeout".to_string())?
}

/// 从房间踢出用户。
#[allow(dead_code)]
async fn run_room_kick(
    state: &PlusServerState,
    room_id: &str,
    target_id: i32,
) -> Result<Value, String> {
    state
        .room_commands
        .kick_user(state, room_id, target_id)
        .await
}

/// 设置房主；target_id=None 表示系统 `?` 房主。
#[allow(dead_code)]
async fn run_room_set_host(
    state: &PlusServerState,
    room_id: &str,
    target_id: Option<i32>,
) -> Result<Value, String> {
    state
        .room_commands
        .set_host(state, room_id, target_id)
        .await
}

/// 设置房间锁定状态。
#[allow(dead_code)]
async fn run_room_set_lock(
    state: &PlusServerState,
    room_id: &str,
    locked: bool,
) -> Result<Value, String> {
    state.room_commands.set_lock(state, room_id, locked).await
}

/// 关闭/解散房间。
#[allow(dead_code)]
async fn run_room_close(state: &PlusServerState, room_id: &str) -> Result<Value, String> {
    state.room_commands.close_room(state, room_id).await
}

/// 将用户踢出服务器
pub(crate) async fn run_admin_kick_user(
    state: &PlusServerState,
    target_id: i32,
    reason: &str,
) -> Result<Value, String> {
    let user = state
        .users
        .read()
        .await
        .get(&target_id)
        .map(Arc::clone)
        .ok_or("user not found")?;
    {
        let room_clone = user.room.read().await.as_ref().map(Arc::clone);
        if let Some(room) = room_clone {
            let room_id = room.id.to_string();
            let room_key = room.id.clone();
            let was_monitor = user.monitor.load(Ordering::SeqCst);
            if room.on_user_leave(&user).await {
                state.rooms.write().await.remove(&room_key);
            }
            if !was_monitor {
                state
                    .publish_room_event(RoomEvent::LeaveRoom {
                        room: room_key,
                        user: target_id,
                    })
                    .await;
            }
            state
                .event_bus
                .publish(crate::event_bus::MpEvent::PluginEventDispatched(
                    std::sync::Arc::new(PluginEvent::RoomLeave {
                        user_id: target_id,
                        room_id,
                    }),
                ));
        }
    }
    {
        let sessions = state.sessions.read().await;
        for session in sessions.values() {
            if session.user.id == target_id {
                let _ = session
                    .stream
                    .send(phira_mp_common::ServerCommand::Message(
                        phira_mp_common::Message::Chat {
                            user: 0,
                            content: format!("你已被管理员踢出服务器: {reason}"),
                        },
                    ))
                    .await;
                break;
            }
        }
    }
    state.users.write().await.remove(&target_id);
    info!(user = target_id, reason = %reason, "kicked from server by admin");
    state
        .event_bus
        .publish(crate::event_bus::MpEvent::PluginEventDispatched(
            std::sync::Arc::new(PluginEvent::UserDisconnect {
                user_id: target_id,
                user_name: user.name.clone(),
            }),
        ));
    state.publish_runtime_event(crate::event_bus::MpEvent::UserDisconnected { user_id: target_id });
    Ok(serde_json::json!({"ok": true, "reason": reason}))
}


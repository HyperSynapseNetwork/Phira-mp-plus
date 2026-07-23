//! PlusServerState struct definition (fields only).
//!
//! Extracted from the original `server.rs`.

use crate::ban::BanManager;
use crate::benchmark_snapshot::BenchmarkReportStore;
use crate::extensions::ExtensionManager;
use crate::plugin::PluginManager;
use crate::plugin_http::SseHub;
use phira_mp_common::{generate_secret_key, RoomId};
use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Weak};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex, Notify, RwLock, Semaphore};

use super::benchmark::BenchRequest;
use super::config::{IdMap, LiveConfig, PlusConfig, SafeMap};

pub(crate) const USER_ROOM_HISTORY_LIMIT: usize = 64;
pub(crate) const BENCHMARK_QUEUE_CAPACITY: usize = 1;
pub(crate) const ROOM_METADATA_REFRESH_CONCURRENCY: usize = 8;
pub(crate) const CONNECTION_LIMITER_CLEANUP_SECS: u64 = 60;

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

/// Phira-mp+ 服务器状态
pub struct PlusServerState {
    pub config: PlusConfig,
    /// Hot-reloadable runtime config.
    pub live_config: Arc<RwLock<LiveConfig>>,
    pub sessions: IdMap<Arc<crate::session::Session>>,
    pub users: SafeMap<i32, Arc<crate::session::User>>,
    /// Serializes final user registration/reconnect replacement so concurrent
    /// authentication for one account cannot create two authoritative users.
    pub user_registration_gate: Mutex<()>,
    pub rooms: SafeMap<RoomId, Arc<crate::room::Room>>,
    pub lost_con_tx: mpsc::Sender<Uuid>,
    pub plugin_manager: Arc<PluginManager>,
    pub extensions: Arc<ExtensionManager>,
    pub ban_manager: Arc<BanManager>,
    pub shutdown: Notify,
    /// Persistent lifecycle flag used by accept/auth tasks during ordered shutdown.
    pub shutting_down: AtomicBool,
    /// 连接速率限制器（按 IP）
    pub connection_limiter: crate::rate_limiter::ConnectionRateLimiter,
    /// 轮次数据持久化存储（Touches/Judges 按轮次写入磁盘）
    pub round_store: Arc<crate::round_store::RoundStore>,
    /// 用户房间访问历史: user_id → (room_id, room_uuid, join_timestamp_ms)
    pub user_room_history: SafeMap<i32, Vec<(String, String, i64)>>,
    /// 压测请求发送端（背景 tokio 任务消费）
    pub bench_tx: tokio::sync::mpsc::Sender<BenchRequest>,
    /// Concurrent pre-authentication handshakes. The listener never waits for authentication.
    pub pre_auth_gate: Arc<Semaphore>,
    /// Capacity reservation held for the entire authenticated session lifetime.
    pub session_gate: Arc<Semaphore>,
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
    /// Bounded persistence worker with retry and acknowledged flush/shutdown.
    /// Touch/Judge high-frequency rows retain their explicit telemetry path.
    pub persistence_worker: Arc<crate::persistence_worker::PersistenceWorker>,
    /// Actor-boundary diagnostics. Full RoomState ownership is still migrating.
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
    pub(super) listener: TcpListener,
}

/// 服务器统计信息
pub struct ServerStats {
    pub users_online: usize,
    pub active_rooms: usize,
    pub active_sessions: usize,
    pub loaded_plugins: usize,
    pub port: u16,
}

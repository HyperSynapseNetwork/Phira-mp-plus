//! Server configuration and runtime state.

use crate::ban::BanManager;
use crate::extensions::ExtensionManager;
use crate::plugin::{PluginEvent, PluginManager, WasmRuntimeConfig};
use crate::plugin_http::{PluginHttpServer, SseHub};
use anyhow::Result;
use phira_mp_common::{generate_secret_key, RoomEvent, RoomId, ServerCommand};
use phira_mp_plus_server_api as api;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::{atomic::Ordering, Arc, Weak},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, Notify, RwLock},
};
use tracing::{info, trace, warn};
use uuid::Uuid;

/// Chart information from the Phira API
#[derive(Debug, Deserialize, Clone)]
pub struct Chart {
    pub id: i32,
    pub name: String,
}

/// Record information from the Phira API
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Record {
    pub id: i32,
    pub player: i32,
    pub score: i32,
    pub perfect: i32,
    pub good: i32,
    pub bad: i32,
    pub miss: i32,
    pub max_combo: i32,
    pub accuracy: f32,
    pub full_combo: bool,
    pub std: f32,
    pub std_score: f32,
}

pub type SafeMap<K, V> = RwLock<HashMap<K, V>>;

pub(crate) fn normalize_phira_api_endpoint(value: &str) -> Result<String, String> {
    let endpoint = value.trim().trim_end_matches('/').to_string();
    if endpoint.is_empty() {
        return Err("phira_api_endpoint cannot be empty".to_string());
    }
    let url = reqwest::Url::parse(&endpoint).map_err(|e| format!("invalid phira_api_endpoint: {e}"))?;
    match url.scheme() {
        "http" | "https" => Ok(endpoint),
        other => Err(format!("unsupported phira_api_endpoint scheme: {other}")),
    }
}

pub(crate) fn parse_room_endpoint_value(value: &str) -> Result<Option<String>, String> {
    let raw = value.trim();
    if raw.is_empty()
        || raw.eq_ignore_ascii_case("default")
        || raw.eq_ignore_ascii_case("global")
        || raw.eq_ignore_ascii_case("none")
        || raw.eq_ignore_ascii_case("null")
        || raw.eq_ignore_ascii_case("clear")
        || raw == "全局"
        || raw == "默认"
        || raw == "清除"
    {
        Ok(None)
    } else {
        normalize_phira_api_endpoint(raw).map(Some)
    }
}

/// 测试用 sim 用户 ID 起点（避免与真实用户冲突）
const TEST_USER_ID_BASE: i32 = 1_000_000_000;

/// 自旋获取 tokio RwLock 读锁（同步上下文使用）
macro_rules! read_lock {
    ($lock:expr) => {{
        loop {
            match $lock.try_read() {
                Ok(g) => break g,
                Err(_) => std::thread::yield_now(),
            }
        }
    }};
}
pub type IdMap<V> = SafeMap<Uuid, V>;

/// 自旋获取 tokio RwLock 读锁（同步上下文，如压测）
macro_rules! sync_read {
    ($lock:expr) => {{
        loop {
            match $lock.try_read() {
                Ok(guard) => break guard,
                Err(_) => std::thread::yield_now(),
            }
        }
    }};
}

/// 自旋获取 tokio RwLock 写锁（同步上下文，如压测）
macro_rules! sync_write {
    ($lock:expr) => {{
        loop {
            match $lock.try_write() {
                Ok(guard) => break guard,
                Err(_) => std::thread::yield_now(),
            }
        }
    }};
}

/// Phira-mp+ 增强配置（支持 YAML 文件、环境变量、CLI 参数三层覆盖）
#[derive(Debug, Clone, Deserialize)]
pub struct PlusConfig {
    pub port: u16,
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    #[serde(default)]
    pub monitors: Vec<i32>,
    #[serde(default = "default_plugins_dir")]
    pub plugins_dir: String,
    pub extensions_file: Option<String>,
    #[serde(default = "default_true")]
    pub cli_enabled: bool,
    #[serde(default)]
    pub max_rooms: Option<usize>,
    #[serde(default)]
    pub max_users_per_room: Option<usize>,
    #[serde(default = "default_rate_limit")]
    pub connection_rate_limit: u32,
    #[serde(default = "default_rate_window")]
    pub connection_rate_window: u32,
    #[serde(default)]
    pub server_name: Option<String>,
    #[serde(default)]
    pub admin_token: Option<String>,
    #[serde(default = "default_phira_api")]
    pub phira_api_endpoint: String,
    #[serde(default)]
    pub chat_enabled: bool,
    /// 轮次 Touches/Judges 数据保留天数（0 = 不保留）
    #[serde(default = "default_retention_days")]
    pub round_data_retention_days: u32,
    /// PostgreSQL 数据库连接 URL（如 postgres://user:pass@localhost/dbname）
    /// 未设置时保留旧 JSON/文件回退，但统一结构化持久化需要配置 PostgreSQL。
    #[serde(default)]
    pub database_url: Option<String>,
    /// 统一持久化数据保留天数（0 = 不自动清理 PostgreSQL 历史数据）。
    #[serde(default = "default_persistence_retention_days")]
    pub persistence_retention_days: u32,
    /// Touches/Judges 高频遥测在 PostgreSQL 中的独立保留天数。
    /// 未设置时遵循 `persistence_retention_days`；设置为 0 表示不自动清理遥测。
    #[serde(default)]
    pub touch_judge_retention_days: Option<u32>,
    /// 拥有游戏内 `_+命令` 入口和管理 WIT/API 的 Phira 用户 ID 列表。
    #[serde(default)]
    pub admin_phira_ids: Vec<i32>,
    /// 压测使用的 Phira token 列表。可在配置文件中直接填写，或通过 benchmark-bind 命令写入 data/benchmark-auth.json。
    #[serde(default)]
    pub benchmark_phira_tokens: Vec<String>,
    /// 兼容单账号配置：benchmark_phira_token: "..."。
    #[serde(default)]
    pub benchmark_phira_token: Option<String>,
    /// WASM sandbox/resource limits.
    #[serde(default)]
    pub wasm_runtime: WasmRuntimeConfig,
}

fn default_http_port() -> u16 { 12347 }
fn default_plugins_dir() -> String { "plugins".to_string() }
fn default_true() -> bool { true }
fn default_rate_limit() -> u32 { 30 }
fn default_rate_window() -> u32 { 10 }
fn default_phira_api() -> String { "https://phira.5wyxi.com".to_string() }
fn default_retention_days() -> u32 { 7 }
fn default_persistence_retention_days() -> u32 { 30 }

impl Default for PlusConfig {
    fn default() -> Self {
        Self {
            port: 12346,
            http_port: 12347,
            monitors: vec![2],
            plugins_dir: "plugins".to_string(),
            extensions_file: None,
            cli_enabled: true,
            max_rooms: None,
            max_users_per_room: None,
            connection_rate_limit: 30,
            connection_rate_window: 10,
            server_name: None,
            admin_token: None,
            phira_api_endpoint: "https://phira.5wyxi.com".to_string(),
            chat_enabled: true,
            round_data_retention_days: 7,
            database_url: None,
            persistence_retention_days: 30,
            touch_judge_retention_days: None,
            admin_phira_ids: Vec::new(),
            benchmark_phira_tokens: Vec::new(),
            benchmark_phira_token: None,
            wasm_runtime: WasmRuntimeConfig::default(),
        }
    }
}

impl PlusConfig {
    /// 从 YAML 文件加载配置
    pub fn from_yaml(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read config '{path}': {e}"))?;
        let config: Self = serde_yaml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("failed to parse config '{path}': {e}"))?;
        Ok(config)
    }

    /// 合并 CLI 参数覆盖（非默认值的 CLI 参数覆盖 YAML 配置）
    pub fn merge_cli(mut self, cli: PlusConfigCli) -> Self {
        if cli.port != 12346 { self.port = cli.port; }
        if cli.http_port != 12347 { self.http_port = cli.http_port; }
        if !cli.monitors.is_empty() { self.monitors = cli.monitors; }
        if cli.plugins_dir != "plugins" { self.plugins_dir = cli.plugins_dir; }
        if let Some(ext) = cli.extensions_file { self.extensions_file = Some(ext); }
        if cli.no_cli { self.cli_enabled = false; }
        self
    }
}

/// CLI 覆盖配置（来自命令行参数）
pub struct PlusConfigCli {
    pub port: u16,
    pub http_port: u16,
    pub monitors: Vec<i32>,
    pub plugins_dir: String,
    pub extensions_file: Option<String>,
    pub no_cli: bool,
    pub log_file: String,
}

/// 压测请求: (时长s, 房间数, 结果回传)
type BenchRequest = (u64, usize, std::sync::mpsc::Sender<String>);

const BENCH_AUTH_FILE: &str = "data/benchmark-auth.json";

#[derive(Debug, Default, Deserialize, Serialize)]
struct BenchmarkAuthFile {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    tokens: Vec<String>,
}

fn sanitize_benchmark_tokens<I>(items: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut out: Vec<String> = Vec::new();
    for item in items {
        for token in item.split(|ch: char| ch == ',' || ch == ';' || ch.is_whitespace()) {
            let token = token.trim();
            if token.is_empty() || token.len() > 32 {
                continue;
            }
            if !out.iter().any(|existing| existing.as_str() == token) {
                out.push(token.to_string());
            }
        }
    }
    out
}

fn load_benchmark_tokens(config: &PlusConfig) -> Vec<String> {
    let mut configured = config.benchmark_phira_tokens.clone();
    if let Some(token) = &config.benchmark_phira_token {
        configured.push(token.clone());
    }
    let configured = sanitize_benchmark_tokens(configured);
    if !configured.is_empty() {
        return configured;
    }

    let Ok(content) = std::fs::read_to_string(BENCH_AUTH_FILE) else {
        return Vec::new();
    };
    match serde_json::from_str::<BenchmarkAuthFile>(&content) {
        Ok(file) => {
            let mut tokens = file.tokens;
            if let Some(token) = file.token {
                tokens.push(token);
            }
            sanitize_benchmark_tokens(tokens)
        }
        Err(err) => {
            warn!(path = BENCH_AUTH_FILE, "failed to parse benchmark auth file: {err}");
            Vec::new()
        }
    }
}

fn save_benchmark_tokens(tokens: &[String]) -> Result<(), String> {
    std::fs::create_dir_all("data").map_err(|e| format!("create data directory: {e}"))?;
    let file = BenchmarkAuthFile {
        token: None,
        tokens: tokens.to_vec(),
    };
    let payload = serde_json::to_string_pretty(&file).map_err(|e| format!("serialize benchmark auth: {e}"))?;
    std::fs::write(BENCH_AUTH_FILE, payload).map_err(|e| format!("write {BENCH_AUTH_FILE}: {e}"))
}

#[derive(Debug, Deserialize)]
struct RemotePhiraUserInfo {
    id: i32,
    name: String,
}

async fn fetch_phira_user_name(endpoint: &str, token: &str) -> Option<(i32, String)> {
    let endpoint = endpoint.trim_end_matches('/');
    let url = format!("{endpoint}/me");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()?;
    let response = client
        .get(url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let info = response.json::<RemotePhiraUserInfo>().await.ok()?;
    Some((info.id, info.name))
}

async fn fetch_phira_chart(endpoint: &str, chart_id: i32) -> Option<Chart> {
    let endpoint = endpoint.trim_end_matches('/');
    let url = format!("{endpoint}/chart/{chart_id}");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()?;
    client
        .get(url)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json::<Chart>()
        .await
        .ok()
}


/// Phira-mp+ 服务器状态
pub struct PlusServerState {
    pub config: PlusConfig,
    pub sessions: IdMap<Arc<super::session::Session>>,
    pub users: SafeMap<i32, Arc<super::session::User>>,
    pub rooms: SafeMap<RoomId, Arc<super::room::Room>>,
    pub lost_con_tx: mpsc::Sender<Uuid>,
    pub plugin_manager: Arc<PluginManager>,
    pub extensions: Arc<ExtensionManager>,
    pub ban_manager: Arc<BanManager>,
    pub shutdown: Notify,
    /// 连接速率限制器（按 IP）
    pub connection_limiter: super::rate_limiter::ConnectionRateLimiter,
    /// 轮次数据持久化存储（Touches/Judges 按轮次写入磁盘）
    pub round_store: Arc<super::round_store::RoundStore>,
    /// 用户房间访问历史: user_id → (room_id, room_uuid, join_timestamp_ms)
    pub user_room_history: SafeMap<i32, Vec<(String, String, i64)>>,
    /// 压测请求发送端（背景 tokio 任务消费）
    pub bench_tx: tokio::sync::mpsc::UnboundedSender<BenchRequest>,
    /// Runtime v2 命令元数据注册表。Step 1 仅用于 help/补全/未来统一入口，不改变现有执行逻辑。
    pub command_registry: Arc<crate::command_registry::CommandRegistry>,
    /// Runtime v2 事件总线。当前记录新增 Runtime v2 事件和诊断统计，旧路径仍逐步迁移。
    pub event_bus: Arc<crate::event_bus::EventBus>,
    /// Runtime v2 Simulation 状态管理器。当前只创建隔离 shadow world，不污染真实 rooms/users。
    pub simulation: Arc<crate::simulation::SimulationManager>,
    /// Runtime v2 持久化 Worker 骨架。现有 db.rs 写入路径暂不迁移。
    pub persistence_worker: Arc<crate::persistence_worker::PersistenceWorker>,
    /// 压测用 Phira token 列表（来自配置或 benchmark-bind 命令）。
    pub bench_tokens: RwLock<Vec<String>>,
    /// 管理员 Phira ID 集合。可由配置、PostgreSQL 设置、CLI/WIT 动态修改。
    pub admin_ids: RwLock<HashSet<i32>>,
    /// 房间 monitor key（与 phira-web-monitor 共享密钥）
    pub room_monitor_key: Vec<u8>,
    pub events: Arc<SseHub>,
    /// 房间 monitor 会话（唯一）
    pub room_monitor: RwLock<Option<Weak<super::session::Session>>>,
    /// 游戏 monitor 会话（按用户 ID）
    pub game_monitors: SafeMap<i32, Weak<super::session::Session>>,
}

/// Phira-mp+ 服务器
pub struct PlusServer {
    pub state: Arc<PlusServerState>,
    listener: TcpListener,
    _lost_con_handle: tokio::task::JoinHandle<()>,
}

impl PlusServer {
    /// 创建新的 Phira-mp+ 服务器
    pub async fn new(config: PlusConfig) -> Result<Self> {
        let addrs: &[std::net::SocketAddr] = &[
            std::net::SocketAddr::new(
                std::net::Ipv6Addr::UNSPECIFIED.into(),
                config.port,
            ),
        ];

        let listener = TcpListener::bind(addrs).await?;
        for addr in addrs {
            info!("Phira-mp+ Local Address: http://{}", addr);
        }

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
        let (bench_tx, bench_rx) = tokio::sync::mpsc::unbounded_channel::<BenchRequest>();

        let command_registry = Arc::new(crate::command_registry::runtime_v2_registry());
        let event_bus = Arc::new(crate::event_bus::EventBus::new(1024));
        let simulation = Arc::new(crate::simulation::SimulationManager::new());
        let persistence_worker = crate::persistence_worker::PersistenceWorker::spawn(4096);

        let events = Arc::new(SseHub::new());
        let state = Arc::new(PlusServerState {
            config,
            sessions: IdMap::default(),
            users: SafeMap::default(),
            rooms: SafeMap::default(),
            lost_con_tx,
            plugin_manager,
            extensions,
            ban_manager,
            shutdown: Notify::new(),
            connection_limiter: super::rate_limiter::ConnectionRateLimiter::new(
                rate_limit,
                rate_window,
            ),
            round_store: Arc::new(super::round_store::RoundStore::new(
                "data",
                retention_days,
            )),
            user_room_history: SafeMap::default(),
            bench_tx: bench_tx.clone(),
            command_registry,
            event_bus,
            simulation,
            persistence_worker,
            bench_tokens: RwLock::new(bench_tokens),
            admin_ids: RwLock::new(admin_ids),
            room_monitor_key: generate_secret_key("room_monitor", 64).unwrap_or_default(),
            room_monitor: RwLock::new(None),
            game_monitors: SafeMap::default(),
            events,
        });
        let bench_state = Arc::clone(&state);
        tokio::spawn(async move {
            let mut bench_rx = bench_rx;
            while let Some((duration, rooms, result_tx)) = bench_rx.recv().await {
                let bs = Arc::clone(&bench_state);
                let output = bs.run_benchmark_network(duration, rooms).await;
                let _ = result_tx.send(output);
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
        state.plugin_manager.set_send_chat(Arc::new(move |uid, msg| {
            let s = Arc::clone(&s);
            tokio::spawn(async move {
                let cmd = phira_mp_common::ServerCommand::Message(
                    phira_mp_common::Message::Chat { user: 0, content: msg },
                );

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
        })).await;

        // 设置默认状态查询（所有插件可用 state.query host API）
        let state_query_all = api::ServerStateQuery::new({
            let s = Arc::clone(&state);
            move |method: &str, args: &[Value]| -> Result<Value, String> {
                server_state_query_inner(&s, method, args)
            }
        });
        state.plugin_manager.set_default_state(state_query_all).await;

        let http_server = Arc::new(PluginHttpServer::new(
            http_port,
            Arc::clone(&state.events),
        ));
        let http_handle = api::HttpHandle::new(crate::plugin_http::HttpHandleBridge(Arc::clone(&http_server)));
        state.plugin_manager.set_http_handle(http_handle).await;

        // 加载插件
        let plugin_count = state.plugin_manager.load_plugins().await.unwrap_or(0);
        info!("loaded {} plugin(s)", plugin_count);

        let state_for_webapi = Arc::clone(&state);
        let webapi_state_query = api::ServerStateQuery::new(move |method: &str, args: &[Value]| {
            server_state_query(&state_for_webapi, method, args)
        });
        let http_for_webapi = Arc::clone(&http_server);
        let state_for_webapi2 = Arc::clone(&state);
        let sq = webapi_state_query.clone();
        let sq_rooms = sq.clone();
        let state_rooms = Arc::clone(&state_for_webapi2);
        http_for_webapi.register_route_sync("/api/rooms", Arc::new(move |_, _| {
            let rooms = sq_rooms.call("rooms.list", &[]).map_err(|e| (500u16, e))?;
            let online_count = state_rooms.users.try_read().map(|g| g.len()).unwrap_or(0);
            Ok(serde_json::json!({
                "rooms": rooms,
                "player_count": online_count,
                "total_players": crate::internal_hooks::player_count(),
            }))
        }));
        let runtime_state = Arc::clone(&state_for_webapi2);
        http_for_webapi.register_route_sync("/api/runtime", Arc::new(move |_, _| {
            server_state_query_inner(&runtime_state, "runtime.status", &[])
                .map_err(|e| (500u16, e))
        }));
        let simulation_state = Arc::clone(&state_for_webapi2);
        http_for_webapi.register_route_sync("/api/simulation", Arc::new(move |_, _| {
            server_state_query_inner(&simulation_state, "simulation.status", &[])
                .map_err(|e| (500u16, e))
        }));
        let simulation_world_state = Arc::clone(&state_for_webapi2);
        http_for_webapi.register_route_sync("/api/simulation/world", Arc::new(move |_, _| {
            server_state_query_inner(&simulation_world_state, "simulation.world", &[serde_json::json!(20)])
                .map_err(|e| (500u16, e))
        }));
        // GET /api/players/all — 所有连接过服务器的玩家
        http_for_webapi.register_route_sync("/api/players/all", Arc::new(move |_, _| {
            let players: Vec<i32> = crate::internal_hooks::all_players()
                .into_iter().map(|(id, _)| id).collect();
            Ok(serde_json::json!({"total": players.len(), "players": players}))
        }));
        let s2 = Arc::clone(&state_for_webapi2);
        http_for_webapi.register_route_sync("/api/rooms/<name>", Arc::new(move |_, params| {
            let name = params.first().cloned().unwrap_or_default();
            server_state_query_inner(&s2, "rooms.by_name", &[serde_json::json!(name)])
                .map_err(|e| (500u16, e))
        }));
        let sq = webapi_state_query.clone();
        http_for_webapi.register_route_sync("/api/user_name/<id>", Arc::new(move |_, params| {
            let uid: i32 = params.first().and_then(|p| p.parse().ok()).unwrap_or(0);
            sq.call("user_name", &[serde_json::json!(uid)]).map_err(|e| (500u16, e))
        }));
        // 内置 benchmark/benchmark-bind 已由 CLI 核心直接处理；
        // test.* WIT/host API 仍保留给插件和自动化调用。

        // 初始化内置功能（欢迎语/追踪/排行等）
        crate::internal_hooks::init_internal_hooks(&state, &http_server, &state.plugin_manager).await;

        // 启动中央 HTTP 服务器（所有路由已注册完毕）
        let http_state = Arc::clone(&state);
        tokio::spawn(async move {
            http_server.start(http_state).await;
        });

        // 定期持久化 auth 缓存（避免每次认证都写盘）
        let persist_state = Arc::clone(&state);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                if let Err(e) = persist_state.extensions.persist().await {
                    warn!("auth cache persist: {e}");
                }
            }
        });

        // 轮次文件与统一 PostgreSQL 持久化定期清理（每小时检查一次）
        let telemetry_retention_days = state
            .config
            .touch_judge_retention_days
            .unwrap_or(state.config.persistence_retention_days);
        if retention_days > 0 || state.config.persistence_retention_days > 0 || telemetry_retention_days > 0 {
            let cleanup_state = Arc::clone(&state);
            tokio::spawn(async move {
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
                        ).await;
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

        let id = Uuid::new_v4();
        let session = match super::session::Session::new(id, addr, stream, Arc::clone(&self.state)).await {
            Ok(s) => s,
            Err(e) => {
                warn!("failed to create session for {ip}: {e:?}");
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
        self.state.plugin_manager.trigger(event).await;
    }

    /// 获取服务器统计信息
    pub async fn stats(&self) -> ServerStats {
        let user_count = self.state.users.read().await.values().filter(|user| user.id > 0).count();
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
    /// 获取房间 monitor 会话
    pub async fn get_room_monitor(&self) -> Option<Arc<super::session::Session>> {
        self.room_monitor.read().await.as_ref().and_then(Weak::upgrade)
    }
    /// 设置房间 monitor 会话
    pub async fn set_room_monitor(&self, session: Weak<super::session::Session>) {
        *self.room_monitor.write().await = Some(session);
    }
    /// 获取游戏 monitor 会话
    pub async fn get_game_monitor(&self, player_id: i32) -> Option<Arc<super::session::Session>> {
        self.game_monitors.read().await.get(&player_id).and_then(Weak::upgrade)
    }
    /// 设置游戏 monitor 会话
    pub async fn set_game_monitor(&self, player_id: i32, session: Weak<super::session::Session>) {
        self.game_monitors.write().await.insert(player_id, session);
    }

    pub async fn publish_room_event(&self, event: RoomEvent) {
        if let Some(db) = crate::internal_hooks::DB.get() {
            let (room_id, user_id) = match &event {
                RoomEvent::CreateRoom { room, .. }
                | RoomEvent::UpdateRoom { room, .. }
                | RoomEvent::NewRound { room, .. } => (Some(room.to_string()), None),
                RoomEvent::JoinRoom { room, user }
                | RoomEvent::LeaveRoom { room, user } => (Some(room.to_string()), Some(*user)),
            };
            db.record_room_event_sync(event.event_type(), room_id.clone(), user_id, event.clone().inner());
            if let Some(room_id) = room_id {
                if let Ok(rid) = room_id.clone().try_into() {
                    if let Some(room) = self.rooms.read().await.get(&rid).map(Arc::clone) {
                        self.persist_room_snapshot_background(room);
                    }
                }
            }
        }
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
        let rid: RoomId = room_id.to_string().try_into().map_err(|_| "invalid room_id".to_string())?;
        let endpoint = match endpoint {
            Some(value) => Some(normalize_phira_api_endpoint(&value)?),
            None => None,
        };
        let max_users = self.config.max_users_per_room.unwrap_or(8);
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
        }).await;
        self.plugin_manager.trigger(&PluginEvent::RoomCreate {
            user_id: 0,
            room_id: rid.to_string(),
        }).await;
        Ok(serde_json::json!({
            "ok": true,
            "room_id": rid.to_string(),
            "uuid": room.uuid.to_string(),
            "persistent_empty": room.is_persistent_empty(),
            "phira_api_endpoint": room.effective_phira_api_endpoint(self).await,
            "phira_api_endpoint_override": room.phira_api_endpoint_override().await,
        }))
    }

    pub async fn set_room_persistent_empty(&self, room_id: &str, persistent: bool) -> Result<Value, String> {
        let rid: RoomId = room_id.to_string().try_into().map_err(|_| "invalid room_id".to_string())?;
        let room = {
            let rooms = self.rooms.read().await;
            rooms.get(&rid).map(Arc::clone).ok_or("room not found")?
        };
        room.set_persistent_empty(persistent);
        self.plugin_manager.trigger(&PluginEvent::RoomModify {
            user_id: 0,
            room_id: rid.to_string(),
            data: serde_json::json!({"action":"persistent_empty","value": persistent}).to_string(),
        }).await;
        Ok(serde_json::json!({"ok": true, "room_id": rid.to_string(), "persistent_empty": persistent}))
    }

    /// 如果房间没有真实房主或系统 `?` 房主，让指定普通玩家成为房主。
    /// `announce=false` 用于无人空房间首个玩家加入：只更新服务器状态与 host 标记，
    /// 不广播 `NewHost`，避免客户端在还没收到 JoinRoom 用户表前显示 `? 成为房主`。
    pub async fn assign_room_host_if_missing(
        &self,
        room: &Arc<crate::room::Room>,
        user: &Arc<super::session::User>,
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
        tokio::spawn(async move {
            let Some(db) = crate::internal_hooks::DB.get() else { return; };
            if !db.is_active() { return; }
            let users = room.users().await;
            let monitors = room.monitors().await;
            let host_id = room.host_id().await.or_else(|| room.is_system_host().then_some(-1));
            let chart = room.chart.read().await.clone();
            let state = match &*room.state.read().await {
                crate::room::InternalRoomState::SelectChart => serde_json::json!({"kind":"select_chart"}),
                crate::room::InternalRoomState::WaitForReady { started } => {
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
        Self::refresh_room_display_metadata_with_endpoint(room, endpoint).await;
    }

    async fn refresh_room_display_metadata_with_endpoint(room: &Arc<crate::room::Room>, endpoint: String) {
        let people = room.users().await.into_iter().chain(room.monitors().await.into_iter()).collect::<Vec<_>>();
        for user in people {
            let mut display = user.name.clone();
            if let Some(token) = user.auth_token().await {
                if let Some((remote_id, remote_name)) = fetch_phira_user_name(&endpoint, &token).await {
                    if remote_id == user.id || user.id < 0 {
                        display = remote_name;
                    }
                }
            }
            room.set_display_name(user.id, display).await;
        }
        let chart_id = room.chart.read().await.as_ref().map(|chart| chart.id);
        if let Some(chart_id) = chart_id {
            if let Some(chart) = fetch_phira_chart(&endpoint, chart_id).await {
                *room.chart.write().await = Some(chart);
                room.publish_update(phira_mp_common::PartialRoomData {
                    chart: Some(chart_id),
                    ..Default::default()
                }).await;
            }
        }
    }

    /// 后台刷新房间展示元数据。
    ///
    /// 这个流程会访问 Phira `/me` 和 `/chart/<id>`，自定义 endpoint 慢、不可达或 502 时可能
    /// 等到 reqwest 超时。加入房间、强制迁移、设置 endpoint 等协议关键路径不能等待它，
    /// 否则客户端会先看到 timeout，随后重连才发现服务端其实已经把用户放进房间。
    pub fn refresh_room_display_metadata_background(&self, room: &Arc<crate::room::Room>) {
        let room = Arc::clone(room);
        let fallback_endpoint = self.config.phira_api_endpoint.clone();
        tokio::spawn(async move {
            let endpoint = room
                .phira_api_endpoint_override()
                .await
                .unwrap_or(fallback_endpoint);
            PlusServerState::refresh_room_display_metadata_with_endpoint(&room, endpoint).await;
        });
    }
}

impl PlusServerState {

    pub async fn admin_id_list(&self) -> Vec<i32> {
        let mut ids: Vec<i32> = self.admin_ids.read().await.iter().copied().collect();
        ids.sort_unstable();
        ids
    }

    pub async fn is_admin_id(&self, user_id: i32) -> bool {
        self.admin_ids.read().await.contains(&user_id)
    }

    async fn persist_admin_ids(&self) {
        let ids = self.admin_id_list().await;
        if let Some(db) = crate::internal_hooks::DB.get() {
            if let Err(err) = db.set_admin_ids(&ids).await {
                warn!("persist admin ids failed: {err}");
            }
        }
        let _ = std::fs::create_dir_all("data");
        if let Ok(json) = serde_json::to_string_pretty(&ids) {
            let _ = std::fs::write("data/admin-phira-ids.json", json);
        }
    }

    pub async fn set_admin_ids(&self, ids: Vec<i32>) -> Vec<i32> {
        {
            let mut guard = self.admin_ids.write().await;
            guard.clear();
            guard.extend(ids.into_iter().filter(|id| *id > 0));
        }
        self.persist_admin_ids().await;
        self.admin_id_list().await
    }

    pub async fn add_admin_id(&self, user_id: i32) -> Vec<i32> {
        if user_id > 0 {
            self.admin_ids.write().await.insert(user_id);
        }
        self.persist_admin_ids().await;
        self.admin_id_list().await
    }

    pub async fn remove_admin_id(&self, user_id: i32) -> Vec<i32> {
        self.admin_ids.write().await.remove(&user_id);
        self.persist_admin_ids().await;
        self.admin_id_list().await
    }

    /// 运行压测（同步版 — 在 ServerStateQuery 线程中调用，无 tokio）
    pub fn run_benchmark_sync(self: &Arc<Self>, duration_secs: u64, target_rooms: usize) -> String {
        use std::time::Instant;
        let started_at = Instant::now();
        let mut out = String::new();
        macro_rules! o { ($($t:tt)*) => { out.push_str(&format!($($t)*)); out.push('\n'); } }

        info!(target_rooms, duration_secs, "benchmark started");

        o!("  ◆ Phira-mp+ 服务端压测");
        o!("  │ 目标房间: {target_rooms}  测试时长: {duration_secs}s");
        o!("  │");
        o!("  ├─ [阶段1] 创建房间");
        let t0 = Instant::now();
        let mut created = 0usize;
        for i in 0..target_rooms {
            let rid_str = format!("bench-{i}");
            let rid: phira_mp_common::RoomId = match rid_str.to_string().try_into() {
                Ok(r) => r,
                Err(_) => continue,
            };
            let test_uid = TEST_USER_ID_BASE + i as i32;
            let test_user = Arc::new(super::session::User::new(
                test_uid, format!("bench#{i}"),
                crate::l10n::Language::default(),
                Arc::clone(self),
                None,
            ));
            sync_write!(self.users).insert(test_uid, Arc::clone(&test_user));

            let room = Arc::new(crate::room::Room::new(
                rid.clone(), Arc::downgrade(&test_user),
                Some(Arc::clone(&self.plugin_manager)),
                Arc::downgrade(self),
                self.config.max_users_per_room.unwrap_or(8), None,
            ));
            let mut rooms = sync_write!(self.rooms);
            if let std::collections::hash_map::Entry::Vacant(e) = rooms.entry(rid) {
                e.insert(room);
                created += 1;
            }
            drop(rooms);

            if created > 0 && created % 50 == 0 {
                let e = t0.elapsed().as_secs_f64();
                if e > 0.0 { o!("  │  已创建 {created} 间 ({:.0} 间/秒)", created as f64 / e); }
            }
        }
        let phase1 = t0.elapsed();
        let create_rate = if phase1.as_secs_f64() > 0.0 { created as f64 / phase1.as_secs_f64() } else { 0.0 };
        o!("  │ ✓ 创建 {created} 间, 耗时 {:.1}s ({:.0} 间/秒)", phase1.as_secs_f64(), create_rate);

        info!("benchmark phase: room creation complete");
        o!("  │");
        o!("  ├─ [阶段2] 填充用户 (每房间+4人)");
        info!("benchmark phase: filling users");
        let t0 = Instant::now();
        let mut joined = 0usize;
        let rooms_snapshot: Vec<Arc<crate::room::Room>> = sync_read!(self.rooms).values().map(Arc::clone).collect();
        for room in rooms_snapshot.iter().take(created) {
            for _ in 0..4 {
                let test_uid = TEST_USER_ID_BASE + 1_000_000 + joined as i32;
                let test_user = Arc::new(super::session::User::new(
                    test_uid, format!("player#{joined}"),
                    crate::l10n::Language::default(), Arc::clone(self), None,
                ));
                sync_write!(self.users).insert(test_uid, Arc::clone(&test_user));
                // add_user 是 async — 用 try_block 绕过
                let weak = Arc::downgrade(&test_user);
                let mut uguard = sync_write!(room.users);
                uguard.retain(|it| it.strong_count() > 0);
                if uguard.len() < room.max_users_count() { uguard.push(weak); }
                joined += 1;
            }
        }
        let phase2 = t0.elapsed();
        o!("  │ ✓ 加入 {joined} 人, 耗时 {:.1}s ({:.0} 人/秒)", phase2.as_secs_f64(), joined as f64 / phase2.as_secs_f64().max(0.001));

        info!("benchmark phase: pressure loop");
        o!("  │");
        o!("  ├─ [阶段3] 压力保持 {duration_secs}s");
        let t0 = Instant::now();
        let mut query_latencies: Vec<f64> = Vec::new();
        let mut op_count = 0u64;

        while t0.elapsed().as_secs() < duration_secs {
            let mut batch_ops = 0u64;
            let mut batch_lat = 0.0f64;
            for _ in 0..50 {
                let q = Instant::now();
                let _ = sync_read!(self.rooms).len();
                batch_lat += q.elapsed().as_secs_f64() * 1_000_000.0;
                batch_ops += 1;
            }
            for _ in 0..50 {
                let q = Instant::now();
                let _ = sync_read!(self.users).len();
                batch_lat += q.elapsed().as_secs_f64() * 1_000_000.0;
                batch_ops += 1;
            }
            query_latencies.push(batch_lat / batch_ops as f64);
            op_count += batch_ops;

            let elapsed = t0.elapsed().as_secs_f64();
            if elapsed > 0.0 && op_count % 1000 == 0 {
                o!("  │  运行 {elapsed:.0}s | 操作 {op_count} | {:.0} ops/s", batch_ops as f64 / (batch_lat / 1_000_000.0).max(0.001));
            }
        }
        let _phase3 = t0.elapsed();
        let avg_lat = if !query_latencies.is_empty() {
            query_latencies.iter().sum::<f64>() / query_latencies.len() as f64
        } else { 0.0 };
        let mut p99 = avg_lat;
        if query_latencies.len() > 10 {
            let mut s = query_latencies.clone();
            s.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap());
            p99 = s[(s.len() as f64 * 0.99) as usize];
        }
        o!("  │ ✓ 操作 {op_count} 次 | avg={avg_lat:.0}µs p99={p99:.0}µs");

        o!("  │");
        o!("  ├─ [阶段4] 清理测试数据");
        info!("benchmark phase: cleanup");
        let t4 = Instant::now();
        sync_write!(self.rooms).retain(|rid, _| !rid.to_string().starts_with("bench-"));
        sync_write!(self.users).retain(|id, _| *id < TEST_USER_ID_BASE || *id >= TEST_USER_ID_BASE + 10_000_000);
        o!("  │ ✓ 清理完成 ({:.1}s)", t4.elapsed().as_secs_f64());

        let total = started_at.elapsed();
        o!("  │");
        o!("  └─ 压测完成 ({:.1}s)", total.as_secs_f64());
        o!("");
        o!("  ◆ 报告");
        o!("  │  房间创建: {create_rate:.0} 间/秒 ({created} 间)");
        o!("  │  用户加入: {:.0} 人/秒 ({joined} 人)", joined as f64 / phase2.as_secs_f64().max(0.001));
        o!("  │  查询延迟: avg={avg_lat:.0}µs  p99={p99:.0}µs");
        o!("  │  稳定运行: {duration_secs}s 操作 {op_count} 次");

        let max_rooms = (1000.0 / avg_lat.max(1.0) * 100.0) as usize;
        let rec_users = if avg_lat < 10.0 { 8 } else if avg_lat < 50.0 { 6 } else { 4 };
        o!("  │");
        o!("  ├─ 推荐");
        o!("  │  · 最大稳定房间: ~{max_rooms}  每房间上限: {rec_users}  同时游玩: ~{}", max_rooms * rec_users);
        o!("  │  · 调整 server_config.yml 后重启对比");
        out
    }

    /// 清理压测残留数据（同步版）
    pub fn cleanup_benchmark_sync(&self) {
        sync_write!(self.rooms).retain(|rid, _| !rid.to_string().starts_with("bench-"));
        sync_write!(self.users).retain(|id, _| *id < TEST_USER_ID_BASE || *id >= TEST_USER_ID_BASE + 10_000_000);
    }

    /// 绑定真实 Phira 账号 token 作为网络压测客户端。
    pub async fn bind_benchmark_tokens(&self, raw_tokens: Vec<String>) -> Result<usize, String> {
        let tokens = sanitize_benchmark_tokens(raw_tokens);
        if tokens.is_empty() {
            return Err("未提供有效 token；可传入空格/逗号分隔的 1 个或多个 Phira token".to_string());
        }
        save_benchmark_tokens(&tokens)?;
        let count = tokens.len();
        *self.bench_tokens.write().await = tokens;
        Ok(count)
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
            o!("  │  请先执行: benchmark-bind <token1[,token2...]>");
            o!("  │  或直接修改 server_config.yml: benchmark_phira_tokens: [\"...\"]");
            o!("  │  也可以写入 {BENCH_AUTH_FILE}: {{\"tokens\":[\"...\"]}}");
            return out;
        }

        let room_count = target_rooms.max(1);
        let token_slots = tokens.len().max(1);
        if tokens.len() < target_rooms {
            o!("  │ 账号不足：将复用 {} 个 token 分批创建/重建 {} 间房间", tokens.len(), target_rooms);
            o!("  │ 最终只保持最多 {} 个真实客户端在线；创建吞吐仍覆盖目标房间数", tokens.len());
            o!("  │");
        }

        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], self.config.port));
        let started_at = Instant::now();
        let mut clients_by_slot: Vec<Option<BenchClient>> = (0..token_slots).map(|_| None).collect();
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
        o!("  │ ✓ 创建/重建 {created} 间, 重建 {rebuilt} 次, 当前保持 {} 个客户端, 耗时 {:.1}s", clients.len(), phase1.elapsed().as_secs_f64());

        if created == 0 {
            o!("  │");
            o!("  ✗ 没有成功创建任何房间，压测停止");
            for failure in failures.iter().take(8) {
                o!("  │  - {failure}");
            }
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
        o!("  │ ✓ 加入 {joined} 人, 活跃客户端 {}, 耗时 {:.1}s", clients.len(), phase2.elapsed().as_secs_f64());

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
        let avg_ms = if latencies.is_empty() { 0.0 } else { latencies.iter().sum::<f64>() / latencies.len() as f64 };
        let p99_ms = if latencies.len() > 1 {
            let mut sorted = latencies.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            sorted[((sorted.len() - 1) as f64 * 0.99).round() as usize]
        } else { avg_ms };
        o!("  │ ✓ Ping/Pong {op_count} 次, 失败 {failed_ops} 次, avg={avg_ms:.2}ms p99={p99_ms:.2}ms");

        o!("  │");
        o!("  ├─ [阶段4] 通过协议 LeaveRoom 并清理 bench-* 房间");
        for client in &mut clients {
            let _ = bench_leave_room(&mut client.stream).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        self.rooms.write().await.retain(|rid, _| !rid.to_string().starts_with("bench-"));
        o!("  │ ✓ 清理完成");

        o!("  │");
        o!("  └─ 压测完成 ({:.1}s)", started_at.elapsed().as_secs_f64());
        o!("");
        o!("  ◆ 报告");
        o!("  │  真实客户端: {}  创建/重建房间: {created}  重建次数: {rebuilt}  加入用户: {joined}", clients.len());
        o!("  │  网络操作: {op_count}  失败: {failed_ops}");
        o!("  │  延迟: avg={avg_ms:.2}ms  p99={p99_ms:.2}ms");
        if !failures.is_empty() {
            o!("  │");
            o!("  ├─ 失败样例（最多显示 8 条）");
            for failure in failures.iter().take(8) {
                o!("  │  · {failure}");
            }
        }
        out
    }

    /// 管理员强制把用户迁移到指定房间，绕过房间人数、锁定、进行中等普通加入限制。
    pub async fn force_move_user_to_room(&self, room_id: &str, target_id: i32, monitor: bool) -> Result<Value, String> {
        let rid: RoomId = room_id.to_string().try_into().map_err(|_| "invalid room_id".to_string())?;
        let target_room = {
            let rooms = self.rooms.read().await;
            rooms.get(&rid).map(Arc::clone).ok_or("room not found")?
        };
        let user = {
            let users = self.users.read().await;
            users.get(&target_id).map(Arc::clone).ok_or("user not found")?
        };

        let old_room = user.room.read().await.as_ref().map(Arc::clone);
        let old_room_id = old_room.as_ref().map(|room| room.id.to_string());
        let was_monitor = user.monitor.load(Ordering::SeqCst);
        let same_room = old_room.as_ref().is_some_and(|room| room.id.to_string() == rid.to_string());

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
                }).await;
            }
            self.plugin_manager.trigger(&PluginEvent::RoomLeave {
                user_id: target_id,
                room_id: old_id_text,
            }).await;
        }

        user.monitor.store(monitor, Ordering::SeqCst);
        target_room.force_add_user(Arc::downgrade(&user), monitor).await;
        *user.room.write().await = Some(Arc::clone(&target_room));
        if monitor {
            target_room.live.store(true, Ordering::SeqCst);
        }
        self.assign_room_host_if_missing(&target_room, &user, monitor, false).await;
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
                }).await;
            }
        }

        let mut users = target_room.users().await;
        users.extend(target_room.monitors().await);
        user.try_send(ServerCommand::JoinRoom(Ok(phira_mp_common::JoinRoomResponse {
            state: target_room.client_room_state().await,
            users: users.into_iter().map(|user| user.to_info()).collect(),
            live: target_room.is_live(),
        }))).await;
        user.try_send(ServerCommand::ChangeHost(target_room.check_host(&user).await.is_ok())).await;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.user_room_history.write().await
            .entry(target_id)
            .or_default()
            .push((rid.to_string(), target_room.uuid.to_string(), now));
        if let Some(db) = crate::internal_hooks::DB.get() {
            db.record_user_room_history_sync(target_id, rid.to_string(), target_room.uuid.to_string(), now);
        }

        self.plugin_manager.trigger(&PluginEvent::RoomJoin {
            user_id: target_id,
            room_id: rid.to_string(),
            is_monitor: monitor,
        }).await;
        self.plugin_manager.trigger(&PluginEvent::RoomModify {
            user_id: target_id,
            room_id: rid.to_string(),
            data: serde_json::json!({"action":"force-move","from": old_room_id.clone(),"monitor": monitor}).to_string(),
        }).await;

        target_room.send(phira_mp_common::Message::Chat {
            user: 0,
            content: format!("用户 {} 已被管理员强制转移到本房间", user.name),
        }).await;

        Ok(serde_json::json!({
            "ok": true,
            "room_id": rid.to_string(),
            "target_id": target_id,
            "monitor": monitor,
            "from": old_room_id,
        }))
    }

    pub async fn set_room_hidden(&self, room_id: &str, hidden: bool) -> Result<Value, String> {
        let rid: RoomId = room_id.to_string().try_into().map_err(|_| "invalid room_id".to_string())?;
        let room = {
            let rooms = self.rooms.read().await;
            rooms.get(&rid).map(Arc::clone).ok_or("room not found")?
        };
        room.set_hidden(hidden);
        self.plugin_manager.trigger(&PluginEvent::RoomModify {
            user_id: 0,
            room_id: rid.to_string(),
            data: format!(r#"{{"action":"hidden","value":{hidden}}}"#),
        }).await;
        Ok(serde_json::json!({"ok": true, "room_id": rid.to_string(), "hidden": hidden}))
    }

    pub async fn get_room_phira_api_endpoint(&self, room_id: &str) -> Result<Value, String> {
        let rid: RoomId = room_id.to_string().try_into().map_err(|_| "invalid room_id".to_string())?;
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

    pub async fn set_room_phira_api_endpoint(&self, room_id: &str, endpoint: Option<String>) -> Result<Value, String> {
        let rid: RoomId = room_id.to_string().try_into().map_err(|_| "invalid room_id".to_string())?;
        let room = {
            let rooms = self.rooms.read().await;
            rooms.get(&rid).map(Arc::clone).ok_or("room not found")?
        };
        let normalized = match endpoint {
            Some(value) => Some(normalize_phira_api_endpoint(&value)?),
            None => None,
        };
        room.set_phira_api_endpoint_override(normalized.clone()).await;
        self.refresh_room_display_metadata_background(&room);
        let using_room_override = normalized.is_some();
        let effective_endpoint = normalized
            .clone()
            .unwrap_or_else(|| self.config.phira_api_endpoint.clone());
        self.plugin_manager.trigger(&PluginEvent::RoomModify {
            user_id: 0,
            room_id: rid.to_string(),
            data: serde_json::json!({
                "action": "phira_api_endpoint",
                "value": normalized.clone(),
                "effective": effective_endpoint.clone(),
            }).to_string(),
        }).await;
        Ok(serde_json::json!({
            "ok": true,
            "room_id": rid.to_string(),
            "phira_api_endpoint": effective_endpoint,
            "phira_api_endpoint_override": normalized,
            "using_room_override": using_room_override,
        }))
    }
}

async fn bench_send_command(stream: &mut tokio::net::TcpStream, payload: &phira_mp_common::ClientCommand) -> Result<(), String> {
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
    stream.write_all(&len_buf[..n]).await.map_err(|e| format!("write length: {e}"))?;
    stream.write_all(&buffer).await.map_err(|e| format!("write payload: {e}"))?;
    stream.flush().await.map_err(|e| format!("flush: {e}"))
}

async fn bench_recv_command(stream: &mut tokio::net::TcpStream) -> Result<phira_mp_common::ServerCommand, String> {
    use tokio::io::AsyncReadExt;
    let mut len = 0u32;
    let mut pos = 0;
    loop {
        let byte = stream.read_u8().await.map_err(|e| format!("read length: {e}"))?;
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
    stream.read_exact(&mut buffer).await.map_err(|e| format!("read payload: {e}"))?;
    phira_mp_common::decode_packet(&buffer).map_err(|e| format!("decode packet: {e}"))
}

async fn bench_connect_auth(addr: std::net::SocketAddr, token: &str) -> Result<tokio::net::TcpStream, String> {
    use tokio::io::AsyncWriteExt;
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(addr),
    ).await.map_err(|_| "connect timeout".to_string())?
        .map_err(|e| format!("connect: {e}"))?;
    stream.set_nodelay(true).map_err(|e| format!("set_nodelay: {e}"))?;
    stream.write_u8(1).await.map_err(|e| format!("write protocol version: {e}"))?;
    bench_send_command(&mut stream, &phira_mp_common::ClientCommand::Authenticate {
        token: token.to_string().try_into().map_err(|e| format!("invalid token: {e}"))?,
    }).await?;
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            match bench_recv_command(&mut stream).await? {
                phira_mp_common::ServerCommand::Authenticate(Ok(_)) => return Ok(()),
                phira_mp_common::ServerCommand::Authenticate(Err(err)) => return Err(format!("authenticate rejected: {err}")),
                phira_mp_common::ServerCommand::Message(_) => {}
                other => trace!(?other, "benchmark ignored packet while authenticating"),
            }
        }
    }).await.map_err(|_| "authenticate timeout".to_string())??;
    Ok(stream)
}

async fn bench_create_room(stream: &mut tokio::net::TcpStream, room_id: &str) -> Result<(), String> {
    bench_send_command(stream, &phira_mp_common::ClientCommand::CreateRoom {
        id: room_id.to_string().try_into().map_err(|e| format!("invalid room id: {e}"))?,
    }).await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match bench_recv_command(stream).await? {
                phira_mp_common::ServerCommand::CreateRoom(Ok(())) => return Ok(()),
                phira_mp_common::ServerCommand::CreateRoom(Err(err)) => return Err(format!("create room rejected: {err}")),
                phira_mp_common::ServerCommand::Message(_) | phira_mp_common::ServerCommand::OnJoinRoom(_) => {}
                other => trace!(?other, "benchmark ignored packet while creating room"),
            }
        }
    }).await.map_err(|_| "create room timeout".to_string())?
}

async fn bench_join_room(stream: &mut tokio::net::TcpStream, room_id: &str, monitor: bool) -> Result<(), String> {
    bench_send_command(stream, &phira_mp_common::ClientCommand::JoinRoom {
        id: room_id.to_string().try_into().map_err(|e| format!("invalid room id: {e}"))?,
        monitor,
    }).await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match bench_recv_command(stream).await? {
                phira_mp_common::ServerCommand::JoinRoom(Ok(_)) => return Ok(()),
                phira_mp_common::ServerCommand::JoinRoom(Err(err)) => return Err(format!("join room rejected: {err}")),
                phira_mp_common::ServerCommand::Message(_) | phira_mp_common::ServerCommand::OnJoinRoom(_) => {}
                other => trace!(?other, "benchmark ignored packet while joining room"),
            }
        }
    }).await.map_err(|_| "join room timeout".to_string())?
}

async fn bench_ping(stream: &mut tokio::net::TcpStream) -> Result<(), String> {
    bench_send_command(stream, &phira_mp_common::ClientCommand::Ping).await?;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            match bench_recv_command(stream).await? {
                phira_mp_common::ServerCommand::Pong => return Ok(()),
                phira_mp_common::ServerCommand::Message(_) | phira_mp_common::ServerCommand::OnJoinRoom(_) => {}
                other => trace!(?other, "benchmark ignored packet while waiting pong"),
            }
        }
    }).await.map_err(|_| "pong timeout".to_string())?
}

async fn bench_leave_room(stream: &mut tokio::net::TcpStream) -> Result<(), String> {
    bench_send_command(stream, &phira_mp_common::ClientCommand::LeaveRoom).await?;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            match bench_recv_command(stream).await? {
                phira_mp_common::ServerCommand::LeaveRoom(_) => return Ok(()),
                phira_mp_common::ServerCommand::Message(_) | phira_mp_common::ServerCommand::OnJoinRoom(_) => {}
                _ => {}
            }
        }
    }).await.map_err(|_| "leave timeout".to_string())?
}

/// 从房间踢出用户
async fn run_room_kick(state: &PlusServerState, room_id: &str, target_id: i32) -> Result<Value, String> {
    use phira_mp_common::RoomId;
    let rid: RoomId = room_id.to_string().try_into().map_err(|_| "invalid room_id".to_string())?;
    let room = state.rooms.read().await.get(&rid).map(Arc::clone)
        .ok_or("room not found")?;
    let users = room.users().await;
    let monitors = room.monitors().await;
    let user = users.into_iter().chain(monitors).find(|u| u.id == target_id)
        .ok_or("user not in room")?;
    room.send(phira_mp_common::Message::Chat {
        user: 0, content: format!("用户 {} 已被管理员踢出房间", user.name),
    }).await;
    let was_monitor = user.monitor.load(Ordering::SeqCst);
    let should_drop = room.on_user_leave(&user).await;
    if should_drop {
        state.rooms.write().await.remove(&rid);
    }
    if !was_monitor {
        state
            .publish_room_event(RoomEvent::LeaveRoom {
                room: rid.clone(),
                user: target_id,
            })
            .await;
    }
    state.plugin_manager.trigger(&PluginEvent::RoomModify {
        user_id: target_id, room_id: room_id.to_string(),
        data: r#"{"action":"kicked"}"#.to_string(),
    }).await;
    Ok(serde_json::json!({"ok": true}))
}

/// 设置房主；target_id=None 表示系统 `?` 房主。
async fn run_room_set_host(state: &PlusServerState, room_id: &str, target_id: Option<i32>) -> Result<Value, String> {
    use phira_mp_common::RoomId;
    let rid: RoomId = room_id.to_string().try_into().map_err(|_| "invalid room_id".to_string())?;
    let room = state.rooms.read().await.get(&rid).map(Arc::clone)
        .ok_or("room not found")?;
    match target_id {
        Some(target_id) => {
            room.send(phira_mp_common::Message::Chat {
                user: 0, content: format!("房主已转移给用户 {}", target_id),
            }).await;
            room.set_host(Some(target_id), true).await.map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"ok": true, "host": target_id, "host_is_system": false}))
        }
        None => {
            room.send(phira_mp_common::Message::Chat {
                user: 0, content: "房主已设为系统 ?".to_string(),
            }).await;
            room.set_host(None, true).await.map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"ok": true, "host": -1, "host_is_system": true}))
        }
    }
}

/// 转移房主
async fn run_room_transfer(state: &PlusServerState, room_id: &str, target_id: i32) -> Result<Value, String> {
    if target_id < 0 {
        run_room_set_host(state, room_id, None).await
    } else {
        run_room_set_host(state, room_id, Some(target_id)).await
    }
}

/// 设置房间锁定状态
async fn run_room_set_lock(state: &PlusServerState, room_id: &str, locked: bool) -> Result<Value, String> {
    use phira_mp_common::RoomId;
    let rid: RoomId = room_id.to_string().try_into().map_err(|_| "invalid room_id".to_string())?;
    let room = state.rooms.read().await.get(&rid).map(Arc::clone)
        .ok_or("room not found")?;
    room.locked.store(locked, Ordering::SeqCst);
    room.send(phira_mp_common::Message::LockRoom { lock: locked }).await;
    room.publish_update(phira_mp_common::PartialRoomData {
        lock: Some(locked),
        ..Default::default()
    })
    .await;
    Ok(serde_json::json!({"ok": true, "locked": locked}))
}

/// 关闭/解散房间
async fn run_room_close(state: &PlusServerState, room_id: &str) -> Result<Value, String> {
    use phira_mp_common::RoomId;
    let rid: RoomId = room_id.to_string().try_into().map_err(|_| "invalid room_id".to_string())?;
    let room = state.rooms.read().await.get(&rid).map(Arc::clone)
        .ok_or("room not found")?;
    room.send(phira_mp_common::Message::Chat {
        user: 0, content: "房间已被管理员关闭".to_string(),
    }).await;
    let users = room.users().await;
    for user in &users {
        *user.room.write().await = None;
        state
            .publish_room_event(RoomEvent::LeaveRoom {
                room: rid.clone(),
                user: user.id,
            })
            .await;
    }
    for monitor in room.monitors().await {
        *monitor.room.write().await = None;
    }
    state.rooms.write().await.remove(&rid);
    Ok(serde_json::json!({"ok": true}))
}

/// 将用户踢出服务器
async fn run_admin_kick_user(state: &PlusServerState, target_id: i32, reason: &str) -> Result<Value, String> {
    let user = state.users.read().await.get(&target_id).map(Arc::clone)
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
                .plugin_manager
                .trigger(&PluginEvent::RoomLeave {
                    user_id: target_id,
                    room_id,
                })
                .await;
        }
    }
    {
        let sessions = state.sessions.read().await;
        for session in sessions.values() {
            if session.user.id == target_id {
                let _ = session.stream.send(
                    phira_mp_common::ServerCommand::Message(
                        phira_mp_common::Message::Chat { user: 0, content: format!("你已被管理员踢出服务器: {reason}") },
                    )
                ).await;
                break;
            }
        }
    }
    state.users.write().await.remove(&target_id);
    info!(user = target_id, reason = %reason, "kicked from server by admin");
    state.plugin_manager.trigger(&PluginEvent::UserDisconnect {
        user_id: target_id,
        user_name: user.name.clone(),
    }).await;
    Ok(serde_json::json!({"ok": true, "reason": reason}))
}

/// 统一状态查询入口（插件 / WASM host API 均通过此函数查询）
///
/// 支持的 method:
/// - `player.touches`  → 查询指定用户的最近触控数据
/// - `player.judges`   → 查询指定用户的最近判定数据
/// - 其它方法委托给 webapi feature 下的 server_state_query
fn server_state_query_inner(state: &Arc<PlusServerState>, method: &str, args: &[Value]) -> Result<Value, String> {
    match method {
        "runtime.status" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let simulation = s.simulation.status().await;
                let persistence = s.persistence_worker.stats().await;
                let events = s.event_bus.stats(16);
                let commands = s.command_registry.iter().count();
                let _ = tx.send(Ok(serde_json::json!({
                    "runtime_v2": true,
                    "note": "Runtime v2 is partially installed; real Room/Session runtime is still the current production path.",
                    "commands": {"registered": commands},
                    "event_bus": events,
                    "simulation": simulation,
                    "persistence_worker": persistence,
                })));
            });
            rx.recv_timeout(std::time::Duration::from_millis(2000))
                .unwrap_or(Err("runtime.status timeout".to_string()))
        }
        "simulation.status" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let status = s.simulation.status().await;
                let _ = tx.send(Ok(serde_json::to_value(status).unwrap_or_default()));
            });
            rx.recv_timeout(std::time::Duration::from_millis(2000))
                .unwrap_or(Err("simulation.status timeout".to_string()))
        }
        "simulation.world" => {
            let limit = args.get(0).and_then(|v| v.as_u64()).unwrap_or(20) as usize;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s
                    .simulation
                    .world_snapshot(limit)
                    .await
                    .map(|world| serde_json::to_value(world).unwrap_or_default())
                    .ok_or_else(|| "simulation shadow world not available".to_string());
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_millis(2000))
                .unwrap_or(Err("simulation.world timeout".to_string()))
        }
        "player.touches" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = async {
                    let rooms = s.rooms.read().await;
                    for room in rooms.values() {
                        let data = room.get_player_touches(uid).await;
                        if !data.is_empty() {
                            return Ok(serde_json::to_value(data).unwrap_or_default());
                        }
                    }
                    Err("no data".to_string())
                }.await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_millis(2000))
                .unwrap_or(Err("query timeout".to_string()))
        }
        "player.judges" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = async {
                    let rooms = s.rooms.read().await;
                    for room in rooms.values() {
                        let data = room.get_player_judges(uid).await;
                        if !data.is_empty() {
                            return Ok(serde_json::to_value(data).unwrap_or_default());
                        }
                    }
                    Err("no data".to_string())
                }.await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_millis(2000))
                .unwrap_or(Err("query timeout".to_string()))
        }
        "round.data" => {
            let round_uuid = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
            let player_id = args.get(1).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if round_uuid.is_empty() {
                return Err("missing round_uuid".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let uuid = round_uuid.to_string();
            tokio::spawn(async move {
                let result = s.round_store.read_player_data(&uuid, player_id).await
                    .map(|data| serde_json::to_value(data).unwrap_or_default())
                    .ok_or_else(|| "round data not found".to_string());
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_millis(2000))
                .unwrap_or(Err("query timeout".to_string()))
        }
        "round.list" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let rounds = s.round_store.list_rounds().await;
                let _ = tx.send(Ok(serde_json::to_value(rounds).unwrap_or_default()));
            });
            rx.recv_timeout(std::time::Duration::from_millis(2000))
                .unwrap_or(Err("query timeout".to_string()))
        }
        "test.run_benchmark" => {
            let duration = args.get(0).and_then(|v| v.as_u64()).unwrap_or(10).max(5).min(300);
            let rooms = args.get(1).and_then(|v| v.as_u64()).unwrap_or(100).max(1).min(5000) as usize;

            // 通过 mpsc 通道发送请求给背景 tokio 任务，阻塞等待结果
            let (tx, rx) = std::sync::mpsc::channel();
            if state.bench_tx.send((duration, rooms, tx)).is_err() {
                return Err("benchmark channel closed".to_string());
            }
            match rx.recv_timeout(std::time::Duration::from_secs(duration + 120)) {
                Ok(result) => Ok(serde_json::json!({"output": result})),
                Err(_) => Err("benchmark timeout or cancelled".to_string()),
            }
        }
        "test.bind_phira_tokens" => {
            let mut raw = Vec::new();
            for arg in args {
                if let Some(s) = arg.as_str() {
                    raw.push(s.to_string());
                } else if let Some(list) = arg.as_array() {
                    raw.extend(list.iter().filter_map(|v| v.as_str().map(ToString::to_string)));
                }
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s.bind_benchmark_tokens(raw).await
                    .map(|count| serde_json::json!({"ok": true, "count": count, "path": BENCH_AUTH_FILE}));
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("test.bind_phira_tokens timeout".to_string()))
        }
        "test.cleanup" => {
            state.cleanup_benchmark_sync();
            Ok(serde_json::json!({"ok": true}))
        }
        "room.uuid" => {
            let room_id = args.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
            if room_id.is_empty() { return Err("missing room_id".to_string()); }
            use phira_mp_common::RoomId;
            let rid: RoomId = match room_id.try_into() { Ok(r) => r, Err(_) => return Err("invalid room_id".to_string()) };
            let rooms = state.rooms.try_read().map_err(|_| "lock error".to_string())?;
            match rooms.get(&rid) {
                Some(room) => Ok(serde_json::json!({"uuid": room.uuid.to_string(), "created_at": room.created_at})),
                None => Err("room not found".to_string()),
            }
        }
        "room.history" => {
            let room_id = args.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
            if room_id.is_empty() { return Err("missing room_id".to_string()); }
            use phira_mp_common::RoomId;
            let rid: RoomId = match room_id.try_into() { Ok(r) => r, Err(_) => return Err("invalid room_id".to_string()) };
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let rooms = s.rooms.read().await;
                let result = match rooms.get(&rid) {
                    Some(room) => {
                        let history = room.play_history.read().await;
                        let rounds: Vec<Value> = history.iter().map(|r| serde_json::json!({
                            "round_id": r.round_id.to_string(),
                            "chart_id": r.chart_id,
                            "chart_name": r.chart_name,
                            "players": r.results.len(),
                        })).collect();
                        let total = rounds.len();
                        Ok(serde_json::json!({"rounds": rounds, "total": total}))
                    }
                    None => Err("room not found".to_string()),
                };
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(3))
                .unwrap_or(Err("room.history timeout".to_string()))
        }
        "room.round_info" => {
            let round_uuid = args.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
            if round_uuid.is_empty() { return Err("missing round_uuid".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let rooms = s.rooms.read().await;
                let mut found = None;
                for room in rooms.values() {
                    let history = room.play_history.read().await;
                    if let Some(round) = history.iter().find(|r| r.round_id.to_string() == round_uuid) {
                        found = Some((room.uuid.to_string(), room.id.to_string(), round.clone()));
                        break;
                    }
                }
                let result = match found {
                    Some((room_uuid, room_id, round)) => Ok(serde_json::json!({
                        "room_uuid": room_uuid, "room_id": room_id,
                        "round_id": round.round_id.to_string(),
                        "chart_id": round.chart_id, "chart_name": round.chart_name,
                        "results": round.results.iter().map(|r| serde_json::json!({
                            "user_id": r.user_id, "user_name": r.user_name,
                            "score": r.score, "accuracy": r.accuracy,
                            "full_combo": r.full_combo, "aborted": r.aborted,
                        })).collect::<Vec<_>>(),
                    })),
                    None => Err("round not found".to_string()),
                };
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(3))
                .unwrap_or(Err("room.round_info timeout".to_string()))
        }
        "room.list_since" => {
            let since_ms = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let rooms = s.rooms.read().await;
                let list: Vec<Value> = rooms.values().filter(|r| r.created_at >= since_ms).map(|r| {
                    serde_json::json!({
                        "room_id": r.id.to_string(),
                        "uuid": r.uuid.to_string(),
                        "created_at": r.created_at,
                        "phira_api_endpoint": r.effective_phira_api_endpoint_sync(&s.config.phira_api_endpoint),
                    })
                }).collect();
                let total = list.len();
                let _ = tx.send(Ok(serde_json::json!({"rooms": list, "total": total})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(3))
                .unwrap_or(Err("room.list_since timeout".to_string()))
        }

        "room.create_empty" => {
            let room_id = args.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
            if room_id.is_empty() { return Err("missing room_id".to_string()); }
            let endpoint = match args.get(1) {
                Some(Value::Null) | None => None,
                Some(v) => Some(parse_room_endpoint_value(v.as_str().unwrap_or(""))?),
            }.flatten();
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s.create_empty_room(&room_id, endpoint, true).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.create_empty timeout".to_string()))
        }
        "room.set_persistent_empty" => {
            let room_id = args.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let persistent = args.get(1).and_then(|v| v.as_bool()).unwrap_or(true);
            if room_id.is_empty() { return Err("missing room_id".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s.set_room_persistent_empty(&room_id, persistent).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.set_persistent_empty timeout".to_string()))
        }
        "room.kick" => {
            let room_id = args.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let target_id = args.get(1).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if room_id.is_empty() { return Err("missing room_id".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = run_room_kick(&s, &room_id, target_id).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.kick timeout".to_string()))
        }
        "room.transfer_host" => {
            let room_id = args.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let target_id = args.get(1).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if room_id.is_empty() { return Err("missing room_id".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = run_room_transfer(&s, &room_id, target_id).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.transfer_host timeout".to_string()))
        }
        "room.set_host" => {
            let room_id = args.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let target = args.get(1);
            let target_id = match target {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) if s.trim() == "?" || s.eq_ignore_ascii_case("system") || s.eq_ignore_ascii_case("none") => None,
                Some(Value::String(s)) => Some(s.parse::<i32>().map_err(|_| "invalid target_id".to_string())?),
                Some(v) => v.as_i64().map(|n| n as i32),
            };
            if room_id.is_empty() { return Err("missing room_id".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = run_room_set_host(&s, &room_id, target_id).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.set_host timeout".to_string()))
        }
        "room.force_move" => {
            let room_id = args.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let target_id = args.get(1).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let monitor = args.get(2).and_then(|v| v.as_bool()).unwrap_or(false);
            if room_id.is_empty() { return Err("missing room_id".to_string()); }
            if target_id == 0 { return Err("invalid target_id".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s.force_move_user_to_room(&room_id, target_id, monitor).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.force_move timeout".to_string()))
        }
        "room.set_lock" => {
            let room_id = args.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let locked = args.get(1).and_then(|v| v.as_bool()).unwrap_or(true);
            if room_id.is_empty() { return Err("missing room_id".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = run_room_set_lock(&s, &room_id, locked).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.set_lock timeout".to_string()))
        }
        "room.set_hidden" => {
            let room_id = args.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let hidden = args.get(1).and_then(|v| v.as_bool()).unwrap_or(true);
            if room_id.is_empty() { return Err("missing room_id".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s.set_room_hidden(&room_id, hidden).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.set_hidden timeout".to_string()))
        }
        "room.is_hidden" => {
            let room_id = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
            if room_id.is_empty() { return Err("missing room_id".to_string()); }
            let rid: RoomId = room_id.to_string().try_into().map_err(|_| "invalid room_id".to_string())?;
            let rooms = state.rooms.try_read().map_err(|_| "lock error".to_string())?;
            let room = rooms.get(&rid).ok_or("room not found")?;
            Ok(serde_json::json!({"room_id": room_id, "hidden": room.is_hidden()}))
        }
        "room.get_phira_api_endpoint" => {
            let room_id = args.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
            if room_id.is_empty() { return Err("missing room_id".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s.get_room_phira_api_endpoint(&room_id).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.get_phira_api_endpoint timeout".to_string()))
        }
        "room.set_phira_api_endpoint" => {
            let room_id = args.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
            if room_id.is_empty() { return Err("missing room_id".to_string()); }
            let endpoint = match args.get(1) {
                Some(Value::Null) | None => None,
                Some(v) => Some(parse_room_endpoint_value(v.as_str().unwrap_or(""))?),
            }.flatten();
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s.set_room_phira_api_endpoint(&room_id, endpoint).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.set_phira_api_endpoint timeout".to_string()))
        }
        "room.clear_phira_api_endpoint" => {
            let room_id = args.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
            if room_id.is_empty() { return Err("missing room_id".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s.set_room_phira_api_endpoint(&room_id, None).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.clear_phira_api_endpoint timeout".to_string()))
        }
        "room.close" => {
            let room_id = args.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
            if room_id.is_empty() { return Err("missing room_id".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = run_room_close(&s, &room_id).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.close timeout".to_string()))
        }
        "admin.kick_user" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if uid <= 0 { return Err("invalid user_id".to_string()); }
            let reason = args.get(1).and_then(|v| v.as_str()).unwrap_or("kicked by admin").to_string();
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = run_admin_kick_user(&s, uid, &reason).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.kick_user timeout".to_string()))
        }
        "admin.ban_user" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let reason = args.get(1).and_then(|v| v.as_str()).unwrap_or("banned").to_string();
            if uid <= 0 { return Err("invalid user_id".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s
                    .ban_manager
                    .ban_user(uid, &reason)
                    .await
                    .map(|reason| serde_json::json!({"ok": true, "reason": reason}));
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.ban_user timeout".to_string()))
        }
        "admin.unban_user" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if uid <= 0 { return Err("invalid user_id".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s.ban_manager.unban_user(uid).await
                    .map(|_| serde_json::json!({"ok": true}));
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.unban_user timeout".to_string()))
        }
        "admin.is_banned" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if uid <= 0 { return Err("invalid user_id".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let reason = s.ban_manager.ban_reason(uid).await;
                let _ = tx.send(Ok(serde_json::json!({
                    "banned": reason.is_some(),
                    "reason": reason,
                })));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.is_banned timeout".to_string()))
        }
        "admin.ban_list" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let list = s.ban_manager.list_banned().await;
                let _ = tx.send(Ok(serde_json::to_value(list).unwrap_or_default()));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.ban_list timeout".to_string()))
        }
        "admin.list_users" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let users = s.users.read().await;
                let list: Vec<Value> = users.values().filter(|user| user.id > 0).map(|u| serde_json::json!({
                    "id": u.id, "name": u.name, "monitor": u.monitor.load(Ordering::SeqCst)
                })).collect();
                let _ = tx.send(Ok(serde_json::to_value(list).unwrap_or_default()));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.list_users timeout".to_string()))
        }
        "user.room_history" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if uid <= 0 { return Err("invalid user_id".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let history = s.user_room_history.read().await;
                let entries = history.get(&uid).cloned().unwrap_or_default();
                let list: Vec<Value> = entries.iter().map(|(room_id, room_uuid, ts)| {
                    serde_json::json!({"room_id": room_id, "room_uuid": room_uuid, "joined_at": ts})
                }).collect();
                let total = list.len();
                let _ = tx.send(Ok(serde_json::json!({"rooms": list, "total": total})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("user.room_history timeout".to_string()))
        }

        "persist.events" => {
            let since = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
            let limit = args.get(1).and_then(|v| v.as_i64()).unwrap_or(100).clamp(1, 1000);
            let kind = args.get(2).and_then(|v| v.as_str()).map(str::to_string);
            let room_id = args.get(3).and_then(|v| v.as_str()).map(str::to_string);
            let user_id = args.get(4).and_then(|v| v.as_i64()).and_then(|v| i32::try_from(v).ok());
            let (tx, rx) = std::sync::mpsc::channel();
            tokio::spawn(async move {
                let rows = if let Some(db) = crate::internal_hooks::DB.get() {
                    db.query_events(since, limit, kind.as_deref(), room_id.as_deref(), user_id).await
                } else { Vec::new() };
                let total = rows.len();
                let _ = tx.send(Ok(serde_json::json!({"events": rows, "total": total})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("persist.events timeout".to_string()))
        }
        "persist.rooms" => {
            let since = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
            let limit = args.get(1).and_then(|v| v.as_i64()).unwrap_or(100).clamp(1, 1000);
            let (tx, rx) = std::sync::mpsc::channel();
            tokio::spawn(async move {
                let rows = if let Some(db) = crate::internal_hooks::DB.get() {
                    db.query_room_snapshots(since, limit).await
                } else { Vec::new() };
                let total = rows.len();
                let _ = tx.send(Ok(serde_json::json!({"rooms": rows, "total": total})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("persist.rooms timeout".to_string()))
        }

        "persist.touches" => {
            let since = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
            let limit = args.get(1).and_then(|v| v.as_i64()).unwrap_or(100).clamp(1, 1000);
            let round_uuid = args.get(2).and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map(str::to_string);
            let player_id = args.get(3).and_then(|v| v.as_i64()).and_then(|v| i32::try_from(v).ok());
            let (tx, rx) = std::sync::mpsc::channel();
            tokio::spawn(async move {
                let rows = if let Some(db) = crate::internal_hooks::DB.get() {
                    db.query_touch_batches(since, limit, round_uuid.as_deref(), player_id).await
                } else { Vec::new() };
                let total = rows.len();
                let _ = tx.send(Ok(serde_json::json!({"touches": rows, "total": total})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("persist.touches timeout".to_string()))
        }
        "persist.judges" => {
            let since = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
            let limit = args.get(1).and_then(|v| v.as_i64()).unwrap_or(100).clamp(1, 1000);
            let round_uuid = args.get(2).and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map(str::to_string);
            let player_id = args.get(3).and_then(|v| v.as_i64()).and_then(|v| i32::try_from(v).ok());
            let (tx, rx) = std::sync::mpsc::channel();
            tokio::spawn(async move {
                let rows = if let Some(db) = crate::internal_hooks::DB.get() {
                    db.query_judge_batches(since, limit, round_uuid.as_deref(), player_id).await
                } else { Vec::new() };
                let total = rows.len();
                let _ = tx.send(Ok(serde_json::json!({"judges": rows, "total": total})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("persist.judges timeout".to_string()))
        }
        "persist.playtime" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if uid <= 0 { return Err("invalid user_id".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            tokio::spawn(async move {
                let row = if let Some(db) = crate::internal_hooks::DB.get() { db.get_playtime(uid).await } else { None };
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
                let total = row.as_ref().map(|r| r.total_secs + r.session_start.map(|s| now.saturating_sub(s)).unwrap_or(0)).unwrap_or(0);
                let _ = tx.send(Ok(serde_json::json!({"user_id": uid, "total_secs": total, "session_start": row.and_then(|r| r.session_start)})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("persist.playtime timeout".to_string()))
        }
        "persist.top_playtime" => {
            let limit = args.get(0).and_then(|v| v.as_i64()).unwrap_or(10).clamp(1, 100);
            let (tx, rx) = std::sync::mpsc::channel();
            tokio::spawn(async move {
                let rows = if let Some(db) = crate::internal_hooks::DB.get() { db.top_playtime(limit).await } else { Vec::new() };
                let total = rows.len();
                let _ = tx.send(Ok(serde_json::json!({"players": rows, "total": total})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("persist.top_playtime timeout".to_string()))
        }
        "admin.ids" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let ids = s.admin_id_list().await;
                let _ = tx.send(Ok(serde_json::json!({"admin_phira_ids": ids})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.ids timeout".to_string()))
        }
        "admin.is_admin" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let value = s.is_admin_id(uid).await;
                let _ = tx.send(Ok(serde_json::json!({"user_id": uid, "admin": value})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.is_admin timeout".to_string()))
        }
        "admin.add_id" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if uid <= 0 { return Err("invalid user_id".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let ids = s.add_admin_id(uid).await;
                let _ = tx.send(Ok(serde_json::json!({"admin_phira_ids": ids})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.add_id timeout".to_string()))
        }
        "admin.remove_id" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if uid <= 0 { return Err("invalid user_id".to_string()); }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let ids = s.remove_admin_id(uid).await;
                let _ = tx.send(Ok(serde_json::json!({"admin_phira_ids": ids})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.remove_id timeout".to_string()))
        }
        "admin.set_ids" => {
            let mut ids = Vec::new();
            if let Some(array) = args.get(0).and_then(|v| v.as_array()) {
                ids.extend(array.iter().filter_map(|v| v.as_i64()).filter_map(|v| i32::try_from(v).ok()).filter(|id| *id > 0));
            } else {
                ids.extend(args.iter().filter_map(|v| v.as_i64()).filter_map(|v| i32::try_from(v).ok()).filter(|id| *id > 0));
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let ids = s.set_admin_ids(ids).await;
                let _ = tx.send(Ok(serde_json::json!({"admin_phira_ids": ids})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.set_ids timeout".to_string()))
        }
        _ => {
            // webapi feature 下的扩展查询
            server_state_query(state, method, args)
        }
    }
}

/// Web API 状态查询（内置，无 feature gate）
fn server_state_query(state: &Arc<PlusServerState>, method: &str, args: &[Value]) -> Result<Value, String> {

    #[derive(Serialize)]
    struct RoomSnapshot {
        name: String,
        data: RoomData,
    }
    #[derive(Serialize)]
    struct UserSnapshot {
        id: i32,
        name: String,
        monitor: bool,
        is_host: bool,
        in_room: bool,
        has_session: bool,
    }
    #[derive(Serialize)]
    struct RoomData {
        // Backward-compatible fields used by the old Web API consumers.
        host: i32,
        users: Vec<i32>,
        lock: bool,
        cycle: bool,
        chart: Option<i32>,
        chart_name: Option<String>,
        state: String,
        playing_users: Vec<i32>,
        rounds: Vec<RoundInfo>,
        hidden: bool,
        phira_api_endpoint: String,
        phira_api_endpoint_override: Option<String>,

        // Full room information for dashboards and admin panels.
        id: String,
        uuid: String,
        created_at: i64,
        live: bool,
        locked: bool,
        cycling: bool,
        persistent_empty: bool,
        max_users: usize,
        player_count: usize,
        monitor_count: usize,
        user_ids: Vec<i32>,
        monitor_ids: Vec<i32>,
        host_user: Option<UserSnapshot>,
        host_is_system: bool,
        users_info: Vec<UserSnapshot>,
        monitors_info: Vec<UserSnapshot>,
        chart_info: Option<Value>,
        phira_api_endpoint_effective: String,
        phira_api_endpoint_using_override: bool,
        ready_users: Vec<i32>,
        finished_users: Vec<i32>,
        aborted_users: Vec<i32>,
        result_count: usize,
        current_round_id: Option<String>,
        state_detail: Value,
        round_history: Vec<RoundInfo>,
    }
    #[derive(Serialize, Clone)]
    struct RoundInfo {
        round_id: String,
        chart: i32,
        chart_id: i32,
        chart_name: String,
        records: Vec<Value>,
        results: Vec<Value>,
    }

    fn user_snapshot(room: &crate::room::Room, user: &super::session::User, is_host: bool, in_room: bool) -> UserSnapshot {
        let has_session = user.session.try_read()
            .ok()
            .and_then(|session| session.as_ref().and_then(|weak| weak.upgrade()))
            .is_some();
        UserSnapshot {
            id: user.id,
            name: room.display_name_sync(user),
            monitor: user.monitor.load(Ordering::SeqCst),
            is_host,
            in_room,
            has_session,
        }
    }

    fn build_snapshot(state: &PlusServerState, name: &str, room: &crate::room::Room) -> RoomSnapshot {
        let chart_op = read_lock!(room.chart).clone();
        let users_arcs: Vec<_> = {
            let ul = read_lock!(room.users);
            ul.iter().filter_map(|w| w.upgrade()).collect()
        };
        let monitor_arcs: Vec<_> = {
            let ml = read_lock!(room.monitors);
            ml.iter().filter_map(|w| w.upgrade()).collect()
        };
        let host_arc = read_lock!(room.host).upgrade();
        let host_is_system = room.is_system_host();
        let host = host_arc.as_ref().map(|u| u.id).unwrap_or(-1);

        let guard = read_lock!(room.state);
        let (st, playing_users, ready_users, finished_users, aborted_users, result_count, state_detail) = match &*guard {
            crate::room::InternalRoomState::SelectChart => (
                "SELECTING_CHART".to_string(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                0usize,
                serde_json::json!({"kind":"select_chart"}),
            ),
            crate::room::InternalRoomState::WaitForReady { started } => {
                let mut ready: Vec<i32> = started.iter().copied().collect();
                ready.sort_unstable();
                (
                    "WAITING_FOR_READY".to_string(),
                    Vec::new(),
                    ready.clone(),
                    Vec::new(),
                    Vec::new(),
                    0usize,
                    serde_json::json!({"kind":"wait_for_ready", "ready_users": ready}),
                )
            }
            crate::room::InternalRoomState::Playing { results, aborted } => {
                let mut finished: Vec<i32> = results.keys().copied().collect();
                finished.sort_unstable();
                let mut aborted_vec: Vec<i32> = aborted.iter().copied().collect();
                aborted_vec.sort_unstable();
                let playing: Vec<i32> = users_arcs.iter()
                    .filter(|u| !results.contains_key(&u.id) && !aborted.contains(&u.id))
                    .map(|u| u.id)
                    .collect();
                (
                    "PLAYING".to_string(),
                    playing,
                    Vec::new(),
                    finished.clone(),
                    aborted_vec.clone(),
                    results.len(),
                    serde_json::json!({
                        "kind":"playing",
                        "finished_users": finished,
                        "aborted_users": aborted_vec,
                        "result_count": results.len(),
                    }),
                )
            }
        };
        drop(guard);

        let mut users: Vec<i32> = users_arcs.iter().map(|u| u.id).collect();
        let monitor_ids: Vec<i32> = monitor_arcs.iter().map(|u| u.id).collect();
        users.extend(monitor_ids.iter().copied());
        let user_ids: Vec<i32> = users_arcs.iter().map(|u| u.id).collect();

        let users_info: Vec<UserSnapshot> = users_arcs.iter()
            .map(|u| user_snapshot(room, u, u.id == host, true))
            .collect();
        let monitors_info: Vec<UserSnapshot> = monitor_arcs.iter()
            .map(|u| user_snapshot(room, u, u.id == host, true))
            .collect();
        let host_user = host_arc.as_ref().map(|u| user_snapshot(room, u, true, user_ids.contains(&u.id) || monitor_ids.contains(&u.id)));

        let phira_api_endpoint = room.effective_phira_api_endpoint_sync(&state.config.phira_api_endpoint);
        let phira_api_endpoint_override = room.phira_api_endpoint_override_sync();
        let using_override = phira_api_endpoint_override.is_some();
        let current_round_id = read_lock!(room.current_round_id).as_ref().map(|id| id.to_string());
        let hist = read_lock!(room.play_history);
        let rounds: Vec<RoundInfo> = hist.iter().map(|r| {
            let results: Vec<Value> = r.results.iter().map(|res| serde_json::json!({
                "player": res.user_id,
                "user_id": res.user_id,
                "user_name": res.user_name.clone(),
                "score": res.score,
                "accuracy": res.accuracy,
                "perfect": res.perfect,
                "good": res.good,
                "bad": res.bad,
                "miss": res.miss,
                "max_combo": res.max_combo,
                "full_combo": res.full_combo,
                "aborted": res.aborted,
                "std_score": res.std_score,
            })).collect();
            RoundInfo {
                round_id: r.round_id.to_string(),
                chart: r.chart_id,
                chart_id: r.chart_id,
                chart_name: r.chart_name.clone(),
                records: results.clone(),
                results,
            }
        }).collect();
        drop(hist);

        let chart_info = chart_op.as_ref().map(|c| serde_json::json!({
            "id": c.id,
            "name": c.name.clone(),
        }));

        RoomSnapshot {
            name: name.into(),
            data: RoomData {
                host,
                users,
                lock: room.locked.load(Ordering::SeqCst),
                cycle: room.cycle.load(Ordering::SeqCst),
                chart: chart_op.as_ref().map(|c| c.id),
                chart_name: chart_op.as_ref().map(|c| c.name.clone()),
                state: st,
                playing_users,
                rounds: rounds.clone(),
                hidden: room.is_hidden(),
                phira_api_endpoint: phira_api_endpoint.clone(),
                phira_api_endpoint_override: phira_api_endpoint_override.clone(),
                id: name.into(),
                uuid: room.uuid.to_string(),
                created_at: room.created_at,
                live: room.is_live(),
                locked: room.is_locked(),
                cycling: room.is_cycle(),
                persistent_empty: room.is_persistent_empty(),
                max_users: room.max_users_count(),
                player_count: user_ids.len(),
                monitor_count: monitor_ids.len(),
                user_ids,
                monitor_ids,
                host_user,
                host_is_system,
                users_info,
                monitors_info,
                chart_info,
                phira_api_endpoint_effective: phira_api_endpoint,
                phira_api_endpoint_using_override: using_override,
                ready_users,
                finished_users,
                aborted_users,
                result_count,
                current_round_id,
                state_detail,
                round_history: rounds,
            },
        }
    }

    match method {
        "rooms.list" => {
            let rooms = read_lock!(state.rooms);
            let list: Vec<Value> = rooms.iter().filter(|(_, room)| {
                !room.is_hidden()
            }).map(|(rid, room)| {
                let ss = build_snapshot(state, &rid.to_string(), room);
                serde_json::to_value(ss).unwrap_or_default()
            }).collect();
            Ok(Value::Array(list))
        }
        "rooms.by_name" => {
            let name = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
            let rid: phira_mp_common::RoomId = name.to_string().try_into()
                .map_err(|_| "invalid room name".to_string())?;
            let rooms = read_lock!(state.rooms);
            let room = rooms.get(&rid).ok_or("room not found")?;
            if room.is_hidden() { return Err("room not found".to_string()); }
            let ss = build_snapshot(state, name, room);
            serde_json::to_value(ss).map_err(|e| e.to_string())
        }
        "user_name" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let users = read_lock!(state.users);
            let name = users.get(&uid).map(|u| u.name.clone());
            drop(users);
            Ok(serde_json::json!({"user_id": uid, "name": name}))
        }
        "send_chat" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let msg = args.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let server = Arc::clone(state);
            tokio::spawn(async move {
                let users = server.users.read().await;
                if let Some(user) = users.get(&uid) {
                    user.try_send(phira_mp_common::ServerCommand::Message(
                        phira_mp_common::Message::Chat { user: 0, content: msg },
                    )).await;
                }
            });
            Ok(serde_json::json!({"sent": true}))
        }
        "send_room_chat" => {
            let room_name = args.get(0).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let msg = args.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
            if room_name.starts_with('.') { return Ok(serde_json::json!({"sent": false})); }
            let rid: phira_mp_common::RoomId = match room_name.try_into() {
                Ok(r) => r,
                Err(_) => return Ok(serde_json::json!({"sent": false, "error": "invalid room"})),
            };
            let rooms = read_lock!(state.rooms);
            if let Some(room) = rooms.get(&rid) {
                let content = format!("[结算] {}", msg);
                let cmd = phira_mp_common::ServerCommand::Message(
                    phira_mp_common::Message::Chat { user: 0, content },
                );
                let users_list = read_lock!(room.users);
                let monitors_list = read_lock!(room.monitors);
                let mut sent = 0usize;
                for wu in users_list.iter().chain(monitors_list.iter()) {
                    if let Some(u) = wu.upgrade() {
                        let session = read_lock!(u.session);
                        if let Some(session) = session.as_ref().and_then(|w| w.upgrade()) {
                            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                                let cmd = cmd.clone();
                                handle.spawn(async move {
                                    let _ = session.stream.send(cmd).await;
                                });
                                sent += 1;
                            } else {
                                let cmd = cmd.clone();
                                std::thread::spawn(move || {
                                    // 没有 tokio 上下文，创建临时运行时
                                    let rt = tokio::runtime::Builder::new_current_thread()
                                        .enable_all().build().expect("build rt");
                                    rt.block_on(async move {
                                        let _ = session.stream.send(cmd).await;
                                    });
                                });
                                sent += 1;
                            }
                        }
                    }
                }
                Ok(serde_json::json!({"sent": sent}))
            } else {
                Ok(serde_json::json!({"sent": false, "error": "room not found"}))
            }
        }
        "rooms.by_user" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let user = {
                let users = read_lock!(state.users);
                users.get(&uid).map(Arc::clone).ok_or("user not found")?
            };
            let rg = read_lock!(user.room);
            let room = rg.as_ref().ok_or("user not in room")?;
            let name = room.id.to_string();
            let ss = build_snapshot(state, &name, room);
            serde_json::to_value(ss).map_err(|e| e.to_string())
        }
        _ => Err(format!("unknown query method: {method}")),
    }
}

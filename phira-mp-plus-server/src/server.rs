//! Phira-mp+ 增强服务器
//!
//! 在原始 phira-mp 服务器基础上增加 WASM 插件系统支持、
//! CLI 管理控制台和扩展数据系统。支持 YAML 配置文件。

use crate::ban::BanManager;
use crate::extensions::ExtensionManager;
use crate::plugin::{PluginEvent, PluginManager, WasmRuntimeConfig};
use crate::plugin_http::PluginHttpServer;
use phira_mp_plus_server_api as api;
use anyhow::Result;
use phira_mp_common::{RoomId, generate_secret_key};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{Notify, RwLock, mpsc};
use tracing::{info, trace, warn};

use uuid::Uuid;
use std::sync::Weak;
use std::sync::atomic::Ordering;

pub type SafeMap<K, V> = RwLock<HashMap<K, V>>;

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
    ($lock:expr) => { loop { match $lock.try_read() { Ok(g) => break g, Err(_) => std::thread::yield_now() } } };
}
/// 自旋获取 tokio RwLock 写锁（同步上下文，如压测）
macro_rules! sync_write {
    ($lock:expr) => { loop { match $lock.try_write() { Ok(g) => break g, Err(_) => std::thread::yield_now() } } };
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
    /// 未设置时自动回退 JSON 文件存储
    #[serde(default)]
    pub database_url: Option<String>,
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
    /// 房间 monitor key（与 phira-web-monitor 共享密钥）
    pub room_monitor_key: Vec<u8>,
    /// Legacy HTTP room-event bridge; native monitors use the TCP protocol.
    pub room_sse_tx: RwLock<Option<tokio::sync::broadcast::Sender<String>>>,
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
        let (bench_tx, bench_rx) = tokio::sync::mpsc::unbounded_channel::<BenchRequest>();

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
            room_monitor_key: generate_secret_key("room_monitor", 64).unwrap_or_default(),
            room_monitor: RwLock::new(None),
            game_monitors: SafeMap::default(),
            room_sse_tx: RwLock::new(None),
        });
        // 压测背景任务
        let bench_state = Arc::clone(&state);
        tokio::spawn(async move {
            let mut bench_rx = bench_rx;
            while let Some((duration, rooms, result_tx)) = bench_rx.recv().await {
                let bs = Arc::clone(&bench_state);
                let output = tokio::task::spawn_blocking(move || {
                    bs.run_benchmark_sync(duration, rooms)
                }).await.unwrap_or_else(|e| format!("benchmark panicked: {e}"));
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
            let cmd = phira_mp_common::ServerCommand::Message(
                phira_mp_common::Message::Chat { user: 0, content: msg },
            );
            tokio::spawn(async move {
                let users = s.users.read().await;
                if let Some(user) = users.get(&uid) {
                    let session = user.session.read().await;
                    if let Some(session) = session.as_ref().and_then(|w| w.upgrade()) {
                        let _ = session.stream.send(cmd).await;
                    }
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

        // 初始化中央 HTTP/SSE 服务器（插件可通过 PluginContext 注册路由）
        let http_server = Arc::new(PluginHttpServer::new(http_port));
        let http_handle = api::HttpHandle::new(crate::plugin_http::HttpHandleBridge(Arc::clone(&http_server)));
        state.plugin_manager.set_http_handle(http_handle).await;

        // 加载插件
        let plugin_count = state.plugin_manager.load_plugins().await.unwrap_or(0);
        info!("loaded {} plugin(s)", plugin_count);

        // ── 注册内置功能（原内置插件系统已合并入核心） ──
        // Web API 路由
        let state_for_webapi = Arc::clone(&state);
        let webapi_state_query = api::ServerStateQuery::new(move |method: &str, args: &[Value]| {
            server_state_query(&state_for_webapi, method, args)
        });
        let http_for_webapi = Arc::clone(&http_server);
        let state_for_webapi2 = Arc::clone(&state);
        let sq = webapi_state_query.clone();
        http_for_webapi.register_route_sync("/api/rooms", Arc::new(move |_, _| {
            sq.call("rooms.list", &[]).map_err(|e| (500u16, e))
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
        // 压测 CLI 命令注册
        let bench_state = Arc::clone(&state);
        let _ = state.plugin_manager.register_cli_command(crate::plugin::CliCommand {
            name: "benchmark".to_string(),
            description: "运行服务端压力测试".to_string(),
            usage: "benchmark [dur_s=30] [rooms=100]".to_string(),
            handler: Arc::new(move |args| {
                let duration: u64 = args.first().and_then(|a| a.parse().ok()).filter(|&v| v >= 5 && v <= 300).unwrap_or(30);
                let rooms: usize = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(100).max(10).min(5000);
                let sq = bench_state.clone();
                match crate::server::server_state_query_inner(
                    &sq, "test.run_benchmark", &[serde_json::json!(duration), serde_json::json!(rooms)]
                ) {
                    Ok(v) => v.get("output").and_then(|o| o.as_str())
                        .map(|s| s.lines().map(|l| l.to_string()).collect())
                        .unwrap_or_else(|| vec!["  ✗ parse error".to_string()]),
                    Err(e) => vec![format!("  ✗ {}", e)],
                }
            }),
        }).await;

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

        // 轮次数据定期清理（每小时检查一次）
        if retention_days > 0 {
            let cleanup_state = Arc::clone(&state);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                    cleanup_state.round_store.cleanup_expired().await;
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

// ── Monitor 助手 ──

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
}

// ── 压测方法 ──

impl PlusServerState {
    /// 运行压测（同步版 — 在 ServerStateQuery 线程中调用，无 tokio）
    pub fn run_benchmark_sync(self: &Arc<Self>, duration_secs: u64, target_rooms: usize) -> String {
        use std::time::Instant;
        let started_at = Instant::now();
        let mut out = String::new();
        macro_rules! o { ($($t:tt)*) => { out.push_str(&format!($($t)*)); out.push('\n'); } }

        eprintln!("  ⟳ 压测: {target_rooms} 房间 / {duration_secs}s");

        // ── 阶段1: 顺序创建目标房间 ──
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
            ));
            sync_write!(self.users).insert(test_uid, Arc::clone(&test_user));

            let room = Arc::new(crate::room::Room::new(
                rid.clone(), Arc::downgrade(&test_user),
                Some(Arc::clone(&self.plugin_manager)),
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

        eprintln!("  ⟳ 50% 用户填充完成");
        // ── 阶段2: 模拟用户加入 ──
        o!("  │");
        o!("  ├─ [阶段2] 填充用户 (每房间+4人)");
        eprintln!("  ⟳ 25% 填充用户...");
        let t0 = Instant::now();
        let mut joined = 0usize;
        let rooms_snapshot: Vec<Arc<crate::room::Room>> = sync_read!(self.rooms).values().map(Arc::clone).collect();
        for room in rooms_snapshot.iter().take(created) {
            for _ in 0..4 {
                let test_uid = TEST_USER_ID_BASE + 1_000_000 + joined as i32;
                let test_user = Arc::new(super::session::User::new(
                    test_uid, format!("player#{joined}"),
                    crate::l10n::Language::default(), Arc::clone(self),
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

        eprintln!("  ⟳ 75% 压力测试中...");
        // ── 阶段3: 压力保持 ──
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

        // ── 阶段4: 清理 ──
        o!("  │");
        o!("  ├─ [阶段4] 清理测试数据");
        eprintln!("  ⟳ 100% 清理中");
        let t4 = Instant::now();
        sync_write!(self.rooms).retain(|rid, _| !rid.to_string().starts_with("bench-"));
        sync_write!(self.users).retain(|id, _| *id < TEST_USER_ID_BASE || *id >= TEST_USER_ID_BASE + 10_000_000);
        o!("  │ ✓ 清理完成 ({:.1}s)", t4.elapsed().as_secs_f64());

        // ── 汇总 ──
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
}

// ── HTTP 真实请求压测 ──

// ── 状态查询 / Room/User 管理（供 WASM host API / dispatch） ──

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
    let _ = room.on_user_leave(&user).await;
    state.plugin_manager.trigger(&PluginEvent::RoomModify {
        user_id: target_id, room_id: room_id.to_string(),
        data: r#"{"action":"kicked"}"#.to_string(),
    }).await;
    Ok(serde_json::json!({"ok": true}))
}

/// 转移房主
async fn run_room_transfer(state: &PlusServerState, room_id: &str, target_id: i32) -> Result<Value, String> {
    use phira_mp_common::RoomId;
    let rid: RoomId = room_id.to_string().try_into().map_err(|_| "invalid room_id".to_string())?;
    let room = state.rooms.read().await.get(&rid).map(Arc::clone)
        .ok_or("room not found")?;
    room.send(phira_mp_common::Message::Chat {
        user: 0, content: format!("房主已转移给用户 {}", target_id),
    }).await;
    room.transfer_host(target_id).await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({"ok": true}))
}

/// 设置房间锁定状态
async fn run_room_set_lock(state: &PlusServerState, room_id: &str, locked: bool) -> Result<Value, String> {
    use phira_mp_common::RoomId;
    let rid: RoomId = room_id.to_string().try_into().map_err(|_| "invalid room_id".to_string())?;
    let room = state.rooms.read().await.get(&rid).map(Arc::clone)
        .ok_or("room not found")?;
    room.locked.store(locked, Ordering::SeqCst);
    room.send(phira_mp_common::Message::LockRoom { lock: locked }).await;
    room.on_state_change().await;
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
    for u in room.users().await { *u.room.write().await = None; }
    for u in room.monitors().await { *u.room.write().await = None; }
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
            if room.on_user_leave(&user).await {
                state.rooms.write().await.remove(&room.id);
            }
            state.plugin_manager.trigger(&PluginEvent::RoomLeave { user_id: target_id, room_id }).await;
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
            let rooms = args.get(1).and_then(|v| v.as_u64()).unwrap_or(100).max(10).min(5000) as usize;

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
        "test.cleanup" => {
            state.cleanup_benchmark_sync();
            Ok(serde_json::json!({"ok": true}))
        }
        // ── 房间/轮次查询接口 ──
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
                        Ok(serde_json::json!({"rounds": rounds, "total": rounds.len()}))
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
                    })
                }).collect();
                let _ = tx.send(Ok(serde_json::json!({"rooms": list, "total": list.len()})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(3))
                .unwrap_or(Err("room.list_since timeout".to_string()))
        }
        // ── 房间管理（room-management host API） ──
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
        // ── 用户管理（user-management host API） ──
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
                let result = s.ban_manager.ban_user(uid, &reason).await
                    .map(|_| serde_json::json!({"ok": true}));
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
                let banned = s.ban_manager.is_banned(uid).await;
                let _ = tx.send(Ok(serde_json::json!({"banned": banned})));
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
                let _ = tx.send(Ok(serde_json::json!({"rooms": list, "total": list.len()})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("user.room_history timeout".to_string()))
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
    struct RoomData {
        host: i32,
        users: Vec<i32>,
        lock: bool,
        cycle: bool,
        chart: Option<i32>,
        chart_name: Option<String>,
        state: String,
        playing_users: Vec<i32>,
        rounds: Vec<RoundInfo>,
    }
    #[derive(Serialize)]
    struct RoundInfo {
        chart: i32,
        records: Vec<Value>,
    }

    fn build_snapshot(name: &str, room: &crate::room::Room) -> RoomSnapshot {
        let chart_op = read_lock!(room.chart).clone();
        let guard = read_lock!(room.state);
        let ul = read_lock!(room.users);
        let ml = read_lock!(room.monitors);

        let (st, pu) = match &*guard {
            crate::room::InternalRoomState::SelectChart =>
                ("SELECTING_CHART".into(), vec![]),
            crate::room::InternalRoomState::WaitForReady { .. } =>
                ("WAITING_FOR_READY".into(), vec![]),
            crate::room::InternalRoomState::Playing { results, aborted } => {
                let p: Vec<i32> = ul.iter().filter_map(|wu| {
                    let u = wu.upgrade()?;
                    (!results.contains_key(&u.id) && !aborted.contains(&u.id)).then_some(u.id)
                }).collect();
                ("PLAYING".into(), p)
            }
        };
        drop(guard);

        let mut users: Vec<i32> = ul.iter().filter_map(|w| w.upgrade().map(|u| u.id)).collect();
        users.extend(ml.iter().filter_map(|w| w.upgrade().map(|u| u.id)));
        drop(ul); drop(ml);

        let host = read_lock!(room.host).upgrade().map(|u| u.id).unwrap_or(0);
        let hist = read_lock!(room.play_history);
        let rounds: Vec<RoundInfo> = hist.iter().map(|r| RoundInfo {
            chart: r.chart_id,
            records: r.results.iter().map(|res| serde_json::json!({
                "player": res.user_id, "score": res.score, "accuracy": res.accuracy,
                "perfect": res.perfect, "good": res.good, "bad": res.bad,
                "miss": res.miss, "max_combo": res.max_combo, "full_combo": res.full_combo,
            })).collect(),
        }).collect();
        drop(hist);

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
                playing_users: pu,
                rounds,
            },
        }
    }

    match method {
        "rooms.list" => {
            let rooms = read_lock!(state.rooms);
            let list: Vec<Value> = rooms.iter().filter(|(rid, _)| {
                !rid.to_string().starts_with("+-")
            }).map(|(rid, room)| {
                let ss = build_snapshot(&rid.to_string(), room);
                serde_json::to_value(ss).unwrap_or_default()
            }).collect();
            Ok(Value::Array(list))
        }
        "rooms.by_name" => {
            let name = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
            if name.starts_with("+-") { return Err("room not found".to_string()); }
            let rid: phira_mp_common::RoomId = name.to_string().try_into()
                .map_err(|_| "invalid room name".to_string())?;
            let rooms = read_lock!(state.rooms);
            let room = rooms.get(&rid).ok_or("room not found")?;
            let ss = build_snapshot(name, room);
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
            let ss = build_snapshot(&name, room);
            serde_json::to_value(ss).map_err(|e| e.to_string())
        }
        _ => Err(format!("unknown query method: {method}")),
    }
}

//! Phira-mp+ 增强服务器
//!
//! 在原始 phira-mp 服务器基础上增加 WASM 插件系统支持、
//! CLI 管理控制台和扩展数据系统。

use crate::ban::BanManager;
use crate::cli::CliHandler;
use crate::extensions::ExtensionManager;
use crate::plugin::{self, PluginEvent, PluginManager};
use crate::plugin_http::PluginHttpServer;
use phira_mp_plus_server_api as api;
use anyhow::Result;
use phira_mp_common::RoomId;
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
use tracing::{info, warn};
use uuid::Uuid;

pub type SafeMap<K, V> = RwLock<HashMap<K, V>>;
pub type IdMap<V> = SafeMap<Uuid, V>;

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
}

/// Phira-mp+ 配置
#[derive(Debug, Deserialize)]
pub struct PlusConfig {
    pub port: u16,
    pub http_port: u16,
    pub monitors: Vec<i32>,
    pub plugins_dir: String,
    pub extensions_file: Option<String>,
    pub cli_enabled: bool,
}

impl Default for PlusConfig {
    fn default() -> Self {
        Self {
            port: 12346,
            http_port: 12347,
            monitors: vec![2],
            plugins_dir: "plugins".to_string(),
            extensions_file: None,
            cli_enabled: true,
        }
    }
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
            println!("Phira-mp+ Local Address: http://{}", addr);
        }

        let (lost_con_tx, mut lost_con_rx) = mpsc::channel(16);

        // 初始化扩展管理器
        let extensions = Arc::new(ExtensionManager::new(config.extensions_file.clone()));

        // 初始化插件管理器
        let plugin_manager = Arc::new(PluginManager::new(
            &config.plugins_dir,
            Arc::clone(&extensions),
        ));

        // 初始化黑名单管理器
        let ban_manager = Arc::new(BanManager::new(Arc::clone(&extensions)));

        let http_port = config.http_port;

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

        // 初始化 PostgreSQL 数据库（--features postgres）
        // 环境变量 DATABASE_URL 指定连接，默认 postgres://localhost:5432/phira_mp_plus
        #[cfg(feature = "postgres")]
        {
            let db_url = std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://localhost:5432/phira_mp_plus".to_string());
            match crate::jsondb::PgDatabase::new(&db_url).await {
                Ok(pg) => {
                    let handle = pg.into_handle();
                    state.plugin_manager.set_db_handle(handle).await;
                    info!("PostgreSQL database initialized");
                }
                Err(e) => {
                    warn!("PostgreSQL init failed (plugins without db will work): {e}");
                }
            }
        }
        // 无 PostgreSQL 时提供空数据库句柄（插件降级运行）
        #[cfg(not(feature = "postgres"))]
        {
            warn!("PostgreSQL not enabled (--features postgres), player-tracker will skip recording");
            // 提供一个空的 DatabaseHandle 避免插件 init 报错
            let empty_db = phira_mp_plus_server_api::DatabaseHandle::new(|_, _| {
                Err("database not available".to_string())
            });
            state.plugin_manager.set_db_handle(empty_db).await;
        }

        // 初始化中央 HTTP/SSE 服务器（插件可通过 PluginContext 注册路由）
        let http_server = Arc::new(PluginHttpServer::new(http_port));
        let http_handle = api::HttpHandle::new(crate::plugin_http::HttpHandleBridge(Arc::clone(&http_server)));
        state.plugin_manager.set_http_handle(http_handle).await;

        // 加载插件
        let plugin_count = state.plugin_manager.load_plugins().await.unwrap_or(0);
        info!("loaded {} plugin(s)", plugin_count);

        // 注册事件日志插件（调试用）
        let _ = state.plugin_manager.register_native(
            plugin::create_event_logger(),
            "event-logger",
        ).await;

        // 注册 Web API 插件（--features webapi）→ 必须早于 HTTP 服务器启动
        #[cfg(feature = "webapi")]
        {
            let state_query = api::ServerStateQuery::new({
                let s = Arc::clone(&state);
                move |method: &str, args: &[Value]| -> Result<Value, String> {
                    server_state_query(&s, method, args)
                }
            });
            let _ = state.plugin_manager.register_native_with_state(
                phira_mp_plus_webapi::WebApiPlugin::create(),
                "webapi",
                Some(state_query),
            ).await;
        }

        // 注册玩家记录插件（--features player-tracker）
        #[cfg(feature = "player-tracker")]
        {
            if let Err(e) = state.plugin_manager.register_native(
                phira_mp_plus_player_tracker::PlayerTracker::create(),
                "player-tracker",
            ).await {
                warn!("player-tracker plugin init failed: {e}");
            }
        }

        // 启动中央 HTTP 服务器（所有路由已注册完毕）
        let http_state = Arc::clone(&state);
        tokio::spawn(async move {
            http_server.start(http_state).await;
        });

        Ok(Self {
            state,
            listener,
            _lost_con_handle: lost_con_handle,
        })
    }

    /// 接受新连接
    pub async fn accept(&self) -> Result<()> {
        let (stream, addr) = self.listener.accept().await?;
        let mut guard = self.state.sessions.write().await;
        let id = {
            let mut id = Uuid::new_v4();
            while guard.contains_key(&id) {
                id = Uuid::new_v4();
            }
            id
        };
        let session = super::session::Session::new(id, stream, Arc::clone(&self.state)).await?;
        info!(
            "received connections from {} ({}), version: {}",
            addr,
            session.id,
            session.version()
        );
        guard.insert(id, session);
        Ok(())
    }

    /// 启动 CLI 管理控制台
    pub async fn start_cli(&self) -> Result<()> {
        if !self.state.config.cli_enabled {
            info!("CLI management console is disabled");
            return Ok(());
        }
        let state = Arc::clone(&self.state);

        // 在独立任务中运行 CLI
        tokio::spawn(async move {
            let cli = CliHandler::new(state);
            cli.start().await;
        });

        Ok(())
    }

    /// 触发插件事件
    pub async fn trigger_event(&self, event: &PluginEvent) {
        self.state.plugin_manager.trigger(event).await;
    }

    /// 获取服务器统计信息
    pub async fn stats(&self) -> ServerStats {
        let user_count = self.state.users.read().await.len();
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

// ── Web API 状态查询 ──

use std::sync::atomic::Ordering;

/// 处理来自插件查询服务端状态的请求
#[cfg(feature = "webapi")]
fn server_state_query(state: &Arc<PlusServerState>, method: &str, args: &[Value]) -> Result<Value, String> {
    use serde::Serialize;

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
        let chart_op = room.chart.blocking_read().clone();
        let guard = room.state.blocking_read();
        let ul = room.users.blocking_read();
        let ml = room.monitors.blocking_read();

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

        let host = room.host.blocking_read().upgrade().map(|u| u.id).unwrap_or(0);
        let hist = room.play_history.blocking_read();
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
                state: st,
                playing_users: pu,
                rounds,
            },
        }
    }

    match method {
        "rooms.list" => {
            let rooms = state.rooms.blocking_read();
            let list: Vec<Value> = rooms.iter().map(|(rid, room)| {
                let ss = build_snapshot(&rid.to_string(), room);
                serde_json::to_value(ss).unwrap_or_default()
            }).collect();
            Ok(Value::Array(list))
        }
        "rooms.by_name" => {
            let name = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
            let rid: phira_mp_common::RoomId = name.to_string().try_into()
                .map_err(|_| "invalid room name".to_string())?;
            let rooms = state.rooms.blocking_read();
            let room = rooms.get(&rid).ok_or("room not found")?;
            let ss = build_snapshot(name, room);
            serde_json::to_value(ss).map_err(|e| e.to_string())
        }
        "rooms.by_user" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let user = {
                let users = state.users.blocking_read();
                users.get(&uid).map(Arc::clone).ok_or("user not found")?
            };
            let rg = user.room.blocking_read();
            let room = rg.as_ref().ok_or("user not in room")?;
            let name = room.id.to_string();
            let ss = build_snapshot(&name, room);
            serde_json::to_value(ss).map_err(|e| e.to_string())
        }
        _ => Err(format!("unknown query method: {method}")),
    }
}

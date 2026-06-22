//! Phira-mp+ 增强服务器
//!
//! 在原始 phira-mp 服务器基础上增加 WASM 插件系统支持、
//! CLI 管理控制台和扩展数据系统。

use crate::cli::CliHandler;
use crate::extensions::ExtensionManager;
use crate::plugin::{self, PluginEvent, PluginManager};
use anyhow::Result;
use phira_mp_common::RoomId;
use serde::Deserialize;

/// Chart information from the Phira API
#[derive(Debug, Deserialize, Clone)]
pub struct Chart {
    pub id: i32,
    pub name: String,
}

/// Record information from the Phira API
#[derive(Debug, Deserialize, Clone)]
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
use tokio::sync::{RwLock, mpsc};
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
}

/// Phira-mp+ 配置
#[derive(Debug, Deserialize)]
pub struct PlusConfig {
    pub port: u16,
    pub monitors: Vec<i32>,
    pub plugins_dir: String,
    pub extensions_file: Option<String>,
    pub cli_enabled: bool,
}

impl Default for PlusConfig {
    fn default() -> Self {
        Self {
            port: 12346,
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

        let state = Arc::new(PlusServerState {
            config,
            sessions: IdMap::default(),
            users: SafeMap::default(),
            rooms: SafeMap::default(),
            lost_con_tx,
            plugin_manager,
            extensions,
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

        // 加载插件
        let plugin_count = state.plugin_manager.load_plugins().await.unwrap_or(0);
        info!("loaded {} plugin(s)", plugin_count);

        // 注册事件日志插件（调试用）
        let _ = state.plugin_manager.register_native(
            plugin::create_event_logger(),
            "event-logger",
        ).await;

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
        let cli = CliHandler::new(
            Arc::clone(&self.state.plugin_manager),
            Arc::clone(&self.state.extensions),
        );
        let _running = cli.is_running();

        // 在独立任务中运行 CLI
        tokio::spawn(async move {
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

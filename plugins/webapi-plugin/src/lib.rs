//! Phira-mp+ Web API 插件 — 独立项目
//!
//! 提供 REST API 和 SSE 接口查询房间信息、游玩记录等。
//! 编译为动态库后由服务器加载，或作为原生插件直接注册。
//!
//! # 构建
//! ```bash
//! cd plugins/webapi-plugin
//! cargo build
//! ```
//! 产物在 `target/debug/libphira_mp_plus_webapi.so`。
//!
//! # 注册到服务器
//! ```rust
//! state.plugin_manager.register_native(
//!     phira_mp_plus_webapi::WebApiPlugin::create(Arc::clone(&state)),
//!     "webapi",
//! ).await;
//! ```

pub mod sse;
pub mod api;

use phira_mp_plus_server::plugin::{self, PluginEvent, PluginInfo, native::NativePlugin};
use phira_mp_plus_server::server::PlusServerState;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::info;

pub use sse::SseEvent;

/// Web API 插件
pub struct WebApiPlugin {
    state: Arc<PlusServerState>,
    sse_tx: broadcast::Sender<SseEvent>,
    _server_handle: Option<tokio::task::JoinHandle<()>>,
}

impl WebApiPlugin {
    /// 创建插件实例（工厂方法）
    pub fn create(state: Arc<PlusServerState>) -> Box<dyn NativePlugin> {
        let (sse_tx, _) = broadcast::channel(256);
        Box::new(WebApiPlugin {
            state,
            sse_tx,
            _server_handle: None,
        })
    }
}

impl NativePlugin for WebApiPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "webapi".to_string(),
            version: "0.1.0".to_string(),
            author: "Phira-mp+".to_string(),
            description: "提供 REST API 和 SSE 接口查询房间信息".to_string(),
        }
    }

    fn init(&mut self, _ctx: &plugin::native::PluginContext) -> Result<(), String> {
        info!("Web API plugin initializing...");
        let state = Arc::clone(&self.state);
        let sse_tx = self.sse_tx.clone();
        let handle = tokio::spawn(async move {
            api::start_http_server(state, sse_tx).await;
        });
        self._server_handle = Some(handle);
        info!("Web API plugin initialized on port 12347");
        Ok(())
    }

    fn cleanup(&mut self) {
        info!("Web API plugin cleaned up");
    }

    fn on_event(&self, _ctx: &plugin::native::PluginContext, event: &PluginEvent) -> Vec<String> {
        match event {
            PluginEvent::RoomCreate { room_id, .. } => {
                let name = room_id.clone();
                let rid = name.clone().try_into().ok();
                if let Some(rid) = rid {
                    let rooms = self.state.rooms.blocking_read();
                    if let Some(room) = rooms.get(&rid) {
                        let data = api::build_snapshot(&name, room);
                        if let Some(ss) = data {
                            let _ = self.sse_tx.send(SseEvent::CreateRoom { room: name, data: ss });
                        }
                    }
                }
            }
            PluginEvent::RoomJoin { user_id, room_id, .. } => {
                let _ = self.sse_tx.send(SseEvent::JoinRoom {
                    room: room_id.clone(), user: *user_id,
                });
                self.send_update(room_id);
            }
            PluginEvent::RoomLeave { user_id, room_id } => {
                let _ = self.sse_tx.send(SseEvent::LeaveRoom {
                    room: room_id.clone(), user: *user_id,
                });
                self.send_update(room_id);
            }
            PluginEvent::RoomModify { room_id, .. } => {
                self.send_update(room_id);
            }
            PluginEvent::GameStart { room_id, .. } => {
                let _ = self.sse_tx.send(SseEvent::StartRound {
                    room: room_id.clone(),
                });
            }
            PluginEvent::GameEnd { user_id, room_id, score, accuracy } => {
                let record = phira_mp_plus_server::server::Record {
                    id: 0, player: *user_id, score: *score,
                    perfect: 0, good: 0, bad: 0, miss: 0,
                    max_combo: 0, accuracy: *accuracy, full_combo: false,
                    std: 0.0, std_score: 0.0,
                };
                let _ = self.sse_tx.send(SseEvent::PlayerScore {
                    room: room_id.clone(), record,
                });
            }
            _ => {}
        }
        vec![]
    }
}

impl WebApiPlugin {
    fn send_update(&self, room_id: &str) {
        let rid = room_id.to_string().try_into().ok();
        let data = rid.and_then(|rid| {
            let rooms = self.state.rooms.blocking_read();
            let room = rooms.get(&rid)?;
            let snapshot = api::build_snapshot(room_id, room);
            drop(rooms);
            snapshot
        });
        if let Some(ss) = data {
            let json = serde_json::to_value(&ss.data).unwrap_or_default();
            let _ = self.sse_tx.send(SseEvent::UpdateRoom {
                room: room_id.to_string(),
                data: json,
            });
        }
    }
}

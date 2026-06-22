//! Phira-mp+ Web API 插件
//!
//! 提供 HTTP REST API 和 SSE 接口用于查询房间信息。
//! 通过 `webapi` 特性启用：`cargo build --features webapi`

use crate::plugin::{self, PluginEvent, PluginInfo};
use crate::server::{Chart, PlusServerState};
use serde::Serialize;
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::broadcast;
use tracing::{error, info};

#[cfg(feature = "webapi")]
use axum::{Json, Router, extract::Path, response::Sse, response::sse::Event, routing::get};
#[cfg(feature = "webapi")]
use futures::stream::Stream;
#[cfg(feature = "webapi")]
use tokio_stream::wrappers::BroadcastStream;
#[cfg(feature = "webapi")]
use tokio_stream::StreamExt as _;
#[cfg(feature = "webapi")]
use tower_http::cors::CorsLayer;

// ── SSE 事件广播 ──

/// SSE 事件类型
#[derive(Debug, Clone)]
pub enum SseEvent {
    CreateRoom { room: String, data: RoomSnapshot },
    UpdateRoom { room: String, data: serde_json::Value },
    JoinRoom { room: String, user: i32 },
    LeaveRoom { room: String, user: i32 },
    PlayerScore { room: String, record: crate::server::Record },
    StartRound { room: String },
}

/// SSE 广播通道容量
const SSE_CHANNEL_CAP: usize = 256;

// ── 数据模型 ──

/// 房间快照（用于 API 输出）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct RoomSnapshot {
    pub name: String,
    pub data: RoomData,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct RoomData {
    pub host: i32,
    pub users: Vec<i32>,
    pub lock: bool,
    pub cycle: bool,
    pub chart: Option<i32>,
    pub state: String,
    pub playing_users: Vec<i32>,
    pub rounds: Vec<RoundSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct RoundSnapshot {
    pub chart: i32,
    pub records: Vec<RecordData>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct RecordData {
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

// ── 房间快照构建 ──

fn build_room_snapshot(name: &str, room: &crate::room::Room) -> Option<RoomSnapshot> {
    let chart = room.chart.blocking_read().clone()?;
    Some(build_room_snapshot_inner(name, room, &Some(chart)))
}

fn build_room_snapshot_inner(name: &str, room: &crate::room::Room, chart: &Option<Chart>) -> RoomSnapshot {
    let state_guard = room.state.blocking_read();

    let users_list = room.users.blocking_read();
    let monitors_list = room.monitors.blocking_read();

    let (state_str, playing_users) = match &*state_guard {
        crate::room::InternalRoomState::SelectChart => ("SELECTING_CHART".to_string(), Vec::new()),
        crate::room::InternalRoomState::WaitForReady { .. } => ("WAITING_FOR_READY".to_string(), Vec::new()),
        crate::room::InternalRoomState::Playing { results, aborted } => {
            let pu: Vec<i32> = users_list.iter()
                .filter_map(|wu| {
                    let u = wu.upgrade()?;
                    (!results.contains_key(&u.id) && !aborted.contains(&u.id)).then_some(u.id)
                })
                .collect();
            ("PLAYING".to_string(), pu)
        }
    };
    drop(state_guard);

    let mut users: Vec<i32> = users_list.iter().filter_map(|wu| wu.upgrade().map(|u| u.id)).collect();
    users.extend(monitors_list.iter().filter_map(|wm| wm.upgrade().map(|u| u.id)));
    drop(users_list);
    drop(monitors_list);

    let host_id = room.host.blocking_read().upgrade().map(|u| u.id).unwrap_or(0);

    // 构建 rounds
    let history = room.play_history.blocking_read();
    let rounds: Vec<RoundSnapshot> = history.iter().map(|round| {
        RoundSnapshot {
            chart: round.chart_id,
            records: round.results.iter().map(|r| RecordData {
                id: 0,
                player: r.user_id,
                score: r.score,
                perfect: r.perfect,
                good: r.good,
                bad: r.bad,
                miss: r.miss,
                max_combo: r.max_combo,
                accuracy: r.accuracy,
                full_combo: r.full_combo,
                std: 0.0,
                std_score: r.std_score,
            }).collect(),
        }
    }).collect();
    drop(history);

    RoomSnapshot {
        name: name.to_string(),
        data: RoomData {
            host: host_id,
            users,
            lock: room.locked.load(Ordering::SeqCst),
            cycle: room.cycle.load(Ordering::SeqCst),
            chart: chart.as_ref().map(|c| c.id),
            state: state_str,
            playing_users,
            rounds,
        }
    }
}

// ── Web API 插件 ──

/// Web API 插件工厂
pub fn create(state: Arc<PlusServerState>) -> Box<dyn plugin::native::NativePlugin> {
    Box::new(WebApiPlugin {
        state,
        sse_tx: broadcast::Sender::new(SSE_CHANNEL_CAP),
        server_handle: std::sync::Mutex::new(None),
    })
}

struct WebApiPlugin {
    state: Arc<PlusServerState>,
    sse_tx: broadcast::Sender<SseEvent>,
    server_handle: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl plugin::native::NativePlugin for WebApiPlugin {
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
        // 启动 HTTP 服务器
        let state = Arc::clone(&self.state);
        let sse_tx = self.sse_tx.clone();
        let handle = tokio::spawn(async move {
            start_http_server(state, sse_tx).await;
        });
        *self.server_handle.lock().unwrap() = Some(handle);
        info!("Web API plugin initialized on port 12347");
        Ok(())
    }

    fn cleanup(&mut self) {
        if let Some(handle) = self.server_handle.lock().unwrap().take() {
            handle.abort();
        }
        info!("Web API plugin cleaned up");
    }

    fn on_event(&self, _ctx: &plugin::native::PluginContext, event: &PluginEvent) -> Vec<String> {
        // 将插件事件转发为 SSE 事件
        match event {
            PluginEvent::RoomCreate { user_id: _, room_id } => {
                let room_opt = {
                    let rooms = self.state.rooms.blocking_read();
                    let rid: phira_mp_common::RoomId = match room_id.clone().try_into() {
                        Ok(id) => id,
                        Err(_) => return vec![],
                    };
                    rooms.get(&rid).map(|r| (room_id.clone(), Arc::clone(r)))
                };
                if let Some((name, room)) = room_opt {
                    let snapshot = build_room_snapshot(&name, &room);
                    if let Some(ss) = snapshot {
                        let _ = self.sse_tx.send(SseEvent::CreateRoom { room: name, data: ss });
                    }
                }
            }
            PluginEvent::RoomJoin { user_id, room_id, .. } => {
                let _ = self.sse_tx.send(SseEvent::JoinRoom {
                    room: room_id.clone(),
                    user: *user_id,
                });
                // 也发送 update_room
                self.send_room_update(room_id);
            }
            PluginEvent::RoomLeave { user_id, room_id } => {
                let _ = self.sse_tx.send(SseEvent::LeaveRoom {
                    room: room_id.clone(),
                    user: *user_id,
                });
                self.send_room_update(room_id);
            }
            PluginEvent::RoomModify { room_id, .. } => {
                self.send_room_update(room_id);
            }
            PluginEvent::GameStart { room_id, .. } => {
                let _ = self.sse_tx.send(SseEvent::StartRound {
                    room: room_id.clone(),
                });
            }
            PluginEvent::GameEnd { user_id, room_id, score, accuracy } => {
                // 构造 Record
                let record = crate::server::Record {
                    id: 0,
                    player: *user_id,
                    score: *score,
                    perfect: 0,
                    good: 0,
                    bad: 0,
                    miss: 0,
                    max_combo: 0,
                    accuracy: *accuracy,
                    full_combo: false,
                    std: 0.0,
                    std_score: 0.0,
                };
                let _ = self.sse_tx.send(SseEvent::PlayerScore {
                    room: room_id.clone(),
                    record,
                });
            }
            PluginEvent::UserConnect { .. } | PluginEvent::UserDisconnect { .. }
            | PluginEvent::PlayerTouches { .. } | PluginEvent::PlayerJudges { .. } => {}
        }
        vec![]
    }
}

impl WebApiPlugin {
    fn send_room_update(&self, room_id: &str) {
        let rooms = self.state.rooms.blocking_read();
        let rid: phira_mp_common::RoomId = match room_id.to_string().try_into() {
            Ok(id) => id,
            Err(_) => return,
        };
        if let Some(room) = rooms.get(&rid) {
            let snapshot = build_room_snapshot(room_id, room);
            if let Some(ss) = snapshot {
                let json = serde_json::to_value(&ss.data).unwrap_or_default();
                let _ = self.sse_tx.send(SseEvent::UpdateRoom {
                    room: room_id.to_string(),
                    data: json,
                });
            }
        }
    }
}

// ── HTTP 服务器 ──

#[cfg(feature = "webapi")]
async fn start_http_server(state: Arc<PlusServerState>, sse_tx: broadcast::Sender<SseEvent>) {

    let app_state = Arc::new(ApiState { state, sse_tx });

    let app = Router::new()
        .route("/api/rooms/info", get(rooms_info))
        .route("/api/rooms/info/{name}", get(room_info_by_name))
        .route("/api/rooms/user/{user_id}", get(room_info_by_user))
        .route("/api/rooms/listen", get(rooms_listen))
        .layer(CorsLayer::permissive())
        .with_state(app_state);

    let addr = "0.0.0.0:12347";
    info!("Web API server listening on http://{}", addr);
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to bind Web API server: {e}");
            return;
        }
    };
    if let Err(e) = axum::serve(listener, app).await {
        error!("Web API server error: {e}");
    }
}

#[cfg(not(feature = "webapi"))]
async fn start_http_server(_state: Arc<PlusServerState>, _sse_tx: broadcast::Sender<SseEvent>) {
    warn!("Web API plugin requires 'webapi' feature: cargo build --features webapi");
}

// ── Axum 应用状态 ──

struct ApiState {
    state: Arc<PlusServerState>,
    sse_tx: broadcast::Sender<SseEvent>,
}

// ── 路由处理函数 ──

#[cfg(feature = "webapi")]
async fn rooms_info(
    axum::extract::State(api): axum::extract::State<Arc<ApiState>>,
) -> Json<Vec<RoomSnapshot>> {
    let rooms = api.state.rooms.read().await;
    let mut result = Vec::new();
    for (rid, room) in rooms.iter() {
        let name_str = rid.to_string();
        if let Some(ss) = build_room_snapshot_async(&name_str, room).await {
            result.push(ss);
        }
    }
    Json(result)
}

#[cfg(feature = "webapi")]
async fn room_info_by_name(
    axum::extract::State(api): axum::extract::State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Result<Json<RoomSnapshot>, (axum::http::StatusCode, String)> {
    let rid: phira_mp_common::RoomId = name.clone().try_into()
        .map_err(|_| (axum::http::StatusCode::BAD_REQUEST, "invalid room name".into()))?;
    let rooms = api.state.rooms.read().await;
    match rooms.get(&rid) {
        Some(room) => {
            build_room_snapshot_async(&name, room).await
                .map(Json)
                .ok_or_else(|| (axum::http::StatusCode::NOT_FOUND, "no chart selected".into()))
        }
        None => Err((axum::http::StatusCode::NOT_FOUND, "room not found".into())),
    }
}

#[cfg(feature = "webapi")]
async fn room_info_by_user(
    axum::extract::State(api): axum::extract::State<Arc<ApiState>>,
    Path(user_id): Path<i32>,
) -> Result<Json<RoomSnapshot>, (axum::http::StatusCode, String)> {
    let user = {
        let users = api.state.users.read().await;
        users.get(&user_id).map(Arc::clone)
    };
    match user {
        Some(u) => {
            let room_guard = u.room.read().await;
            match room_guard.as_ref() {
                Some(room) => {
                    let name = room.id.to_string();
                    build_room_snapshot_async(&name, room).await
                        .map(Json)
                        .ok_or_else(|| (axum::http::StatusCode::NOT_FOUND, "no chart selected".into()))
                }
                None => Err((axum::http::StatusCode::NOT_FOUND, "user not in room".into())),
            }
        }
        None => Err((axum::http::StatusCode::NOT_FOUND, "user not found".into())),
    }
}

#[cfg(feature = "webapi")]
async fn rooms_listen(
    axum::extract::State(api): axum::extract::State<Arc<ApiState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    use axum::response::sse::KeepAlive;

    let rx = api.sse_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| {
        match result {
            Ok(event) => {
                let (event_type, data) = sse_event_to_string(event);
                Some(Ok(Event::default()
                    .event(event_type)
                    .data(data)))
            }
            Err(_) => None,
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(feature = "webapi")]
fn sse_event_to_string(event: SseEvent) -> (String, String) {
    match event {
        SseEvent::CreateRoom { room, data } => {
            ("create_room".into(), serde_json::json!({"room": room, "data": data}).to_string())
        }
        SseEvent::UpdateRoom { room, data } => {
            ("update_room".into(), serde_json::json!({"room": room, "data": data}).to_string())
        }
        SseEvent::JoinRoom { room, user } => {
            ("join_room".into(), serde_json::json!({"room": room, "user": user}).to_string())
        }
        SseEvent::LeaveRoom { room, user } => {
            ("leave_room".into(), serde_json::json!({"room": room, "user": user}).to_string())
        }
        SseEvent::PlayerScore { room, record } => {
            ("player_score".into(), serde_json::json!({"room": room, "record": record}).to_string())
        }
        SseEvent::StartRound { room } => {
            ("start_round".into(), serde_json::json!({"room": room}).to_string())
        }
    }
}

// ── 异步版 snapshot 构建 ──

#[cfg(feature = "webapi")]
async fn build_room_snapshot_async(name: &str, room: &crate::room::Room) -> Option<RoomSnapshot> {
    let chart = room.chart.read().await.clone();
    let state_guard = room.state.read().await;
    let (state_str, playing_users) = match &*state_guard {
        crate::room::InternalRoomState::SelectChart => ("SELECTING_CHART".to_string(), vec![]),
        crate::room::InternalRoomState::WaitForReady { .. } => ("WAITING_FOR_READY".to_string(), vec![]),
        crate::room::InternalRoomState::Playing { results, aborted } => {
            let mut pu = Vec::new();
            for u in room.users().await {
                if !results.contains_key(&u.id) && !aborted.contains(&u.id) {
                    pu.push(u.id);
                }
            }
            ("PLAYING".to_string(), pu)
        }
    };
    drop(state_guard);

    let users: Vec<i32> = room.users().await.into_iter()
        .chain(room.monitors().await.into_iter())
        .map(|u| u.id)
        .collect();

    let host_id = room.host.read().await.upgrade().map(|u| u.id).unwrap_or(0);

    let history = room.play_history.read().await;
    let rounds: Vec<RoundSnapshot> = history.iter().map(|round| {
        RoundSnapshot {
            chart: round.chart_id,
            records: round.results.iter().map(|r| RecordData {
                id: 0,
                player: r.user_id,
                score: r.score,
                perfect: r.perfect,
                good: r.good,
                bad: r.bad,
                miss: r.miss,
                max_combo: r.max_combo,
                accuracy: r.accuracy,
                full_combo: r.full_combo,
                std: 0.0,
                std_score: r.std_score,
            }).collect(),
        }
    }).collect();
    drop(history);

    Some(RoomSnapshot {
        name: name.to_string(),
        data: RoomData {
            host: host_id,
            users,
            lock: room.locked.load(Ordering::SeqCst),
            cycle: room.cycle.load(Ordering::SeqCst),
            chart: chart.as_ref().map(|c| c.id),
            state: state_str,
            playing_users,
            rounds,
        },
    })
}

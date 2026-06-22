//! REST API 处理器、SSE 流、房间快照构建

use phira_mp_plus_server::room::{InternalRoomState, Room};
use phira_mp_plus_server::server::PlusServerState;
use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::broadcast;
use tracing::{error, info};

use crate::sse::SseEvent;
use axum::{Json, Router, extract::Path, response::Sse, response::sse::Event, routing::get};
use futures::stream::Stream;
use std::convert::Infallible;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt as _;
use tower_http::cors::CorsLayer;

// ── 数据模型 ──

/// API 输出用的房间快照
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

// ── 快照构建 ──

/// 构建房间快照（阻塞版本，从 on_event 同步调用）
pub fn build_snapshot(name: &str, room: &Room) -> Option<RoomSnapshot> {
    let chart = room.chart.blocking_read().clone();
    let state_guard = room.state.blocking_read();

    let users_list = room.users.blocking_read();
    let monitors_list = room.monitors.blocking_read();

    let (state_str, playing_users) = match &*state_guard {
        InternalRoomState::SelectChart => ("SELECTING_CHART".to_string(), Vec::new()),
        InternalRoomState::WaitForReady { .. } => ("WAITING_FOR_READY".to_string(), Vec::new()),
        InternalRoomState::Playing { results, aborted } => {
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

    let mut users: Vec<i32> = users_list.iter()
        .filter_map(|wu| wu.upgrade().map(|u| u.id)).collect();
    users.extend(monitors_list.iter().filter_map(|wm| wm.upgrade().map(|u| u.id)));
    drop(users_list);
    drop(monitors_list);

    let host_id = room.host.blocking_read()
        .upgrade().map(|u| u.id).unwrap_or(0);

    let history = room.play_history.blocking_read();
    let rounds: Vec<RoundSnapshot> = history.iter().map(|round| RoundSnapshot {
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

/// 异步版快照构建（从 HTTP handler 调用）
pub async fn build_snapshot_async(name: &str, room: &Room) -> Option<RoomSnapshot> {
    let chart = room.chart.read().await.clone();
    let state_guard = room.state.read().await;
    let users_list = room.users.read().await;
    let monitors_list = room.monitors.read().await;

    let (state_str, playing_users) = match &*state_guard {
        InternalRoomState::SelectChart => ("SELECTING_CHART".to_string(), Vec::new()),
        InternalRoomState::WaitForReady { .. } => ("WAITING_FOR_READY".to_string(), Vec::new()),
        InternalRoomState::Playing { results, aborted } => {
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

    let mut users: Vec<i32> = users_list.iter()
        .filter_map(|wu| wu.upgrade().map(|u| u.id)).collect();
    users.extend(monitors_list.iter().filter_map(|wm| wm.upgrade().map(|u| u.id)));
    drop(users_list);
    drop(monitors_list);

    let host_id = room.host.read().await.upgrade().map(|u| u.id).unwrap_or(0);

    let history = room.play_history.read().await;
    let rounds: Vec<RoundSnapshot> = history.iter().map(|round| RoundSnapshot {
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

// ── HTTP 服务器 ──

/// 启动 Axum HTTP 服务器（监听 :12347）
pub async fn start_http_server(state: Arc<PlusServerState>, sse_tx: broadcast::Sender<SseEvent>) {
    let app_state = Arc::new(ApiState { state, sse_tx });

    let app = Router::new()
        .route("/api/rooms/info", get(rooms_info))
        .route("/api/rooms/info/{name}", get(room_info_by_name))
        .route("/api/rooms/user/{user_id}", get(room_info_by_user))
        .route("/api/rooms/listen", get(rooms_listen))
        .layer(CorsLayer::permissive())
        .with_state(app_state);

    let addr = "0.0.0.0:12347";
    info!("Web API listening on http://{}", addr);
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => { error!("Failed to bind Web API: {e}"); return; }
    };
    if let Err(e) = axum::serve(listener, app).await {
        error!("Web API server error: {e}");
    }
}

// ── Axum app state ──

struct ApiState {
    state: Arc<PlusServerState>,
    sse_tx: broadcast::Sender<SseEvent>,
}

// ── Route handlers ──

async fn rooms_info(
    axum::extract::State(api): axum::extract::State<Arc<ApiState>>,
) -> Json<Vec<RoomSnapshot>> {
    let rooms = api.state.rooms.read().await;
    let mut result = Vec::new();
    for (rid, room) in rooms.iter() {
        let name = rid.to_string();
        if let Some(ss) = build_snapshot_async(&name, room).await {
            result.push(ss);
        }
    }
    Json(result)
}

async fn room_info_by_name(
    axum::extract::State(api): axum::extract::State<Arc<ApiState>>,
    Path(name): Path<String>,
) -> Result<Json<RoomSnapshot>, (axum::http::StatusCode, String)> {
    let rid: phira_mp_common::RoomId = name.clone().try_into()
        .map_err(|_| (axum::http::StatusCode::BAD_REQUEST, "invalid room name".into()))?;
    let rooms = api.state.rooms.read().await;
    match rooms.get(&rid) {
        Some(room) => build_snapshot_async(&name, room).await
            .map(Json)
            .ok_or_else(|| (axum::http::StatusCode::NOT_FOUND, "no chart".into())),
        None => Err((axum::http::StatusCode::NOT_FOUND, "room not found".into())),
    }
}

async fn room_info_by_user(
    axum::extract::State(api): axum::extract::State<Arc<ApiState>>,
    Path(user_id): Path<i32>,
) -> Result<Json<RoomSnapshot>, (axum::http::StatusCode, String)> {
    let users = api.state.users.read().await;
    let user = match users.get(&user_id) {
        Some(u) => Arc::clone(u),
        None => return Err((axum::http::StatusCode::NOT_FOUND, "user not found".into())),
    };
    drop(users);
    let room_guard = user.room.read().await;
    match room_guard.as_ref() {
        Some(room) => {
            let name = room.id.to_string();
            build_snapshot_async(&name, room).await
                .map(Json)
                .ok_or_else(|| (axum::http::StatusCode::NOT_FOUND, "no chart".into()))
        }
        None => Err((axum::http::StatusCode::NOT_FOUND, "user not in room".into())),
    }
}

async fn rooms_listen(
    axum::extract::State(api): axum::extract::State<Arc<ApiState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    use axum::response::sse::KeepAlive;
    let rx = api.sse_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|result| {
        match result {
            Ok(ev) => {
                let (ty, data) = sse_event_to_string(ev);
                Some(Ok(Event::default().event(ty).data(data)))
            }
            Err(_) => None,
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn sse_event_to_string(event: SseEvent) -> (String, String) {
    match event {
        SseEvent::CreateRoom { room, data } =>
            ("create_room".into(), serde_json::json!({"room": room, "data": data}).to_string()),
        SseEvent::UpdateRoom { room, data } =>
            ("update_room".into(), serde_json::json!({"room": room, "data": data}).to_string()),
        SseEvent::JoinRoom { room, user } =>
            ("join_room".into(), serde_json::json!({"room": room, "user": user}).to_string()),
        SseEvent::LeaveRoom { room, user } =>
            ("leave_room".into(), serde_json::json!({"room": room, "user": user}).to_string()),
        SseEvent::PlayerScore { room, record } =>
            ("player_score".into(), serde_json::json!({"room": room, "record": record}).to_string()),
        SseEvent::StartRound { room } =>
            ("start_round".into(), serde_json::json!({"room": room}).to_string()),
    }
}

//! Web API 处理器 — 由中央 HTTP 服务器调用

use phira_mp_plus_server::room::{InternalRoomState, Room};
use phira_mp_plus_server::server::PlusServerState;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::Ordering;

// ── 数据模型 ──

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
    pub records: Vec<serde_json::Value>,
}

// ── 处理器 ──

/// GET /api/rooms/info
pub fn rooms_info(state: &Arc<PlusServerState>) -> Result<Value, (u16, String)> {
    let rooms = state.rooms.blocking_read();
    let mut list = Vec::new();
    for (rid, room) in rooms.iter() {
        if let Some(ss) = build_snapshot(&rid.to_string(), room) {
            list.push(serde_json::to_value(ss).unwrap_or_default());
        }
    }
    Ok(Value::Array(list))
}

/// GET /api/rooms/info/<name>
pub fn room_by_name(state: &Arc<PlusServerState>, name: &str) -> Result<Value, (u16, String)> {
    let rid: phira_mp_common::RoomId = name.to_string().try_into()
        .map_err(|_| (400u16, "invalid room name".to_string()))?;
    let rooms = state.rooms.blocking_read();
    match rooms.get(&rid) {
        Some(room) => build_snapshot(name, room)
            .and_then(|ss| serde_json::to_value(ss).ok())
            .ok_or_else(|| (404u16, "no chart".to_string())),
        None => Err((404u16, "room not found".to_string())),
    }
}

/// GET /api/rooms/user/<user_id>
pub fn room_by_user(state: &Arc<PlusServerState>, user_id: i32) -> Result<Value, (u16, String)> {
    let users = state.users.blocking_read();
    let user = users.get(&user_id)
        .ok_or_else(|| (404u16, "user not found".to_string()))?;
    let room_guard = user.room.blocking_read();
    match room_guard.as_ref() {
        Some(room) => {
            let name = room.id.to_string();
            build_snapshot(&name, room)
                .and_then(|ss| serde_json::to_value(ss).ok())
                .ok_or_else(|| (404u16, "no chart".to_string()))
        }
        None => Err((404u16, "user not in room".to_string())),
    }
}

// ── 快照构建 ──

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
    let rounds: Vec<RoundSnapshot> = history.iter().map(|round| {
        let records: Vec<serde_json::Value> = round.results.iter().map(|r| {
            serde_json::json!({
                "player": r.user_id,
                "score": r.score,
                "perfect": r.perfect,
                "good": r.good,
                "bad": r.bad,
                "miss": r.miss,
                "max_combo": r.max_combo,
                "accuracy": r.accuracy,
                "full_combo": r.full_combo,
                "std_score": r.std_score,
            })
        }).collect();
        RoundSnapshot { chart: round.chart_id, records }
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

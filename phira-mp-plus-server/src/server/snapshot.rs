//! Room/user snapshot types and builders.
//!
//! After Phase 2 Work C, Room no longer holds mutable state. Snapshots are
//! built from control_snapshot() (sync, reads actor cache) and the actor
//! RoomSnapshot from room_snapshot().

use crate::server::state::PlusServerState;
use serde::Serialize;
use serde_json::Value;
use std::sync::atomic::Ordering;

// ── PlusServerState snapshot helper ──────────────────────────────────

impl PlusServerState {
    /// Get the latest RoomActor snapshot for a room, if available.
    /// Falls back to None if the room has no actor yet.
    pub fn room_snapshot(
        &self,
        room_id: &str,
    ) -> Option<crate::room_actor::actor::RoomSnapshot> {
        self.room_commands.room_snapshot(room_id)
    }
}

// ── Snapshot types ─────────────────────────────────────────────────

#[derive(Serialize)]
pub(super) struct RoomSnapshot {
    pub(super) name: String,
    pub(super) data: RoomData,
}

#[derive(Serialize)]
pub(super) struct UserSnapshot {
    pub(super) id: i32,
    pub(super) name: String,
    pub(super) monitor: bool,
    pub(super) is_host: bool,
    pub(super) in_room: bool,
    pub(super) has_session: bool,
}

#[derive(Serialize)]
pub(super) struct RoomData {
    pub(super) host: i32,
    pub(super) users: Vec<i32>,
    pub(super) lock: bool,
    pub(super) cycle: bool,
    pub(super) chart: Option<i32>,
    pub(super) chart_name: Option<String>,
    pub(super) state: String,
    pub(super) playing_users: Vec<i32>,
    pub(super) rounds: Vec<RoundInfo>,
    pub(super) hidden: bool,
    pub(super) phira_api_endpoint: String,
    pub(super) phira_api_endpoint_override: Option<String>,
    pub(super) id: String,
    pub(super) uuid: String,
    pub(super) created_at: i64,
    pub(super) live: bool,
    pub(super) locked: bool,
    pub(super) cycling: bool,
    pub(super) persistent_empty: bool,
    pub(super) max_users: usize,
    pub(super) player_count: usize,
    pub(super) monitor_count: usize,
    pub(super) user_ids: Vec<i32>,
    pub(super) monitor_ids: Vec<i32>,
    pub(super) host_user: Option<UserSnapshot>,
    pub(super) host_is_system: bool,
    pub(super) users_info: Vec<UserSnapshot>,
    pub(super) monitors_info: Vec<UserSnapshot>,
    pub(super) chart_info: Option<Value>,
    pub(super) phira_api_endpoint_effective: String,
    pub(super) phira_api_endpoint_using_override: bool,
    pub(super) ready_users: Vec<i32>,
    pub(super) finished_users: Vec<i32>,
    pub(super) aborted_users: Vec<i32>,
    pub(super) result_count: usize,
    pub(super) current_round_id: Option<String>,
    pub(super) state_detail: Value,
    pub(super) round_history: Vec<RoundInfo>,
}

#[derive(Serialize, Clone)]
pub(super) struct RoundInfo {
    pub(super) round_id: String,
    pub(super) chart: i32,
    pub(super) chart_id: i32,
    pub(super) chart_name: String,
    pub(super) records: Vec<Value>,
    pub(super) results: Vec<Value>,
}

// ── Builders ────────────────────────────────────────────────────────

fn user_snapshot(
    room: &crate::room::Room,
    user: &crate::session::User,
    is_host: bool,
    in_room: bool,
) -> UserSnapshot {
    let has_session = user
        .session
        .try_read()
        .ok()
        .and_then(|session| session.as_ref().and_then(|weak| weak.upgrade()))
        .is_some();
    UserSnapshot {
        id: user.id,
        name: user.name.clone(),
        monitor: user.monitor.load(Ordering::SeqCst),
        is_host,
        in_room,
        has_session,
    }
}

pub(super) fn build_snapshot(
    state: &PlusServerState,
    name: &str,
    room: &crate::room::Room,
) -> RoomSnapshot {
    // Read from actor snapshot cache and control snapshot (both sync).
    let actor_snap = state.room_snapshot(&room.id.to_string());
    let control = room.control_snapshot();

    let users_arcs: Vec<_> = {
        let ul = crate::read_lock!(room.users);
        ul.iter().filter_map(|w| w.upgrade()).collect()
    };
    let monitor_arcs: Vec<_> = {
        let ml = crate::read_lock!(room.monitors);
        ml.iter().filter_map(|w| w.upgrade()).collect()
    };
    let host_is_system = control.system_host;
    let host = if host_is_system {
        -1
    } else {
        control.host_id.unwrap_or(-1)
    };

    // Read lifecycle from actor snapshot.
    let actor_stripped = actor_snap.as_ref().map(|s| s.stripped);
    let actor_chart = actor_snap.as_ref().and_then(|s| s.chart);

    let (st, playing_users, ready_users, finished_users, aborted_users, result_count, state_detail) =
        match actor_stripped {
            Some(phira_mp_common::StrippedRoomState::SelectingChart) | None => (
                "SELECTING_CHART".to_string(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                0usize,
                serde_json::json!({"kind":"select_chart"}),
            ),
            Some(phira_mp_common::StrippedRoomState::WaitingForReady) => (
                "WAITING_FOR_READY".to_string(),
                Vec::new(),
                users_arcs.iter().map(|u| u.id).collect(),
                Vec::new(),
                Vec::new(),
                0usize,
                serde_json::json!({"kind":"wait_for_ready", "ready_users": users_arcs.iter().map(|u| u.id).collect::<Vec<_>>()}),
            ),
            Some(phira_mp_common::StrippedRoomState::Playing) => {
                let finished: Vec<i32> = users_arcs.iter().map(|u| u.id).collect();
                let playing: Vec<i32> = Vec::new();
                (
                    "PLAYING".to_string(),
                    playing,
                    Vec::new(),
                    finished.clone(),
                    Vec::new(),
                    finished.len(),
                    serde_json::json!({
                        "kind":"playing",
                        "finished_users": finished,
                        "aborted_users": Vec::<i32>::new(),
                        "result_count": finished.len(),
                    }),
                )
            }
        };

    let mut users: Vec<i32> = users_arcs.iter().map(|u| u.id).collect();
    let monitor_ids: Vec<i32> = monitor_arcs.iter().map(|u| u.id).collect();
    users.extend(monitor_ids.iter().copied());
    let user_ids: Vec<i32> = users_arcs.iter().map(|u| u.id).collect();

    let users_info: Vec<UserSnapshot> = users_arcs
        .iter()
        .map(|u| user_snapshot(room, u, u.id == host, true))
        .collect();
    let monitors_info: Vec<UserSnapshot> = monitor_arcs
        .iter()
        .map(|u| user_snapshot(room, u, u.id == host, true))
        .collect();
    // Host user lookup from user list.
    let host_user = users_arcs.iter().find(|u| u.id == host).map(|u| {
        user_snapshot(
            room,
            u,
            true,
            user_ids.contains(&u.id) || monitor_ids.contains(&u.id),
        )
    });

    let phira_api_endpoint = state.config.phira_api_endpoint.clone();
    let phira_api_endpoint_override: Option<String> = control.phira_api_endpoint;
    let using_override = phira_api_endpoint_override.is_some();
    let current_round_id: Option<String> = None; // No longer on Room.
    let rounds: Vec<RoundInfo> = room
        .play_history
        .recent_sync()
        .iter()
        .map(|r| {
            let results: Vec<Value> = r
                .results
                .iter()
                .map(|res| {
                    serde_json::json!({
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
                    })
                })
                .collect();
            RoundInfo {
                round_id: r.round_id.to_string(),
                chart: r.chart_id,
                chart_id: r.chart_id,
                chart_name: r.chart_name.clone(),
                records: results.clone(),
                results,
            }
        })
        .collect();

    let chart_info = actor_chart.map(|cid| {
        serde_json::json!({
            "id": cid,
            "name": "",
        })
    });

    RoomSnapshot {
        name: name.into(),
        data: RoomData {
            host,
            users,
            lock: control.locked,
            cycle: control.cycle,
            chart: actor_chart,
            chart_name: None,
            state: st,
            playing_users,
            rounds: rounds.clone(),
            hidden: control.hidden,
            phira_api_endpoint: phira_api_endpoint.clone(),
            phira_api_endpoint_override: phira_api_endpoint_override.clone(),
            id: room.id.to_string(),
            uuid: room.uuid.to_string(),
            created_at: room.created_at,
            live: room.is_live(),
            locked: control.locked,
            cycling: control.cycle,
            persistent_empty: control.persistent_empty,
            max_users: control.max_users,
            player_count: users_arcs.len(),
            monitor_count: monitor_arcs.len(),
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

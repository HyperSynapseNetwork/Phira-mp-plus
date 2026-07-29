//! Room state query helpers for the phira-room-state WIT interface.
//!
//! These functions extract structured data from room snapshots returned
//! by the server state query dispatch.

use serde_json;

/// Extract the `data` sub-object from a room snapshot.
pub(crate) fn extract_snapshot_data(v: &serde_json::Value) -> Result<&serde_json::Value, String> {
    v.get("data").ok_or_else(|| "snapshot missing 'data' field".to_string())
}

/// Build a `RoomPlayer` vector from room snapshot data.
pub(crate) fn build_room_players(data: &serde_json::Value) -> Vec<crate::plugin_abi::wit_abi::phira::plugin::phira_room_state::RoomPlayer> {
    use crate::plugin_abi::wit_abi::phira::plugin::phira_room_state::RoomPlayer;

    let ready: Vec<i32> = data.get("ready_users")
        .and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
    let finished: Vec<i32> = data.get("finished_users")
        .and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();

    let mut players = Vec::new();

    // Users (non-monitor players)
    if let Some(users_info) = data.get("users_info").and_then(|v| v.as_array()) {
        for u in users_info {
            let uid = u.get("id").and_then(|v| v.as_i64()).unwrap_or(0) as u32;
            players.push(RoomPlayer {
                user_id: uid,
                display_name: u.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                is_monitor: false,
                is_ready: ready.contains(&(uid as i32)),
                is_host: u.get("is_host").and_then(|v| v.as_bool()).unwrap_or(false),
                is_finished: finished.contains(&(uid as i32)),
                score: None,
                accuracy: None,
            });
        }
    }

    // Monitors
    if let Some(monitors_info) = data.get("monitors_info").and_then(|v| v.as_array()) {
        for m in monitors_info {
            let uid = m.get("id").and_then(|v| v.as_i64()).unwrap_or(0) as u32;
            players.push(RoomPlayer {
                user_id: uid,
                display_name: m.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                is_monitor: true,
                is_ready: ready.contains(&(uid as i32)),
                is_host: m.get("is_host").and_then(|v| v.as_bool()).unwrap_or(false),
                is_finished: finished.contains(&(uid as i32)),
                score: None,
                accuracy: None,
            });
        }
    }

    players
}

/// Extract the current round info from room snapshot data.
pub(crate) fn extract_current_round(data: &serde_json::Value) -> Option<crate::plugin_abi::wit_abi::phira::plugin::phira_room_state::RoundInfo> {
    use crate::plugin_abi::wit_abi::phira::plugin::phira_room_state::RoundInfo;

    // Try current_round_id first, then fall back to the last round in
    // round_history when the room state indicates an active round.
    if let Some(rid) = data.get("current_round_id").and_then(|v| v.as_str()) {
        if !rid.is_empty() {
            let phase = data.get("state").and_then(|v| v.as_str()).unwrap_or("").to_string();
            return Some(RoundInfo {
                round_id: rid.to_string(),
                chart_id: data.get("chart").and_then(|v| v.as_i64()).map(|n| n as u32),
                chart_name: data.get("chart_name").and_then(|v| v.as_str()).map(str::to_string),
                phase,
            });
        }
    }
    // Fall back to the last round in round_history
    if let Some(rounds) = data.get("round_history").and_then(|v| v.as_array()) {
        if let Some(last) = rounds.last() {
            let round_id = last.get("round_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if !round_id.is_empty() {
                let phase = data.get("state").and_then(|v| v.as_str()).unwrap_or("").to_string();
                return Some(RoundInfo {
                    round_id,
                    chart_id: last.get("chart_id").and_then(|v| v.as_i64()).map(|n| n as u32),
                    chart_name: last.get("chart_name").and_then(|v| v.as_str()).map(str::to_string),
                    phase,
                });
            }
        }
    }
    None
}

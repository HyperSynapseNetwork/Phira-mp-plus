//! Gameplay telemetry handling for sessions.
//!
//! Touch/Judge processing is intentionally outside `session.rs`: this path is
//! high-frequency and coordinates persistence, EventBus, plugins and monitor
//! broadcast.
//!
//! Touch/Judge telemetry goes through the HighFrequencyWriter which
//! bypasses WAL for maximum throughput.

use crate::persistence::high_frequency::{HighFrequencyItem, HighFrequencyKind};
use crate::plugin::PluginEvent;
use crate::room::Room;
use crate::session::User;
use phira_mp_common::{JudgeEvent, ServerCommand, TouchFrame};
use std::sync::Arc;
use tracing::{debug, trace, warn};

/// Cache a touch batch via the actor mailbox, then persist via
/// HighFrequencyWriter (bypassing WAL for maximum throughput).
async fn persist_touches(
    user: &Arc<User>,
    room: &Arc<Room>,
    touch_data: &[crate::plugin::TouchEventPoint],
    has_active_monitors: bool,
) {
    // Route touch caching through the per-room actor mailbox.
    if let Err(e) = user
        .server
        .room_commands
        .add_touches(&room.id.to_string(), user.id, touch_data)
        .await
    {
        trace!(
            room = %room.id, user_id = user.id,
            "failed to cache touches via actor mailbox: {e}"
        );
    }
    let round_id = user
        .server
        .room_commands
        .room_snapshot(&room.id.to_string())
        .and_then(|snap| snap.round_id.map(|rid| rid.to_string()));
    if !should_persist_round_telemetry(touch_data.len(), round_id.is_some(), has_active_monitors) {
        debug!(room = %room.id, user_id = user.id, "touch data received without current round; cached only");
        return;
    }

    let rid = round_id.expect("round id checked by telemetry persistence plan");
    let payload = serde_json::json!({
        "event_id": uuid::Uuid::new_v4().to_string(),
        "room_id": room.id.to_string(),
        "round_id": rid,
        "user_id": user.id,
        "count": touch_data.len(),
        "data": touch_data,
    });

    let created_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let item = HighFrequencyItem {
        kind: HighFrequencyKind::Touch,
        round_id: rid.to_string(),
        user_id: user.id,
        payload,
        created_at_ms,
        admission_seq: 0,
    };

    match user
        .server
        .high_frequency_writer
        .enqueue(item)
        .await
    {
        Ok(crate::persistence::high_frequency::EnqueueOutcome::MainQueue)
        | Ok(crate::persistence::high_frequency::EnqueueOutcome::OverflowQueue) => {
            trace!(room = %room.id, user_id = user.id, "touch batch enqueued to high frequency writer");
        }
        Ok(crate::persistence::high_frequency::EnqueueOutcome::Dropped) => {
            warn!(room = %room.id, user_id = user.id, "touch batch dropped by high frequency writer");
        }
        Err(e) => {
            warn!(room = %room.id, user_id = user.id, "touch batch could not be enqueued to high frequency writer: {e}");
        }
    }
}

/// Cache a judge batch via the actor mailbox, then persist via
/// HighFrequencyWriter (bypassing WAL for maximum throughput).
async fn persist_judges(
    user: &Arc<User>,
    room: &Arc<Room>,
    judge_data: &[crate::plugin::JudgeEventItem],
    has_active_monitors: bool,
) {
    // Route judge caching through the per-room actor mailbox.
    if let Err(e) = user
        .server
        .room_commands
        .add_judges(&room.id.to_string(), user.id, judge_data)
        .await
    {
        trace!(
            room = %room.id, user_id = user.id,
            "failed to cache judges via actor mailbox: {e}"
        );
    }
    let round_id = user
        .server
        .room_commands
        .room_snapshot(&room.id.to_string())
        .and_then(|snap| snap.round_id.map(|rid| rid.to_string()));
    if !should_persist_round_telemetry(judge_data.len(), round_id.is_some(), has_active_monitors) {
        debug!(room = %room.id, user_id = user.id, "judge data received without current round; cached only");
        return;
    }

    let rid = round_id.expect("round id checked by telemetry persistence plan");
    let payload = serde_json::json!({
        "event_id": uuid::Uuid::new_v4().to_string(),
        "room_id": room.id.to_string(),
        "round_id": rid,
        "user_id": user.id,
        "count": judge_data.len(),
        "data": judge_data,
    });

    let created_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let item = HighFrequencyItem {
        kind: HighFrequencyKind::Judge,
        round_id: rid.to_string(),
        user_id: user.id,
        payload,
        created_at_ms,
        admission_seq: 0,
    };

    match user
        .server
        .high_frequency_writer
        .enqueue(item)
        .await
    {
        Ok(crate::persistence::high_frequency::EnqueueOutcome::MainQueue)
        | Ok(crate::persistence::high_frequency::EnqueueOutcome::OverflowQueue) => {
            trace!(room = %room.id, user_id = user.id, "judge batch enqueued to high frequency writer");
        }
        Ok(crate::persistence::high_frequency::EnqueueOutcome::Dropped) => {
            warn!(room = %room.id, user_id = user.id, "judge batch dropped by high frequency writer");
        }
        Err(e) => {
            warn!(room = %room.id, user_id = user.id, "judge batch could not be enqueued to high frequency writer: {e}");
        }
    }
}

fn should_persist_round_telemetry(
    item_count: usize,
    has_current_round: bool,
    _has_active_monitors: bool,
) -> bool {
    item_count > 0 && has_current_round
}

fn should_broadcast_monitor_telemetry(has_active_monitors: bool) -> bool {
    has_active_monitors
}

pub(crate) async fn handle_touches(user: Arc<User>, room: Arc<Room>, frames: Arc<Vec<TouchFrame>>) {
    let frame_count = frames.len();
    let has_active_monitors = room.has_active_monitors().await;
    debug!(
        "received {} touch frames from {} (active_monitors={})",
        frame_count, user.id, has_active_monitors
    );
    if let Some(frame) = frames.last() {
        user.game_time.store(frame.time.to_bits(), std::sync::atomic::Ordering::SeqCst);
    }
    let touch_data: Vec<crate::plugin::TouchEventPoint> = frames
        .iter()
        .flat_map(|frame| {
            frame
                .points
                .iter()
                .map(|(finger, pos)| crate::plugin::TouchEventPoint {
                    time: frame.time,
                    finger: *finger,
                    x: pos.x(),
                    y: pos.y(),
                })
        })
        .collect();

    if !touch_data.is_empty() {
        persist_touches(&user, &room, &touch_data, has_active_monitors).await;
    }

    // OpenUDS touches 流投递：有订阅者才查 round + 序列化，避免热路径白耗。
    if let Some(mgr) = crate::openuds::get_stream_manager() {
        if mgr.has_subscribers("touches").await {
            let round_id = user
                .server
                .room_commands
                .room_snapshot(&room.id.to_string())
                .and_then(|snap| snap.round_id.map(|rid| rid.to_string()));
            let frames_json = serde_json::to_value(&touch_data).unwrap_or_default();
            mgr.deliver_touches(user.id, &room.id.to_string(), round_id.as_deref(), frames_json)
                .await;
        }
    }

    user.server
        .publish_runtime_event(crate::event_bus::MpEvent::TouchesReceived {
            room_id: room.id.clone(),
            user_id: user.id,
            count: frame_count,
        });

    let player_id = user.id;
    if user.server.plugin_manager.has_plugins().await {
        let pm = Arc::clone(&user.server.plugin_manager);
        let room_id = room.id.to_string();
        if !pm.try_dispatch_event(PluginEvent::PlayerTouches {
            user_id: player_id,
            room_id,
            data: touch_data,
        }) {
            trace!(
                user_id = player_id,
                "dropping plugin touch event because plugin event queue is saturated"
            );
        }
    }

    if should_broadcast_monitor_telemetry(has_active_monitors) {
        // 审计 P1-A: use bounded broadcast channel with try_send.
        // If the channel is full the oldest telemetry frame is dropped,
        // ensuring slow monitors never block the game hot path.
        if let Some(server) = room.server.upgrade() {
            if let Some(tx) = server.room_commands.monitor_telemetry_sender(&room.id.to_string()) {
                let _ = tx.send(ServerCommand::Touches {
                    player: player_id,
                    frames,
                });
            }
        }
    } else {
        trace!(room = %room.id, user_id = user.id, "touch data persisted without active monitor broadcast");
    }
}

pub(crate) async fn handle_judges(user: Arc<User>, room: Arc<Room>, judges: Arc<Vec<JudgeEvent>>) {
    let judge_count = judges.len();
    let has_active_monitors = room.has_active_monitors().await;
    debug!(
        "received {} judge events from {} (active_monitors={})",
        judge_count, user.id, has_active_monitors
    );
    let judge_data: Vec<crate::plugin::JudgeEventItem> = judges
        .iter()
        .map(|j| crate::plugin::JudgeEventItem {
            time: j.time,
            line_id: j.line_id,
            note_id: j.note_id,
            judgement: j.judgement.as_str().to_string(),
        })
        .collect();

    if !judge_data.is_empty() {
        persist_judges(&user, &room, &judge_data, has_active_monitors).await;
    }

    // OpenUDS judges 流投递：有订阅者才查 round + 序列化，避免热路径白耗。
    if let Some(mgr) = crate::openuds::get_stream_manager() {
        if mgr.has_subscribers("judges").await {
            let round_id = user
                .server
                .room_commands
                .room_snapshot(&room.id.to_string())
                .and_then(|snap| snap.round_id.map(|rid| rid.to_string()));
            let events_json = serde_json::to_value(&judge_data).unwrap_or_default();
            mgr.deliver_judges(user.id, &room.id.to_string(), round_id.as_deref(), events_json)
                .await;
        }
    }

    user.server
        .publish_runtime_event(crate::event_bus::MpEvent::JudgesReceived {
            room_id: room.id.clone(),
            user_id: user.id,
            count: judge_count,
        });

    let player_id = user.id;
    if user.server.plugin_manager.has_plugins().await {
        let pm = Arc::clone(&user.server.plugin_manager);
        let room_id = room.id.to_string();
        if !pm.try_dispatch_event(PluginEvent::PlayerJudges {
            user_id: player_id,
            room_id,
            data: judge_data,
        }) {
            trace!(
                user_id = player_id,
                "dropping plugin judge event because plugin event queue is saturated"
            );
        }
    }

    if should_broadcast_monitor_telemetry(has_active_monitors) {
        // 审计 P1-A: use bounded broadcast channel with try_send.
        if let Some(server) = room.server.upgrade() {
            if let Some(tx) = server.room_commands.monitor_telemetry_sender(&room.id.to_string()) {
                let _ = tx.send(ServerCommand::Judges {
                    player: player_id,
                    judges,
                });
            }
        }
    } else {
        trace!(room = %room.id, user_id = user.id, "judge data persisted without active monitor broadcast");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_judge_round_persistence_does_not_require_active_monitor() {
        assert!(should_persist_round_telemetry(1, true, false));
        assert!(should_persist_round_telemetry(1, true, true));
    }

    #[test]
    fn touch_judge_round_persistence_still_requires_payload_and_round() {
        assert!(!should_persist_round_telemetry(0, true, false));
        assert!(!should_persist_round_telemetry(1, false, false));
        assert!(!should_persist_round_telemetry(0, false, true));
    }

    #[test]
    fn active_monitor_only_controls_realtime_broadcast() {
        assert!(should_broadcast_monitor_telemetry(true));
        assert!(!should_broadcast_monitor_telemetry(false));
    }
}

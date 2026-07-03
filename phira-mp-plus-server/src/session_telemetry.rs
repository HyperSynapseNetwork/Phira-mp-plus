//! Gameplay telemetry handling for sessions.
//!
//! Touch/Judge processing is intentionally outside `session.rs`: this path is
//! high-frequency, touches persistence, EventBus, plugins, monitor broadcast,
//! and Runtime v2 cutover logic. Keeping it isolated makes cutover management
//! and future actorization safer.
//!
//! Cutover contract:
//! - DirectOnly: write direct RoundStore/db.rs only.
//! - WorkerPreferred: write direct (authoritative) + best-effort enqueue
//!   to Runtime v2 worker as async mirror / batch observation.
//!   Worker failure is warned; direct write is always committed first.

use crate::plugin::PluginEvent;
use crate::room::Room;
use crate::session::User;
use phira_mp_common::{JudgeEvent, ServerCommand, TouchFrame};
use std::sync::{atomic::Ordering, Arc};
use tracing::{debug, trace, warn};

pub(crate) async fn handle_touches(user: Arc<User>, room: Arc<Room>, frames: Arc<Vec<TouchFrame>>) {
    let frame_count = frames.len();
    let has_active_monitors = room.has_active_monitors().await;
    debug!(
        "received {} touch frames from {} (active_monitors={})",
        frame_count, user.id, has_active_monitors
    );
    if let Some(frame) = frames.last() {
        user.game_time.store(frame.time.to_bits(), Ordering::SeqCst);
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
        let data_for_event = touch_data.clone();
        tokio::spawn(async move {
            pm.trigger(&PluginEvent::PlayerTouches {
                user_id: player_id,
                room_id,
                data: data_for_event,
            })
            .await;
        });
    }

    if should_broadcast_monitor_telemetry(has_active_monitors) {
        let monitor_room = Arc::clone(&room);
        tokio::spawn(async move {
            monitor_room
                .broadcast_monitors(ServerCommand::Touches {
                    player: player_id,
                    frames,
                })
                .await;
        });
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
        let data_for_event = judge_data.clone();
        tokio::spawn(async move {
            pm.trigger(&PluginEvent::PlayerJudges {
                user_id: player_id,
                room_id,
                data: data_for_event,
            })
            .await;
        });
    }

    if should_broadcast_monitor_telemetry(has_active_monitors) {
        let monitor_room = Arc::clone(&room);
        tokio::spawn(async move {
            monitor_room
                .broadcast_monitors(ServerCommand::Judges {
                    player: player_id,
                    judges,
                })
                .await;
        });
    } else {
        trace!(room = %room.id, user_id = user.id, "judge data persisted without active monitor broadcast");
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

/// Decide how to persist gameplay telemetry based on cutover mode.
///
/// Two modes:
/// - DirectOnly: writes through the direct RoundStore/db.rs path.
/// - WorkerPreferred: always writes direct (authoritative) + best-effort
///   enqueue to the Runtime v2 worker as async mirror / batch observation.
///   Worker enqueue failure is warned but never blocks data persistence.
async fn persist_touches(
    user: &Arc<User>,
    room: &Arc<Room>,
    touch_data: &[crate::plugin::TouchEventPoint],
    has_active_monitors: bool,
) {
    room.store_player_touches(user.id, touch_data).await;
    let round_id = room
        .current_round_id
        .read()
        .await
        .as_ref()
        .map(|rid| rid.to_string());
    if should_persist_round_telemetry(touch_data.len(), round_id.is_some(), has_active_monitors) {
        let rid = round_id
            .as_ref()
            .expect("round id checked by telemetry persistence plan");
        let should_enqueue = user
            .server
            .persistence_worker
            .telemetry_should_enqueue_worker()
            .await;

        // Always write direct (authoritative path)
        if let Some(rs) = &room.round_store {
            rs.append_touches(rid, user.id, touch_data).await;
        }

        // If WorkerPreferred, also enqueue to worker as async mirror
        if should_enqueue {
            let payload = serde_json::json!({
                "room_id": room.id.to_string(),
                "round_id": rid,
                "user_id": user.id,
                "count": touch_data.len(),
                "data": touch_data,
            });
            let enqueued = user
                .server
                .persistence_worker
                .enqueue(crate::persistence_worker::PersistenceEvent::TouchBatch {
                    round_id: rid.to_string(),
                    user_id: user.id,
                    payload,
                    simulation: false,
                })
                .await;
            if enqueued.is_err() {
                warn!(room = %room.id, user = user.id, "touch worker enqueue failed (mirror); direct write already committed");
            }
        }
    } else {
        debug!(room = %room.id, user_id = user.id, "touch data received without current round; cached only");
    }
}

async fn persist_judges(
    user: &Arc<User>,
    room: &Arc<Room>,
    judge_data: &[crate::plugin::JudgeEventItem],
    has_active_monitors: bool,
) {
    room.store_player_judges(user.id, judge_data).await;
    let round_id = room
        .current_round_id
        .read()
        .await
        .as_ref()
        .map(|rid| rid.to_string());
    if should_persist_round_telemetry(judge_data.len(), round_id.is_some(), has_active_monitors) {
        let rid = round_id
            .as_ref()
            .expect("round id checked by telemetry persistence plan");
        let should_enqueue = user
            .server
            .persistence_worker
            .telemetry_should_enqueue_worker()
            .await;

        // Always write direct (authoritative path)
        if let Some(rs) = &room.round_store {
            rs.append_judges(rid, user.id, judge_data).await;
        }

        // If WorkerPreferred, also enqueue to worker as async mirror
        if should_enqueue {
            let payload = serde_json::json!({
                "room_id": room.id.to_string(),
                "round_id": rid,
                "user_id": user.id,
                "count": judge_data.len(),
                "data": judge_data,
            });
            let enqueued = user
                .server
                .persistence_worker
                .enqueue(crate::persistence_worker::PersistenceEvent::JudgeBatch {
                    round_id: rid.to_string(),
                    user_id: user.id,
                    payload,
                    simulation: false,
                })
                .await;
            if enqueued.is_err() {
                warn!(room = %room.id, user = user.id, "judge worker enqueue failed (mirror); direct write already committed");
            }
        }
    } else {
        debug!(room = %room.id, user_id = user.id, "judge data received without current round; cached only");
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

//! Gameplay telemetry handling for sessions.
//!
//! Touch/Judge processing is intentionally outside `session.rs`: this path is
//! high-frequency, touches persistence, EventBus, plugins, monitor broadcast,
//! and Runtime v2 cutover logic. Keeping it isolated makes future worker-only
//! cutover and actorization safer.

use crate::plugin::PluginEvent;
use crate::room::Room;
use crate::session::User;
use phira_mp_common::{JudgeEvent, ServerCommand, TouchFrame};
use std::sync::{atomic::Ordering, Arc};
use tracing::{debug, trace};

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
            frame.points.iter().map(|(finger, pos)| crate::plugin::TouchEventPoint {
                time: frame.time,
                finger: *finger,
                x: pos.x(),
                y: pos.y(),
            })
        })
        .collect();

    if !touch_data.is_empty() {
        persist_touches(&user, &room, &touch_data).await;
    }

    user.server.publish_runtime_event(crate::event_bus::MpEvent::TouchesReceived {
        room_id: room.id.clone(),
        user_id: user.id,
        count: frame_count,
    });

    let pm = Arc::clone(&user.server.plugin_manager);
    let room_id = room.id.to_string();
    let touch_data_for_event = touch_data.clone();
    let player_id = user.id;
    tokio::spawn(async move {
        pm.trigger(&PluginEvent::PlayerTouches {
            user_id: player_id,
            room_id,
            data: touch_data_for_event,
        })
        .await;
    });

    if has_active_monitors {
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
            judgement: format!("{:?}", j.judgement),
        })
        .collect();

    if !judge_data.is_empty() {
        persist_judges(&user, &room, &judge_data).await;
    }

    user.server.publish_runtime_event(crate::event_bus::MpEvent::JudgesReceived {
        room_id: room.id.clone(),
        user_id: user.id,
        count: judge_count,
    });

    let pm = Arc::clone(&user.server.plugin_manager);
    let room_id = room.id.to_string();
    let judge_data_for_event = judge_data.clone();
    let player_id = user.id;
    tokio::spawn(async move {
        pm.trigger(&PluginEvent::PlayerJudges {
            user_id: player_id,
            room_id,
            data: judge_data_for_event,
        })
        .await;
    });

    if has_active_monitors {
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

async fn persist_touches(
    user: &Arc<User>,
    room: &Arc<Room>,
    touch_data: &[crate::plugin::TouchEventPoint],
) {
    // Touches are gameplay telemetry, not monitor-only data. Persist them
    // whenever a round is active so unattended real games do not silently lose
    // touch batches.
    room.store_player_touches(user.id, touch_data).await;
    let round_id = room.current_round_id.read().await.as_ref().map(|rid| rid.to_string());
    if let Some(rid) = round_id.as_ref() {
        let telemetry_mode = user.server.persistence_worker.telemetry_cutover_mode().await;
        let mut runtime_enqueue_ok = false;
        if telemetry_mode.should_enqueue_worker() {
            let payload = serde_json::json!({
                "runtime_v2_source": "session_direct",
                "runtime_v2_stage": "telemetry_cutover",
                "telemetry_cutover_mode": telemetry_mode.as_str(),
                "room_id": room.id.to_string(),
                "round_id": rid,
                "user_id": user.id,
                "count": touch_data.len(),
                "data": touch_data,
            });
            runtime_enqueue_ok = user
                .server
                .persistence_worker
                .enqueue(crate::persistence_worker::PersistenceEvent::TouchBatch {
                    round_id: rid.to_string(),
                    user_id: user.id,
                    payload,
                    simulation: false,
                })
                .await
                .is_ok();
        }
        let should_write_legacy = telemetry_mode.should_write_legacy()
            || (telemetry_mode.fallback_to_legacy_on_enqueue_failure() && !runtime_enqueue_ok);
        if should_write_legacy {
            if let Some(rs) = &room.round_store {
                rs.append_touches(rid, user.id, touch_data).await;
            }
        } else {
            trace!(room = %room.id, user_id = user.id, mode = telemetry_mode.as_str(), "touch legacy direct write skipped by telemetry cutover mode");
        }
    } else {
        debug!(room = %room.id, user_id = user.id, "touch data received without current round; cached only");
    }
}

async fn persist_judges(
    user: &Arc<User>,
    room: &Arc<Room>,
    judge_data: &[crate::plugin::JudgeEventItem],
) {
    // Judges are gameplay telemetry, not monitor-only data. Persist them
    // whenever a round is active so unattended real games do not silently lose
    // judge batches.
    room.store_player_judges(user.id, judge_data).await;
    let round_id = room.current_round_id.read().await.as_ref().map(|rid| rid.to_string());
    if let Some(rid) = round_id.as_ref() {
        let telemetry_mode = user.server.persistence_worker.telemetry_cutover_mode().await;
        let mut runtime_enqueue_ok = false;
        if telemetry_mode.should_enqueue_worker() {
            let payload = serde_json::json!({
                "runtime_v2_source": "session_direct",
                "runtime_v2_stage": "telemetry_cutover",
                "telemetry_cutover_mode": telemetry_mode.as_str(),
                "room_id": room.id.to_string(),
                "round_id": rid,
                "user_id": user.id,
                "count": judge_data.len(),
                "data": judge_data,
            });
            runtime_enqueue_ok = user
                .server
                .persistence_worker
                .enqueue(crate::persistence_worker::PersistenceEvent::JudgeBatch {
                    round_id: rid.to_string(),
                    user_id: user.id,
                    payload,
                    simulation: false,
                })
                .await
                .is_ok();
        }
        let should_write_legacy = telemetry_mode.should_write_legacy()
            || (telemetry_mode.fallback_to_legacy_on_enqueue_failure() && !runtime_enqueue_ok);
        if should_write_legacy {
            if let Some(rs) = &room.round_store {
                rs.append_judges(rid, user.id, judge_data).await;
            }
        } else {
            trace!(room = %room.id, user_id = user.id, mode = telemetry_mode.as_str(), "judge legacy direct write skipped by telemetry cutover mode");
        }
    } else {
        debug!(room = %room.id, user_id = user.id, "judge data received without current round; cached only");
    }
}

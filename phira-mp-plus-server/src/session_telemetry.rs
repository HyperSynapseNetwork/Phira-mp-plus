//! Gameplay telemetry handling for sessions.
//!
//! Touch/Judge processing is intentionally outside `session.rs`: this path is
//! high-frequency and coordinates persistence, EventBus, plugins and monitor
//! broadcast.
//!
//! Cutover contract:
//! - `direct_only`: RoundStore/db.rs is the only writer.
//! - `worker_preferred`: direct write is attempted first. After direct acknowledgement,
//!   Worker receives a migration mirror; if direct persistence fails and Worker accepts
//!   the event, Worker becomes the canonical compensation path for that batch.
//! - `worker_authoritative`: Worker is the normal-operation single writer; a
//!   direct fallback occurs only when Worker enqueue is explicitly rejected.

use crate::plugin::PluginEvent;
use crate::room::Room;
use crate::session::User;
use phira_mp_common::{JudgeEvent, ServerCommand, TouchFrame};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, trace, warn};

const TELEMETRY_CRITICAL_REPORT_INTERVAL_MS: u64 = 10_000;
static LAST_TELEMETRY_CRITICAL_REPORT_MS: AtomicU64 = AtomicU64::new(0);

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

fn worker_event_is_mirror(
    mode: crate::telemetry::TelemetryCutoverMode,
    direct_written: bool,
) -> bool {
    matches!(
        mode,
        crate::telemetry::TelemetryCutoverMode::WorkerPreferred
    ) && direct_written
}

fn worker_is_canonical_fallback(
    mode: crate::telemetry::TelemetryCutoverMode,
    direct_attempted: bool,
    direct_written: bool,
    worker_enqueued: bool,
) -> bool {
    matches!(
        mode,
        crate::telemetry::TelemetryCutoverMode::WorkerPreferred
    ) && direct_attempted
        && !direct_written
        && worker_enqueued
}

/// Persist a touch batch according to the currently active cutover contract.
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
    if !should_persist_round_telemetry(touch_data.len(), round_id.is_some(), has_active_monitors) {
        debug!(room = %room.id, user_id = user.id, "touch data received without current round; cached only");
        return;
    }

    let rid = round_id
        .as_deref()
        .expect("round id checked by telemetry persistence plan");
    let mode = user
        .server
        .persistence_worker
        .telemetry_cutover_mode()
        .await;
    let decision = mode.cutover_decision();
    let mut direct_attempted = false;
    let mut direct_written = false;
    let mut direct_write_ms = None;

    if decision.write_direct_before_worker_result {
        direct_attempted = true;
        let started = Instant::now();
        if let Some(store) = &room.round_store {
            direct_written = store.append_touches(rid, user.id, touch_data).await;
        }
        direct_write_ms = Some(elapsed_ms(started));
    }

    let mut worker_enqueued = false;
    let mut worker_enqueue_ms = 0;
    if decision.enqueue_worker {
        let event_id = uuid::Uuid::new_v4().to_string();
        let payload = serde_json::json!({
            "runtime_v2_event_id": event_id,
            "room_id": room.id.to_string(),
            "round_id": rid,
            "user_id": user.id,
            "count": touch_data.len(),
            "data": touch_data,
            "runtime_v2_dual_write": worker_event_is_mirror(mode, direct_written),
            "runtime_v2_persistence_mode": mode.as_str(),
        });
        let started = Instant::now();
        worker_enqueued = user
            .server
            .persistence_worker
            .enqueue(crate::persistence_worker::PersistenceEvent::TouchBatch {
                round_id: rid.to_string(),
                user_id: user.id,
                payload: Arc::new(payload),
                simulation: false,
            })
            .await
            .is_ok();
        worker_enqueue_ms = elapsed_ms(started);
    }

    let fallback_direct = !decision.write_direct_before_worker_result
        && decision.should_write_direct_after_worker_enqueue(worker_enqueued);
    if fallback_direct {
        direct_attempted = true;
        let started = Instant::now();
        if let Some(store) = &room.round_store {
            direct_written = store.append_touches(rid, user.id, touch_data).await;
        }
        direct_write_ms = Some(elapsed_ms(started));
        warn!(room = %room.id, user = user.id, mode = mode.as_str(),
            "touch Worker enqueue was rejected; used direct fallback");
    } else if decision.enqueue_worker && !worker_enqueued {
        warn!(room = %room.id, user = user.id, mode = mode.as_str(), direct_written,
            "touch Worker enqueue failed after the direct attempt");
    }

    let worker_canonical_fallback =
        worker_is_canonical_fallback(mode, direct_attempted, direct_written, worker_enqueued);
    if worker_canonical_fallback {
        warn!(room = %room.id, user = user.id,
            "touch direct persistence failed; Worker accepted canonical fallback");
    }
    if direct_attempted && !direct_written {
        warn!(room = %room.id, user = user.id, mode = mode.as_str(),
            "touch direct persistence did not receive an acknowledgement");
    }
    // Worker enqueue is a pipeline-acceptance boundary, not a database commit ACK.
    // Final database/dead-letter outcomes are exposed by the batcher/worker statistics.
    let persistence_path_accepted = direct_written || worker_enqueued;
    if !persistence_path_accepted {
        report_unaccepted_telemetry("touch", room, user).await;
    }

    user.server
        .persistence_worker
        .record_telemetry_cutover_observation(crate::persistence::TelemetryCutoverObservation {
            kind: "touch".to_string(),
            mode: mode.as_str().to_string(),
            item_count: touch_data.len(),
            worker_attempted: decision.enqueue_worker,
            worker_enqueued,
            worker_enqueue_ms,
            direct_attempted,
            direct_written,
            direct_write_ms,
            fallback_direct,
            worker_canonical_fallback,
            persistence_path_accepted,
        })
        .await;
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
    if !should_persist_round_telemetry(judge_data.len(), round_id.is_some(), has_active_monitors) {
        debug!(room = %room.id, user_id = user.id, "judge data received without current round; cached only");
        return;
    }

    let rid = round_id
        .as_deref()
        .expect("round id checked by telemetry persistence plan");
    let mode = user
        .server
        .persistence_worker
        .telemetry_cutover_mode()
        .await;
    let decision = mode.cutover_decision();
    let mut direct_attempted = false;
    let mut direct_written = false;
    let mut direct_write_ms = None;

    if decision.write_direct_before_worker_result {
        direct_attempted = true;
        let started = Instant::now();
        if let Some(store) = &room.round_store {
            direct_written = store.append_judges(rid, user.id, judge_data).await;
        }
        direct_write_ms = Some(elapsed_ms(started));
    }

    let mut worker_enqueued = false;
    let mut worker_enqueue_ms = 0;
    if decision.enqueue_worker {
        let event_id = uuid::Uuid::new_v4().to_string();
        let payload = serde_json::json!({
            "runtime_v2_event_id": event_id,
            "room_id": room.id.to_string(),
            "round_id": rid,
            "user_id": user.id,
            "count": judge_data.len(),
            "data": judge_data,
            "runtime_v2_dual_write": worker_event_is_mirror(mode, direct_written),
            "runtime_v2_persistence_mode": mode.as_str(),
        });
        let started = Instant::now();
        worker_enqueued = user
            .server
            .persistence_worker
            .enqueue(crate::persistence_worker::PersistenceEvent::JudgeBatch {
                round_id: rid.to_string(),
                user_id: user.id,
                payload: Arc::new(payload),
                simulation: false,
            })
            .await
            .is_ok();
        worker_enqueue_ms = elapsed_ms(started);
    }

    let fallback_direct = !decision.write_direct_before_worker_result
        && decision.should_write_direct_after_worker_enqueue(worker_enqueued);
    if fallback_direct {
        direct_attempted = true;
        let started = Instant::now();
        if let Some(store) = &room.round_store {
            direct_written = store.append_judges(rid, user.id, judge_data).await;
        }
        direct_write_ms = Some(elapsed_ms(started));
        warn!(room = %room.id, user = user.id, mode = mode.as_str(),
            "judge Worker enqueue was rejected; used direct fallback");
    } else if decision.enqueue_worker && !worker_enqueued {
        warn!(room = %room.id, user = user.id, mode = mode.as_str(), direct_written,
            "judge Worker enqueue failed after the direct attempt");
    }

    let worker_canonical_fallback =
        worker_is_canonical_fallback(mode, direct_attempted, direct_written, worker_enqueued);
    if worker_canonical_fallback {
        warn!(room = %room.id, user = user.id,
            "judge direct persistence failed; Worker accepted canonical fallback");
    }
    if direct_attempted && !direct_written {
        warn!(room = %room.id, user = user.id, mode = mode.as_str(),
            "judge direct persistence did not receive an acknowledgement");
    }
    // Worker enqueue is a pipeline-acceptance boundary, not a database commit ACK.
    // Final database/dead-letter outcomes are exposed by the batcher/worker statistics.
    let persistence_path_accepted = direct_written || worker_enqueued;
    if !persistence_path_accepted {
        report_unaccepted_telemetry("judge", room, user).await;
    }

    user.server
        .persistence_worker
        .record_telemetry_cutover_observation(crate::persistence::TelemetryCutoverObservation {
            kind: "judge".to_string(),
            mode: mode.as_str().to_string(),
            item_count: judge_data.len(),
            worker_attempted: decision.enqueue_worker,
            worker_enqueued,
            worker_enqueue_ms,
            direct_attempted,
            direct_written,
            direct_write_ms,
            fallback_direct,
            worker_canonical_fallback,
            persistence_path_accepted,
        })
        .await;
}

async fn report_unaccepted_telemetry(kind: &str, room: &Room, user: &User) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let mut previous = LAST_TELEMETRY_CRITICAL_REPORT_MS.load(Ordering::Acquire);
    loop {
        if now.saturating_sub(previous) < TELEMETRY_CRITICAL_REPORT_INTERVAL_MS {
            return;
        }
        match LAST_TELEMETRY_CRITICAL_REPORT_MS.compare_exchange_weak(
            previous,
            now,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => break,
            Err(current) => previous = current,
        }
    }
    crate::supervisor_actor::report_critical_failure(
        "round-telemetry",
        format!(
            "{kind} batch was accepted by neither persistence path (room={}, user={})",
            room.id, user.id
        ),
    )
    .await;
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
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

    #[test]
    fn worker_preferred_is_a_mirror_only_after_direct_acknowledgement() {
        let mode = crate::telemetry::TelemetryCutoverMode::WorkerPreferred;
        assert!(worker_event_is_mirror(mode, true));
        assert!(!worker_event_is_mirror(mode, false));
        assert!(worker_is_canonical_fallback(mode, true, false, true));
        assert!(!worker_is_canonical_fallback(mode, true, true, true));
        assert!(!worker_is_canonical_fallback(mode, true, false, false));
    }
}

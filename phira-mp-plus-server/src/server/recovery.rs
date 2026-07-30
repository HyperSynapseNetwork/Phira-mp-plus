//! Server startup state recovery — crash recovery for unfinished rounds,
//! schema validation, persistent room restoration, playtime stale session
//! cleanup, and DLQ replay.
//!
//! After a server restart (planned or crash), in-memory state is empty while
//! PostgreSQL still holds data from the previous run.  This module re-discovers
//! that data and reconciles the server state so that:
//!
//! 1. Unfinished rounds are marked as aborted (so plugins/telemetry see a
//!    terminal state and don't wait for a round that will never finish).
//! 2. Database health is logged (schema version, user / playtime counts).
//! 3. Persistent empty rooms are recreated from mp_settings.
//! 4. Stale playtime sessions are cleaned up.
//! 5. Dead-letter queue entries are replayed.

use std::sync::Arc;

use crate::db::DbManager;
use anyhow::Result;
use tracing::{error, info, warn};

use super::state::PlusServerState;

/// Run all startup recovery steps.
///
/// Must be called **after** the PostgreSQL connection is established and
/// migrations have run, but **before** accepting network connections or
/// initialising plugins that depend on a consistent state.
///
/// Failures are **fatal** — any critical recovery step that fails will
/// prevent the server from becoming ready.
pub async fn recover_state(state: &Arc<PlusServerState>, db: &DbManager) -> Result<()> {
    // ── 1. Crash recovery: abort unfinished rounds ──────────────────────
    let unfinished = db.find_unfinished_rounds().await;
    let count = unfinished.len();
    if count > 0 {
        warn!(
            "startup recovery: found {count} unfinished round(s) from \
             previous server session — marking as aborted"
        );
        let mut abort_failures = 0u32;
        for round in &unfinished {
            warn!(
                "crash recovery: aborting unfinished round {} (room={}, \
                 chart_id={}, started_at={})",
                round.round_uuid, round.room_id, round.chart_id, round.started_at,
            );
            if db.abort_round(&round.round_uuid).await {
                info!(
                    "crash recovery: successfully aborted round {}",
                    round.round_uuid
                );
            } else {
                error!(
                    "crash recovery: failed to abort round {}",
                    round.round_uuid
                );
                abort_failures += 1;
            }
        }
        if abort_failures > 0 {
            return Err(anyhow::anyhow!(
                "startup recovery: failed to abort {abort_failures}/{count} unfinished round(s)"
            ));
        }
        info!(
            "startup recovery: aborted {count} unfinished round(s) from \
             previous server session"
        );
    } else {
        info!("startup recovery: no unfinished rounds to recover");
    }

    // ── 2. Schema version validation ────────────────────────────────────
    let schema_version = db.get_schema_version().await;
    match schema_version {
        Some(ver) => info!("startup recovery: schema version = {ver}"),
        None => {
            return Err(anyhow::anyhow!(
                "startup recovery: _pmp_schema_version table is empty or \
                 inaccessible — database may be corrupt"
            ));
        }
    }

    // ── 3. Database diagnostics ─────────────────────────────────────────
    let user_count = db.count_users().await;
    let playtime_count = db.count_playtime().await;
    info!(
        "startup recovery: database state — {} user record(s), \
         {} playtime entry(ies)",
        user_count, playtime_count,
    );

    // ── 4. WAL health check ─────────────────────────────────────────────
    // The PersistenceWorker replays the WAL in a background task. Wait
    // briefly for replay to complete, then fail-closed if unhealthy.
    let mut wal_healthy = false;
    for _ in 0..50 {
        if state.persistence_worker.is_healthy().await {
            wal_healthy = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    if !wal_healthy {
        return Err(anyhow::anyhow!(
            "startup recovery: persistence WAL replay has not completed or is degraded"
        ));
    }
    info!("startup recovery: persistence WAL is healthy");

    // ── 5. Persistent empty room restoration ────────────────────────────
    restore_persistent_rooms(state, db).await;

    // ── 6. Playtime stale session cleanup ───────────────────────────────
    cleanup_stale_playtime_sessions(db).await;

    // ── 7. Dead-letter queue replay ─────────────────────────────────────
    replay_dead_letter_queue(state).await;

    Ok(())
}

/// Restore persistent empty rooms from mp_settings.
///
/// Reads the `persistent_rooms` key from `mp_settings` (a JSON array of room
/// IDs) and recreates each room via `state.create_empty_room()`.
async fn restore_persistent_rooms(state: &Arc<PlusServerState>, db: &DbManager) {
    let room_ids = match db.get_persistent_rooms().await {
        Some(ids) => ids,
        None => {
            info!("startup recovery: no persistent empty rooms to restore");
            return;
        }
    };
    info!(
        "startup recovery: restoring {} persistent empty room(s)",
        room_ids.len()
    );
    for room_id in &room_ids {
        match state.create_empty_room(room_id, None, true).await {
            Ok(_) => info!("startup recovery: restored persistent room {room_id}"),
            Err(e) => warn!(
                "startup recovery: failed to restore persistent room {room_id}: {e}"
            ),
        }
    }
}

/// Clean up stale playtime sessions that were orphaned by a crash.
///
/// A session is considered stale if `session_start` is older than 24 hours.
/// The elapsed time is accrued to `total_secs` and `session_start` is cleared.
async fn cleanup_stale_playtime_sessions(db: &DbManager) {
    match db.cleanup_stale_playtime_sessions().await {
        Ok(affected) => {
            if affected > 0 {
                info!("startup recovery: cleaned up {affected} stale playtime session(s)");
            } else {
                info!("startup recovery: no stale playtime sessions to clean up");
            }
        }
        Err(e) => warn!("startup recovery: failed to clean stale playtime sessions: {e}"),
    }
}

/// Replay the dead-letter queue: scan the DLQ JSONL file and re-enqueue
/// events that were not persisted in the previous run.
///
/// This is a best-effort recovery. Events that cannot be reconstructed are
/// logged and skipped. The DLQ file path comes from the runtime config.
async fn replay_dead_letter_queue(state: &Arc<PlusServerState>) {
    let dlq_path = state
        .config
        .runtime
        .persistence_dead_letter_path
        .as_deref()
        .map(std::path::Path::new);

    let Some(path) = dlq_path else {
        info!("startup recovery: no dead-letter path configured, skipping DLQ replay");
        return;
    };

    let content = match tokio::fs::read_to_string(path).await {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!("startup recovery: no dead-letter file found, skipping DLQ replay");
            return;
        }
        Err(e) => {
            warn!("startup recovery: failed to read dead-letter file: {e}");
            return;
        }
    };

    let mut replayed = 0u32;
    let mut skipped = 0u32;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let record: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                warn!("startup recovery: skipping malformed DLQ entry: {e}");
                skipped += 1;
                continue;
            }
        };

        let kind = match record.get("kind").and_then(|v| v.as_str()) {
            Some(k) => k,
            None => {
                warn!("startup recovery: skipping DLQ entry without kind");
                skipped += 1;
                continue;
            }
        };

        let event_payload = match record.get("event") {
            Some(v) if !v.is_null() => v,
            _ => {
                warn!("startup recovery: skipping DLQ entry without event payload (kind={kind})");
                skipped += 1;
                continue;
            }
        };

        let event = match reconstruct_event(kind, event_payload) {
            Some(e) => e,
            None => {
                warn!("startup recovery: skipping DLQ entry with unsupported kind {kind}");
                skipped += 1;
                continue;
            }
        };

        // Enqueue — best effort; failure is logged but does not block recovery.
        if state.persistence_worker.enqueue(event).await.is_err() {
            warn!("startup recovery: failed to re-enqueue DLQ event (kind={kind})");
            skipped += 1;
        } else {
            replayed += 1;
        }
    }

    if replayed > 0 || skipped > 0 {
        info!(
            "startup recovery: DLQ replay finished — {replayed} replayed, {skipped} skipped"
        );
    } else {
        info!("startup recovery: no DLQ entries to replay");
    }
}

/// Attempt to reconstruct a `PersistenceEvent` from the dead-letter record's
/// `kind` and `event` fields.
fn reconstruct_event(kind: &str, event: &serde_json::Value) -> Option<crate::persistence::PersistenceEvent> {
    use crate::persistence::PersistenceEvent;
    use std::sync::Arc;

    match kind {
        "room_snapshot" => {
            let room_id = event.get("room_id")?.as_str()?;
            let payload = event.get("payload")?.clone();
            Some(PersistenceEvent::RoomSnapshot {
                room_id: room_id.to_string(),
                payload: Arc::new(payload),
            })
        }
        "user_room_history" => {
            let user_id = event.get("user_id")?.as_i64()? as i32;
            let room_id = event.get("room_id")?.as_str()?;
            let room_uuid = event.get("room_uuid")?.as_str()?;
            let joined_at = event.get("joined_at")?.as_i64()?;
            Some(PersistenceEvent::UserRoomHistory {
                user_id,
                room_id: room_id.to_string(),
                room_uuid: room_uuid.to_string(),
                joined_at,
            })
        }
        "user_online" => {
            let user_id = event.get("user_id")?.as_i64()? as i32;
            Some(PersistenceEvent::UserOnline { user_id })
        }
        "user_offline" => {
            let user_id = event.get("user_id")?.as_i64()? as i32;
            Some(PersistenceEvent::UserOffline { user_id })
        }
        "user_disconnect" => {
            let user_id = event.get("user_id")?.as_i64()? as i32;
            let user_name = event.get("user_name")?.as_str()?;
            Some(PersistenceEvent::UserDisconnect {
                user_id,
                user_name: user_name.to_string(),
            })
        }
        "user_seen" => {
            let user_id = event.get("user_id")?.as_i64()? as i32;
            let user_name = event.get("user_name")?.as_str()?;
            let language = event.get("language")?.as_str()?;
            let ip = event.get("ip")?.as_str()?;
            Some(PersistenceEvent::UserSeen {
                user_id,
                user_name: user_name.to_string(),
                language: language.to_string(),
                ip: ip.to_string(),
            })
        }
        "round_result" => {
            // RoundResult requires a PlayResult which is complex to reconstruct.
            // Skip for now — round results are low-risk for data loss since the
            // client has the result and can re-submit.
            None
        }
        "benchmark.completed" => {
            // Benchmark reports are not replayed; they are diagnostic only.
            None
        }
        // For unknown kinds, attempt to reconstruct as a ServerEvent.
        // The event payload from dead_letter_payload() has the shape
        // {"kind": ..., "payload": ...}.
        _ => {
            let inner_kind = event.get("kind")?.as_str()?;
            let payload = event.get("payload")?.clone();
            Some(PersistenceEvent::ServerEvent {
                kind: inner_kind.to_string(),
                payload: Arc::new(payload),
            })
        }
    }
}

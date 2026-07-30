//! Server startup state recovery — crash recovery for unfinished rounds,
//! schema validation, persistent room restoration, DLQ replay, and playtime
//! stale session cleanup.
//!
//! After a server restart (planned or crash), in-memory state is empty while
//! PostgreSQL still holds data from the previous run.  This module re-discovers
//! that data and reconciles the server state so that:
//!
//! 1. Unfinished rounds are marked as aborted (so plugins/telemetry see a
//!    terminal state and don't wait for a round that will never finish).
//! 2. Database health is logged (schema version, user / playtime counts).
//! 3. Persistent empty rooms are recreated from mp_settings.
//! 4. Dead-letter queue entries are replayed (**before** stale session cleanup
//!    so that old UserAuthenticated events cannot re-set sessions online).
//! 5. All open playtime sessions from the previous instance are closed (after
//!    WAL and DLQ replay, with a 1-hour cap on recovered playtime).
//!
//! Every step returns `Err` on failure so that `recover_state` propagates
//! the error and prevents the server from becoming ready.

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
    let unfinished = db
        .find_unfinished_rounds()
        .await
        .map_err(|e| anyhow::anyhow!("startup recovery: failed to query unfinished rounds: {e}"))?;
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
    restore_persistent_rooms(state, db).await?;

    // ── 6. Dead-letter queue replay ─────────────────────────────────────
    // Run DLQ replay BEFORE stale session cleanup so that any
    // UserAuthenticated events from the DLQ are processed first and can be
    // properly cleaned up by the session cleanup that follows.
    replay_dead_letter_queue(state).await?;

    // ── 7. Playtime stale session cleanup ───────────────────────────────
    // Run AFTER WAL replay and DLQ replay so that old UserAuthenticated
    // events cannot re-set sessions online after cleanup.
    close_all_stale_playtime_sessions(db).await?;

    Ok(())
}

/// Restore persistent empty rooms from mp_settings.
///
/// Reads the `persistent_rooms` key from `mp_settings` (a JSON array of room
/// IDs) and recreates each room via `state.create_empty_room()`.
///
/// Returns `Err` if `get_persistent_rooms` itself fails (indicating a database
/// problem).  Individual room creation failures are logged but do not abort the
/// step so that a single misconfigured room does not block the entire startup.
async fn restore_persistent_rooms(state: &Arc<PlusServerState>, db: &DbManager) -> Result<()> {
    let room_ids = match db.get_persistent_rooms().await {
        Ok(Some(ids)) => ids,
        Ok(None) => {
            info!("startup recovery: no persistent empty rooms to restore");
            return Ok(());
        }
        Err(e) => {
            return Err(anyhow::anyhow!(
                "startup recovery: failed to read persistent rooms from DB: {e}"
            ));
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
    Ok(())
}

/// Close all open playtime sessions from the previous server instance.
///
/// Every `session_start` that is still set at startup belongs to a previous
/// server instance (planned shutdown or crash).  The elapsed time is accrued
/// to `total_secs` (capped at `MAX_RECOVERY_SECS` per session) and
/// `session_start` is cleared so the row is ready for the next normal
/// online/offline cycle.
///
/// Returns `Err` on database failure so that recovery is not silently skipped.
const MAX_RECOVERY_SECS: i64 = 3600; // 1 hour cap per session
async fn close_all_stale_playtime_sessions(db: &DbManager) -> Result<()> {
    let affected = db
        .close_all_stale_sessions(MAX_RECOVERY_SECS)
        .await
        .map_err(|e| anyhow::anyhow!("startup recovery: close all stale playtime sessions: {e}"))?;
    if affected > 0 {
        info!("startup recovery: closed {affected} stale playtime session(s) from previous instance");
    } else {
        info!("startup recovery: no stale playtime sessions to close");
    }
    Ok(())
}

/// Replay the dead-letter queue: scan the DLQ JSONL file and re-enqueue
/// events that were not persisted in the previous run.
///
/// **Rename-before-read** semantics prevent a race where the persistence
/// worker appends new records to the active DLQ file while replay reads
/// it — those new records would be renamed away with the old data and lost.
/// Instead we rename the active file to a generation-stamped name first;
/// the worker then writes to a brand-new active DLQ automatically.
///
/// If any record fails WAL admission the replaying file is **preserved** so
/// that no data is silently lost.  A critical error is logged when records
/// are skipped for any reason.
///
/// Returns `Err` only when the DLQ file itself cannot be renamed or read
/// (indicating a filesystem or configuration problem), or when a critical
/// unsupported event kind is encountered.  Individual entry replay failures
/// are logged but do not abort the step.
async fn replay_dead_letter_queue(state: &Arc<PlusServerState>) -> Result<()> {
    let dlq_path = state
        .config
        .runtime
        .persistence_dead_letter_path
        .as_deref()
        .map(std::path::Path::new);

    let Some(path) = dlq_path else {
        info!("startup recovery: no dead-letter path configured, skipping DLQ replay");
        return Ok(());
    };

    // ── 1. Rename the active DLQ file BEFORE reading ──────────────────────
    // Rename first so any concurrent worker append goes to a brand-new
    // active DLQ file and never races against our read-then-rename cycle.
    // If enqueue fails for any record the replaying file is preserved so
    // that records are not silently lost.
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let replaying_name = format!("{file_name}.replaying-{timestamp}");
    let replaying_path = path.with_file_name(&replaying_name);

    match tokio::fs::rename(path, &replaying_path).await {
        Ok(()) => info!(
            "startup recovery: renamed active DLQ to {} for replay",
            replaying_name,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            info!("startup recovery: no dead-letter file found, skipping DLQ replay");
            return Ok(());
        }
        Err(e) => {
            return Err(anyhow::anyhow!(
                "startup recovery: failed to rename DLQ file {}: {e}",
                path.display(),
            ));
        }
    };

    // ── 2. Read from the renamed (stable) file ────────────────────────────
    let content = match tokio::fs::read_to_string(&replaying_path).await {
        Ok(c) => c,
        Err(e) => {
            return Err(anyhow::anyhow!(
                "startup recovery: failed to read replaying DLQ file {}: {e}",
                replaying_path.display(),
            ));
        }
    };

    // Known non-critical event kinds that can be safely skipped during replay.
    // round_result: low-risk since the client has the result and can re-submit.
    // benchmark.completed: diagnostic only.
    const NON_CRITICAL_KINDS: &[&str] = &["round_result", "benchmark.completed"];

    let mut replayed = 0u32;
    let mut skipped = 0u32;
    let mut enqueue_failures = 0u32;
    let mut critical_unsupported = false;

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
            None if NON_CRITICAL_KINDS.contains(&kind) => {
                warn!("startup recovery: skipping non-critical DLQ entry (kind={kind})");
                skipped += 1;
                continue;
            }
            None => {
                error!(
                    "startup recovery: critical unsupported DLQ entry (kind={kind}) \
                     — service will not become ready"
                );
                critical_unsupported = true;
                skipped += 1;
                continue;
            }
        };

        // Enqueue — best effort; failure is logged but does not block recovery.
        if state.persistence_worker.enqueue(event).await.is_err() {
            warn!("startup recovery: failed to re-enqueue DLQ event (kind={kind})");
            skipped += 1;
            enqueue_failures += 1;
        } else {
            replayed += 1;
        }
    }

    if replayed > 0 || skipped > 0 {
        info!(
            "startup recovery: DLQ replay finished — {replayed} replayed, {skipped} skipped, \
             {enqueue_failures} enqueue failure(s)",
        );
    } else {
        info!("startup recovery: no DLQ entries to replay");
    }

    // ── 3. Critical unsupported events prevent the server from starting ───
    // Keep the replaying file for operator inspection.
    if critical_unsupported {
        return Err(anyhow::anyhow!(
            "startup recovery: DLQ contains one or more critical unsupported events \
             — replaying file preserved at {}",
            replaying_path.display(),
        ));
    }

    // ── 4. Enqueue failures: preserve the replaying file for retry ────────
    if enqueue_failures > 0 {
        error!(
            "startup recovery: {enqueue_failures} DLQ event(s) failed WAL admission \
             — replaying file preserved at {}",
            replaying_path.display(),
        );
        return Ok(());
    }

    // ── 5. Log a critical error if any records were otherwise skipped ─────
    if skipped > 0 {
        error!(
            "startup recovery: {skipped} DLQ entry(ies) were skipped during replay \
             — data may have been lost",
        );
    }

    // ── 6. All records successfully replayed — clean up ───────────────────
    match tokio::fs::remove_file(&replaying_path).await {
        Ok(()) => info!(
            "startup recovery: deleted replayed DLQ file {}",
            replaying_path.display(),
        ),
        Err(e) => warn!(
            "startup recovery: failed to delete replayed DLQ file {}: {e}",
            replaying_path.display(),
        ),
    }

    Ok(())
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
        "user_authenticated" => {
            let user_id = event.get("user_id")?.as_i64()? as i32;
            let user_name = event.get("user_name")?.as_str()?;
            let language = event.get("language")?.as_str()?;
            let ip = event.get("ip")?.as_str()?;
            let connected_at = event.get("connected_at")?.as_i64()?;
            Some(PersistenceEvent::UserAuthenticated {
                event_id: event.get("event_id")?.as_str()?.to_string(),
                session_id: event.get("session_id")?.as_str()?.to_string(),
                user_id,
                user_name: user_name.to_string(),
                language: language.to_string(),
                ip: ip.to_string(),
                connected_at,
            })
        }
        "round_completed" => {
            let round_uuid = event.get("round_uuid")?.as_str()?;
            let room_id = event.get("room_id")?.as_str()?;
            let finished_at = event.get("finished_at")?.as_i64()?;
            let aborted_users: Vec<i32> = event
                .get("aborted_users")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            let results: Vec<crate::room::PlayResult> = serde_json::from_value(
                event.get("results")?.clone(),
            )
            .ok()?;
            Some(PersistenceEvent::RoundCompleted {
                event_id: event.get("event_id")?.as_str()?.to_string(),
                round_uuid: round_uuid.to_string(),
                room_id: room_id.to_string(),
                results,
                finished_at,
                aborted_users,
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

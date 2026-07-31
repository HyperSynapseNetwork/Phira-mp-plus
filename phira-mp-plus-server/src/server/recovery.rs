//! Server startup state recovery — WAL drain, DLQ replay, crash recovery for
//! unfinished rounds, schema validation, persistent room restoration, and
//! playtime stale session cleanup.
//!
//! After a server restart (planned or crash), in-memory state is empty while
//! PostgreSQL still holds data from the previous run.  This module re-discovers
//! that data and reconciles the server state so that:
//!
//! 1. Schema version and database health are logged.
//! 2. The persistence WAL is fully replayed and drained (**before** crash
//!    recovery so that RoundCompleted events in the WAL are applied to
//!    PostgreSQL — otherwise rounds that actually completed would be falsely
//!    marked as aborted).
//! 3. Dead-letter queue events are replayed and flushed (**before** crash
//!    recovery for the same reason, and **before** stale session cleanup so
//!    that old UserAuthenticated events cannot re-set sessions online).
//! 4. Unfinished rounds (those still without `finished_at` after WAL and DLQ
//!    replay) are verified against `mp_events` for a completion event and
//!    aborted only if truly unfinished.
//! 5. Persistent empty rooms are recreated from mp_settings.
//! 6. All open playtime sessions from the previous instance are closed (after
//!    WAL and DLQ replay, with a 1-hour cap on recovered playtime).
//!
//! Every step returns `Err` on failure so that `recover_state` propagates
//! the error and prevents the server from becoming ready.

use std::path::Path;
use std::sync::Arc;

use crate::db::DbManager;
use crate::room_actor::RoomSnapshot;
use anyhow::Result;
use tracing::{error, info, warn};

use super::state::PlusServerState;

/// Stages of the startup recovery sequence, logged in order as each runs.
enum RecoveryStage {
    SchemaValidation,
    WalHealth,
    DlqReplay,
    RoundRecovery,
    RoomRestore,
    PlaytimeCleanup,
}

impl std::fmt::Display for RecoveryStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SchemaValidation => write!(f, "schema-validation"),
            Self::WalHealth => write!(f, "wal-health"),
            Self::DlqReplay => write!(f, "dlq-replay"),
            Self::RoundRecovery => write!(f, "round-recovery"),
            Self::RoomRestore => write!(f, "room-restore"),
            Self::PlaytimeCleanup => write!(f, "playtime-cleanup"),
        }
    }
}

/// Log entry into a recovery stage.
fn log_stage(stage: &RecoveryStage) {
    info!("startup recovery: stage = {stage}");
}

/// Run all startup recovery steps.
///
/// Must be called **after** the PostgreSQL connection is established and
/// migrations have run, but **before** accepting network connections or
/// initialising plugins that depend on a consistent state.
///
/// Failures are **fatal** — any critical recovery step that fails will
/// prevent the server from becoming ready.
pub async fn recover_state(state: &Arc<PlusServerState>, db: &DbManager) -> Result<()> {
    // ── 1. Schema version validation ────────────────────────────────────
    log_stage(&RecoveryStage::SchemaValidation);
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

    // ── 2. Database diagnostics ─────────────────────────────────────────
    let user_count = db.count_users().await;
    let playtime_count = db.count_playtime().await;
    info!(
        "startup recovery: database state — {} user record(s), \
         {} playtime entry(ies)",
        user_count, playtime_count,
    );

    // ── 3. WAL health check ─────────────────────────────────────────────
    log_stage(&RecoveryStage::WalHealth);
    // The PersistenceWorker replays the WAL in a background task. Wait
    // briefly for replay to complete **and drain** (is_healthy returns true
    // only when initial replay has drained), then fail-closed if unhealthy.
    // This must happen BEFORE crash recovery so that RoundCompleted events
    // in the WAL are applied to PostgreSQL before we query for unfinished
    // rounds — otherwise rounds that actually completed would be falsely
    // marked as aborted.
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

    // ── 4. Dead-letter queue replay + flush ─────────────────────────────
    log_stage(&RecoveryStage::DlqReplay);
    // Run DLQ replay BEFORE crash recovery so that any RoundCompleted
    // events in the DLQ are committed to PostgreSQL before we decide which
    // rounds are truly unfinished.  Also run BEFORE stale session cleanup
    // so that old UserAuthenticated events cannot re-set sessions online.
    replay_dead_letter_queue(state).await?;

    // ── 5. Crash recovery: abort truly unfinished rounds ────────────────
    log_stage(&RecoveryStage::RoundRecovery);
    // Run AFTER WAL replay and DLQ replay so that rounds whose
    // RoundCompleted event was in the WAL or DLQ have their finished_at
    // set in PostgreSQL before we query for unfinished rounds.
    abort_unfinished_rounds(db).await?;

    // ── 6. Persistent empty room restoration ────────────────────────────
    log_stage(&RecoveryStage::RoomRestore);
    restore_persistent_rooms(state, db).await?;

    // ── 7. Playtime stale session cleanup ───────────────────────────────
    log_stage(&RecoveryStage::PlaytimeCleanup);
    // Run AFTER WAL replay, DLQ replay, and crash recovery so that old
    // UserAuthenticated events cannot re-set sessions online after cleanup.
    close_all_stale_playtime_sessions(db).await?;

    Ok(())
}

/// Query unfinished rounds after WAL+DLQ replay and abort those that are
/// truly unfinished.  A round may appear unfinished (finished_at IS NULL)
/// even after WAL replay if the RoundCompleted event was in the WAL but
/// the UPDATE to mp_rounds did not complete.  In that case we check for a
/// `round.completed` event in mp_events before aborting.
async fn abort_unfinished_rounds(db: &DbManager) -> Result<()> {
    let unfinished = db
        .find_unfinished_rounds()
        .await
        .map_err(|e| anyhow::anyhow!("startup recovery: failed to query unfinished rounds: {e}"))?;
    let count = unfinished.len();
    if count == 0 {
        info!("startup recovery: no unfinished rounds to recover");
        return Ok(());
    }

    let mut aborted: u32 = 0;
    let mut skipped: u32 = 0;
    let mut abort_failures: u32 = 0;

    for round in &unfinished {
        // Safety check: if a round.completed event exists in mp_events
        // then the round actually completed in the WAL — do NOT abort it.
        if db.has_round_completion_event(&round.round_uuid).await {
            warn!(
                "crash recovery: round {} has a completion event in mp_events \
                 but finished_at is NULL — skipping abort (round was completed)",
                round.round_uuid,
            );
            skipped += 1;
            continue;
        }

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
            aborted += 1;
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
            "startup recovery: failed to abort {abort_failures}/{count} unfinished round(s) \
             ({aborted} aborted, {skipped} skipped due to completion event)"
        ));
    }

    if aborted > 0 {
        info!(
            "startup recovery: aborted {aborted} unfinished round(s) from \
             previous server session ({skipped} had completion events and were skipped)",
        );
    } else if skipped > 0 {
        info!(
            "startup recovery: all {skipped} unfinished round(s) had completion events — \
             none were aborted",
        );
    }

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
            Ok(_) => {
                info!("startup recovery: restored persistent room {room_id}");
                // Load the latest RoomSnapshot from mp_room_snapshots and
                // apply lock/cycle/chart/hidden state so the persistent room
                // reflects its pre-restart configuration.
                if let Some(payload) = db.get_latest_room_snapshot(room_id).await {
                    match serde_json::from_value::<RoomSnapshot>(payload) {
                        Ok(snapshot) => {
                            // Apply lock state
                            if let Err(e) = state
                                .room_commands
                                .set_lock(state, room_id, snapshot.locked)
                                .await
                            {
                                warn!(
                                    "startup recovery: failed to set lock for room {room_id}: {e}"
                                );
                            }
                            // Apply cycle state
                            if let Err(e) = state
                                .room_commands
                                .set_cycle(state, room_id, snapshot.cycle)
                                .await
                            {
                                warn!(
                                    "startup recovery: failed to set cycle for room {room_id}: {e}"
                                );
                            }
                            // Apply hidden state
                            if let Err(e) = state
                                .room_commands
                                .set_hidden(state, room_id, snapshot.hidden)
                                .await
                            {
                                warn!(
                                    "startup recovery: failed to set hidden for room {room_id}: {e}"
                                );
                            }
                            // Apply chart state
                            if let Some(chart_id) = snapshot.chart {
                                let chart_name = snapshot
                                    .chart_name
                                    .clone()
                                    .unwrap_or_else(|| format!("#{chart_id}"));
                                if let Err(e) = state
                                    .room_commands
                                    .set_chart(state, room_id, chart_id, &chart_name, 0)
                                    .await
                                {
                                    warn!(
                                        "startup recovery: failed to set chart for room {room_id}: {e}"
                                    );
                                }
                            }
                            // Apply host state
                            if let Some(host_id) = snapshot.host {
                                if let Err(e) = state
                                    .room_commands
                                    .set_host(state, room_id, Some(host_id))
                                    .await
                                {
                                    warn!(
                                        "startup recovery: failed to set host for room {room_id}: {e}"
                                    );
                                }
                            }
                            info!(
                                "startup recovery: applied snapshot state to room {room_id} \
                                 (locked={}, cycle={}, hidden={}, chart={:?}, host={:?})",
                                snapshot.locked,
                                snapshot.cycle,
                                snapshot.hidden,
                                snapshot.chart,
                                snapshot.host,
                            );
                        }
                        Err(e) => {
                            warn!(
                                "startup recovery: failed to parse RoomSnapshot for room {room_id}: {e}"
                            );
                        }
                    }
                } else {
                    info!(
                        "startup recovery: no snapshot found for persistent room {room_id}, \
                         using defaults"
                    );
                }
            }
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

/// Summary of a single DLQ file replay.
#[derive(Default)]
struct DlqReplaySummary {
    replayed: u32,
    skipped: u32,
    quarantined: u32,
    enqueue_failures: u32,
    critical_unsupported: bool,
    /// Enqueue failures for UserAuthenticated or RoundCompleted events.
    critical_enqueue_failures: u32,
}

/// Process a single DLQ replaying file: read entries, re-enqueue valid ones,
/// quarantine malformed JSON lines to a `.quarantine` sidecar file, and
/// return a summary of the outcome.
///
/// Known non-critical event kinds are skipped without quarantine since they
/// carry no meaningful data loss risk:
///   - `round_result`: low-risk, the client has the result and can re-submit.
///   - `benchmark.completed`: diagnostic only.
async fn process_dlq_file(
    state: &Arc<PlusServerState>,
    file_path: &Path,
) -> Result<DlqReplaySummary> {
    let content = match tokio::fs::read_to_string(file_path).await {
        Ok(c) => c,
        Err(e) => {
            return Err(anyhow::anyhow!(
                "startup recovery: failed to read DLQ file {}: {e}",
                file_path.display(),
            ));
        }
    };

    const NON_CRITICAL_KINDS: &[&str] = &["round_result", "benchmark.completed"];
    // Event kinds whose enqueue failure must prevent the server from
    // becoming ready.  These carry data that cannot be safely recovered
    // after stale session cleanup (UserAuthenticated) or that represents
    // a terminal state that must not be lost (RoundCompleted).
    const CRITICAL_KINDS: &[&str] = &["user_authenticated", "round_completed"];

    let mut summary = DlqReplaySummary::default();
    let mut quarantined_lines: Vec<String> = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let record: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                warn!("startup recovery: quarantining malformed DLQ entry: {e}");
                quarantined_lines.push(line.to_string());
                summary.quarantined += 1;
                continue;
            }
        };

        let kind = match record.get("kind").and_then(|v| v.as_str()) {
            Some(k) => k,
            None => {
                warn!("startup recovery: skipping DLQ entry without kind");
                summary.skipped += 1;
                continue;
            }
        };

        let event_payload = match record.get("event") {
            Some(v) if !v.is_null() => v,
            _ => {
                warn!(
                    "startup recovery: skipping DLQ entry without event payload (kind={kind})"
                );
                summary.skipped += 1;
                continue;
            }
        };

        let event = match reconstruct_event(kind, event_payload) {
            Some(e) => e,
            None if NON_CRITICAL_KINDS.contains(&kind) => {
                warn!("startup recovery: skipping non-critical DLQ entry (kind={kind})");
                summary.skipped += 1;
                continue;
            }
            None => {
                error!(
                    "startup recovery: critical unsupported DLQ entry (kind={kind}) \
                     — service will not become ready"
                );
                summary.critical_unsupported = true;
                summary.skipped += 1;
                continue;
            }
        };

        // Enqueue — best effort; failure is logged.
        if state.persistence_worker.enqueue(event).await.is_err() {
            warn!("startup recovery: failed to re-enqueue DLQ event (kind={kind})");
            summary.skipped += 1;
            summary.enqueue_failures += 1;
            if CRITICAL_KINDS.contains(&kind) {
                summary.critical_enqueue_failures += 1;
            }
        } else {
            summary.replayed += 1;
        }
    }

    // ── Write quarantined lines to a sidecar file ─────────────────────────
    if !quarantined_lines.is_empty() {
        // Append ".quarantine" to the full filename rather than replacing the
        // extension so the quarantine file is uniquely paired with its source.
        let quarantine_name = format!(
            "{}.quarantine",
            file_path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
        );
        let quarantine_path = file_path.with_file_name(&quarantine_name);
        if let Some(parent) = quarantine_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        // Terminate each line so the quarantine file is valid line-delimited JSON.
        let qcontent = quarantined_lines.join("\n") + "\n";
        match tokio::fs::write(&quarantine_path, qcontent).await {
            Ok(()) => info!(
                "startup recovery: quarantined {} malformed DLQ entry(ies) to {}",
                summary.quarantined,
                quarantine_path.display(),
            ),
            Err(e) => warn!(
                "startup recovery: failed to write quarantine file {}: {e}",
                quarantine_path.display(),
            ),
        }
    }

    Ok(summary)
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
/// **Orphan replaying files** (`*.replaying-*`) from previous crashes are
/// scanned and replayed first, ordered by timestamp/generation, so that no
/// records are lost across restarts.
///
/// **Malformed records** are written to a quarantine file rather than
/// silently skipped.
///
/// **After replay**, persistence is flushed to ensure all replayed events
/// are committed before stale session cleanup runs.
///
/// If any record fails WAL admission the replaying file is **preserved** so
/// that no data is silently lost.  A critical error is logged when records
/// are skipped for any reason.
///
/// Returns `Err` only when a DLQ file itself cannot be renamed or read
/// (indicating a filesystem or configuration problem), or when a critical
/// unsupported event kind is encountered.  Individual entry replay failures
/// are logged but do not abort the step.
async fn replay_dead_letter_queue(state: &Arc<PlusServerState>) -> Result<()> {
    let dlq_path = state
        .config
        .runtime
        .persistence_dead_letter_path
        .as_deref()
        .map(Path::new);

    let Some(path) = dlq_path else {
        info!("startup recovery: no dead-letter path configured, skipping DLQ replay");
        return Ok(());
    };

    let parent_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();

    // ── 1. Scan for orphan *.replaying-* files from previous crashes ────
    // These exist when the server crashed mid-replay or when a replaying
    // file was preserved due to enqueue failures on a previous restart.
    // Process them oldest-first by filename sort order (the millisecond
    // timestamp suffix sorts correctly as a string).
    let prefix = format!("{file_name}.replaying-");
    let mut orphan_paths: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(parent_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&prefix) {
                orphan_paths.push(entry.path());
            }
        }
    }
    orphan_paths.sort();

    if !orphan_paths.is_empty() {
        info!(
            "startup recovery: found {} orphan DLQ replaying file(s) — processing oldest first",
            orphan_paths.len(),
        );
    }

    // ── 2. Rename the active DLQ file BEFORE reading ──────────────────────
    // Rename first so any concurrent worker append goes to a brand-new
    // active DLQ file and never races against our read-then-rename cycle.
    // If enqueue fails for any record the replaying file is preserved so
    // that records are not silently lost.
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let replaying_name = format!("{file_name}.replaying-{timestamp}");
    let replaying_path = path.with_file_name(&replaying_name);

    let active_exists = match tokio::fs::rename(path, &replaying_path).await {
        Ok(()) => {
            info!(
                "startup recovery: renamed active DLQ to {} for replay",
                replaying_name,
            );
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            return Err(anyhow::anyhow!(
                "startup recovery: failed to rename DLQ file {}: {e}",
                path.display(),
            ));
        }
    };

    // ── 3. Process all replaying files (orphans first, then active) ───────
    let mut all_paths: Vec<std::path::PathBuf> = Vec::new();
    // Orphans go first (already sorted by timestamp).
    all_paths.extend(orphan_paths);
    // Active (now renamed) comes last.
    if active_exists {
        all_paths.push(replaying_path.clone());
    }

    if all_paths.is_empty() {
        info!("startup recovery: no dead-letter file found, skipping DLQ replay");
        return Ok(());
    }

    let mut needs_flush = false;
    let mut total_skipped: u32 = 0;
    let mut total_quarantined: u32 = 0;
    let mut total_enqueue_failures: u32 = 0;
    let mut total_critical_enqueue_failures: u32 = 0;
    let mut critical_unsupported = false;

    for file_path in &all_paths {
        let summary = process_dlq_file(state, file_path).await?;
        if summary.replayed > 0 {
            needs_flush = true;
        }
        if summary.critical_unsupported {
            critical_unsupported = true;
        }
        total_skipped += summary.skipped;
        total_quarantined += summary.quarantined;
        total_enqueue_failures += summary.enqueue_failures;
        total_critical_enqueue_failures += summary.critical_enqueue_failures;
    }

    // Log aggregate stats.
    if needs_flush || total_skipped > 0 || total_quarantined > 0 || total_enqueue_failures > 0 {
        info!(
            "startup recovery: DLQ replay finished over {} file(s) — \
             {} quarantined, {} skipped, {} enqueue failure(s)",
            all_paths.len(),
            total_quarantined,
            total_skipped,
            total_enqueue_failures,
        );
    } else {
        info!("startup recovery: no DLQ entries to replay");
    }

    // ── 4. Flush persistence to ensure enqueued events are committed ───────
    // Capture the current WAL sequence as a fence so we can verify
    // all replayed events are committed before proceeding to stale
    // session cleanup. This must happen BEFORE step 6 (stale sessions)
    // so that recovered UserAuthenticated events are visible before
    // playtime is finalised.
    if needs_flush {
        let fence_seq = state.persistence_worker.wal_sequence();
        info!(
            "startup recovery: flushing persistence after DLQ replay (WAL fence={})",
            fence_seq,
        );
        state
            .persistence_worker
            .flush(std::time::Duration::from_secs(30))
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "startup recovery: persistence flush failed after DLQ replay: {e}"
                )
            })?;

        // Verify all events up to the fence have been committed. WalOnly
        // events may still be pending; the periodic WAL recovery scanner
        // processes them on its next cycle. Poll until they are committed
        // or the deadline expires.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut remaining = state.persistence_worker.pending_wal_count().await;
        while remaining > 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            remaining = state.persistence_worker.pending_wal_count().await;
        }
        if remaining > 0 {
            return Err(anyhow::anyhow!(
                "startup recovery: {remaining} WAL entry(ies) still pending after \
                 DLQ replay flush — persistence has not committed all replayed events",
            ));
        }
    }

    // ── 5. Critical unsupported events prevent the server from starting ───
    // Keep all replaying files for operator inspection.
    if critical_unsupported {
        return Err(anyhow::anyhow!(
            "startup recovery: DLQ contains one or more critical unsupported events \
             — replaying file(s) preserved at {}",
            replaying_path.display(),
        ));
    }

    // ── 6. Critical enqueue failures prevent the server from starting ────
    // UserAuthenticated and RoundCompleted events carry data that must not
    // be silently lost.  Keep all replaying files for operator retry.
    if total_critical_enqueue_failures > 0 {
        return Err(anyhow::anyhow!(
            "startup recovery: {total_critical_enqueue_failures} critical DLQ event(s) \
             (UserAuthenticated/RoundCompleted) failed WAL admission — \
             replaying file(s) preserved",
        ));
    }

    // ── 7. Non-critical enqueue failures: preserve files for retry ───────
    if total_enqueue_failures > 0 {
        error!(
            "startup recovery: {total_enqueue_failures} DLQ event(s) failed WAL admission \
             — replaying file(s) preserved",
        );
        return Ok(());
    }

    // ── 8. Log warnings for any non-critical losses ──────────────────────
    if total_skipped > 0 {
        error!(
            "startup recovery: {total_skipped} DLQ entry(ies) were skipped during replay \
             — data may have been lost",
        );
    }
    if total_quarantined > 0 {
        warn!(
            "startup recovery: {total_quarantined} malformed DLQ entry(ies) were quarantined",
        );
    }

    // ── 9. All records successfully processed — clean up replaying files ─
    for file_path in &all_paths {
        match tokio::fs::remove_file(file_path).await {
            Ok(()) => info!(
                "startup recovery: deleted replayed DLQ file {}",
                file_path.display(),
            ),
            Err(e) => warn!(
                "startup recovery: failed to delete replayed DLQ file {}: {e}",
                file_path.display(),
            ),
        }
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
            let instance_id = event.get("server_instance_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| crate::server_instance::current());
            Some(PersistenceEvent::UserOffline {
                user_id,
                server_instance_id: instance_id.to_string(),
            })
        }
        "user_disconnect" => {
            let user_id = event.get("user_id")?.as_i64()? as i32;
            let user_name = event.get("user_name")?.as_str()?;
            let instance_id = event.get("server_instance_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| crate::server_instance::current());
            Some(PersistenceEvent::UserDisconnect {
                user_id,
                user_name: user_name.to_string(),
                server_instance_id: instance_id.to_string(),
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
            // Use the recorded server_instance_id if present (new events),
            // falling back to the current instance ID for pre-migration DLQ
            // entries that lack the field.
            let instance_id = event.get("server_instance_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| crate::server_instance::current());
            Some(PersistenceEvent::UserAuthenticated {
                event_id: event.get("event_id")?.as_str()?.to_string(),
                session_id: event.get("session_id")?.as_str()?.to_string(),
                user_id,
                user_name: user_name.to_string(),
                language: language.to_string(),
                ip: ip.to_string(),
                connected_at,
                server_instance_id: instance_id.to_string(),
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

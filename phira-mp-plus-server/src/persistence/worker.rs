//! Runtime v2 PersistenceWorker.
//!
//! Ordinary persistence events use bounded backpressure instead of queue-full
//! loss. Flush and shutdown are ordered control messages with acknowledgements,
//! so accepted work can be drained before process termination. Production
//! Touch/Judge telemetry supports three explicit cutover modes. The default
//! remains direct-only; worker-preferred mirrors direct writes; and the guarded
//! worker-authoritative mode makes the batcher the normal-operation single
//! writer while retaining a direct fallback only when enqueue is rejected.
//! Accepted telemetry batches are retained in memory after database failure,
//! ordinary events now use an fsync-before-admission WAL with startup replay and ACK compaction. Touch/Judge durability still terminates at TelemetryBatcher admission until batch commit acknowledgements are wired end-to-end.

use crate::persistence::message::PersistenceEvent;
use crate::persistence::stats::{
    record_dead_letter_failed, record_dead_letter_written, record_dropped, record_queued,
    record_telemetry_cutover_observation, PersistenceStats, TelemetryCutoverObservation,
};
use crate::persistence::wal::PersistenceWal;
use crate::telemetry_batcher::{
    TelemetryBatcher, TelemetryBatcherPolicy, TelemetryBatcherStats, TelemetryCutoverMode,
};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tracing::{info, warn};

static DEAD_LETTER_FAILURE_REPORTED: AtomicBool = AtomicBool::new(false);

enum WorkerMessage {
    Event {
        wal_id: uuid::Uuid,
        event: PersistenceEvent,
    },
    Flush {
        timeout: Duration,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Shutdown {
        timeout: Duration,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Debug)]
pub struct PersistenceWorker {
    tx: mpsc::Sender<WorkerMessage>,
    /// Serializes event/control insertion so nothing can be accepted behind a
    /// Shutdown marker and then remain unprocessed.
    send_gate: Mutex<()>,
    /// Idle-mode diagnostic hint. Persistence remains active while idle so
    /// accepted events are never discarded merely because gameplay is quiet.
    suspended: AtomicBool,
    closed: AtomicBool,
    stats: Arc<RwLock<PersistenceStats>>,
    telemetry_batcher: Arc<TelemetryBatcher>,
    telemetry_cutover_mode: Arc<RwLock<TelemetryCutoverMode>>,
    wal: Arc<PersistenceWal>,
}

async fn report_dead_letter_durability_failure(error: String) {
    if !DEAD_LETTER_FAILURE_REPORTED.swap(true, Ordering::AcqRel) {
        crate::supervisor_actor::report_critical_failure("persistence-dead-letter", error).await;
    }
}

/// Returns true if the event was durably stored (dead-letter written successfully).
async fn preserve_failed_event(
    wal_id: uuid::Uuid,
    path: Option<&Path>,
    event: &PersistenceEvent,
    stage: &str,
    error: &str,
    stats: &Arc<RwLock<PersistenceStats>>,
) -> bool {
    let kind = event.kind();
    let simulation = event.is_simulation();
    let summary = event.summary();
    let Some(payload) = event.dead_letter_payload() else {
        return false;
    };
    let Some(path) = path else {
        let durability_error =
            "dead-letter journal disabled; failed event was not preserved".to_string();
        record_dead_letter_failed(stats, kind, simulation, summary, durability_error.clone()).await;
        report_dead_letter_durability_failure(durability_error).await;
        return false;
    };
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let record = json!({
        "schema_version": 1,
        "dead_letter_id": wal_id.to_string(),  // reuse wal_id as stable dead_letter_id
        "wal_id": wal_id.to_string(),
        "failed_at_ms": timestamp_ms,
        "stage": stage,
        "kind": kind,
        "simulation": simulation,
        "summary": summary,
        "error": error,
        "event": payload,
    });
    match append_dead_letter(path, &record).await {
        Ok(()) => {
            record_dead_letter_written(stats, event.kind(), simulation, event.summary()).await;
            true
        }
        Err(dead_letter_error) => {
            let durability_error =
                format!("failed to persist dead-letter record: {dead_letter_error}");
            record_dead_letter_failed(
                stats,
                event.kind(),
                simulation,
                event.summary(),
                durability_error.clone(),
            )
            .await;
            report_dead_letter_durability_failure(durability_error).await;
            false
        }
    }
}

async fn append_dead_letter(path: &Path, record: &serde_json::Value) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600) // secure permissions: owner read/write only
        .open(path)
        .await
        .map_err(|error| format!("open {}: {error}", path.display()))?;
    let mut line = serde_json::to_vec(record)
        .map_err(|error| format!("serialize dead-letter record: {error}"))?;
    line.push(b'\n');
    file.write_all(&line)
        .await
        .map_err(|error| format!("append {}: {error}", path.display()))?;
    file.flush()
        .await
        .map_err(|error| format!("flush {}: {error}", path.display()))?;
    file.sync_data()
        .await
        .map_err(|error| format!("sync {}: {error}", path.display()))?;
    drop(file);
    // Sync parent directory so the dead-letter entry survives a rename.
    if let Some(parent) = path.parent().filter(|p| p.as_os_str() != "") {
        if let Ok(dir) = tokio::fs::File::open(parent).await {
            dir.sync_all().await.map_err(|error| {
                format!("sync parent directory {}: {error}", parent.display())
            })?;
        }
    }
    Ok(())
}

/// Normal worker loop: processes replayed events first, then new admissions
/// from the channel, dispatching each through the persistence pipeline and
/// ACKing the WAL on completion.
async fn process_worker_loop(
    rx: &mut mpsc::Receiver<WorkerMessage>,
    replay: &mut std::collections::VecDeque<(uuid::Uuid, PersistenceEvent)>,
    worker_stats: &Arc<RwLock<PersistenceStats>>,
    worker_telemetry: &Arc<TelemetryBatcher>,
    worker_dead_letter_path: &Option<std::path::PathBuf>,
    worker_wal: &Arc<PersistenceWal>,
) {
    use crate::persistence::pipeline::{
        persist_benchmark_report_if_needed, persist_production_event_if_needed,
        persist_simulation_event_if_needed, stage_production_telemetry_if_needed,
        BenchmarkReportStage, PersistenceWriteStage, ProductionTelemetryStage,
    };
    use crate::persistence::stats::{
        record_benchmark_report_persist_request, record_benchmark_report_persist_skipped,
        record_db_dispatch_failure, record_db_dispatch_skipped_no_database,
        record_db_dispatch_success, record_processed, record_production_persist_request,
        record_production_persist_skipped, record_production_telemetry_stage_failed,
        record_production_telemetry_staged, record_simulation_persist_request,
    };
    use tracing::{debug, trace, warn};

    // Pending ACK retry queue. When worker_wal.ack() fails, the wal_id is
    // queued here for retry on subsequent iterations. Flush/Shutdown drain
    // this queue before returning.
    let mut pending_acks: std::collections::VecDeque<(uuid::Uuid, u32)> =
        std::collections::VecDeque::new();

    loop {
        let message = if let Some((wal_id, event)) = replay.pop_front() {
            WorkerMessage::Event { wal_id, event }
        } else {
            let Some(message) = rx.recv().await else {
                break;
            };
            message
        };
        // Retry pending WAL ACKs (up to 5 per iteration).
        for _ in 0..5 {
            let Some((retry_id, retry_attempt)) = pending_acks.front().copied() else {
                break;
            };
            if retry_attempt > 10 {
                crate::supervisor_actor::report_critical_failure(
                    "persistence-wal-ack",
                    format!("ACK retry exhausted for wal_id={retry_id} after 10 attempts; WAL entry will not be removed"),
                ).await;
                pending_acks.pop_front();
                continue;
            }
            match worker_wal.ack(retry_id).await {
                Ok(()) => {
                    debug!(wal_id = %retry_id, "ACK retry succeeded");
                    pending_acks.pop_front();
                }
                Err(e) => {
                    trace!(wal_id = %retry_id, attempt = %retry_attempt, error = %e, "ACK retry failed");
                    // Move to back with incremented attempt count
                    if let Some(mut entry) = pending_acks.pop_front() {
                        entry.1 = entry.1.saturating_add(1);
                        pending_acks.push_back(entry);
                    }
                }
            }
        }

        let (wal_id, event) = match message {
            WorkerMessage::Event { wal_id, event } => (wal_id, event),
            WorkerMessage::Flush { timeout, reply } => {
                // Drain pending ACKs before flushing telemetry.
                drain_pending_acks(worker_wal, &mut pending_acks).await;
                let result = worker_telemetry.flush(timeout).await;
                if let Err(error) = &result {
                    warn!(%error, "telemetry flush failed; persistence worker remains active");
                }
                // Report error if pending ACKs remain after drain.
                let ack_pending = pending_acks.len();
                let combined = if ack_pending > 0 {
                    Err(format!("{ack_pending} WAL ACKs still pending after drain"))
                } else {
                    result
                };
                let _ = reply.send(combined);
                continue;
            }
            WorkerMessage::Shutdown { timeout, reply } => {
                // Drain pending ACKs before shutting down.
                drain_pending_acks(worker_wal, &mut pending_acks).await;
                let result = worker_telemetry.shutdown(timeout).await;
                let ack_pending = pending_acks.len();
                let should_stop = ack_pending == 0 && result.is_ok();
                if let Err(error) = &result {
                    warn!(%error, "telemetry shutdown failed; persistence worker remains active");
                }
                if ack_pending > 0 {
                    warn!(count = ack_pending, "WAL ACKs still pending during shutdown");
                }
                let _ = reply.send(result);
                if should_stop {
                    break;
                }
                continue;
            }
        };
        let kind = event.kind();
        let simulation = event.is_simulation();
        let summary = event.summary();
        // Track whether this event reached a durable terminal state.
        // Only durable events get WAL ACKed; non-durable entries remain
        // in the WAL for replay on restart (crash recovery).
        let mut durable = false;
        match persist_benchmark_report_if_needed(&event).await {
            BenchmarkReportStage::Acknowledged { elapsed_ms } => {
                durable = true;
                record_benchmark_report_persist_request(worker_stats).await;
                record_db_dispatch_success(
                    worker_stats,
                    crate::persistence::PersistencePipeline::BenchmarkReport,
                    elapsed_ms,
                )
                .await;
            }
            BenchmarkReportStage::Failed { elapsed_ms, error } => {
                if preserve_failed_event(
                    wal_id,
                    worker_dead_letter_path.as_deref(),
                    &event,
                    "benchmark_report",
                    &error,
                    worker_stats,
                )
                .await
                {
                    durable = true;
                }
                record_benchmark_report_persist_skipped(worker_stats).await;
                record_db_dispatch_failure(
                    worker_stats,
                    crate::persistence::PersistencePipeline::BenchmarkReport,
                    elapsed_ms,
                    error,
                )
                .await;
            }
            BenchmarkReportStage::SkippedNoDatabase => {
                record_benchmark_report_persist_skipped(worker_stats).await;
                record_db_dispatch_skipped_no_database(
                    worker_stats,
                    crate::persistence::PersistencePipeline::BenchmarkReport,
                )
                .await;
            }
            BenchmarkReportStage::NotBenchmark => {
                match persist_simulation_event_if_needed(&event).await {
                    PersistenceWriteStage::Acknowledged {
                        pipeline,
                        elapsed_ms,
                    } => {
                        durable = true;
                        record_simulation_persist_request(worker_stats).await;
                        record_db_dispatch_success(worker_stats, pipeline, elapsed_ms).await;
                    }
                    PersistenceWriteStage::Failed {
                        pipeline,
                        elapsed_ms,
                        error,
                    } => {
                        if preserve_failed_event(
                            wal_id,
                            worker_dead_letter_path.as_deref(),
                            &event,
                            "simulation",
                            &error,
                            worker_stats,
                        )
                        .await
                        {
                            durable = true;
                        }
                        record_simulation_persist_request(worker_stats).await;
                        record_db_dispatch_failure(worker_stats, pipeline, elapsed_ms, error).await;
                    }
                    PersistenceWriteStage::SkippedNoDatabase { pipeline } => {
                        record_db_dispatch_skipped_no_database(worker_stats, pipeline).await;
                    }
                    PersistenceWriteStage::NotApplicable => {
                        match stage_production_telemetry_if_needed(&event, worker_telemetry).await {
                            ProductionTelemetryStage::Staged => {
                                record_production_telemetry_staged(worker_stats).await;
                            }
                            ProductionTelemetryStage::Failed(error) => {
                                if preserve_failed_event(
                                    wal_id,
                                    worker_dead_letter_path.as_deref(),
                                    &event,
                                    "telemetry_stage",
                                    &error,
                                    worker_stats,
                                )
                                .await
                                {
                                    durable = true;
                                }
                                record_production_telemetry_stage_failed(worker_stats, error).await;
                            }
                            ProductionTelemetryStage::NotTelemetry => {
                                match persist_production_event_if_needed(&event).await {
                                    PersistenceWriteStage::Acknowledged {
                                        pipeline,
                                        elapsed_ms,
                                    } => {
                                        durable = true;
                                        record_production_persist_request(worker_stats).await;
                                        record_db_dispatch_success(
                                            worker_stats,
                                            pipeline,
                                            elapsed_ms,
                                        )
                                        .await;
                                    }
                                    PersistenceWriteStage::Failed {
                                        pipeline,
                                        elapsed_ms,
                                        error,
                                    } => {
                                        if preserve_failed_event(
                                            wal_id,
                                            worker_dead_letter_path.as_deref(),
                                            &event,
                                            "production",
                                            &error,
                                            worker_stats,
                                        )
                                        .await
                                        {
                                            durable = true;
                                        }
                                        record_production_persist_request(worker_stats).await;
                                        record_db_dispatch_failure(
                                            worker_stats,
                                            pipeline,
                                            elapsed_ms,
                                            error,
                                        )
                                        .await;
                                    }
                                    PersistenceWriteStage::SkippedNoDatabase { pipeline } => {
                                        record_db_dispatch_skipped_no_database(
                                            worker_stats,
                                            pipeline,
                                        )
                                        .await;
                                    }
                                    PersistenceWriteStage::NotApplicable => {
                                        if !event.is_simulation()
                                            && !matches!(
                                                &event,
                                                PersistenceEvent::Flush
                                                    | PersistenceEvent::Shutdown
                                            )
                                        {
                                            record_production_persist_skipped(worker_stats).await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        match event {
            PersistenceEvent::Shutdown => {
                debug!(kind = %kind, "persistence worker shutdown requested");
                record_processed(worker_stats, kind, simulation, summary).await;
                break;
            }
            PersistenceEvent::Flush => {
                debug!(kind = %kind, "persistence worker flush marker received");
            }
            other => {
                trace!(?other, "persistence worker consumed mirrored event");
            }
        }
        record_processed(worker_stats, kind.clone(), simulation, summary).await;

        // Only ACK events that reached a durable terminal state.
        // DatabaseCommitted / DurableDeadLetterStored → ACK
        // TelemetryStaged / DeadLetterFailed / SkippedNoDB → retain in WAL
        if durable {
            if let Err(error) = worker_wal.ack(wal_id).await {
                crate::supervisor_actor::report_critical_failure("persistence-wal-ack", error).await;
                pending_acks.push_back((wal_id, 0));
            }
        } else {
            tracing::warn!(
                wal_id = %wal_id, kind = %kind,
                "WAL entry not ACKed (non-durable outcome); will replay on restart"
            );
        }

        // Auto-compaction: trigger when ACK ratio drops below threshold.
        if worker_wal.should_compact() {
            if let Err(e) = worker_wal.compact().await {
                tracing::warn!(error = %e, "auto-compaction failed");
            } else {
                tracing::debug!("auto-compaction completed");
            }
        }
    }
}

/// Drain the pending ACK queue, retrying each entry until success or exhausted.
async fn drain_pending_acks(
    worker_wal: &Arc<PersistenceWal>,
    pending_acks: &mut std::collections::VecDeque<(uuid::Uuid, u32)>,
) {
    use tracing::{debug, warn};
    while let Some((id, attempt)) = pending_acks.pop_front() {
        match worker_wal.ack(id).await {
            Ok(()) => debug!(wal_id = %id, "pending ACK drained"),
            Err(e) => {
                if attempt > 10 {
                    warn!(wal_id = %id, "pending ACK exhausted during drain");
                } else {
                    warn!(wal_id = %id, error = %e, "pending ACK drain failed, will retry");
                    pending_acks.push_back((id, attempt + 1));
                    break; // stop draining on failure, retry next iteration
                }
            }
        }
    }
}

/// Degraded worker loop: entered when WAL replay fails. Only accepts
/// Shutdown commands; all other messages are logged and discarded so no
/// data is processed with an unverified WAL.
async fn process_degraded_worker_loop(
    rx: &mut mpsc::Receiver<WorkerMessage>,
    worker_telemetry: &Arc<TelemetryBatcher>,
) {
    use tracing::{error, info, warn};

    error!("persistence worker entered degraded mode: WAL replay failed, rejecting all events");

    loop {
        let Some(message) = rx.recv().await else {
            break;
        };
        match message {
            WorkerMessage::Event { wal_id, .. } => {
                warn!(wal_id = %wal_id, "dropping event in degraded persistence worker");
                continue;
            }
            WorkerMessage::Flush { timeout, reply } => {
                let result = worker_telemetry.flush(timeout).await;
                let _ = reply.send(result);
                continue;
            }
            WorkerMessage::Shutdown { timeout, reply } => {
                info!("degraded persistence worker shutting down");
                let result = worker_telemetry.shutdown(timeout).await;
                let _ = reply.send(result);
                break;
            }
        }
    }
}

impl PersistenceWorker {
    pub fn spawn(queue_capacity: usize) -> Arc<Self> {
        Self::spawn_with_policy(
            queue_capacity,
            TelemetryBatcherPolicy::default(),
            TelemetryCutoverMode::default(),
        )
    }

    pub fn spawn_with_policy(
        queue_capacity: usize,
        telemetry_policy: TelemetryBatcherPolicy,
        telemetry_cutover_mode: TelemetryCutoverMode,
    ) -> Arc<Self> {
        Self::spawn_with_policy_and_dead_letter(
            queue_capacity,
            telemetry_policy,
            telemetry_cutover_mode,
            Some("data/persistence-dead-letter.jsonl".to_string()),
        )
    }

    pub fn spawn_with_policy_and_dead_letter(
        queue_capacity: usize,
        telemetry_policy: TelemetryBatcherPolicy,
        telemetry_cutover_mode: TelemetryCutoverMode,
        dead_letter_path: Option<String>,
    ) -> Arc<Self> {
        Self::spawn_with_policy_and_journals(
            queue_capacity,
            telemetry_policy,
            telemetry_cutover_mode,
            dead_letter_path,
            "data/persistence-worker.wal.jsonl".to_string(),
        )
    }

    pub fn spawn_with_policy_and_journals(
        queue_capacity: usize,
        telemetry_policy: TelemetryBatcherPolicy,
        telemetry_cutover_mode: TelemetryCutoverMode,
        dead_letter_path: Option<String>,
        wal_path: String,
    ) -> Arc<Self> {
        let capacity = queue_capacity.max(16);
        let dead_letter_path = dead_letter_path
            .map(|path| path.trim().to_string())
            .filter(|path| !path.is_empty());
        let (tx, mut rx) = mpsc::channel::<WorkerMessage>(capacity);
        let initial_cutover = telemetry_cutover_mode;
        let telemetry_batcher = TelemetryBatcher::spawn(telemetry_policy.clone());
        // Wire WAL ACK channel: TelemetryBatcher will ACK WAL IDs after DB commit.
        let (wal_ack_tx, mut wal_ack_rx) = mpsc::channel::<uuid::Uuid>(1024);
        telemetry_batcher.set_wal_ack_tx(wal_ack_tx);
        let telemetry_cutover_mode = Arc::new(RwLock::new(initial_cutover));
        let stats = Arc::new(RwLock::new(PersistenceStats {
            capacity,
            dead_letter_path: dead_letter_path.clone(),
            telemetry_cutover_mode: initial_cutover.as_str().to_string(),
            telemetry: TelemetryBatcherStats::from_policy(&telemetry_policy),
            ..PersistenceStats::default()
        }));
        let worker_stats = Arc::clone(&stats);
        let worker_telemetry = Arc::clone(&telemetry_batcher);
        let worker_dead_letter_path = dead_letter_path.map(PathBuf::from);
        let wal = Arc::new(PersistenceWal::new(wal_path));
        let worker_wal = Arc::clone(&wal);

        // Spawn a task to receive WAL ACKs from the TelemetryBatcher.
        let wal_ack_worker_wal = Arc::clone(&wal);
        crate::supervisor_actor::spawn_named("persistence-wal-ack", async move {
            use tracing::{debug, error};
            while let Some(wal_id) = wal_ack_rx.recv().await {
                if let Err(e) = wal_ack_worker_wal.ack(wal_id).await {
                    error!(wal_id = %wal_id, error = %e, "WAL ACK from batcher failed");
                } else {
                    debug!(wal_id = %wal_id, "WAL ACK from telemetry batcher");
                }
            }
        });

        crate::supervisor_actor::spawn_critical("persistence-worker", async move {
            // Check WAL instance consistency before replay to detect
            // accidental deletion/truncation of an already-initialized WAL.
            // If consistency check fails, enter degraded mode instead of replay.
            let mut replay_ok = true;
            if let Err(e) = worker_wal.check_instance_consistency().await {
                crate::supervisor_actor::report_critical_failure(
                    "persistence-wal-consistency",
                    e,
                )
                .await;
                replay_ok = false;
            }
            if replay_ok {
            match worker_wal.replay().await {
                Ok(events) => {
                    let mut replay: std::collections::VecDeque<(uuid::Uuid, PersistenceEvent)> =
                        std::collections::VecDeque::from(events);
                    process_worker_loop(
                        &mut rx,
                        &mut replay,
                        &worker_stats,
                        &worker_telemetry,
                        &worker_dead_letter_path,
                        &worker_wal,
                    )
                    .await;
                }
                Err(error) => {
                    crate::supervisor_actor::report_critical_failure(
                        "persistence-wal",
                        format!("WAL replay failed — persistence worker cannot start: {error}"),
                    )
                    .await;
                    // Fail-closed: enter a degraded loop that only accepts
                    // shutdown/control messages; all events are rejected.
                    process_degraded_worker_loop(&mut rx, &worker_telemetry).await;
                }
            }
            } else {
                // Instance consistency check failed — enter degraded mode.
                process_degraded_worker_loop(&mut rx, &worker_telemetry).await;
            }
        });

        Arc::new(Self {
            tx,
            send_gate: Mutex::new(()),
            stats,
            telemetry_batcher,
            telemetry_cutover_mode,
            wal,
            suspended: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        })
    }

    /// Check whether the persistence worker is suspended (idle mode).
    pub fn is_suspended(&self) -> bool {
        self.suspended.load(Ordering::Relaxed)
    }

    /// Set the idle-mode hint. Ingestion deliberately remains active.
    pub async fn set_suspended(&self, suspended: bool) {
        self.suspended.store(suspended, Ordering::Release);
        if suspended {
            info!("persistence idle hint enabled; ingestion remains active");
        } else {
            info!("persistence idle hint cleared");
        }
    }

    pub async fn enqueue(&self, event: PersistenceEvent) -> Result<(), PersistenceEvent> {
        let kind = event.kind();
        let simulation = event.is_simulation();
        let summary = event.summary();
        let _send_guard = self.send_gate.lock().await;
        if self.closed.load(Ordering::Acquire) {
            record_dropped(&self.stats, kind, simulation, summary,
                "persistence worker is shutting down".to_string()).await;
            return Err(event);
        }
        // Capacity check: reject early if channel is full, BEFORE WAL admit.
        // This avoids the cancellation window where an event is fsynced to WAL
        // but the async send is cancelled (event stuck in WAL, not in queue).
        // Admit to WAL first. The admission ID will be used to ACK on completion.
        let wal_id = match self.wal.admit(event.clone()).await {
            Ok(id) => id,
            Err(error) => {
                record_dropped(&self.stats, kind, simulation, summary, error).await;
                return Err(event);
            }
        };
        // Enqueue to worker via try_send (synchronous, no cancellation window).
        // If the channel is full or closed, the event stays in WAL and will be
        // replayed on next startup (crash recovery).
        let event_for_err = event.clone();
        match self.tx.try_send(WorkerMessage::Event { wal_id, event }) {
            Ok(()) => {
                record_queued(&self.stats, kind, simulation, summary).await;
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!("persistence worker queue full; event admitted to WAL but not queued (will replay on restart)");
                record_queued(&self.stats, kind, simulation, summary).await;
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                warn!("persistence worker queue closed after WAL admit; event will replay on restart");
                Err(event_for_err)
            }
        }
    }

    /// Drain every event accepted before this control message.
    pub async fn flush(&self, timeout: Duration) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        {
            let _send_guard = self.send_gate.lock().await;
            if self.closed.load(Ordering::Acquire) {
                return Err("persistence worker is shutting down".to_string());
            }
            self.tx
                .send(WorkerMessage::Flush { timeout, reply })
                .await
                .map_err(|_| "persistence worker is closed".to_string())?;
        }
        tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| "persistence flush timed out".to_string())?
            .map_err(|_| "persistence flush acknowledgement was dropped".to_string())??;
        self.wal.compact().await?;
        Ok(())
    }

    /// Drain accepted events, flush telemetry, then stop the worker.
    pub async fn shutdown(&self, timeout: Duration) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        {
            let _send_guard = self.send_gate.lock().await;
            if self.closed.swap(true, Ordering::AcqRel) {
                return Ok(());
            }
            if self
                .tx
                .send(WorkerMessage::Shutdown { timeout, reply })
                .await
                .is_err()
            {
                self.closed.store(false, Ordering::Release);
                return Err("persistence worker is closed".to_string());
            }
        }
        let result = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("persistence shutdown acknowledgement was dropped".to_string()),
            Err(_) => Err("persistence shutdown timed out".to_string()),
        };
        if let Err(error) = result {
            // A failed control operation did not establish that the worker stopped.
            // Re-open admission so an operator can retry flush/shutdown explicitly.
            self.closed.store(false, Ordering::Release);
            return Err(error);
        }
        self.wal.compact().await?;
        Ok(())
    }

    pub async fn stats(&self) -> PersistenceStats {
        let mut stats = self.stats.read().await.clone();
        stats.refresh_derived();
        stats.telemetry = self.telemetry_batcher.stats().await;
        let mode = *self.telemetry_cutover_mode.read().await;
        stats.telemetry_cutover_mode = mode.as_str().to_string();
        stats.telemetry.cutover_mode = mode.as_str().to_string();
        stats
    }

    pub async fn record_telemetry_cutover_observation(
        &self,
        observation: TelemetryCutoverObservation,
    ) {
        record_telemetry_cutover_observation(&self.stats, observation).await;
    }

    pub async fn telemetry_cutover_mode(&self) -> TelemetryCutoverMode {
        *self.telemetry_cutover_mode.read().await
    }

    pub async fn record_runtime_config_snapshot(&self) {
        let stats = self.stats().await;
        let mode = self.telemetry_cutover_mode().await;
        let decision = mode.cutover_decision();
        if let Some(db) = crate::internal_hooks::DB.get().filter(|db| db.is_active()) {
            db.record_runtime_persistence_meta_sync("runtime_v2.persistence_policy", json!({
                "queue_capacity": stats.capacity,
                "queue_health": stats.queue_health,
                "pending_ratio_percent": stats.pending_ratio_percent,
                "dead_letter_path": stats.dead_letter_path,
                "dead_letter_written": stats.dead_letter_written,
                "dead_letter_failed": stats.dead_letter_failed,
                "telemetry_cutover_mode": stats.telemetry_cutover_mode,
                "telemetry_cutover_decision": {
                    "enqueue_worker": decision.enqueue_worker,
                    "write_direct_before_worker_result": decision.write_direct_before_worker_result,
                    "fallback_direct_when_worker_rejects": decision.fallback_direct_when_worker_rejects,
                },
                "telemetry": {
                    "enabled": stats.telemetry.enabled,
                    "dry_run": stats.telemetry.dry_run,
                    "queue_capacity": stats.telemetry.queue_capacity,
                    "max_items_per_batch": stats.telemetry.max_items_per_batch,
                    "flush_interval_ms": stats.telemetry.flush_interval_ms,
                    "schema_version": stats.telemetry.schema_version
                },
                "telemetry_cutover_observation": {
                    "observed_batches": stats.telemetry_cutover.observed_batches,
                    "worker_enqueue_success_ratio_percent": stats.telemetry_cutover.worker_enqueue_success_ratio_percent,
                    "worker_dry_run_success_ratio_percent": stats.telemetry_cutover.worker_dry_run_success_ratio_percent,
                    "readiness": stats.telemetry_cutover.readiness
                },
                "source": "server_config.runtime_v2"
            }));
        }
    }

    pub async fn set_telemetry_cutover_mode(
        &self,
        mode: TelemetryCutoverMode,
    ) -> Result<TelemetryCutoverMode, String> {
        if matches!(mode, TelemetryCutoverMode::WorkerAuthoritative) {
            let telemetry = self.telemetry_batcher.stats().await;
            if !telemetry.enabled {
                return Err(
                    "worker_authoritative requires telemetry_batcher.enabled=true".to_string(),
                );
            }
            if telemetry.dry_run {
                return Err(
                    "worker_authoritative requires telemetry_batcher.dry_run=false".to_string(),
                );
            }
            let database_active = crate::internal_hooks::DB
                .get()
                .is_some_and(|database| database.is_active());
            if !database_active {
                return Err("worker_authoritative requires an active database".to_string());
            }
        }
        {
            let mut current = self.telemetry_cutover_mode.write().await;
            *current = mode;
        }
        let mut stats = self.stats.write().await;
        stats.telemetry_cutover_mode = mode.as_str().to_string();
        stats.telemetry_cutover_changes += 1;
        stats.telemetry.cutover_mode = mode.as_str().to_string();
        stats.last_error = None;
        if let Some(db) = crate::internal_hooks::DB.get().filter(|db| db.is_active()) {
            let decision = mode.cutover_decision();
            db.record_runtime_persistence_meta_sync("telemetry.cutover_mode", json!({
                "mode": mode.as_str(),
                "description": mode.description(),
                "decision": {
                    "enqueue_worker": decision.enqueue_worker,
                    "write_direct_before_worker_result": decision.write_direct_before_worker_result,
                    "fallback_direct_when_worker_rejects": decision.fallback_direct_when_worker_rejects,
                },
                "available_modes": TelemetryCutoverMode::variants().iter().map(|mode| mode.as_str()).collect::<Vec<_>>(),
                "updated_by": "runtime_v2.persistence_worker"
            }));
        }
        Ok(mode)
    }

    pub async fn is_healthy(&self) -> bool {
        self.wal.replay_succeeded() && !self.closed.load(Ordering::Acquire)
    }

    pub async fn telemetry_should_write_direct(&self) -> bool {
        self.telemetry_cutover_mode().await.should_write_direct()
    }

    pub async fn telemetry_should_enqueue_worker(&self) -> bool {
        self.telemetry_cutover_mode().await.should_enqueue_worker()
    }

    pub(crate) async fn record_mirrored_from_event_bus(&self) {
        self.stats.write().await.mirrored_from_event_bus += 1;
    }

    pub(crate) async fn record_skipped_event_bus_event(&self) {
        self.stats.write().await.skipped_event_bus_events += 1;
    }

    pub(crate) async fn record_bridge_lagged(&self, skipped: u64) {
        let mut stats = self.stats.write().await;
        stats.bridge_lagged += skipped;
        stats.last_error = Some(format!(
            "persistence event-bus mirror lagged by {skipped} event(s)"
        ));
    }
}

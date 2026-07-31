//! Single-event persistence pipeline processing and dead-letter preservation.
//!
//! Extracted from `worker.rs` to keep the worker loop focused on message
//! dispatch while the per-event processing logic lives here.

use crate::persistence::message::PersistenceEvent;
use crate::persistence::stats::{
    record_dead_letter_failed, record_dead_letter_written, record_db_dispatch_failure,
    record_db_dispatch_success, record_processed, record_production_persist_request,
    record_production_persist_skipped, record_benchmark_report_persist_request,
    record_benchmark_report_persist_skipped, PersistenceStats,
};
use crate::persistence::wal::PersistenceWal;
use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, RwLock};
use tracing::debug;

static DEAD_LETTER_FAILURE_REPORTED: AtomicBool = AtomicBool::new(false);

async fn report_dead_letter_durability_failure(error: String) {
    if !DEAD_LETTER_FAILURE_REPORTED.swap(true, Ordering::AcqRel) {
        crate::supervisor_actor::report_critical_failure("persistence-dead-letter", error).await;
    }
}

/// Append a JSON record to the dead-letter journal file.
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
        .open(path)
        .await
        .map_err(|error| format!("open {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = file.metadata().await {
            let mode = metadata.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                let mut perms = metadata.permissions();
                perms.set_mode(perms.mode() & !0o077);
                let _ = file.set_permissions(perms).await;
            }
        }
    }
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
    if let Some(parent) = path.parent().filter(|p| p.as_os_str() != "") {
        if let Ok(dir) = tokio::fs::File::open(parent).await {
            dir.sync_all().await.map_err(|error| {
                format!("sync parent directory {}: {error}", parent.display())
            })?;
        }
    }
    Ok(())
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
    let summary = event.summary();
    let Some(payload) = event.dead_letter_payload() else {
        return false;
    };
    let Some(path) = path else {
        let durability_error =
            "dead-letter journal disabled; failed event was not preserved".to_string();
        record_dead_letter_failed(stats, kind, summary, durability_error.clone()).await;
        report_dead_letter_durability_failure(durability_error).await;
        return false;
    };
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let record = json!({
        "schema_version": 1,
        "dead_letter_id": wal_id.to_string(),
        "wal_id": wal_id.to_string(),
        "failed_at_ms": timestamp_ms,
        "stage": stage,
        "kind": kind,
        "summary": summary,
        "error": error,
        "event": payload,
    });
    match append_dead_letter(path, &record).await {
        Ok(()) => {
            record_dead_letter_written(stats, event.kind(), event.summary()).await;
            true
        }
        Err(dead_letter_error) => {
            let durability_error =
                format!("failed to persist dead-letter record: {dead_letter_error}");
            record_dead_letter_failed(
                stats,
                event.kind(),
                event.summary(),
                durability_error.clone(),
            )
            .await;
            report_dead_letter_durability_failure(durability_error).await;
            false
        }
    }
}

/// Process a single event through the persistence pipeline and optionally
/// acknowledge it in the WAL.
///
/// Returns `true` if the event was a Shutdown marker — the caller should
/// break the worker loop.
pub async fn process_event_through_pipeline(
    wal_id: uuid::Uuid,
    event: PersistenceEvent,
    needs_wal_ack: bool,
    worker_stats: &Arc<RwLock<PersistenceStats>>,
    worker_dead_letter_path: &Option<PathBuf>,
    worker_wal: &Arc<PersistenceWal>,
    in_flight: &Arc<Mutex<HashSet<uuid::Uuid>>>,
    pending_acks: &mut std::collections::VecDeque<(uuid::Uuid, u32)>,
) -> bool {
    use crate::persistence::pipeline::{
        persist_benchmark_report_if_needed, persist_production_event_if_needed,
        BenchmarkReportStage, PersistenceWriteStage,
    };

    let kind = event.kind();
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
        BenchmarkReportStage::NotBenchmark => {
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
                PersistenceWriteStage::NotApplicable => {
                    if !matches!(
                        &event,
                        PersistenceEvent::Flush
                            | PersistenceEvent::Shutdown
                    ) {
                        record_production_persist_skipped(worker_stats).await;
                    }
                }
            }
        }
    }

    match &event {
        PersistenceEvent::Shutdown => {
            debug!(kind = %kind, "persistence worker shutdown requested");
            record_processed(worker_stats, kind, summary).await;
            return true;
        }
        PersistenceEvent::Flush => {
            debug!(kind = %kind, "persistence worker flush marker received");
        }
        _ => {}
    }
    record_processed(worker_stats, kind.clone(), summary).await;

    // Only ACK events that reached a durable terminal state AND used WAL.
    if needs_wal_ack {
        if durable {
            if let Err(error) = worker_wal.ack(wal_id).await {
                worker_wal.set_degraded(true);
                crate::supervisor_actor::report_critical_failure("persistence-wal-ack", error).await;
                pending_acks.push_back((wal_id, 0));
            } else {
                worker_wal.set_degraded(false);
                record_wal_committed(worker_stats).await;
                // Remove from in_flight set so the recovery scanner
                // knows this entry no longer needs attention.
                in_flight.lock().await.remove(&wal_id);
            }
        } else {
            tracing::warn!(
                wal_id = %wal_id, kind = %kind,
                "WAL entry not ACKed (non-durable outcome); will replay on restart"
            );
        }
    }

    false
}

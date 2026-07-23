//! Low-overhead PersistenceWorker stats and trace helpers.

use crate::persistence::{PersistencePipeline, PersistenceQueueHealth};
use crate::telemetry::TelemetryBatcherStats;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::RwLock;

pub const MAX_PERSISTENCE_TRACE: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistenceTraceEntry {
    pub seq: u64,
    pub action: String,
    pub kind: String,
    pub simulation: bool,
    pub summary: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistenceLatencyStats {
    pub samples: u64,
    pub total_ms: u64,
    pub avg_ms: u64,
    pub max_ms: u64,
    pub last_ms: u64,
}

impl PersistenceLatencyStats {
    pub fn record(&mut self, elapsed_ms: u64) {
        self.samples += 1;
        self.total_ms = self.total_ms.saturating_add(elapsed_ms);
        self.last_ms = elapsed_ms;
        self.max_ms = self.max_ms.max(elapsed_ms);
        self.avg_ms = if self.samples == 0 {
            0
        } else {
            self.total_ms / self.samples
        };
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TelemetryCutoverObservation {
    pub kind: String,
    pub mode: String,
    pub item_count: usize,
    pub worker_attempted: bool,
    pub worker_enqueued: bool,
    pub worker_enqueue_ms: u64,
    pub direct_attempted: bool,
    pub direct_written: bool,
    pub direct_write_ms: Option<u64>,
    pub fallback_direct: bool,
    /// WorkerPreferred direct persistence failed, but the Worker accepted this
    /// batch as the canonical compensation path rather than as a mirror.
    pub worker_canonical_fallback: bool,
    /// At least one configured persistence path accepted the batch. For the
    /// Worker path this means queue acceptance, not a database commit ACK.
    pub persistence_path_accepted: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TelemetryCutoverStats {
    pub observed_batches: u64,
    pub observed_items: u64,
    pub worker_attempted_batches: u64,
    pub worker_enqueued_batches: u64,
    pub worker_failed_batches: u64,
    pub worker_enqueued_items: u64,
    pub direct_attempted_batches: u64,
    pub direct_written_batches: u64,
    /// Direct persistence was attempted but returned failure/no acknowledgement.
    pub direct_failed_batches: u64,
    /// Direct persistence was not attempted under the selected cutover mode.
    pub direct_skipped_batches: u64,
    pub direct_written_items: u64,
    pub fallback_direct_batches: u64,
    pub worker_canonical_fallback_batches: u64,
    /// Batches accepted by neither the direct backend nor the Worker queue.
    pub unaccepted_batches: u64,
    pub worker_dry_run_success_batches: u64,
    pub worker_dry_run_failed_batches: u64,
    pub worker_enqueue_success_ratio_percent: u8,
    pub worker_dry_run_success_ratio_percent: u8,
    pub readiness: String,
    pub worker_enqueue_latency: PersistenceLatencyStats,
    pub direct_write_latency: PersistenceLatencyStats,
}

impl TelemetryCutoverStats {
    pub fn record(&mut self, observation: &TelemetryCutoverObservation) {
        self.observed_batches += 1;
        self.observed_items = self
            .observed_items
            .saturating_add(observation.item_count as u64);
        if observation.worker_attempted {
            self.worker_attempted_batches += 1;
            self.worker_enqueue_latency
                .record(observation.worker_enqueue_ms);
            if observation.worker_enqueued {
                self.worker_enqueued_batches += 1;
                self.worker_enqueued_items = self
                    .worker_enqueued_items
                    .saturating_add(observation.item_count as u64);
            } else {
                self.worker_failed_batches += 1;
            }
        }
        if observation.direct_attempted {
            self.direct_attempted_batches += 1;
            if observation.direct_written {
                self.direct_written_batches += 1;
                self.direct_written_items = self
                    .direct_written_items
                    .saturating_add(observation.item_count as u64);
                if let Some(elapsed_ms) = observation.direct_write_ms {
                    self.direct_write_latency.record(elapsed_ms);
                }
            } else {
                self.direct_failed_batches += 1;
            }
        } else {
            self.direct_skipped_batches += 1;
        }
        if observation.fallback_direct {
            self.fallback_direct_batches += 1;
        }
        if observation.worker_canonical_fallback {
            self.worker_canonical_fallback_batches += 1;
        }
        if !observation.persistence_path_accepted {
            self.unaccepted_batches += 1;
        }
        // Workers in WorkerPreferred mode both attempt worker and write direct;
        // count those observations toward the dry-run readiness signal.
        if observation.mode == "worker_preferred"
            && observation.worker_attempted
            && observation.direct_attempted
        {
            if observation.worker_enqueued {
                self.worker_dry_run_success_batches += 1;
            } else {
                self.worker_dry_run_failed_batches += 1;
            }
        }
        self.refresh_derived();
    }

    pub fn refresh_derived(&mut self) {
        self.worker_enqueue_success_ratio_percent =
            percent(self.worker_enqueued_batches, self.worker_attempted_batches);
        let dry_total = self
            .worker_dry_run_success_batches
            .saturating_add(self.worker_dry_run_failed_batches);
        self.worker_dry_run_success_ratio_percent =
            percent(self.worker_dry_run_success_batches, dry_total);
        self.readiness = if dry_total == 0 {
            "insufficient_worker_preferred_samples".to_string()
        } else if self.worker_dry_run_failed_batches == 0 {
            "enqueue_path_ready_for_worker_preferred".to_string()
        } else if self.worker_dry_run_success_ratio_percent >= 99 {
            "nearly_ready_but_investigate_enqueue_failures".to_string()
        } else {
            "not_ready_worker_enqueue_failures_present".to_string()
        };
    }
}

fn percent(numerator: u64, denominator: u64) -> u8 {
    if denominator == 0 {
        return 0;
    }
    ((numerator.saturating_mul(100)) / denominator).min(100) as u8
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistenceStats {
    pub capacity: usize,
    pub queued: u64,
    pub processed: u64,
    pub dropped: u64,
    pub pending: u64,
    pub pending_ratio_percent: u8,
    pub queue_health: String,
    pub backpressure_advice: String,
    pub simulation_persist_requests: u64,
    pub production_persist_requests: u64,
    pub production_persist_skipped: u64,
    pub production_telemetry_staged: u64,
    pub production_telemetry_stage_failed: u64,
    /// Events preserved in the local JSONL dead-letter journal after the
    /// configured database retry budget was exhausted.
    pub dead_letter_path: Option<String>,
    pub dead_letter_written: u64,
    /// Dead-letter writes that also failed. A non-zero value is a durability
    /// incident and should mark the service degraded operationally.
    pub dead_letter_failed: u64,
    pub telemetry_cutover: TelemetryCutoverStats,
    pub db_dispatch: BTreeMap<String, PersistenceLatencyStats>,
    pub db_dispatch_failures: BTreeMap<String, u64>,
    pub benchmark_report_persist_requests: u64,
    pub benchmark_report_persist_skipped: u64,
    pub telemetry_cutover_mode: String,
    pub telemetry_cutover_changes: u64,
    pub by_kind: BTreeMap<String, u64>,
    pub recent: Vec<PersistenceTraceEntry>,
    pub telemetry: TelemetryBatcherStats,
    pub last_error: Option<String>,
}

impl PersistenceStats {
    pub fn refresh_derived(&mut self) {
        self.pending = self.queued.saturating_sub(self.processed);
        let capacity = self.capacity.max(1) as u64;
        self.pending_ratio_percent = ((self.pending.saturating_mul(100)) / capacity).min(100) as u8;
        let health = PersistenceQueueHealth::from_counts(self.pending, self.capacity, self.dropped);
        self.queue_health = health.as_str().to_string();
        self.backpressure_advice = match health {
            PersistenceQueueHealth::Idle => "idle; no persistence backlog".to_string(),
            PersistenceQueueHealth::Healthy => "healthy; keep observing worker latency".to_string(),
            PersistenceQueueHealth::Backlogged => {
                "backlogged; inspect DB dispatch latency before increasing queue capacity"
                    .to_string()
            }
            PersistenceQueueHealth::Dropping => {
                "dropping; fix downstream persistence pressure before adding features".to_string()
            }
        };
        self.telemetry_cutover.refresh_derived();
    }
}

pub async fn record_simulation_persist_request(stats: &Arc<RwLock<PersistenceStats>>) {
    stats.write().await.simulation_persist_requests += 1;
}

pub async fn record_production_persist_request(stats: &Arc<RwLock<PersistenceStats>>) {
    stats.write().await.production_persist_requests += 1;
}

pub async fn record_production_persist_skipped(stats: &Arc<RwLock<PersistenceStats>>) {
    stats.write().await.production_persist_skipped += 1;
}

pub async fn record_production_telemetry_staged(stats: &Arc<RwLock<PersistenceStats>>) {
    stats.write().await.production_telemetry_staged += 1;
}

pub async fn record_benchmark_report_persist_request(stats: &Arc<RwLock<PersistenceStats>>) {
    stats.write().await.benchmark_report_persist_requests += 1;
}

pub async fn record_benchmark_report_persist_skipped(stats: &Arc<RwLock<PersistenceStats>>) {
    stats.write().await.benchmark_report_persist_skipped += 1;
}

pub async fn record_production_telemetry_stage_failed(
    stats: &Arc<RwLock<PersistenceStats>>,
    error: String,
) {
    let mut stats = stats.write().await;
    stats.production_telemetry_stage_failed += 1;
    stats.last_error = Some(error);
}

pub async fn record_dead_letter_written(
    stats: &Arc<RwLock<PersistenceStats>>,
    kind: String,
    simulation: bool,
    summary: String,
) {
    let mut stats = stats.write().await;
    stats.dead_letter_written += 1;
    push_trace(&mut stats, "dead_letter", kind, simulation, summary);
}

pub async fn record_dead_letter_failed(
    stats: &Arc<RwLock<PersistenceStats>>,
    kind: String,
    simulation: bool,
    summary: String,
    error: String,
) {
    let mut stats = stats.write().await;
    stats.dead_letter_failed += 1;
    stats.last_error = Some(error);
    push_trace(&mut stats, "dead_letter_failed", kind, simulation, summary);
}

pub async fn record_telemetry_cutover_observation(
    stats: &Arc<RwLock<PersistenceStats>>,
    observation: TelemetryCutoverObservation,
) {
    let mut stats = stats.write().await;
    stats.telemetry_cutover.record(&observation);
    push_trace(
        &mut stats,
        "telemetry_cutover",
        format!("telemetry.{}", observation.kind),
        false,
        format!(
            "mode={} items={} worker={}/{} worker_ms={} direct={}/{} direct_ms={} fallback_direct={} worker_canonical_fallback={} path_accepted={}",
            observation.mode,
            observation.item_count,
            observation.worker_attempted,
            observation.worker_enqueued,
            observation.worker_enqueue_ms,
            observation.direct_attempted,
            observation.direct_written,
            observation.direct_write_ms.unwrap_or(0),
            observation.fallback_direct,
            observation.worker_canonical_fallback,
            observation.persistence_path_accepted,
        ),
    );
}

pub async fn record_db_dispatch_success(
    stats: &Arc<RwLock<PersistenceStats>>,
    pipeline: PersistencePipeline,
    elapsed_ms: u64,
) {
    let mut stats = stats.write().await;
    stats
        .db_dispatch
        .entry(pipeline.as_str().to_string())
        .or_default()
        .record(elapsed_ms);
}

pub async fn record_db_dispatch_failure(
    stats: &Arc<RwLock<PersistenceStats>>,
    pipeline: PersistencePipeline,
    elapsed_ms: u64,
    error: String,
) {
    let mut stats = stats.write().await;
    stats
        .db_dispatch
        .entry(pipeline.as_str().to_string())
        .or_default()
        .record(elapsed_ms);
    *stats
        .db_dispatch_failures
        .entry(pipeline.as_str().to_string())
        .or_insert(0) += 1;
    stats.last_error = Some(error);
}

pub async fn record_queued(
    stats: &Arc<RwLock<PersistenceStats>>,
    kind: String,
    simulation: bool,
    summary: String,
) {
    let mut stats = stats.write().await;
    stats.queued += 1;
    *stats.by_kind.entry(kind.clone()).or_insert(0) += 1;
    push_trace(&mut stats, "queued", kind, simulation, summary);
}

pub async fn record_processed(
    stats: &Arc<RwLock<PersistenceStats>>,
    kind: String,
    simulation: bool,
    summary: String,
) {
    let mut stats = stats.write().await;
    stats.processed += 1;
    push_trace(&mut stats, "processed", kind, simulation, summary);
}

pub async fn record_dropped(
    stats: &Arc<RwLock<PersistenceStats>>,
    kind: String,
    simulation: bool,
    summary: String,
    error: String,
) {
    let mut stats = stats.write().await;
    stats.dropped += 1;
    stats.last_error = Some(error);
    push_trace(&mut stats, "dropped", kind, simulation, summary);
}

fn push_trace(
    stats: &mut PersistenceStats,
    action: impl Into<String>,
    kind: String,
    simulation: bool,
    summary: String,
) {
    let seq = stats.queued + stats.processed + stats.dropped;
    if stats.recent.len() >= MAX_PERSISTENCE_TRACE {
        let overflow = stats.recent.len() + 1 - MAX_PERSISTENCE_TRACE;
        stats.recent.drain(0..overflow);
    }
    stats.recent.push(PersistenceTraceEntry {
        seq,
        action: action.into(),
        kind,
        simulation,
        summary,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_stats_report_backpressure_without_throttling() {
        let mut stats = PersistenceStats {
            capacity: 100,
            queued: 80,
            processed: 0,
            dropped: 0,
            ..PersistenceStats::default()
        };
        stats.refresh_derived();
        assert_eq!(stats.pending, 80);
        assert_eq!(stats.pending_ratio_percent, 80);
        assert_eq!(stats.queue_health, "backlogged");
        assert!(stats.backpressure_advice.contains("DB dispatch latency"));
    }

    #[test]
    fn telemetry_cutover_readiness_uses_worker_preferred_enqueue_observations() {
        let mut cutover = TelemetryCutoverStats::default();
        cutover.record(&TelemetryCutoverObservation {
            kind: "touch".to_string(),
            mode: "worker_preferred".to_string(),
            item_count: 8,
            worker_attempted: true,
            worker_enqueued: true,
            worker_enqueue_ms: 2,
            direct_attempted: true,
            direct_written: true,
            direct_write_ms: Some(7),
            fallback_direct: false,
            worker_canonical_fallback: false,
            persistence_path_accepted: true,
        });
        assert_eq!(cutover.worker_enqueue_success_ratio_percent, 100);
        assert_eq!(cutover.worker_dry_run_success_ratio_percent, 100);
        assert_eq!(cutover.readiness, "enqueue_path_ready_for_worker_preferred");
        assert_eq!(cutover.worker_enqueue_latency.last_ms, 2);
        assert_eq!(cutover.direct_write_latency.last_ms, 7);
    }

    #[test]
    fn telemetry_cutover_tracks_worker_canonical_fallback_and_total_failure() {
        let mut cutover = TelemetryCutoverStats::default();
        cutover.record(&TelemetryCutoverObservation {
            kind: "judge".to_string(),
            mode: "worker_preferred".to_string(),
            item_count: 3,
            worker_attempted: true,
            worker_enqueued: true,
            worker_enqueue_ms: 1,
            direct_attempted: true,
            direct_written: false,
            direct_write_ms: Some(5),
            fallback_direct: false,
            worker_canonical_fallback: true,
            persistence_path_accepted: true,
        });
        cutover.record(&TelemetryCutoverObservation {
            kind: "touch".to_string(),
            mode: "direct_only".to_string(),
            item_count: 2,
            worker_attempted: false,
            worker_enqueued: false,
            worker_enqueue_ms: 0,
            direct_attempted: true,
            direct_written: false,
            direct_write_ms: Some(4),
            fallback_direct: false,
            worker_canonical_fallback: false,
            persistence_path_accepted: false,
        });
        assert_eq!(cutover.worker_canonical_fallback_batches, 1);
        assert_eq!(cutover.direct_failed_batches, 2);
        assert_eq!(cutover.direct_skipped_batches, 0);
        assert_eq!(cutover.unaccepted_batches, 1);
    }

    #[test]
    fn latency_stats_keep_constant_size_aggregates() {
        let mut latency = PersistenceLatencyStats::default();
        latency.record(4);
        latency.record(8);
        assert_eq!(latency.samples, 2);
        assert_eq!(latency.avg_ms, 6);
        assert_eq!(latency.max_ms, 8);
        assert_eq!(latency.last_ms, 8);
    }
}

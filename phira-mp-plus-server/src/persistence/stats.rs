//! Low-overhead PersistenceWorker stats and trace helpers.

use crate::persistence::{PersistencePipeline, PersistenceQueueHealth};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::RwLock;

pub const MAX_PERSISTENCE_TRACE: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistenceTraceEntry {
    pub seq: u64,
    pub action: String,
    pub kind: String,
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
pub struct PersistenceStats {
    pub capacity: usize,
    pub queued: u64,
    pub processed: u64,
    pub dropped: u64,
    pub pending: u64,
    pub pending_ratio_percent: u8,
    pub queue_health: String,
    pub backpressure_advice: String,
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
    pub db_dispatch: BTreeMap<String, PersistenceLatencyStats>,
    pub db_dispatch_failures: BTreeMap<String, u64>,
    pub benchmark_report_persist_requests: u64,
    pub benchmark_report_persist_skipped: u64,
    pub by_kind: BTreeMap<String, u64>,
    pub recent: Vec<PersistenceTraceEntry>,
    pub last_error: Option<String>,

    // ── WAL-specific metrics ────────────────────────────────────────────
    #[serde(default)]
    pub wal_received: u64,
    #[serde(default)]
    pub wal_committed: u64,
    /// Events admitted to WAL only (queue full). Recovered by periodic scanner.
    #[serde(default)]
    pub wal_only: u64,
>>>>>>> 8f0637a (fix: enqueue returns AdmissionOutcome instead of Err on queue-full after WAL admit)
    #[serde(default)]
    pub wal_compactions: u64,
    /// Total bytes processed during WAL compactions.
    #[serde(default)]
    pub wal_bytes: u64,
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
    }

    // ── WAL helpers ────────────────────────────────────────────────────

    pub fn record_wal_received(&mut self) {
        self.wal_received += 1;
    }

    pub fn record_wal_committed(&mut self) {
        self.wal_committed += 1;
    }

    pub fn record_wal_compaction(&mut self, bytes: u64) {
        self.wal_compactions += 1;
        self.wal_bytes += bytes;
    }

}

pub async fn record_production_persist_request(stats: &Arc<RwLock<PersistenceStats>>) {
    stats.write().await.production_persist_requests += 1;
}

pub async fn record_production_persist_skipped(stats: &Arc<RwLock<PersistenceStats>>) {
    stats.write().await.production_persist_skipped += 1;
}

pub async fn record_benchmark_report_persist_request(stats: &Arc<RwLock<PersistenceStats>>) {
    stats.write().await.benchmark_report_persist_requests += 1;
}

pub async fn record_benchmark_report_persist_skipped(stats: &Arc<RwLock<PersistenceStats>>) {
    stats.write().await.benchmark_report_persist_skipped += 1;
}

pub async fn record_dead_letter_written(
    stats: &Arc<RwLock<PersistenceStats>>,
    kind: String,
    summary: String,
) {
    let mut stats = stats.write().await;
    stats.dead_letter_written += 1;
    push_trace(&mut stats, "dead_letter", kind, summary);
}

pub async fn record_dead_letter_failed(
    stats: &Arc<RwLock<PersistenceStats>>,
    kind: String,
    summary: String,
    error: String,
) {
    let mut stats = stats.write().await;
    stats.dead_letter_failed += 1;
    stats.last_error = Some(error);
    push_trace(&mut stats, "dead_letter_failed", kind, summary);
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
    summary: String,
) {
    let mut stats = stats.write().await;
    stats.queued += 1;
    *stats.by_kind.entry(kind.clone()).or_insert(0) += 1;
    push_trace(&mut stats, "queued", kind, summary);
}

pub async fn record_processed(
    stats: &Arc<RwLock<PersistenceStats>>,
    kind: String,
    summary: String,
) {
    let mut stats = stats.write().await;
    stats.processed += 1;
    push_trace(&mut stats, "processed", kind, summary);
}

pub async fn record_dropped(
    stats: &Arc<RwLock<PersistenceStats>>,
    kind: String,
    summary: String,
    error: String,
) {
    let mut stats = stats.write().await;
    stats.dropped += 1;
    stats.last_error = Some(error);
    push_trace(&mut stats, "dropped", kind, summary);
}

// ── Async WAL recording helpers ──────────────────────────────────────

pub async fn record_wal_received(stats: &Arc<RwLock<PersistenceStats>>) {
    stats.write().await.wal_received += 1;
}

pub async fn record_wal_committed(stats: &Arc<RwLock<PersistenceStats>>) {
    stats.write().await.wal_committed += 1;
}

pub async fn record_wal_only(stats: &Arc<RwLock<PersistenceStats>>, kind: String, summary: String) {
    let mut s = stats.write().await;
    s.wal_only += 1;
    push_trace(&mut s, "wal_only", kind, summary);
}

>>>>>>> 8f0637a (fix: enqueue returns AdmissionOutcome instead of Err on queue-full after WAL admit)
pub async fn record_wal_compaction(
    stats: &Arc<RwLock<PersistenceStats>>,
    bytes: u64,
) {
    let mut s = stats.write().await;
    s.wal_compactions += 1;
    s.wal_bytes += bytes;
}

fn push_trace(
    stats: &mut PersistenceStats,
    action: impl Into<String>,
    kind: String,
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

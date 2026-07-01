//! Low-overhead PersistenceWorker stats and trace helpers.

use crate::persistence::PersistenceQueueHealth;
use crate::telemetry_batcher::TelemetryBatcherStats;
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
pub struct PersistenceStats {
    pub capacity: usize,
    pub queued: u64,
    pub processed: u64,
    pub dropped: u64,
    pub pending: u64,
    pub pending_ratio_percent: u8,
    pub queue_health: String,
    pub backpressure_advice: String,
    pub mirrored_from_event_bus: u64,
    pub skipped_event_bus_events: u64,
    pub bridge_lagged: u64,
    pub simulation_persist_requests: u64,
    pub production_persist_requests: u64,
    pub production_persist_skipped: u64,
    pub production_telemetry_staged: u64,
    pub production_telemetry_stage_failed: u64,
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
            PersistenceQueueHealth::Backlogged => "backlogged; inspect DB latency before increasing queue capacity".to_string(),
            PersistenceQueueHealth::Dropping => "dropping; fix downstream persistence pressure before adding features".to_string(),
        };
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
        assert!(stats.backpressure_advice.contains("DB latency"));
    }
}

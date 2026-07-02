//! Runtime v2 PersistenceWorker facade and queue runtime.

use crate::persistence::message::PersistenceEvent;
use crate::persistence::pipeline::{
    persist_benchmark_report_if_needed, persist_production_event_if_needed,
    persist_simulation_event_if_needed, stage_production_telemetry_if_needed, BenchmarkReportStage,
    PersistenceWriteStage, ProductionTelemetryStage,
};
use crate::persistence::stats::{
    record_benchmark_report_persist_request, record_benchmark_report_persist_skipped,
    record_db_dispatch_failure, record_db_dispatch_skipped_no_database, record_db_dispatch_success,
    record_dropped, record_processed, record_production_persist_request,
    record_production_persist_skipped, record_production_telemetry_stage_failed,
    record_production_telemetry_staged, record_queued, record_simulation_persist_request,
    record_telemetry_cutover_observation, PersistenceStats, TelemetryCutoverObservation,
};
use crate::telemetry_batcher::{
    TelemetryBatcher, TelemetryBatcherPolicy, TelemetryBatcherStats, TelemetryCutoverMode,
};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, trace, warn};

#[derive(Debug)]
pub struct PersistenceWorker {
    tx: mpsc::Sender<PersistenceEvent>,
    stats: Arc<RwLock<PersistenceStats>>,
    telemetry_batcher: Arc<TelemetryBatcher>,
    telemetry_cutover_mode: Arc<RwLock<TelemetryCutoverMode>>,
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
        let capacity = queue_capacity.max(16);
        let (tx, mut rx) = mpsc::channel::<PersistenceEvent>(capacity);
        let initial_cutover = telemetry_cutover_mode;
        let telemetry_batcher = TelemetryBatcher::spawn(telemetry_policy.clone());
        let telemetry_cutover_mode = Arc::new(RwLock::new(initial_cutover));
        let stats = Arc::new(RwLock::new(PersistenceStats {
            capacity,
            telemetry_cutover_mode: initial_cutover.as_str().to_string(),
            telemetry: TelemetryBatcherStats::from_policy(&telemetry_policy),
            ..PersistenceStats::default()
        }));
        let worker_stats = Arc::clone(&stats);
        let worker_telemetry = Arc::clone(&telemetry_batcher);

        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let kind = event.kind();
                let simulation = event.is_simulation();
                let summary = event.summary();
                match persist_benchmark_report_if_needed(&event).await {
                    BenchmarkReportStage::Acknowledged { elapsed_ms } => {
                        record_benchmark_report_persist_request(&worker_stats).await;
                        record_db_dispatch_success(
                            &worker_stats,
                            crate::persistence::PersistencePipeline::BenchmarkReport,
                            elapsed_ms,
                        )
                        .await;
                    }
                    BenchmarkReportStage::Failed { elapsed_ms, error } => {
                        record_benchmark_report_persist_skipped(&worker_stats).await;
                        record_db_dispatch_failure(
                            &worker_stats,
                            crate::persistence::PersistencePipeline::BenchmarkReport,
                            elapsed_ms,
                            error,
                        )
                        .await;
                    }
                    BenchmarkReportStage::SkippedNoDatabase => {
                        record_benchmark_report_persist_skipped(&worker_stats).await;
                        record_db_dispatch_skipped_no_database(
                            &worker_stats,
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
                                record_simulation_persist_request(&worker_stats).await;
                                record_db_dispatch_success(&worker_stats, pipeline, elapsed_ms)
                                    .await;
                            }
                            PersistenceWriteStage::Failed {
                                pipeline,
                                elapsed_ms,
                                error,
                            } => {
                                record_simulation_persist_request(&worker_stats).await;
                                record_db_dispatch_failure(
                                    &worker_stats,
                                    pipeline,
                                    elapsed_ms,
                                    error,
                                )
                                .await;
                            }
                            PersistenceWriteStage::SkippedNoDatabase { pipeline } => {
                                record_db_dispatch_skipped_no_database(&worker_stats, pipeline)
                                    .await;
                            }
                            PersistenceWriteStage::NotApplicable => {
                                match stage_production_telemetry_if_needed(
                                    &event,
                                    &worker_telemetry,
                                )
                                .await
                                {
                                    ProductionTelemetryStage::Staged => {
                                        record_production_telemetry_staged(&worker_stats).await;
                                    }
                                    ProductionTelemetryStage::Failed(error) => {
                                        record_production_telemetry_stage_failed(
                                            &worker_stats,
                                            error,
                                        )
                                        .await;
                                    }
                                    ProductionTelemetryStage::NotTelemetry => {
                                        match persist_production_event_if_needed(&event).await {
                                            PersistenceWriteStage::Acknowledged {
                                                pipeline,
                                                elapsed_ms,
                                            } => {
                                                record_production_persist_request(&worker_stats)
                                                    .await;
                                                record_db_dispatch_success(
                                                    &worker_stats,
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
                                                record_production_persist_request(&worker_stats)
                                                    .await;
                                                record_db_dispatch_failure(
                                                    &worker_stats,
                                                    pipeline,
                                                    elapsed_ms,
                                                    error,
                                                )
                                                .await;
                                            }
                                            PersistenceWriteStage::SkippedNoDatabase {
                                                pipeline,
                                            } => {
                                                record_db_dispatch_skipped_no_database(
                                                    &worker_stats,
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
                                                    record_production_persist_skipped(
                                                        &worker_stats,
                                                    )
                                                    .await;
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
                        record_processed(&worker_stats, kind, simulation, summary).await;
                        break;
                    }
                    PersistenceEvent::Flush => {
                        debug!(kind = %kind, "persistence worker flush marker received");
                    }
                    other => {
                        trace!(?other, "persistence worker consumed mirrored event");
                    }
                }
                record_processed(&worker_stats, kind, simulation, summary).await;
            }
        });

        Arc::new(Self {
            tx,
            stats,
            telemetry_batcher,
            telemetry_cutover_mode,
        })
    }

    pub async fn enqueue(&self, event: PersistenceEvent) -> Result<(), PersistenceEvent> {
        let kind = event.kind();
        let simulation = event.is_simulation();
        let summary = event.summary();
        match self.tx.try_send(event) {
            Ok(()) => {
                record_queued(&self.stats, kind, simulation, summary).await;
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(event)) => {
                record_dropped(
                    &self.stats,
                    kind,
                    simulation,
                    summary,
                    "persistence worker queue is full".to_string(),
                )
                .await;
                warn!("persistence worker queue is full; event dropped");
                Err(event)
            }
            Err(mpsc::error::TrySendError::Closed(event)) => {
                record_dropped(
                    &self.stats,
                    kind,
                    simulation,
                    summary,
                    "persistence worker queue is closed".to_string(),
                )
                .await;
                warn!("persistence worker queue is closed; event dropped");
                Err(event)
            }
        }
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
        if let Some(db) = crate::internal_hooks::DB.get() {
            db.record_runtime_persistence_meta_sync("runtime_v2.persistence_policy", json!({
                "queue_capacity": stats.capacity,
                "queue_health": stats.queue_health,
                "pending_ratio_percent": stats.pending_ratio_percent,
                "telemetry_cutover_mode": stats.telemetry_cutover_mode,
                "telemetry_cutover_decision": {
                    "enqueue_worker": decision.enqueue_worker,
                    "write_direct_before_worker_result": decision.write_direct_before_worker_result,
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
    ) -> TelemetryCutoverMode {
        {
            let mut current = self.telemetry_cutover_mode.write().await;
            *current = mode;
        }
        let mut stats = self.stats.write().await;
        stats.telemetry_cutover_mode = mode.as_str().to_string();
        stats.telemetry_cutover_changes += 1;
        stats.telemetry.cutover_mode = mode.as_str().to_string();
        stats.last_error = None;
        if let Some(db) = crate::internal_hooks::DB.get() {
            let decision = mode.cutover_decision();
            db.record_runtime_persistence_meta_sync("telemetry.cutover_mode", json!({
                "mode": mode.as_str(),
                "description": mode.description(),
                "decision": {
                    "enqueue_worker": decision.enqueue_worker,
                    "write_direct_before_worker_result": decision.write_direct_before_worker_result,
                },
                "available_modes": TelemetryCutoverMode::variants().iter().map(|mode| mode.as_str()).collect::<Vec<_>>(),
                "updated_by": "runtime_v2.persistence_worker"
            }));
        }
        mode
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

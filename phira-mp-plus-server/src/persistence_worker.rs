//! Runtime v2 persistence worker skeleton.
//!
//! The existing `db.rs` direct write paths are still active.  This worker is a
//! bounded queue and stats holder for gradually migrating high-frequency writes
//! to batched background persistence without changing current database behavior.
//!
//! Step 5 wires a low-risk EventBus mirror into this worker. Step 20 starts
//! dual-writing low-frequency production events into `mp_events` while keeping
//! existing `db.rs` direct writes as the source of truth. High-frequency
//! production Touch/Judge batches now enter the guarded TelemetryBatcher batch-write path while the direct RoundStore/db.rs path remains available for comparison. Step 23 enriches that path with a Runtime v2 batch/item telemetry schema so future replay, analysis and cutover can use structured data rather than ad-hoc JSON-only rows.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{debug, trace, warn};

use crate::telemetry_batcher::{TelemetryBatcher, TelemetryBatcherPolicy, TelemetryBatcherStats, TelemetryCutoverMode, TelemetryItem, TelemetryKind};

const MAX_PERSISTENCE_TRACE: usize = 128;

#[derive(Debug, Clone)]
pub enum PersistenceEvent {
    RoomSnapshot { room_id: String, payload: Value, simulation: bool },
    ServerEvent { kind: String, payload: Value, simulation: bool },
    TouchBatch { round_id: String, user_id: i32, payload: Value, simulation: bool },
    JudgeBatch { round_id: String, user_id: i32, payload: Value, simulation: bool },
    Flush,
    Shutdown,
}

impl PersistenceEvent {
    pub fn kind(&self) -> String {
        match self {
            Self::RoomSnapshot { .. } => "room_snapshot".to_string(),
            Self::ServerEvent { kind, .. } => kind.clone(),
            Self::TouchBatch { .. } => "touch_batch".to_string(),
            Self::JudgeBatch { .. } => "judge_batch".to_string(),
            Self::Flush => "flush".to_string(),
            Self::Shutdown => "shutdown".to_string(),
        }
    }

    pub fn is_simulation(&self) -> bool {
        match self {
            Self::RoomSnapshot { simulation, .. }
            | Self::ServerEvent { simulation, .. }
            | Self::TouchBatch { simulation, .. }
            | Self::JudgeBatch { simulation, .. } => *simulation,
            Self::Flush | Self::Shutdown => false,
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::RoomSnapshot { room_id, simulation, .. } => {
                format!("room_id={room_id} simulation={simulation}")
            }
            Self::ServerEvent { kind, simulation, .. } => {
                format!("kind={kind} simulation={simulation}")
            }
            Self::TouchBatch { round_id, user_id, simulation, .. } => {
                format!("round_id={round_id} user_id={user_id} simulation={simulation}")
            }
            Self::JudgeBatch { round_id, user_id, simulation, .. } => {
                format!("round_id={round_id} user_id={user_id} simulation={simulation}")
            }
            Self::Flush => "flush".to_string(),
            Self::Shutdown => "shutdown".to_string(),
        }
    }
}

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
    pub mirrored_from_event_bus: u64,
    pub skipped_event_bus_events: u64,
    pub bridge_lagged: u64,
    pub simulation_persist_requests: u64,
    pub production_persist_requests: u64,
    pub production_persist_skipped: u64,
    pub production_telemetry_staged: u64,
    pub production_telemetry_stage_failed: u64,
    pub telemetry_cutover_mode: String,
    pub telemetry_cutover_changes: u64,
    pub by_kind: BTreeMap<String, u64>,
    pub recent: Vec<PersistenceTraceEntry>,
    pub telemetry: TelemetryBatcherStats,
    pub last_error: Option<String>,
}

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
                if persist_simulation_event_if_needed(&event) {
                    record_simulation_persist_request(&worker_stats).await;
                } else {
                    match stage_production_telemetry_if_needed(&event, &worker_telemetry).await {
                        ProductionTelemetryStage::Staged => {
                            record_production_telemetry_staged(&worker_stats).await;
                        }
                        ProductionTelemetryStage::Failed(error) => {
                            record_production_telemetry_stage_failed(&worker_stats, error).await;
                        }
                        ProductionTelemetryStage::NotTelemetry => {
                            if persist_production_event_if_needed(&event) {
                                record_production_persist_request(&worker_stats).await;
                            } else if !event.is_simulation()
                                && !matches!(&event, PersistenceEvent::Flush | PersistenceEvent::Shutdown)
                            {
                                record_production_persist_skipped(&worker_stats).await;
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

        Arc::new(Self { tx, stats, telemetry_batcher, telemetry_cutover_mode })
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
        stats.pending = stats.queued.saturating_sub(stats.processed);
        stats.telemetry = self.telemetry_batcher.stats().await;
        let mode = *self.telemetry_cutover_mode.read().await;
        stats.telemetry_cutover_mode = mode.as_str().to_string();
        stats.telemetry.cutover_mode = mode.as_str().to_string();
        stats
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
                "telemetry_cutover_mode": stats.telemetry_cutover_mode,
                "telemetry_cutover_decision": {
                    "enqueue_worker": decision.enqueue_worker,
                    "write_direct_before_worker_result": decision.write_direct_before_worker_result,
                    "fallback_to_direct_on_enqueue_failure": decision.fallback_to_direct_on_enqueue_failure
                },
                "telemetry": {
                    "enabled": stats.telemetry.enabled,
                    "dry_run": stats.telemetry.dry_run,
                    "queue_capacity": stats.telemetry.queue_capacity,
                    "max_items_per_batch": stats.telemetry.max_items_per_batch,
                    "flush_interval_ms": stats.telemetry.flush_interval_ms,
                    "schema_version": stats.telemetry.schema_version
                },
                "source": "server_config.runtime_v2"
            }));
        }
    }

    pub async fn set_telemetry_cutover_mode(&self, mode: TelemetryCutoverMode) -> TelemetryCutoverMode {
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
                    "fallback_to_direct_on_enqueue_failure": decision.fallback_to_direct_on_enqueue_failure
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

    async fn record_mirrored_from_event_bus(&self) {
        self.stats.write().await.mirrored_from_event_bus += 1;
    }

    async fn record_skipped_event_bus_event(&self) {
        self.stats.write().await.skipped_event_bus_events += 1;
    }

    async fn record_bridge_lagged(&self, skipped: u64) {
        let mut stats = self.stats.write().await;
        stats.bridge_lagged += skipped;
        stats.last_error = Some(format!("persistence event-bus mirror lagged by {skipped} event(s)"));
    }
}

pub fn spawn_event_bus_mirror(
    event_bus: Arc<crate::event_bus::EventBus>,
    worker: Arc<PersistenceWorker>,
) {
    let mut rx = event_bus.subscribe();
    let _handle = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Some(persistence_event) = mirror_event_bus_event(&event) {
                        worker.record_mirrored_from_event_bus().await;
                        let _ = worker.enqueue(persistence_event).await;
                    } else {
                        worker.record_skipped_event_bus_event().await;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    worker.record_bridge_lagged(skipped).await;
                    warn!(skipped, "persistence worker event-bus mirror lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn mirror_event_bus_event(event: &crate::event_bus::MpEvent) -> Option<PersistenceEvent> {
    use crate::event_bus::MpEvent;

    match event {
        MpEvent::UserConnected { user_id } => server_event(
            event.kind(),
            json!({ "user_id": user_id }),
            false,
        ),
        MpEvent::UserDisconnected { user_id } => server_event(
            event.kind(),
            json!({ "user_id": user_id }),
            false,
        ),
        MpEvent::RoomCreated { room_id, room_uuid } => Some(PersistenceEvent::RoomSnapshot {
            room_id: room_id.to_string(),
            payload: json!({
                "event": event.kind(),
                "room_id": room_id.to_string(),
                "room_uuid": room_uuid.to_string(),
            }),
            simulation: false,
        }),
        MpEvent::RoomJoined { room_id, user_id } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "user_id": user_id }),
            false,
        ),
        MpEvent::RoomLeft { room_id, user_id } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "user_id": user_id }),
            false,
        ),
        MpEvent::RoomUpdated { room_id } => Some(PersistenceEvent::RoomSnapshot {
            room_id: room_id.to_string(),
            payload: json!({ "event": event.kind(), "room_id": room_id.to_string() }),
            simulation: false,
        }),
        MpEvent::RoomLocked { room_id, locked } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "locked": locked }),
            false,
        ),
        MpEvent::RoomCycled { room_id, cycle } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "cycle": cycle }),
            false,
        ),
        MpEvent::RoomStateChanged { room_id, state } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "state": state }),
            false,
        ),
        MpEvent::HostChanged { room_id, host } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "host": host }),
            false,
        ),
        MpEvent::ChartSelected { room_id, chart_id } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "chart_id": chart_id }),
            false,
        ),
        MpEvent::GameStarted { room_id, round_id } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "round_id": round_id }),
            false,
        ),
        MpEvent::PlayerReadyChanged { room_id, user_id, ready } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "user_id": user_id, "ready": ready }),
            false,
        ),
        // Production Touch/Judge persistence is now staged from `session.rs`
        // with the actual payload and round id.  Keep EventBus count-only
        // events for runtime stats, but do not persist them or they would
        // duplicate the richer session-direct telemetry item.
        MpEvent::TouchesReceived { .. } | MpEvent::JudgesReceived { .. } => None,
        MpEvent::RoundCompleted { room_id, round_id } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "round_id": round_id }),
            false,
        ),
        MpEvent::ChatMessage { room_id, user_id } => server_event(
            event.kind(),
            json!({ "room_id": room_id.as_ref().map(|id| id.to_string()), "user_id": user_id }),
            false,
        ),
        MpEvent::AdminCommandExecuted { user_id, command } => server_event(
            event.kind(),
            json!({ "user_id": user_id, "command": command }),
            false,
        ),
        MpEvent::SimulationStarted { run_id } => server_event(
            event.kind(),
            json!({ "run_id": run_id.to_string() }),
            true,
        ),
        MpEvent::SimulationStopped { run_id, reason } => server_event(
            event.kind(),
            json!({ "run_id": run_id.to_string(), "reason": reason }),
            true,
        ),
        // Avoid recursive noise once the worker later publishes successful writes.
        MpEvent::PersistenceWritten { .. } => None,
        MpEvent::BenchmarkCompleted { report } => server_event(
            event.kind(),
            serde_json::to_value(report).unwrap_or_else(|err| json!({"serialize_error": err.to_string()})),
            report.mode == crate::benchmark_report::BenchmarkMode::Simulation,
        ),
        MpEvent::Custom { kind, payload } if kind.starts_with("simulation.") => {
            simulation_custom_event(kind, payload)
        }
        MpEvent::Custom { kind, payload } if kind == "room.command" || kind.starts_with("room.command.") => {
            server_event(kind, payload.clone(), false)
        }
        MpEvent::Custom { .. } => None,
    }
}

fn simulation_custom_event(kind: &str, payload: &Value) -> Option<PersistenceEvent> {
    match kind {
        "simulation.touch" => Some(PersistenceEvent::TouchBatch {
            round_id: payload
                .get("sample_round_id")
                .and_then(Value::as_str)
                .unwrap_or("simulation-touch")
                .to_string(),
            user_id: payload
                .get("sample_user_id")
                .and_then(Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
                .unwrap_or(0),
            payload: payload.clone(),
            simulation: true,
        }),
        "simulation.judge" => Some(PersistenceEvent::JudgeBatch {
            round_id: payload
                .get("sample_round_id")
                .and_then(Value::as_str)
                .unwrap_or("simulation-judge")
                .to_string(),
            user_id: payload
                .get("sample_user_id")
                .and_then(Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
                .unwrap_or(0),
            payload: payload.clone(),
            simulation: true,
        }),
        _ => server_event(kind, payload.clone(), true),
    }
}

fn server_event(kind: &str, payload: Value, simulation: bool) -> Option<PersistenceEvent> {
    Some(PersistenceEvent::ServerEvent {
        kind: kind.to_string(),
        payload,
        simulation,
    })
}

fn persist_simulation_event_if_needed(event: &PersistenceEvent) -> bool {
    if !event.is_simulation() {
        return false;
    }
    let Some(db) = crate::internal_hooks::DB.get() else {
        return false;
    };

    match event {
        PersistenceEvent::ServerEvent { kind, payload, .. } => {
            db.record_sim_event_sync(extract_run_id(payload), kind, payload.clone());
            true
        }
        PersistenceEvent::RoomSnapshot { room_id, payload, .. } => {
            let mut payload = payload.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.entry("room_id".to_string()).or_insert_with(|| serde_json::json!(room_id));
            }
            db.record_sim_event_sync(extract_run_id(&payload), "simulation.room_snapshot", payload);
            true
        }
        PersistenceEvent::TouchBatch { round_id, user_id, payload, .. } => {
            let mut payload = payload.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.entry("round_id".to_string()).or_insert_with(|| serde_json::json!(round_id));
                obj.entry("user_id".to_string()).or_insert_with(|| serde_json::json!(user_id));
            }
            db.record_sim_event_sync(extract_run_id(&payload), "simulation.touch_batch", payload);
            true
        }
        PersistenceEvent::JudgeBatch { round_id, user_id, payload, .. } => {
            let mut payload = payload.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.entry("round_id".to_string()).or_insert_with(|| serde_json::json!(round_id));
                obj.entry("user_id".to_string()).or_insert_with(|| serde_json::json!(user_id));
            }
            db.record_sim_event_sync(extract_run_id(&payload), "simulation.judge_batch", payload);
            true
        }
        PersistenceEvent::Flush | PersistenceEvent::Shutdown => false,
    }
}


#[derive(Debug, Clone, PartialEq, Eq)]
enum ProductionTelemetryStage {
    NotTelemetry,
    Staged,
    Failed(String),
}

async fn stage_production_telemetry_if_needed(
    event: &PersistenceEvent,
    batcher: &Arc<TelemetryBatcher>,
) -> ProductionTelemetryStage {
    if event.is_simulation() {
        return ProductionTelemetryStage::NotTelemetry;
    }

    let item = match event {
        PersistenceEvent::TouchBatch { round_id, user_id, payload, .. } => Some(TelemetryItem {
            kind: TelemetryKind::Touch,
            room_id: extract_room_id(payload),
            round_id: Some(round_id.clone()),
            user_id: *user_id,
            item_count: extract_item_count(payload),
            payload: payload.clone(),
        }),
        PersistenceEvent::JudgeBatch { round_id, user_id, payload, .. } => Some(TelemetryItem {
            kind: TelemetryKind::Judge,
            room_id: extract_room_id(payload),
            round_id: Some(round_id.clone()),
            user_id: *user_id,
            item_count: extract_item_count(payload),
            payload: payload.clone(),
        }),
        _ => None,
    };

    let Some(item) = item else {
        return ProductionTelemetryStage::NotTelemetry;
    };

    let kind = item.kind.as_str().to_string();
    let user_id = item.user_id;
    match batcher.enqueue(item).await {
        Ok(()) => ProductionTelemetryStage::Staged,
        Err(_) => ProductionTelemetryStage::Failed(format!(
            "telemetry batcher rejected {kind} batch for user_id={user_id}"
        )),
    }
}

fn persist_production_event_if_needed(event: &PersistenceEvent) -> bool {
    if event.is_simulation() {
        return false;
    }
    let Some(db) = crate::internal_hooks::DB.get() else {
        return false;
    };

    match event {
        PersistenceEvent::ServerEvent { kind, payload, .. } => {
            let payload = with_runtime_v2_persistence_meta(payload.clone());
            db.record_room_event_sync(
                kind,
                extract_room_id(&payload),
                extract_user_id(&payload),
                payload,
            );
            true
        }
        PersistenceEvent::RoomSnapshot { room_id, payload, .. } => {
            let mut payload = with_runtime_v2_persistence_meta(payload.clone());
            if let Some(obj) = payload.as_object_mut() {
                obj.entry("room_id".to_string()).or_insert_with(|| serde_json::json!(room_id));
            }
            db.record_room_event_sync(
                "runtime.room_snapshot",
                Some(room_id.clone()),
                extract_user_id(&payload),
                payload,
            );
            true
        }
        // Production Touch/Judge batches are handled by `stage_production_telemetry_if_needed`.
        // They are intentionally not written as generic low-frequency `mp_events`.
        PersistenceEvent::TouchBatch { .. } | PersistenceEvent::JudgeBatch { .. } => false,
        PersistenceEvent::Flush | PersistenceEvent::Shutdown => false,
    }
}

fn with_runtime_v2_persistence_meta(mut payload: Value) -> Value {
    if let Some(obj) = payload.as_object_mut() {
        obj.entry("runtime_v2_source".to_string())
            .or_insert_with(|| serde_json::json!("persistence_worker"));
        obj.entry("runtime_v2_dual_write".to_string())
            .or_insert_with(|| serde_json::json!(true));
    }
    payload
}

fn extract_room_id(payload: &Value) -> Option<String> {
    payload
        .get("room_id")
        .and_then(Value::as_str)
        .filter(|room_id| !room_id.is_empty())
        .map(ToString::to_string)
}

fn extract_user_id(payload: &Value) -> Option<i32> {
    payload
        .get("user_id")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

fn extract_item_count(payload: &Value) -> usize {
    payload
        .get("count")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(1)
        .max(1)
}

fn extract_run_id(payload: &Value) -> Option<String> {
    payload
        .get("run_id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

async fn record_simulation_persist_request(stats: &Arc<RwLock<PersistenceStats>>) {
    stats.write().await.simulation_persist_requests += 1;
}

async fn record_production_persist_request(stats: &Arc<RwLock<PersistenceStats>>) {
    stats.write().await.production_persist_requests += 1;
}

async fn record_production_persist_skipped(stats: &Arc<RwLock<PersistenceStats>>) {
    stats.write().await.production_persist_skipped += 1;
}

async fn record_production_telemetry_staged(stats: &Arc<RwLock<PersistenceStats>>) {
    stats.write().await.production_telemetry_staged += 1;
}

async fn record_production_telemetry_stage_failed(
    stats: &Arc<RwLock<PersistenceStats>>,
    error: String,
) {
    let mut stats = stats.write().await;
    stats.production_telemetry_stage_failed += 1;
    stats.last_error = Some(error);
}

async fn record_queued(
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

async fn record_processed(
    stats: &Arc<RwLock<PersistenceStats>>,
    kind: String,
    simulation: bool,
    summary: String,
) {
    let mut stats = stats.write().await;
    stats.processed += 1;
    push_trace(&mut stats, "processed", kind, simulation, summary);
}

async fn record_dropped(
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
        stats.recent.remove(0);
    }
    stats.recent.push(PersistenceTraceEntry {
        seq,
        action: action.into(),
        kind,
        simulation,
        summary,
    });
}

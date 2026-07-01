//! Runtime v2 high-frequency telemetry batcher.
//!
//! This is the staging layer for moving production Touch/Judge persistence away
//! from direct hot-path writes and into an actor/worker based pipeline.
//! Step 22 enables guarded batch-write mode by default for the test-stage
//! project: production Touch/Judge events are accepted, batched, flushed, and
//! dual-written into Runtime v2 telemetry tables while the direct legacy path
//! remains available for compatibility comparison. Step 23 writes schema-v2
//! batch headers plus normalized raw item rows for later replay and analysis.
//! Step 24 adds an explicit legacy_only/dual_write/worker_only/fallback_only
//! cutover switch so the test-stage server can safely move production telemetry
//! away from direct Session hot-path writes.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::VecDeque, sync::{atomic::{AtomicU64, Ordering}, Arc}, time::{Duration, SystemTime, UNIX_EPOCH}};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, trace, warn};

const MAX_TELEMETRY_TRACE: usize = 64;
const TELEMETRY_SCHEMA_VERSION: i32 = 2;
static TELEMETRY_BATCH_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryCutoverMode {
    /// Only the legacy RoundStore/db.rs path writes production Touch/Judge data.
    LegacyOnly,
    /// Write through both legacy direct path and Runtime v2 batcher. Safe default during comparison.
    DualWrite,
    /// Only Runtime v2 TelemetryBatcher writes production Touch/Judge data.
    WorkerOnly,
    /// Try Runtime v2 first; if enqueue fails immediately, fall back to legacy direct write.
    FallbackOnly,
}

impl Default for TelemetryCutoverMode {
    fn default() -> Self {
        Self::DualWrite
    }
}

impl TelemetryCutoverMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LegacyOnly => "legacy_only",
            Self::DualWrite => "dual_write",
            Self::WorkerOnly => "worker_only",
            Self::FallbackOnly => "fallback_only",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "legacy" | "legacy_only" | "legacy-only" => Some(Self::LegacyOnly),
            "dual" | "dual_write" | "dual-write" | "both" => Some(Self::DualWrite),
            "worker" | "worker_only" | "worker-only" | "runtime" | "runtime_v2" => Some(Self::WorkerOnly),
            "fallback" | "fallback_only" | "fallback-only" => Some(Self::FallbackOnly),
            _ => None,
        }
    }

    pub fn should_write_legacy(self) -> bool {
        matches!(self, Self::LegacyOnly | Self::DualWrite)
    }

    pub fn should_enqueue_worker(self) -> bool {
        matches!(self, Self::DualWrite | Self::WorkerOnly | Self::FallbackOnly)
    }

    pub fn fallback_to_legacy_on_enqueue_failure(self) -> bool {
        matches!(self, Self::FallbackOnly)
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::LegacyOnly => "legacy RoundStore/db.rs only; Runtime v2 batcher is bypassed",
            Self::DualWrite => "legacy direct write plus Runtime v2 telemetry batch-write",
            Self::WorkerOnly => "Runtime v2 telemetry batch-write only; legacy direct write is bypassed",
            Self::FallbackOnly => "Runtime v2 enqueue first; legacy direct write only on immediate enqueue failure",
        }
    }

    pub fn variants() -> &'static [TelemetryCutoverMode] {
        &[Self::LegacyOnly, Self::DualWrite, Self::WorkerOnly, Self::FallbackOnly]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryBatcherPolicy {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default = "default_queue_capacity")]
    pub queue_capacity: usize,
    #[serde(default = "default_max_items_per_batch")]
    pub max_items_per_batch: usize,
    #[serde(default = "default_flush_interval_ms")]
    pub flush_interval_ms: u64,
}

fn default_enabled() -> bool { true }
fn default_queue_capacity() -> usize { 8192 }
fn default_max_items_per_batch() -> usize { 256 }
fn default_flush_interval_ms() -> u64 { 1000 }

impl Default for TelemetryBatcherPolicy {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            dry_run: false,
            queue_capacity: default_queue_capacity(),
            max_items_per_batch: default_max_items_per_batch(),
            flush_interval_ms: default_flush_interval_ms(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryBatcherStats {
    pub enabled: bool,
    pub dry_run: bool,
    pub cutover_mode: String,
    pub queue_capacity: usize,
    pub max_items_per_batch: usize,
    pub flush_interval_ms: u64,
    pub queued: u64,
    pub accepted: u64,
    pub dropped: u64,
    pub flushed_batches: u64,
    pub flushed_items: u64,
    pub write_batches: u64,
    pub write_items: u64,
    pub write_item_rows: u64,
    pub write_errors: u64,
    pub schema_version: i32,
    pub last_batch_uuid: Option<String>,
    pub touch_items: u64,
    pub judge_items: u64,
    pub pending: usize,
    pub last_error: Option<String>,
    pub recent: Vec<TelemetryTraceEntry>,
}


impl Default for TelemetryBatcherStats {
    fn default() -> Self {
        Self::from_policy(&TelemetryBatcherPolicy::default())
    }
}

impl TelemetryBatcherStats {
    pub fn from_policy(policy: &TelemetryBatcherPolicy) -> Self {
        Self {
            enabled: policy.enabled,
            dry_run: policy.dry_run,
            cutover_mode: TelemetryCutoverMode::default().as_str().to_string(),
            queue_capacity: policy.queue_capacity,
            max_items_per_batch: policy.max_items_per_batch,
            flush_interval_ms: policy.flush_interval_ms,
            queued: 0,
            accepted: 0,
            dropped: 0,
            flushed_batches: 0,
            flushed_items: 0,
            write_batches: 0,
            write_items: 0,
            write_item_rows: 0,
            write_errors: 0,
            schema_version: TELEMETRY_SCHEMA_VERSION,
            last_batch_uuid: None,
            touch_items: 0,
            judge_items: 0,
            pending: 0,
            last_error: None,
            recent: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryTraceEntry {
    pub seq: u64,
    pub action: String,
    pub kind: String,
    pub room_id: Option<String>,
    pub user_id: i32,
    pub item_count: usize,
    pub batch_uuid: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum TelemetryKind {
    Touch,
    Judge,
}

impl TelemetryKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Touch => "touch",
            Self::Judge => "judge",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TelemetryItem {
    pub kind: TelemetryKind,
    pub room_id: Option<String>,
    pub round_id: Option<String>,
    pub user_id: i32,
    pub item_count: usize,
    pub payload: Value,
}

#[derive(Debug)]
pub struct TelemetryBatcher {
    tx: mpsc::Sender<TelemetryItem>,
    stats: Arc<RwLock<TelemetryBatcherStats>>,
}

impl TelemetryBatcher {
    pub fn spawn(policy: TelemetryBatcherPolicy) -> Arc<Self> {
        let capacity = policy.queue_capacity.max(16);
        let worker_policy = TelemetryBatcherPolicy {
            queue_capacity: capacity,
            ..policy
        };
        let (tx, rx) = mpsc::channel::<TelemetryItem>(capacity);
        let stats = Arc::new(RwLock::new(TelemetryBatcherStats::from_policy(&worker_policy)));
        let worker_stats = Arc::clone(&stats);

        tokio::spawn(async move {
            run_batcher(worker_policy, rx, worker_stats).await;
        });

        Arc::new(Self { tx, stats })
    }

    pub async fn enqueue(&self, item: TelemetryItem) -> Result<(), TelemetryItem> {
        let kind = item.kind.as_str().to_string();
        let room_id = item.room_id.clone();
        let user_id = item.user_id;
        let item_count = item.item_count;
        match self.tx.try_send(item) {
            Ok(()) => {
                let mut stats = self.stats.write().await;
                stats.queued += 1;
                push_trace(&mut stats, "queued", kind, room_id, user_id, item_count, None);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(item)) => {
                let mut stats = self.stats.write().await;
                stats.dropped += 1;
                stats.last_error = Some("telemetry batcher queue is full".to_string());
                push_trace(&mut stats, "dropped", kind, room_id, user_id, item_count, None);
                warn!(kind = %item.kind.as_str(), user_id = item.user_id, "telemetry batcher queue is full; item dropped");
                Err(item)
            }
            Err(mpsc::error::TrySendError::Closed(item)) => {
                let mut stats = self.stats.write().await;
                stats.dropped += 1;
                stats.last_error = Some("telemetry batcher queue is closed".to_string());
                push_trace(&mut stats, "dropped", kind, room_id, user_id, item_count, None);
                warn!(kind = %item.kind.as_str(), user_id = item.user_id, "telemetry batcher queue is closed; item dropped");
                Err(item)
            }
        }
    }

    pub async fn stats(&self) -> TelemetryBatcherStats {
        self.stats.read().await.clone()
    }
}

async fn run_batcher(
    policy: TelemetryBatcherPolicy,
    mut rx: mpsc::Receiver<TelemetryItem>,
    stats: Arc<RwLock<TelemetryBatcherStats>>,
) {
    if !policy.enabled {
        debug!("telemetry batcher disabled");
        return;
    }

    let flush_interval = Duration::from_millis(policy.flush_interval_ms.max(100));
    let max_items = policy.max_items_per_batch.max(1);
    let mut pending: VecDeque<TelemetryItem> = VecDeque::new();
    let mut ticker = tokio::time::interval(flush_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            maybe_item = rx.recv() => {
                match maybe_item {
                    Some(item) => {
                        record_accepted(&stats, &item).await;
                        pending.push_back(item);
                        update_pending(&stats, pending.len()).await;
                        if pending.len() >= max_items {
                            flush_pending(&policy, &mut pending, &stats, "max_items").await;
                        }
                    }
                    None => {
                        flush_pending(&policy, &mut pending, &stats, "closed").await;
                        break;
                    }
                }
            }
            _ = ticker.tick() => {
                flush_pending(&policy, &mut pending, &stats, "interval").await;
            }
        }
    }
}

async fn record_accepted(stats: &Arc<RwLock<TelemetryBatcherStats>>, item: &TelemetryItem) {
    let mut stats = stats.write().await;
    stats.accepted += 1;
    match item.kind {
        TelemetryKind::Touch => stats.touch_items += item.item_count as u64,
        TelemetryKind::Judge => stats.judge_items += item.item_count as u64,
    }
    push_trace(
        &mut stats,
        "accepted",
        item.kind.as_str().to_string(),
        item.room_id.clone(),
        item.user_id,
        item.item_count,
        None,
    );
}

async fn update_pending(stats: &Arc<RwLock<TelemetryBatcherStats>>, pending: usize) {
    stats.write().await.pending = pending;
}

async fn flush_pending(
    policy: &TelemetryBatcherPolicy,
    pending: &mut VecDeque<TelemetryItem>,
    stats: &Arc<RwLock<TelemetryBatcherStats>>,
    reason: &str,
) {
    if pending.is_empty() {
        update_pending(stats, 0).await;
        return;
    }

    let items = pending.len();
    let telemetry_points: usize = pending.iter().map(|item| item.item_count).sum();
    let mut flushed = Vec::with_capacity(items);
    while let Some(item) = pending.pop_front() {
        flushed.push(item);
    }
    let first = flushed.first().cloned();
    let batch_uuid = next_batch_uuid();
    let write_result = if policy.dry_run {
        Ok((false, 0usize))
    } else {
        write_runtime_telemetry_batch(&batch_uuid, &flushed, reason).map(|item_rows| (true, item_rows))
    };

    let mut stats = stats.write().await;
    stats.flushed_batches += 1;
    stats.flushed_items += telemetry_points as u64;
    stats.pending = 0;
    let action = match write_result {
        Ok((true, item_rows)) => {
            stats.write_batches += 1;
            stats.write_items += telemetry_points as u64;
            stats.write_item_rows += item_rows as u64;
            stats.last_batch_uuid = Some(batch_uuid.clone());
            "db_flush"
        }
        Ok((false, _)) => {
            stats.last_batch_uuid = Some(batch_uuid.clone());
            "dry_flush"
        },
        Err(err) => {
            stats.write_errors += 1;
            stats.last_error = Some(err);
            "write_failed"
        }
    };
    if let Some(item) = first {
        push_trace(
            &mut stats,
            action,
            item.kind.as_str().to_string(),
            item.room_id,
            item.user_id,
            telemetry_points,
            Some(batch_uuid.clone()),
        );
    }

    trace!(items, telemetry_points, dry_run = policy.dry_run, reason, "telemetry batcher flushed batch");
}


fn write_runtime_telemetry_batch(batch_uuid: &str, items: &[TelemetryItem], reason: &str) -> Result<usize, String> {
    let Some(db) = crate::internal_hooks::DB.get() else {
        return Err("database manager is not initialized".to_string());
    };
    let records: Vec<crate::db::RuntimeTelemetryBatchRecord> = items
        .iter()
        .map(|item| {
            let mut payload = item.payload.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.entry("runtime_v2_source".to_string())
                    .or_insert_with(|| serde_json::json!("telemetry_batcher"));
                obj.entry("runtime_v2_dual_write".to_string())
                    .or_insert_with(|| serde_json::json!(true));
                obj.entry("runtime_v2_stage".to_string())
                    .or_insert_with(|| serde_json::json!("guarded_batch_write"));
                obj.entry("runtime_v2_schema_version".to_string())
                    .or_insert_with(|| serde_json::json!(TELEMETRY_SCHEMA_VERSION));
                obj.entry("batch_uuid".to_string())
                    .or_insert_with(|| serde_json::json!(batch_uuid));
                obj.entry("flush_reason".to_string())
                    .or_insert_with(|| serde_json::json!(reason));
                obj.entry("kind".to_string())
                    .or_insert_with(|| serde_json::json!(item.kind.as_str()));
                obj.entry("user_id".to_string())
                    .or_insert_with(|| serde_json::json!(item.user_id));
                obj.entry("count".to_string())
                    .or_insert_with(|| serde_json::json!(item.item_count));
                if let Some(room_id) = &item.room_id {
                    obj.entry("room_id".to_string())
                        .or_insert_with(|| serde_json::json!(room_id));
                }
                if let Some(round_id) = &item.round_id {
                    obj.entry("round_id".to_string())
                        .or_insert_with(|| serde_json::json!(round_id));
                }
            }
            crate::db::RuntimeTelemetryBatchRecord {
                batch_uuid: batch_uuid.to_string(),
                run_id: extract_run_id(&payload),
                scope: "production".to_string(),
                pipeline: "runtime_v2.telemetry_batcher".to_string(),
                source: "telemetry_batcher".to_string(),
                flush_reason: reason.to_string(),
                schema_version: TELEMETRY_SCHEMA_VERSION,
                dual_write: true,
                kind: item.kind.as_str().to_string(),
                room_id: item.room_id.clone(),
                round_uuid: item.round_id.clone(),
                player_id: item.user_id,
                item_count: i32::try_from(item.item_count).unwrap_or(i32::MAX),
                payload,
            }
        })
        .collect();
    let item_rows = records.iter().map(|record| raw_item_count(&record.payload)).sum();
    if db.record_runtime_telemetry_batches_sync(records) {
        Ok(item_rows)
    } else {
        Err("database is not active; telemetry batch was not written".to_string())
    }
}

fn push_trace(
    stats: &mut TelemetryBatcherStats,
    action: impl Into<String>,
    kind: String,
    room_id: Option<String>,
    user_id: i32,
    item_count: usize,
    batch_uuid: Option<String>,
) {
    let seq = stats.queued + stats.accepted + stats.dropped + stats.flushed_batches;
    if stats.recent.len() >= MAX_TELEMETRY_TRACE {
        stats.recent.remove(0);
    }
    stats.recent.push(TelemetryTraceEntry {
        seq,
        action: action.into(),
        kind,
        room_id,
        user_id,
        item_count,
        batch_uuid,
    });
}


fn next_batch_uuid() -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    let seq = TELEMETRY_BATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("rtv2-tel-{now_ms}-{seq}")
}

fn extract_run_id(payload: &Value) -> Option<String> {
    payload
        .get("run_id")
        .or_else(|| payload.get("simulation_run_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn raw_item_count(payload: &Value) -> usize {
    payload
        .get("data")
        .and_then(Value::as_array)
        .map(|items| items.len())
        .unwrap_or_else(|| {
            payload
                .get("count")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(1)
        })
        .max(1)
}

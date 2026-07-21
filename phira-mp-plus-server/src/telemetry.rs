//! Runtime v2 高频遥测基础设施。
//!
//! 生产 Touch/Judge 支持三种显式切换模式：
//!
//! - **DirectOnly**：只通过 `RoundStore`/`db.rs` 直写（安全默认值）
//! - **WorkerPreferred**：先直写；直写成功时 Worker 是迁移镜像，直写失败但
//!   Worker 接收时由 Worker 作为该批次的权威补偿路径
//! - **WorkerAuthoritative**：只把数据送入有界 PersistenceWorker/TelemetryBatcher；
//!   仅在 Worker 明确拒收、能够确认命令未入队时回退直写
//!
//! Worker 路径提供有界背压、批处理、数据库有限重试以及可确认的 flush。
//! 它仍没有磁盘 WAL，因此不能承诺进程崩溃或主机掉电时零数据丢失。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tracing::{debug, trace, warn};

const MAX_TELEMETRY_TRACE: usize = 64;
pub const TELEMETRY_SCHEMA_VERSION: i32 = 3;
static TELEMETRY_BATCH_SEQ: AtomicU64 = AtomicU64::new(1);

// ── Cutover mode & decision ──────────────────────────────────────────

/// Cutover mode controlling how production Touch/Judge telemetry is persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryCutoverMode {
    /// Only the direct RoundStore/db.rs path writes production Touch/Judge data.
    DirectOnly,
    /// Direct write is attempted first. The Worker is a mirror only after direct
    /// acknowledgement; after direct failure it may be the canonical compensation path.
    WorkerPreferred,
    /// PersistenceWorker/TelemetryBatcher is the normal-operation single writer.
    /// A direct fallback is permitted only when enqueue is explicitly rejected.
    WorkerAuthoritative,
}

/// Structured decision derived from a [`TelemetryCutoverMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelemetryCutoverDecision {
    pub mode: TelemetryCutoverMode,
    pub enqueue_worker: bool,
    pub write_direct_before_worker_result: bool,
    pub fallback_direct_when_worker_rejects: bool,
}

impl TelemetryCutoverDecision {
    pub fn from_mode(mode: TelemetryCutoverMode) -> Self {
        match mode {
            TelemetryCutoverMode::DirectOnly => Self {
                mode,
                enqueue_worker: false,
                write_direct_before_worker_result: true,
                fallback_direct_when_worker_rejects: true,
            },
            TelemetryCutoverMode::WorkerPreferred => Self {
                mode,
                enqueue_worker: true,
                write_direct_before_worker_result: true,
                fallback_direct_when_worker_rejects: true,
            },
            TelemetryCutoverMode::WorkerAuthoritative => Self {
                mode,
                enqueue_worker: true,
                write_direct_before_worker_result: false,
                fallback_direct_when_worker_rejects: true,
            },
        }
    }

    pub fn should_write_direct_after_worker_enqueue(self, worker_enqueue_ok: bool) -> bool {
        self.write_direct_before_worker_result
            || (!worker_enqueue_ok && self.fallback_direct_when_worker_rejects)
    }
}

impl Default for TelemetryCutoverMode {
    fn default() -> Self {
        // Safety: DirectOnly ensures Touches/Judges never silently drop.
        // WorkerPreferred must be explicitly opted into by the operator.
        Self::DirectOnly
    }
}

impl TelemetryCutoverMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DirectOnly => "direct_only",
            Self::WorkerPreferred => "worker_preferred",
            Self::WorkerAuthoritative => "worker_authoritative",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "direct" | "direct_only" | "direct-only" => Some(Self::DirectOnly),
            "worker_preferred" | "worker-preferred" => Some(Self::WorkerPreferred),
            "worker_authoritative" | "worker-authoritative" => Some(Self::WorkerAuthoritative),
            _ => None,
        }
    }

    pub fn should_write_direct(self) -> bool {
        self.cutover_decision().write_direct_before_worker_result
    }

    pub fn should_enqueue_worker(self) -> bool {
        self.cutover_decision().enqueue_worker
    }

    pub fn cutover_decision(self) -> TelemetryCutoverDecision {
        TelemetryCutoverDecision::from_mode(self)
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::DirectOnly => "direct RoundStore/db.rs only; Runtime v2 batcher is bypassed",
            Self::WorkerPreferred => {
                "direct first; Worker mirrors acknowledged writes and compensates direct failures"
            }
            Self::WorkerAuthoritative => {
                "Worker is the normal-operation single writer; direct fallback only on explicit enqueue rejection"
            }
        }
    }

    pub fn variants() -> &'static [TelemetryCutoverMode] {
        &[
            Self::DirectOnly,
            Self::WorkerPreferred,
            Self::WorkerAuthoritative,
        ]
    }
}

// ── Policy ───────────────────────────────────────────────────────────

/// Configuration for the [`TelemetryBatcher`].
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

fn default_enabled() -> bool {
    true
}
fn default_queue_capacity() -> usize {
    4096
}
fn default_max_items_per_batch() -> usize {
    256
}
fn default_flush_interval_ms() -> u64 {
    1000
}

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

// ── Stats & trace ────────────────────────────────────────────────────

/// Live statistics from the [`TelemetryBatcher`].
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
    pub db_dispatch_samples: u64,
    pub db_dispatch_total_ms: u64,
    pub db_dispatch_avg_ms: u64,
    pub db_dispatch_max_ms: u64,
    pub db_dispatch_last_ms: u64,
    pub db_ack_samples: u64,
    pub db_ack_total_ms: u64,
    pub db_ack_avg_ms: u64,
    pub db_ack_max_ms: u64,
    pub db_ack_last_ms: u64,
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
            db_dispatch_samples: 0,
            db_dispatch_total_ms: 0,
            db_dispatch_avg_ms: 0,
            db_dispatch_max_ms: 0,
            db_dispatch_last_ms: 0,
            db_ack_samples: 0,
            db_ack_total_ms: 0,
            db_ack_avg_ms: 0,
            db_ack_max_ms: 0,
            db_ack_last_ms: 0,
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

/// A recent trace entry kept for diagnostics.
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

// ── Item & kind ──────────────────────────────────────────────────────

/// Classification of a telemetry item.
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

/// A single telemetry item to be batched and flushed.
#[derive(Debug, Clone)]
pub struct TelemetryItem {
    pub event_id: String,
    /// WAL admission ID for ACK-after-commit tracking.
    /// Set by the caller when the event was admitted to WAL.
    pub wal_id: Option<uuid::Uuid>,
    pub kind: TelemetryKind,
    pub room_id: Option<String>,
    pub round_id: Option<String>,
    pub user_id: i32,
    pub item_count: usize,
    pub dual_write: bool,
    pub persistence_mode: String,
    pub payload: Value,
}

// ── Batcher actor ────────────────────────────────────────────────────

/// Actor-based telemetry batcher.
///
/// Items are enqueued via [`enqueue`](Self::enqueue) and flushed
/// periodically or when the batch-size threshold is reached.
#[derive(Debug)]
enum TelemetryMessage {
    Item(TelemetryItem),
    Flush(oneshot::Sender<Result<(), String>>),
    Shutdown(oneshot::Sender<Result<(), String>>),
}

#[derive(Debug)]
pub struct TelemetryBatcher {
    tx: mpsc::Sender<TelemetryMessage>,
    send_gate: Mutex<()>,
    closed: AtomicBool,
    stats: Arc<RwLock<TelemetryBatcherStats>>,
}

impl TelemetryBatcher {
    /// Spawn the background batcher task and return a handle.
    pub fn spawn(policy: TelemetryBatcherPolicy) -> Arc<Self> {
        let capacity = policy.queue_capacity.max(16);
        let worker_policy = TelemetryBatcherPolicy {
            queue_capacity: capacity,
            ..policy
        };
        let (tx, rx) = mpsc::channel::<TelemetryMessage>(capacity);
        let stats = Arc::new(RwLock::new(TelemetryBatcherStats::from_policy(
            &worker_policy,
        )));
        let worker_stats = Arc::clone(&stats);

        crate::supervisor_actor::spawn_critical("telemetry-batcher", async move {
            run_batcher(worker_policy, rx, worker_stats).await;
        });

        Arc::new(Self {
            tx,
            send_gate: Mutex::new(()),
            closed: AtomicBool::new(false),
            stats,
        })
    }

    /// Enqueue a telemetry item for batched persistence.
    ///
    /// Uses bounded backpressure. Returns `Err(item)` only when the batcher is closed.
    pub async fn enqueue(&self, item: TelemetryItem) -> Result<(), TelemetryItem> {
        let kind = item.kind.as_str().to_string();
        let room_id = item.room_id.clone();
        let user_id = item.user_id;
        let item_count = item.item_count;
        let _send_guard = self.send_gate.lock().await;
        if self.closed.load(Ordering::Acquire) {
            let mut stats = self.stats.write().await;
            stats.dropped += 1;
            stats.last_error = Some("telemetry batcher is shutting down".to_string());
            push_trace(
                &mut stats, "rejected", kind, room_id, user_id, item_count, None,
            );
            return Err(item);
        }
        match self.tx.send(TelemetryMessage::Item(item)).await {
            Ok(()) => {
                let mut stats = self.stats.write().await;
                stats.queued += 1;
                push_trace(
                    &mut stats, "queued", kind, room_id, user_id, item_count, None,
                );
                Ok(())
            }
            Err(mpsc::error::SendError(TelemetryMessage::Item(item))) => {
                let mut stats = self.stats.write().await;
                stats.dropped += 1;
                stats.last_error = Some("telemetry batcher queue is closed".to_string());
                push_trace(
                    &mut stats, "rejected", kind, room_id, user_id, item_count, None,
                );
                warn!(
                    kind = %item.kind.as_str(),
                    user_id = item.user_id,
                    "telemetry batcher is closed; item rejected before acceptance"
                );
                Err(item)
            }
            Err(_) => unreachable!("enqueue only sends telemetry items"),
        }
    }

    /// Flush all items accepted before this call.
    pub async fn flush(&self, timeout: Duration) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        {
            let _send_guard = self.send_gate.lock().await;
            if self.closed.load(Ordering::Acquire) {
                return Err("telemetry batcher is shutting down".to_string());
            }
            self.tx
                .send(TelemetryMessage::Flush(reply))
                .await
                .map_err(|_| "telemetry batcher is closed".to_string())?;
        }
        tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| "telemetry flush timed out".to_string())?
            .map_err(|_| "telemetry flush acknowledgement was dropped".to_string())??;
        Ok(())
    }

    /// Flush accepted items and stop the batcher.
    pub async fn shutdown(&self, timeout: Duration) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        {
            let _send_guard = self.send_gate.lock().await;
            if self.closed.swap(true, Ordering::AcqRel) {
                return Ok(());
            }
            if self
                .tx
                .send(TelemetryMessage::Shutdown(reply))
                .await
                .is_err()
            {
                self.closed.store(false, Ordering::Release);
                return Err("telemetry batcher is closed".to_string());
            }
        }
        let result = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("telemetry shutdown acknowledgement was dropped".to_string()),
            Err(_) => Err("telemetry shutdown timed out".to_string()),
        };
        if let Err(error) = result {
            self.closed.store(false, Ordering::Release);
            return Err(error);
        }
        Ok(())
    }

    /// Snapshot current batcher statistics.
    pub async fn stats(&self) -> TelemetryBatcherStats {
        self.stats.read().await.clone()
    }
}

// ── Background runner ────────────────────────────────────────────────

async fn run_batcher(
    policy: TelemetryBatcherPolicy,
    mut rx: mpsc::Receiver<TelemetryMessage>,
    stats: Arc<RwLock<TelemetryBatcherStats>>,
) {
    if !policy.enabled {
        debug!("telemetry batcher disabled; control messages remain available");
    }

    let flush_interval = Duration::from_millis(policy.flush_interval_ms.max(100));
    let max_items = policy.max_items_per_batch.max(1);
    let mut pending: VecDeque<TelemetryItem> = VecDeque::new();
    let mut ticker = tokio::time::interval(flush_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            message = rx.recv() => {
                match message {
                    Some(TelemetryMessage::Item(item)) if policy.enabled => {
                        record_accepted(&stats, &item).await;
                        pending.push_back(item);
                        update_pending(&stats, pending.len()).await;
                        if pending.len() >= max_items {
                            let _ = flush_pending(&policy, &mut pending, &stats, "max_items").await;
                        }
                    }
                    Some(TelemetryMessage::Item(item)) => {
                        let mut state = stats.write().await;
                        state.dropped += 1;
                        state.last_error = Some("telemetry batcher disabled".to_string());
                        push_trace(
                            &mut state,
                            "disabled",
                            item.kind.as_str().to_string(),
                            item.room_id.clone(),
                            item.user_id,
                            item.item_count,
                            None,
                        );
                    }
                    Some(TelemetryMessage::Flush(reply)) => {
                        let result = flush_pending(&policy, &mut pending, &stats, "explicit_flush").await;
                        let _ = reply.send(result);
                    }
                    Some(TelemetryMessage::Shutdown(reply)) => {
                        let result = flush_pending(&policy, &mut pending, &stats, "shutdown").await;
                        let should_stop = result.is_ok();
                        let _ = reply.send(result);
                        if should_stop {
                            break;
                        }
                    }
                    None => {
                        let _ = flush_pending(&policy, &mut pending, &stats, "closed").await;
                        break;
                    }
                }
            }
            _ = ticker.tick() => {
                let _ = flush_pending(&policy, &mut pending, &stats, "interval").await;
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

fn elapsed_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn record_db_dispatch_latency(stats: &mut TelemetryBatcherStats, elapsed_ms: u64) {
    stats.db_dispatch_samples += 1;
    stats.db_dispatch_total_ms = stats.db_dispatch_total_ms.saturating_add(elapsed_ms);
    stats.db_dispatch_last_ms = elapsed_ms;
    stats.db_dispatch_max_ms = stats.db_dispatch_max_ms.max(elapsed_ms);
    stats.db_dispatch_avg_ms = stats.db_dispatch_total_ms / stats.db_dispatch_samples.max(1);
}

async fn flush_pending(
    policy: &TelemetryBatcherPolicy,
    pending: &mut VecDeque<TelemetryItem>,
    stats: &Arc<RwLock<TelemetryBatcherStats>>,
    reason: &str,
) -> Result<(), String> {
    if pending.is_empty() {
        update_pending(stats, 0).await;
        return Ok(());
    }

    let items = pending.len();
    let telemetry_points: usize = pending.iter().map(|item| item.item_count).sum();
    let mut flushed = Vec::with_capacity(items);
    while let Some(item) = pending.pop_front() {
        flushed.push(item);
    }
    let first = flushed.first().cloned();
    let batch_uuid = next_batch_uuid();
    let write_started = Instant::now();
    let write_result = if policy.dry_run {
        Ok((false, 0usize))
    } else {
        write_runtime_telemetry_batch(&batch_uuid, &flushed, reason)
            .await
            .map(|item_rows| (true, item_rows))
    };
    let write_elapsed_ms = elapsed_ms(write_started);

    let mut stats = stats.write().await;
    stats.flushed_batches += 1;
    if !policy.dry_run {
        record_db_dispatch_latency(&mut stats, write_elapsed_ms);
    }

    let action = match write_result {
        Ok((true, item_rows)) => {
            stats.flushed_items += telemetry_points as u64;
            stats.write_batches += 1;
            stats.write_items += telemetry_points as u64;
            stats.write_item_rows += item_rows as u64;
            stats.last_batch_uuid = Some(batch_uuid.clone());
            stats.pending = pending.len();
            "db_flush"
        }
        Ok((false, _)) => {
            stats.flushed_items += telemetry_points as u64;
            stats.last_batch_uuid = Some(batch_uuid.clone());
            stats.pending = pending.len();
            "dry_flush"
        }
        Err(err) => {
            for item in flushed.into_iter().rev() {
                pending.push_front(item);
            }
            stats.write_errors += 1;
            stats.pending = pending.len();
            stats.last_error = Some(err.clone());
            if let Some(item) = first {
                push_trace(
                    &mut stats,
                    "write_retained",
                    item.kind.as_str().to_string(),
                    item.room_id,
                    item.user_id,
                    telemetry_points,
                    Some(batch_uuid),
                );
            }
            warn!(items, telemetry_points, reason, %err, "telemetry batch retained after database write failure");
            return Err(err);
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

    trace!(
        items,
        telemetry_points,
        dry_run = policy.dry_run,
        reason,
        "telemetry batcher flushed batch"
    );
    Ok(())
}

async fn write_runtime_telemetry_batch(
    batch_uuid: &str,
    items: &[TelemetryItem],
    reason: &str,
) -> Result<usize, String> {
    let Some(db) = crate::internal_hooks::DB.get().filter(|db| db.is_active()) else {
        return Err("database is not active".to_string());
    };
    let item_rows: usize = items.iter().map(|i| i.item_count).sum();
    let records: Vec<crate::db::RuntimeTelemetryBatchRecord> = items
        .iter()
        .map(|item| crate::db::RuntimeTelemetryBatchRecord {
            event_id: item.event_id.clone(),
            batch_uuid: batch_uuid.to_string(),
            run_id: extract_run_id(&item.payload),
            scope: "production".to_string(),
            pipeline: format!("runtime_v2.telemetry_batcher.{}", item.persistence_mode),
            source: if item.dual_write {
                "telemetry_batcher_mirror".to_string()
            } else {
                "telemetry_batcher_authoritative".to_string()
            },
            flush_reason: reason.to_string(),
            schema_version: TELEMETRY_SCHEMA_VERSION,
            dual_write: item.dual_write,
            kind: item.kind.as_str().to_string(),
            room_id: item.room_id.clone(),
            round_uuid: item.round_id.clone(),
            player_id: item.user_id,
            item_count: i32::try_from(item.item_count).unwrap_or(i32::MAX),
            payload: item.payload.clone(),
        })
        .collect();

    const ATTEMPTS: usize = 3;
    for attempt in 0..ATTEMPTS {
        if db.record_runtime_telemetry_batches(records.clone()).await {
            return Ok(item_rows);
        }
        if attempt + 1 < ATTEMPTS {
            let delay = if attempt == 0 { 50 } else { 250 };
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
    }
    Err("telemetry batch database write failed after retries".to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutover_decisions_match_mode_contract() {
        let direct = TelemetryCutoverMode::DirectOnly.cutover_decision();
        assert!(!direct.enqueue_worker);
        assert!(direct.should_write_direct_after_worker_enqueue(false));
        assert!(direct.should_write_direct_after_worker_enqueue(true));

        let worker = TelemetryCutoverMode::WorkerPreferred.cutover_decision();
        assert!(worker.enqueue_worker);
        assert!(worker.should_write_direct_after_worker_enqueue(false));
        assert!(worker.should_write_direct_after_worker_enqueue(true));

        let authoritative = TelemetryCutoverMode::WorkerAuthoritative.cutover_decision();
        assert!(authoritative.enqueue_worker);
        assert!(authoritative.should_write_direct_after_worker_enqueue(false));
        assert!(!authoritative.should_write_direct_after_worker_enqueue(true));
    }

    #[test]
    fn cutover_helpers_delegate_to_decision_contract() {
        for &mode in TelemetryCutoverMode::variants() {
            let decision = mode.cutover_decision();
            assert_eq!(mode.should_enqueue_worker(), decision.enqueue_worker);
            assert_eq!(
                mode.should_write_direct(),
                decision.write_direct_before_worker_result
            );
        }
    }

    #[test]
    fn db_dispatch_latency_uses_constant_size_aggregates() {
        let mut stats = TelemetryBatcherStats::default();
        record_db_dispatch_latency(&mut stats, 3);
        record_db_dispatch_latency(&mut stats, 7);
        assert_eq!(stats.db_dispatch_samples, 2);
        assert_eq!(stats.db_dispatch_avg_ms, 5);
        assert_eq!(stats.db_dispatch_max_ms, 7);
        assert_eq!(stats.db_dispatch_last_ms, 7);
    }
}

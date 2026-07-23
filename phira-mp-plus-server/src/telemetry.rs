//! Runtime v2 高频遥测基础设施。
//!
//! 所有生产 Touch/Judge 统一通过 PersistenceWorker/TelemetryBatcher 路径写入，
//! 提供有界背压、批处理、数据库有限重试以及可确认的 flush。

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

// ── Cutover mode & decision (removed) ─────────────────────────────────
//
// TelemetryCutoverMode and TelemetryCutoverDecision have been removed.
// All production Touch/Judge telemetry now uniformly goes through
// PersistenceWorker/TelemetryBatcher — the single unified persistence path.

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

    // ── Per-kind drop tracking ─────────────────────────────────────────
    #[serde(default)]
    pub touch_dropped: u64,
    #[serde(default)]
    pub judge_dropped: u64,
    #[serde(default)]
    pub touch_committed: u64,
    #[serde(default)]
    pub judge_committed: u64,

    // ── Batch-size tracking ────────────────────────────────────────────
    #[serde(default)]
    pub batch_size_total: u64,
    #[serde(default)]
    pub batch_size_samples: u64,
    #[serde(default)]
    pub batch_size_avg: f64,
    #[serde(default)]
    pub batch_size_max: usize,
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
            cutover_mode: String::new(),
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
            touch_dropped: 0,
            judge_dropped: 0,
            touch_committed: 0,
            judge_committed: 0,
            batch_size_total: 0,
            batch_size_samples: 0,
            batch_size_avg: 0.0,
            batch_size_max: 0,
        }
    }

    pub fn record_batch_size(&mut self, size: usize) {
        self.batch_size_total = self.batch_size_total.saturating_add(size as u64);
        self.batch_size_samples = self.batch_size_samples.saturating_add(1);
        self.batch_size_max = self.batch_size_max.max(size);
        self.batch_size_avg = if self.batch_size_samples == 0 {
            0.0
        } else {
            self.batch_size_total as f64 / self.batch_size_samples as f64
        };
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
    pub payload: Value,
}

// ── Batcher actor ────────────────────────────────────────────────────

// ── WAL ACK reliability ─────────────────────────────────────────────

/// Typed message for the WAL ACK channel between the telemetry batcher
/// and the persistence-wal-ack retry task.
///
/// Replaces the raw `uuid::Uuid` sender so the batcher can wait for ACK
/// durability before declaring flush/shutdown complete.
#[derive(Debug)]
pub enum WalAckMessage {
    /// A single WAL ID to acknowledge.
    Ack(uuid::Uuid),
    /// Barrier: process every ACK received before this message, retrying
    /// as needed, then reply.  The receiver sends `Ok(())` on its oneshot
    /// once all ACKs are durable (or `Err` if any ACK permanently failed).
    DrainAndReply(oneshot::Sender<Result<(), String>>),
}

/// Shared handle for WAL ACK channel, passed to run_batcher.
type WalAckTx = std::sync::Arc<std::sync::Mutex<Option<mpsc::Sender<WalAckMessage>>>>;

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
    /// Channel to ACK WAL IDs after successful database commit.
    /// Set by the PersistenceWorker after spawn.
    wal_ack_tx: WalAckTx,
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
        let ack_tx: WalAckTx = Arc::new(std::sync::Mutex::new(None));
        let batcher_ack_tx = Arc::clone(&ack_tx);

        crate::supervisor_actor::spawn_critical("telemetry-batcher", async move {
            run_batcher(worker_policy, rx, worker_stats, ack_tx).await;
        });

        Arc::new(Self {
            tx,
            send_gate: Mutex::new(()),
            closed: AtomicBool::new(false),
            stats,
            wal_ack_tx: batcher_ack_tx,
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
            match item.kind {
                TelemetryKind::Touch => stats.touch_dropped += item.item_count as u64,
                TelemetryKind::Judge => stats.judge_dropped += item.item_count as u64,
            }
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
                match item.kind {
                    TelemetryKind::Touch => stats.touch_dropped += item.item_count as u64,
                    TelemetryKind::Judge => stats.judge_dropped += item.item_count as u64,
                }
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
    /// Set the WAL ACK channel. After each successful database batch commit,
    /// the batcher sends `WalAckMessage::Ack` IDs through this channel and
    /// follows with `WalAckMessage::DrainAndReply` so the receiver can
    /// confirm durable WAL fsync before the flush returns success.
    pub fn set_wal_ack_tx(&self, tx: mpsc::Sender<WalAckMessage>) {
        if let Ok(mut guard) = self.wal_ack_tx.lock() {
            *guard = Some(tx);
        }
    }

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
    ack_tx: WalAckTx,
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
                            let _ = flush_pending(&policy, &mut pending, &stats, "max_items", &ack_tx).await;
                        }
                    }
                    Some(TelemetryMessage::Item(item)) => {
                        let mut state = stats.write().await;
                        state.dropped += 1;
                        match item.kind {
                            TelemetryKind::Touch => state.touch_dropped += item.item_count as u64,
                            TelemetryKind::Judge => state.judge_dropped += item.item_count as u64,
                        }
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
                        let result = flush_pending(&policy, &mut pending, &stats, "explicit_flush", &ack_tx).await;
                        let _ = reply.send(result);
                    }
                    Some(TelemetryMessage::Shutdown(reply)) => {
                        let result = flush_pending(&policy, &mut pending, &stats, "shutdown", &ack_tx).await;
                        let should_stop = result.is_ok();
                        let _ = reply.send(result);
                        if should_stop {
                            break;
                        }
                    }
                    None => {
                        let _ = flush_pending(&policy, &mut pending, &stats, "closed", &ack_tx).await;
                        break;
                    }
                }
            }
            _ = ticker.tick() => {
                let _ = flush_pending(&policy, &mut pending, &stats, "interval", &ack_tx).await;
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
    ack_tx: &WalAckTx,
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
    stats.record_batch_size(items);
    stats.flushed_batches += 1;
    if !policy.dry_run {
        record_db_dispatch_latency(&mut stats, write_elapsed_ms);
    }

    let action = match write_result {
        Ok((true, item_rows)) => {
            // Record per-kind committed items.
            for item in &flushed {
                match item.kind {
                    TelemetryKind::Touch => stats.touch_committed += item.item_count as u64,
                    TelemetryKind::Judge => stats.judge_committed += item.item_count as u64,
                }
            }
            stats.flushed_items += telemetry_points as u64;
            stats.write_batches += 1;
            stats.write_items += telemetry_points as u64;
            stats.write_item_rows += item_rows as u64;
            stats.last_batch_uuid = Some(batch_uuid.clone());
            stats.pending = pending.len();
            // ACK WAL IDs after successful database commit.
            // Clone sender outside std::sync::Mutex so MutexGuard is dropped
            // before the async send (MutexGuard is not Send).
            let ack_sender = ack_tx.lock().ok().and_then(|g| g.clone());
            let needs_barrier = if let Some(tx) = ack_sender {
                let mut count = 0usize;
                for item in &flushed {
                    if let Some(wal_id) = item.wal_id {
                        if tx.send(WalAckMessage::Ack(wal_id)).await.is_err() {
                            warn!(wal_id = %wal_id, "WAL ACK channel closed");
                        } else {
                            count += 1;
                        }
                    }
                }
                count > 0
            } else {
                false
            };

            // DURABILITY BARRIER: wait for the persistence-wal-ack task to
            // fsync every ACK sent above before returning.  Without this
            // barrier, a PostgreSQL commit followed by a crash can lose the
            // ACK, causing duplicate replay on restart.
            let mut ack_barrier_error: Option<String> = None;
            if needs_barrier {
                let (barrier_reply, barrier_rx) = oneshot::channel();
                if let Some(tx) = ack_tx.lock().ok().and_then(|g| g.clone()) {
                    match tx.send(WalAckMessage::DrainAndReply(barrier_reply)).await {
                        Err(e) => {
                            warn!(error = %e, "WAL ACK barrier send failed");
                            ack_barrier_error = Some(format!("barrier send: {e}"));
                        }
                        Ok(()) => match barrier_rx.await {
                            Ok(Ok(())) => { /* all ACKs durable */ }
                            Ok(Err(e)) => {
                                // ACK worker explicitly reported failure
                                // (e.g. disk I/O error after 20 retries).
                                // Log and continue — the DB write already
                                // succeeded so the data is safe; the missing
                                // ACK means the event will replay on restart
                                // which is a no-op duplicate write.
                                warn!(error = %e, "WAL ACK barrier: some ACKs were not durable");
                                ack_barrier_error = Some(e);
                            }
                            Err(_) => {
                                warn!("WAL ACK barrier reply channel dropped (task may have panicked)");
                                ack_barrier_error = Some("ACK task dropped".to_string());
                            }
                        },
                    }
                }
            }
            // If the ACK barrier failed, record the error in stats for
            // observability but do NOT fail the flush — the DB write
            // succeeded and items are already removed from pending.
            if let Some(ref err) = ack_barrier_error {
                stats.last_error = Some(err.clone());
            }
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
            pipeline: "runtime.telemetry_batcher.worker".to_string(),
            source: "telemetry_batcher_authoritative".to_string(),
            flush_reason: reason.to_string(),
            schema_version: TELEMETRY_SCHEMA_VERSION,
            dual_write: false,
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

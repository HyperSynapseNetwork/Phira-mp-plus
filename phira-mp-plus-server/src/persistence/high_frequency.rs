//! High-frequency (HF) telemetry writer — bypasses WAL for Touch/Judge data.
//!
//! Touch/Judge data is high-volume and does not need the crash-recovery
//! guarantees that the WAL provides for critical events. This writer:
//!
//! 1. Receives HF items via a bounded channel (backpressure on overflow)
//! 2. Batches by configurable size and flush interval
//! 3. Writes directly to PostgreSQL (bypassing WAL entirely)
//! 4. Retries on failure up to max_retries, then drops batch
//! 5. Tracks atomic counters for observability
//!
//! # Shutdown lifecycle
//!
//! When [`HighFrequencyWriter::shutdown`] is called, a control message is sent
//! to flush all pending items and stop the background task. The same occurs
//! when the last sender is dropped (channel closed).

use crate::db::{DbManager, RuntimeTelemetryBatchRecord};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, error, trace, warn};

// ── Defaults ───────────────────────────────────────────────────────────
const DEFAULT_CHANNEL_CAPACITY: usize = 4096;
const DEFAULT_MAX_BATCH_SIZE: usize = 256;
const DEFAULT_FLUSH_INTERVAL_MS: u64 = 500;
const DEFAULT_MAX_RETRIES: u32 = 3;
const HF_SCHEMA_VERSION: i32 = 3;

// ── HighFrequencyKind ───────────────────────────────────────────────────

/// Classification of a high-frequency telemetry item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighFrequencyKind {
    Touch,
    Judge,
}

impl HighFrequencyKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Touch => "touch",
            Self::Judge => "judge",
        }
    }
}

// ── HighFrequencyConfig ─────────────────────────────────────────────────

/// Configuration for the [`HighFrequencyWriter`].
#[derive(Debug, Clone)]
pub struct HighFrequencyConfig {
    pub channel_capacity: usize,
    pub max_batch_size: usize,
    pub flush_interval_ms: u64,
    pub max_retries: u32,
}

impl Default for HighFrequencyConfig {
    fn default() -> Self {
        Self {
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
            max_batch_size: DEFAULT_MAX_BATCH_SIZE,
            flush_interval_ms: DEFAULT_FLUSH_INTERVAL_MS,
            max_retries: DEFAULT_MAX_RETRIES,
        }
    }
}

// ── HighFrequencyItem ───────────────────────────────────────────────────

/// A single high-frequency telemetry item.
///
/// The `payload` must contain `event_id` (for idempotent INSERT), `room_id`,
/// `count` and `data` (the actual touch/judge points).
#[derive(Debug, Clone)]
pub struct HighFrequencyItem {
    pub kind: HighFrequencyKind,
    pub round_id: String,
    pub user_id: i32,
    pub payload: Value,
    pub created_at_ms: i64,
}

impl HighFrequencyItem {
    /// Extract the idempotency key from the payload.
    pub fn event_id(&self) -> String {
        self.payload
            .get("event_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    /// Extract the optional room_id from the payload.
    pub fn room_id(&self) -> Option<String> {
        self.payload
            .get("room_id")
            .and_then(Value::as_str)
            .map(|s| s.to_string())
    }

    /// Number of telemetry points inside this item.
    pub fn item_count(&self) -> usize {
        self.payload
            .get("count")
            .and_then(Value::as_u64)
            .map(|c| c as usize)
            .unwrap_or_else(|| {
                self.payload
                    .get("data")
                    .and_then(Value::as_array)
                    .map(|a| a.len())
                    .unwrap_or(1)
            })
    }
}

// ── HighFrequencyStats ──────────────────────────────────────────────────

/// Atomic counters for the [`HighFrequencyWriter`].
///
/// All counters use relaxed ordering — they are diagnostic hints, not
/// synchronisation primitives.
#[derive(Debug)]
pub struct HighFrequencyStats {
    /// Total items received via [`enqueue`](HighFrequencyWriter::enqueue).
    pub received: AtomicU64,
    /// Total items committed to the database.
    pub committed: AtomicU64,
    /// Total items dropped after exhausting retries.
    pub dropped: AtomicU64,
    /// Total retry attempts made across all batches.
    pub retrying: AtomicU64,
    /// Timestamp (unix millis) of the oldest unflushed batch item, or 0 if none pending.
    pub oldest_batch_at: AtomicU64,
}

impl HighFrequencyStats {
    /// Take a consistent point-in-time snapshot of the counters.
    pub fn snapshot(&self) -> HighFrequencyStatsSnapshot {
        HighFrequencyStatsSnapshot {
            received: self.received.load(Ordering::Relaxed),
            committed: self.committed.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            retrying: self.retrying.load(Ordering::Relaxed),
            oldest_batch_at: self.oldest_batch_at.load(Ordering::Relaxed),
        }
    }

    /// Reset all counters (used after snapshot extraction for cumulative deltas).
    pub fn reset(&self) {
        self.received.store(0, Ordering::Relaxed);
        self.committed.store(0, Ordering::Relaxed);
        self.dropped.store(0, Ordering::Relaxed);
        self.retrying.store(0, Ordering::Relaxed);
        self.oldest_batch_at.store(0, Ordering::Relaxed);
    }
}

/// Point-in-time copy of [`HighFrequencyStats`]. Exportable over JSON.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HighFrequencyStatsSnapshot {
    pub received: u64,
    pub committed: u64,
    pub dropped: u64,
    pub retrying: u64,
    pub oldest_batch_at: u64,
}

// ── Internal message type ──────────────────────────────────────────────

enum HfMessage {
    Item(HighFrequencyItem),
    Flush(oneshot::Sender<Result<(), String>>),
    Shutdown(oneshot::Sender<Result<(), String>>),
}

// ── HighFrequencyWriter ─────────────────────────────────────────────────

/// High-frequency telemetry writer that bypasses WAL.
///
/// Items are enqueued via [`enqueue`](Self::enqueue), batched in memory, and
/// periodically flushed directly to PostgreSQL.  Flush is triggered either by
/// reaching the configured batch size or by the interval timer.
///
/// # Error handling
///
/// Database write failures are retried up to `max_retries` times with
/// exponential back-off (50 ms, 250 ms).  If all retries are exhausted the
/// batch is **dropped** and the `dropped` counter is incremented.  This is an
/// intentional trade-off: HF data is high-volume, low-value-per-row, and
/// dropping avoids blocking the caller indefinitely.
///
/// # Shutdown
///
/// [`shutdown`](Self::shutdown) sends a control message that flushes any
/// remaining items and stops the background task.  Dropping the last sender
/// has the same effect (the receiver sees `None`).
pub struct HighFrequencyWriter {
    tx: mpsc::Sender<HfMessage>,
    send_gate: Mutex<()>,
    closed: AtomicBool,
    stats: Arc<HighFrequencyStats>,
}

impl HighFrequencyWriter {
    /// Spawn the background writer task and return a handle.
    ///
    /// The task runs until [`shutdown`](Self::shutdown) is called or the
    /// channel is closed.
    pub fn spawn(config: HighFrequencyConfig, db: Arc<DbManager>) -> Self {
        let capacity = config.channel_capacity.max(16);
        let (tx, mut rx) = mpsc::channel::<HfMessage>(capacity);
        let stats = Arc::new(HighFrequencyStats {
            received: AtomicU64::new(0),
            committed: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            retrying: AtomicU64::new(0),
            oldest_batch_at: AtomicU64::new(0),
        });
        let worker_stats = Arc::clone(&stats);
        let worker_db = Arc::clone(&db);

        crate::supervisor_actor::spawn_named("high-frequency-writer", async move {
            run_hf_writer(
                HighFrequencyConfig {
                    channel_capacity: capacity,
                    ..config
                },
                &mut rx,
                worker_stats,
                worker_db,
            )
            .await;
        });

        Self {
            tx,
            send_gate: Mutex::new(()),
            closed: AtomicBool::new(false),
            stats,
        }
    }

    /// Enqueue a single HF item.
    ///
    /// Returns `Err(String)` when the writer is shut down or the channel is
    /// closed.  The item is not sent on error, so the caller may cache or
    /// retry it.
    pub async fn enqueue(&self, item: HighFrequencyItem) -> Result<(), String> {
        let _send_guard = self.send_gate.lock().await;
        if self.closed.load(Ordering::Acquire) {
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            return Err("high frequency writer is shutting down".to_string());
        }
        match self.tx.send(HfMessage::Item(item)).await {
            Ok(()) => {
                self.stats.received.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(mpsc::error::SendError(HfMessage::Item(ref item))) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                warn!(
                    "high frequency writer queue full; item dropped (kind={})",
                    item.kind.as_str()
                );
                Err("high frequency writer queue full or closed".to_string())
            }
            Err(_) => unreachable!("enqueue only sends Item messages"),
        }
    }

    /// Flush all items accepted before this call.
    ///
    /// Waits for the background task to write the current batch to the
    /// database and reply.  Timeout is 5 seconds.
    pub async fn flush(&self) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        {
            let _send_guard = self.send_gate.lock().await;
            if self.closed.load(Ordering::Acquire) {
                return Err("high frequency writer is shutting down".to_string());
            }
            self.tx
                .send(HfMessage::Flush(reply))
                .await
                .map_err(|_| "high frequency writer is closed".to_string())?;
        }
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .map_err(|_| "high frequency flush timed out".to_string())?
            .map_err(|_| "high frequency flush reply dropped".to_string())?
    }

    /// Flush remaining items and stop the background task.
    ///
    /// After shutdown the writer is unusable.  Timeout is 10 seconds.
    pub async fn shutdown(&self) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        {
            let _send_guard = self.send_gate.lock().await;
            if self.closed.swap(true, Ordering::AcqRel) {
                return Ok(());
            }
            if self.tx.send(HfMessage::Shutdown(reply)).await.is_err() {
                self.closed.store(false, Ordering::Release);
                return Err("high frequency writer is closed".to_string());
            }
        }
        let result = match tokio::time::timeout(Duration::from_secs(10), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("high frequency shutdown reply dropped".to_string()),
            Err(_) => Err("high frequency shutdown timed out".to_string()),
        };
        if let Err(ref error) = result {
            self.closed.store(false, Ordering::Release);
            return Err(error.clone());
        }
        result
    }

    /// Reference to the atomic stats counters.
    pub fn stats(&self) -> Arc<HighFrequencyStats> {
        Arc::clone(&self.stats)
    }
}

// ── Background runner ───────────────────────────────────────────────────

/// Unix-millis timestamp helper.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Generate a unique batch identifier for observability.
fn batch_uuid() -> String {
    let ts = now_ms();
    static HF_BATCH_SEQ: AtomicU64 = AtomicU64::new(1);
    let seq = HF_BATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("hf-{ts}-{seq}")
}

/// Convert HF items to the `RuntimeTelemetryBatchRecord` form expected by
/// the existing `record_runtime_telemetry_batches` method.
fn extract_runtime_records(
    batch_id: &str,
    items: &[HighFrequencyItem],
) -> Vec<RuntimeTelemetryBatchRecord> {
    items
        .iter()
        .map(|item| {
            let count = item.item_count();
            RuntimeTelemetryBatchRecord {
                event_id: item.event_id(),
                batch_uuid: batch_id.to_string(),
                run_id: None,
                scope: "production".to_string(),
                pipeline: "runtime.high_frequency.writer".to_string(),
                source: "high_frequency_writer".to_string(),
                flush_reason: "batch".to_string(),
                schema_version: HF_SCHEMA_VERSION,
                dual_write: false,
                kind: item.kind.as_str().to_string(),
                room_id: item.room_id(),
                round_uuid: Some(item.round_id.clone()),
                player_id: item.user_id,
                item_count: i32::try_from(count).unwrap_or(i32::MAX),
                payload: item.payload.clone(),
            }
        })
        .collect()
}

/// Main background loop: receive items, batch, flush.
async fn run_hf_writer(
    config: HighFrequencyConfig,
    rx: &mut mpsc::Receiver<HfMessage>,
    stats: Arc<HighFrequencyStats>,
    db: Arc<DbManager>,
) {
    if !db.is_active() {
        warn!("high frequency writer: database not active; items will be dropped");
        while rx.recv().await.is_some() {
            stats.dropped.fetch_add(1, Ordering::Relaxed);
        }
        return;
    }

    let flush_interval = Duration::from_millis(config.flush_interval_ms.max(100));
    let max_batch_size = config.max_batch_size.max(1);
    let max_retries = config.max_retries.max(1);
    let mut batch: Vec<HighFrequencyItem> = Vec::with_capacity(max_batch_size);
    let mut ticker = tokio::time::interval(flush_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            biased;

            msg = rx.recv() => {
                match msg {
                    Some(HfMessage::Item(item)) => {
                        let created_at = item.created_at_ms as u64;
                        // Track oldest unflushed batch timestamp
                        let old = stats.oldest_batch_at.load(Ordering::Relaxed);
                        if old == 0 || created_at < old {
                            stats.oldest_batch_at.store(created_at, Ordering::Relaxed);
                        }
                        batch.push(item);

                        if batch.len() >= max_batch_size {
                            let _ = flush_batch(
                                &mut batch, &stats, &db, max_retries, "max_items"
                            ).await;
                        }
                    }
                    Some(HfMessage::Flush(reply)) => {
                        let result = flush_batch(
                            &mut batch, &stats, &db, max_retries, "explicit_flush"
                        ).await;
                        let _ = reply.send(result);
                    }
                    Some(HfMessage::Shutdown(reply)) => {
                        let result = flush_batch(
                            &mut batch, &stats, &db, max_retries, "shutdown"
                        ).await;
                        let should_stop = result.is_ok();
                        let _ = reply.send(result);
                        if should_stop {
                            debug!("high frequency writer shut down gracefully");
                        }
                        break;
                    }
                    None => {
                        // Channel closed — flush remaining items and exit.
                        if !batch.is_empty() {
                            let _ = flush_batch(
                                &mut batch, &stats, &db, max_retries, "closed"
                            ).await;
                        }
                        debug!("high frequency writer channel closed, exiting");
                        break;
                    }
                }
            }
            _ = ticker.tick() => {
                if !batch.is_empty() {
                    let _ = flush_batch(
                        &mut batch, &stats, &db, max_retries, "interval"
                    ).await;
                }
            }
        }
    }
}

/// Flush the current batch to the database.
///
/// On failure, retries up to `max_retries` times with backoff.  If all
/// retries are exhausted, the batch is dropped and `stats.dropped` is
/// incremented.
async fn flush_batch(
    batch: &mut Vec<HighFrequencyItem>,
    stats: &Arc<HighFrequencyStats>,
    db: &Arc<DbManager>,
    max_retries: u32,
    reason: &str,
) -> Result<(), String> {
    if batch.is_empty() {
        return Ok(());
    }

    let items = std::mem::take(batch);
    let batch_id = batch_uuid();
    let records = extract_runtime_records(&batch_id, &items);
    let record_count = records.len() as u64;
    let point_count: usize = items.iter().map(|i| i.item_count()).sum();

    // Reset oldest timestamp — will be updated on next item arrival.
    stats.oldest_batch_at.store(0, Ordering::Relaxed);

    for attempt in 0..max_retries {
        if attempt > 0 {
            stats.retrying.fetch_add(1, Ordering::Relaxed);
            let delay = Duration::from_millis(match attempt {
                1 => 50,
                _ => 250,
            });
            tokio::time::sleep(delay).await;
        }

        if db.record_runtime_telemetry_batches(records.clone()).await {
            stats
                .committed
                .fetch_add(record_count, Ordering::Relaxed);
            debug!(
                items = items.len(),
                points = point_count,
                reason,
                "high frequency batch committed"
            );
            return Ok(());
        }

        warn!(
            attempt = attempt + 1,
            max_retries,
            reason,
            "high frequency batch write failed"
        );
    }

    // All retries exhausted — drop the batch.
    stats
        .dropped
        .fetch_add(record_count, Ordering::Relaxed);
    error!(
        items = items.len(),
        points = point_count,
        reason,
        "high frequency batch dropped after {max_retries} retries"
    );
    Err(format!(
        "high frequency batch dropped after {max_retries} retries"
    ))
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_item(kind: HighFrequencyKind, user_id: i32) -> HighFrequencyItem {
        let event_id = uuid::Uuid::new_v4().to_string();
        HighFrequencyItem {
            kind,
            round_id: "round-1".to_string(),
            user_id,
            payload: json!({
                "event_id": event_id,
                "room_id": "room-1",
                "round_id": "round-1",
                "user_id": user_id,
                "count": 3,
                "data": [
                    {"time": 1.0, "x": 0.1, "y": 0.2},
                    {"time": 1.5, "x": 0.3, "y": 0.4},
                    {"time": 2.0, "x": 0.5, "y": 0.6},
                ],
            }),
            created_at_ms: now_ms(),
        }
    }

    #[test]
    fn item_accessors_extract_from_payload() {
        let item = make_item(HighFrequencyKind::Touch, 42);
        assert_eq!(item.event_id().len(), 36);
        assert_eq!(item.room_id(), Some("room-1".to_string()));
        assert_eq!(item.item_count(), 3);
    }

    #[test]
    fn kind_as_str_returns_lowercase() {
        assert_eq!(HighFrequencyKind::Touch.as_str(), "touch");
        assert_eq!(HighFrequencyKind::Judge.as_str(), "judge");
    }

    #[test]
    fn stats_snapshot_is_consistent() {
        let stats = HighFrequencyStats {
            received: AtomicU64::new(10),
            committed: AtomicU64::new(8),
            dropped: AtomicU64::new(2),
            retrying: AtomicU64::new(1),
            oldest_batch_at: AtomicU64::new(12345),
        };
        let snap = stats.snapshot();
        assert_eq!(snap.received, 10);
        assert_eq!(snap.committed, 8);
        assert_eq!(snap.dropped, 2);
        assert_eq!(snap.retrying, 1);
        assert_eq!(snap.oldest_batch_at, 12345);
    }

    #[test]
    fn extract_runtime_records_contains_expected_fields() {
        let items = vec![make_item(HighFrequencyKind::Touch, 42)];
        let records = extract_runtime_records("test-batch", &items);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].player_id, 42);
        assert_eq!(records[0].kind, "touch");
        assert_eq!(records[0].scope, "production");
        assert_eq!(records[0].pipeline, "runtime.high_frequency.writer");
        assert_eq!(records[0].source, "high_frequency_writer");
        assert_eq!(records[0].round_uuid.as_deref(), Some("round-1"));
        assert_eq!(records[0].item_count, 3);
        assert!(!records[0].event_id.is_empty());
    }
}

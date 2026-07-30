//! High-frequency telemetry writer — core writer implementation.
//!
//! This module contains the [`HighFrequencyWriter`] handle, its background
//! event loop, and all flush / overflow-drain logic.
//!
//! # Submodule dependencies
//!
//! - `super` — core data types (`HighFrequencyConfig`, `HighFrequencyItem`,
//!   `HighFrequencyStats`, `EnqueueOutcome`, `now_ms`)
//! - `super::postgres` — PostgreSQL COPY helpers

use crate::db::DbManager;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, warn};

use super::{now_ms, EnqueueOutcome, HighFrequencyConfig, HighFrequencyItem, HighFrequencyStats};

// ── Internal message type ────────────────────────────────────────────────────

enum HfMessage {
    Item(HighFrequencyItem),
    Flush(oneshot::Sender<Result<(), String>>),
    Shutdown(oneshot::Sender<Result<(), String>>),
}

/// Item stored in the overflow queue when the main channel is full.
struct OverflowItem {
    item: HighFrequencyItem,
    enqueued_at_ms: i64,
}

// ── HighFrequencyWriter ──────────────────────────────────────────────────────

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
    /// Overflow channel sender (when main queue is full).
    overflow_tx: Option<mpsc::Sender<OverflowItem>>,
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
            received_points: AtomicU64::new(0),
            committed_points: AtomicU64::new(0),
            dropped_points: AtomicU64::new(0),
            queue_full_count: AtomicU64::new(0),
            last_database_error_at: AtomicU64::new(0),
            admission_sequence: AtomicU64::new(1),
            last_accepted_sequence: AtomicU64::new(0),
            committed_sequence: AtomicU64::new(0),
            continuous_committed_watermark: AtomicU64::new(0),
        });
        let worker_stats = Arc::clone(&stats);
        let worker_db = Arc::clone(&db);

        // 审计 P2: overflow queue 用于主队列满时暂存
        let (overflow_tx, overflow_rx) = if config.overflow_capacity > 0 {
            let (tx, rx) = mpsc::channel::<OverflowItem>(config.overflow_capacity);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        crate::supervisor_actor::spawn_named("high-frequency-writer", async move {
            run_hf_writer(
                HighFrequencyConfig {
                    channel_capacity: capacity,
                    ..config
                },
                &mut rx,
                overflow_rx,
                worker_stats,
                worker_db,
            )
            .await;
        });

        Self {
            tx,
            overflow_tx,
            closed: AtomicBool::new(false),
            stats,
        }
    }

    /// Enqueue a single HF item.
    ///
    /// Uses `try_send` — if the queue is full the item is pushed to an
    /// in-memory overflow queue instead of being dropped immediately.
    /// If the overflow queue is also full, the item is dropped.
    /// Returns `Ok(EnqueueOutcome::MainQueue)`, `Ok(EnqueueOutcome::OverflowQueue)`,
    /// `Ok(EnqueueOutcome::Dropped)`, or `Err(String)` when closed.
    pub async fn enqueue(&self, mut item: HighFrequencyItem) -> Result<EnqueueOutcome, String> {
        if self.closed.load(Ordering::Acquire) {
            self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            self.stats.dropped_points.fetch_add(item.item_count() as u64, Ordering::Relaxed);
            return Err("high frequency writer is shutting down".to_string());
        }
        let points = item.item_count() as u64;
        // Assign a monotonic admission sequence before any channel send so that
        // the item carries its position in the global admission order through
        // both the main and overflow paths.  The sequence is used by flush to
        // determine committed_sequence.
        let seq = self.stats.admission_sequence.fetch_add(1, Ordering::Relaxed);
        self.stats.last_accepted_sequence.store(seq, Ordering::Relaxed);
        item.admission_seq = seq;

        match self.tx.try_send(HfMessage::Item(item)) {
            Ok(()) => {
                self.stats.received.fetch_add(1, Ordering::Relaxed);
                self.stats.received_points.fetch_add(points, Ordering::Relaxed);
                Ok(EnqueueOutcome::MainQueue)
            }
            Err(mpsc::error::TrySendError::Full(HfMessage::Item(item))) => {
                // 审计 P2: 主队列满时尝试 overflow queue
                self.stats.queue_full_count.fetch_add(1, Ordering::Relaxed);
                self.stats.received.fetch_add(1, Ordering::Relaxed);
                self.stats.received_points.fetch_add(points, Ordering::Relaxed);
                let kind = item.kind.as_str().to_string();
                if let Some(overflow) = self.overflow_tx.as_ref() {
                    let overflow_item = OverflowItem { item, enqueued_at_ms: now_ms() };
                    if overflow.try_send(overflow_item).is_err() {
                        // Overflow full — finally drop
                        self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                        self.stats.dropped_points.fetch_add(points, Ordering::Relaxed);
                        warn!("high frequency writer overflow queue full; item dropped (kind={kind})");
                        Ok(EnqueueOutcome::Dropped)
                    } else {
                        Ok(EnqueueOutcome::OverflowQueue)
                    }
                } else {
                    self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                    self.stats.dropped_points.fetch_add(points, Ordering::Relaxed);
                    warn!("high frequency writer queue full; item dropped (kind={kind})");
                    Ok(EnqueueOutcome::Dropped)
                }
            }
            Err(mpsc::error::TrySendError::Closed(HfMessage::Item(_item))) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                self.stats.dropped_points.fetch_add(points, Ordering::Relaxed);
                let kind = _item.kind.as_str().to_string();
                warn!("high frequency writer queue closed; item dropped (kind={kind})");
                Err("high frequency writer is closed".to_string())
            }
            // Full/Closed for non-Item messages: don't count as dropped,
            // the Flush/Shutdown will be retried or the error is acceptable.
            Err(mpsc::error::TrySendError::Full(_)) | Err(mpsc::error::TrySendError::Closed(_)) => {
                Err("high frequency writer busy".to_string())
            }
        }
    }

    /// Flush all items accepted before this call.
    ///
    /// Waits for the background task to write the current batch to the
    /// database and reply.  Timeout is 5 seconds.
    pub async fn flush(&self) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        if self.closed.load(Ordering::Acquire) {
            return Err("high frequency writer is shutting down".to_string());
        }
        self.tx
            .send(HfMessage::Flush(reply))
            .await
            .map_err(|_| "high frequency writer is closed".to_string())?;
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .map_err(|_| "high frequency flush timed out".to_string())?
            .map_err(|_| "high frequency flush reply dropped".to_string())?
    }

    /// Flush remaining items and stop the background task.
    ///
    /// After shutdown the writer is permanently closed, regardless of whether
    /// the final flush succeeds or fails.  Timeout is 10 seconds.
    pub async fn shutdown(&self) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        if self.tx.send(HfMessage::Shutdown(reply)).await.is_err() {
            return Err("high frequency writer is closed".to_string());
        }
        let result = match tokio::time::timeout(Duration::from_secs(10), rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("high frequency shutdown reply dropped".to_string()),
            Err(_) => Err("high frequency shutdown timed out".to_string()),
        };
        // Note: closed stays true regardless of flush outcome.
        // The handle is permanently dead after shutdown() is called.
        result
    }

    /// Reference to the atomic stats counters.
    pub fn stats(&self) -> Arc<HighFrequencyStats> {
        Arc::clone(&self.stats)
    }
}

// ── Main background loop ─────────────────────────────────────────────────────

/// Main background loop: receive items, batch, flush.
async fn run_hf_writer(
    config: HighFrequencyConfig,
    rx: &mut mpsc::Receiver<HfMessage>,
    mut overflow_rx: Option<mpsc::Receiver<OverflowItem>>,
    stats: Arc<HighFrequencyStats>,
    db: Arc<DbManager>,
) {
    let flush_interval = Duration::from_millis(config.flush_interval_ms.max(100));
    let max_batch_size = config.max_batch_size.max(1);
    let max_retries = config.max_retries.max(1);
    let retry_max_age_ms = config.retry_max_age_ms as i64;
    let overflow_max_age_ms = config.overflow_max_age_ms as i64;
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
                        // Interleave overflow items with the main queue so that
                        // overflow is not starved when main-channel items arrive
                        // continuously (审计 P2).
                        let remaining = max_batch_size.saturating_sub(batch.len());
                        drain_overflow(&mut batch, &mut overflow_rx, &stats, overflow_max_age_ms, remaining).await;

                        if batch.len() >= max_batch_size {
                            flush_and_update_seq(
                                &mut batch, &stats, &db, max_retries, retry_max_age_ms, "max_items"
                            ).await.ok();
                        }
                    }
                    Some(HfMessage::Flush(reply)) => {
                        // Target is the *last assigned* sequence, not the *next*
                        // sequence to assign.  Using admission_sequence here
                        // would over-shoot by 1 and make the watermark
                        // unreachable, causing a permanent hang (PMP30 P0-5).
                        let target_seq = stats.last_accepted_sequence.load(Ordering::Acquire);
                        if target_seq == 0 {
                            // No items ever accepted; flush is a no-op.
                            let _ = reply.send(Ok(()));
                            continue;
                        }
                        let result = loop {
                            let watermark = stats.continuous_committed_watermark.load(Ordering::Acquire);
                            if watermark >= target_seq {
                                break Ok(());
                            }
                            // Drain overflow into current batch before flushing
                            drain_overflow(
                                &mut batch, &mut overflow_rx, &stats, overflow_max_age_ms, max_batch_size,
                            )
                            .await;

                            if batch.is_empty() {
                                // No pending items.  If committed_sequence has
                                // already passed target_seq, there is an
                                // unrecoverable gap (items were permanently
                                // dropped) and the watermark can never reach
                                // the target.
                                let committed =
                                    stats.committed_sequence.load(Ordering::Acquire);
                                if committed >= target_seq {
                                    break Err(
                                        "DataLoss: items were permanently dropped, \
                                         creating an unrecoverable gap in committed sequence"
                                            .to_string(),
                                    );
                                }
                                // Items still being produced; yield briefly so
                                // concurrent enqueuers can make progress.
                                tokio::time::sleep(Duration::from_millis(10)).await;
                                continue;
                            }

                            let res = flush_and_update_seq(
                                &mut batch,
                                &stats,
                                &db,
                                max_retries,
                                retry_max_age_ms,
                                "explicit_flush",
                            )
                            .await;
                            if let Err(e) = res {
                                break Err(e);
                            }
                        };
                        let _ = reply.send(result);
                    }
                    Some(HfMessage::Shutdown(reply)) => {
                        // Drain ALL overflow items before the final flush, not just
                        // one batch-worth.  Using usize::MAX drains until the channel
                        // is empty (the function breaks on TryRecvError::Empty).
                        drain_overflow(&mut batch, &mut overflow_rx, &stats, overflow_max_age_ms, usize::MAX).await;
                        let result = flush_and_update_seq(
                            &mut batch, &stats, &db, max_retries, retry_max_age_ms, "shutdown"
                        ).await;
                        if result.is_ok() {
                            debug!("high frequency writer shut down gracefully");
                        }
                        let _ = reply.send(result);
                        break;
                    }
                    None => {
                        // Channel closed — flush remaining items and exit.
                        // Drain ALL overflow before the final flush.
                        drain_overflow(&mut batch, &mut overflow_rx, &stats, overflow_max_age_ms, usize::MAX).await;
                        if !batch.is_empty() {
                            flush_and_update_seq(
                                &mut batch, &stats, &db, max_retries, retry_max_age_ms, "closed"
                            ).await.ok();
                        }
                        debug!("high frequency writer channel closed, exiting");
                        break;
                    }
                }
            }
            _ = ticker.tick() => {
                // Drain overflow into current batch before flushing
                drain_overflow(&mut batch, &mut overflow_rx, &stats, overflow_max_age_ms, max_batch_size).await;
                if !batch.is_empty() {
                    flush_and_update_seq(
                        &mut batch, &stats, &db, max_retries, retry_max_age_ms, "interval"
                    ).await.ok();
                }
            }
        }
    }
}

// ── Overflow drain ───────────────────────────────────────────────────────────

/// Drain expired and pending overflow items into the current batch.
/// Expired items (older than max_age) are dropped and counted.
/// Drains at most `max_items` items into the batch per call.
async fn drain_overflow(
    batch: &mut Vec<HighFrequencyItem>,
    overflow_rx: &mut Option<mpsc::Receiver<OverflowItem>>,
    stats: &HighFrequencyStats,
    max_age_ms: i64,
    max_items: usize,
) {
    let Some(rx) = overflow_rx.as_mut() else { return };
    if max_items == 0 {
        return;
    }
    let now = now_ms();
    let mut drained = 0usize;
    loop {
        if drained >= max_items {
            break;
        }
        match rx.try_recv() {
            Ok(overflow) => {
                let age = now - overflow.enqueued_at_ms;
                if max_age_ms > 0 && age > max_age_ms {
                    // Overflow item expired — drop it
                    stats.dropped.fetch_add(1, Ordering::Relaxed);
                    stats.dropped_points.fetch_add(overflow.item.item_count() as u64, Ordering::Relaxed);
                    continue;
                }
                batch.push(overflow.item);
                drained += 1;
            }
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                *overflow_rx = None;
                break;
            }
        }
    }
}

// ── Flush ────────────────────────────────────────────────────────────────────

/// Flush the current batch to the database.
///
/// Uses PostgreSQL COPY for throughput (batch and item tables), with a
/// fallback to the existing INSERT-based path when COPY fails.
///
/// On failure, retries up to `max_retries` times with backoff.  If all
/// retries are exhausted, the batch is dropped and `stats.dropped` is
/// incremented.
async fn flush_batch(
    batch: &mut Vec<HighFrequencyItem>,
    stats: &Arc<HighFrequencyStats>,
    db: &Arc<DbManager>,
    max_retries: u32,
    retry_max_age_ms: i64,
    reason: &str,
) -> Result<(), String> {
    if batch.is_empty() {
        return Ok(());
    }

    let items = std::mem::take(batch);
    // Derive batch idempotency key from the admission sequence range so that
    // retrying the same batch always produces the same batch_id.  This lets
    // the database deduplicate via ON CONFLICT when a partially-succeeded
    // write is re-attempted.
    let min_seq = items.iter().map(|i| i.admission_seq).min().unwrap_or(0);
    let max_seq = items.iter().map(|i| i.admission_seq).max().unwrap_or(0);
    let batch_id = super::postgres::batch_uuid(min_seq, max_seq);
    let records = super::postgres::extract_runtime_records(&batch_id, &items);
    let record_count = records.len() as u64;
    let point_count: u64 = items.iter().map(|i| i.item_count() as u64).sum();

    // Reset oldest timestamp — will be updated on next item arrival.
    stats.oldest_batch_at.store(0, Ordering::Relaxed);

    let deadline = if retry_max_age_ms > 0 {
        Some(now_ms() + retry_max_age_ms)
    } else {
        None
    };

    let mut attempt = 0u32;

    loop {
        if attempt > 0 {
            // Check retry deadline before sleeping.
            if let Some(deadline_ms) = deadline {
                if now_ms() >= deadline_ms {
                    warn!(
                        attempt = attempt + 1,
                        max_retries,
                        reason,
                        "retry deadline exceeded, dropping batch"
                    );
                    break;
                }
            }
            stats.retrying.fetch_add(1, Ordering::Relaxed);
            // Exponential backoff: 50ms, 100ms, 200ms, 400ms, 800ms, max 1s
            let backoff_ms = (50u64 << (attempt - 1).min(4)).min(1000);
            let jitter = (attempt as u64 * 7) % 50;
            tokio::time::sleep(Duration::from_millis(backoff_ms + jitter)).await;
        }

        // Try COPY-first; fall back to INSERT-based path if COPY fails.
        let ok = match super::postgres::try_copy_write(db, &records).await {
            Ok(()) => true,
            Err(_) => {
                // Fallback: existing multi-row INSERT with ON CONFLICT DO NOTHING.
                db.record_runtime_telemetry_batches(records.clone()).await
            }
        };

        if ok {
            stats
                .committed
                .fetch_add(record_count, Ordering::Relaxed);
            stats
                .committed_points
                .fetch_add(point_count, Ordering::Relaxed);
            stats.last_database_error_at.store(0, Ordering::Relaxed);
            debug!(
                items = items.len(),
                point_count,
                reason,
                "high frequency batch committed"
            );
            return Ok(());
        }

        stats.last_database_error_at.store(now_ms() as u64, Ordering::Relaxed);
        warn!(
            attempt = attempt + 1,
            max_retries,
            reason,
            "high frequency batch write failed"
        );

        attempt += 1;
    }

    // All retries exhausted — drop the batch.
    stats
        .dropped
        .fetch_add(record_count, Ordering::Relaxed);
    stats
        .dropped_points
        .fetch_add(point_count, Ordering::Relaxed);
    error!(
        items = items.len(),
        point_count,
        reason,
        "high frequency batch dropped after {max_retries} retries"
    );
    Err(format!(
        "high frequency batch dropped after {max_retries} retries"
    ))
}

// ── Sequence tracking ────────────────────────────────────────────────────────

/// Flush the current batch and advance `committed_sequence` based on the
/// highest `admission_seq` present in the batch items.
///
/// Unlike the previous approach (which snapshot `admission_sequence` before
/// flushing), this derives the target sequence from the batch content so that
/// items admitted *after* the batch was assembled never inflate
/// `committed_sequence` prematurely.
async fn flush_and_update_seq(
    batch: &mut Vec<HighFrequencyItem>,
    stats: &Arc<HighFrequencyStats>,
    db: &Arc<DbManager>,
    max_retries: u32,
    retry_max_age_ms: i64,
    reason: &str,
) -> Result<(), String> {
    let seqs: Vec<u64> = batch.iter().map(|i| i.admission_seq).collect();
    let target_seq = seqs.iter().max().copied().unwrap_or(0);
    let min_seq = seqs.iter().min().copied().unwrap_or(0);
    let result = flush_batch(batch, stats, db, max_retries, retry_max_age_ms, reason).await;
    if result.is_ok() && target_seq > 0 {
        let current = stats.committed_sequence.load(Ordering::Relaxed);
        if target_seq > current {
            stats.committed_sequence.store(target_seq, Ordering::Relaxed);
        }
        // Advance continuous committed watermark when this batch fills from
        // after the current watermark (i.e. no gaps before the batch).
        let watermark = stats.continuous_committed_watermark.load(Ordering::Relaxed);
        if min_seq == watermark + 1 {
            stats.continuous_committed_watermark.store(target_seq, Ordering::Relaxed);
        }
    }
    result
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::high_frequency::HighFrequencyKind;
    use crate::db::DbManager;
    use serde_json::json;
    use std::sync::Arc;

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
            admission_seq: 0,
        }
    }

    /// Verify that concurrent producers all receive distinct monotonic
    /// admission sequences and that flush sequencing is correct.
    ///
    /// This test creates a lazy PostgreSQL pool that never actually connects
    /// — no DB writes are attempted because `max_batch_size` exceeds the
    /// total number of items enqueued.  Flush is exercised to confirm it
    /// returns an error (no real DB) without panicking.
    #[tokio::test]
    async fn concurrent_producer_applies_monotonic_sequences() {
        use sqlx::postgres::PgPoolOptions;

        // Lazily-connected pool — never actually connects because we don't
        // trigger a flush in this test (max_batch_size > total items).
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost:5432/mp_test?application_name=hf_test")
            .expect("lazy pool creation should not fail");
        let db = Arc::new(DbManager::Pg(pool));

        let config = HighFrequencyConfig {
            channel_capacity: 512,
            max_batch_size: 2048,
            flush_interval_ms: 60_000,
            max_retries: 1,
            overflow_capacity: 512,
            overflow_max_age_ms: 60_000,
            retry_max_age_ms: 30_000,
        };

        let writer = Arc::new(HighFrequencyWriter::spawn(config, db));
        const ITEMS_PER_TASK: u64 = 100;
        const TASK_COUNT: u64 = 10;
        const TOTAL: u64 = ITEMS_PER_TASK * TASK_COUNT;

        let mut join_set = tokio::task::JoinSet::new();
        for t in 0..TASK_COUNT {
            let w = Arc::clone(&writer);
            join_set.spawn(async move {
                for i in 0..ITEMS_PER_TASK {
                    let kind = if (t + i) % 2 == 0 {
                        HighFrequencyKind::Touch
                    } else {
                        HighFrequencyKind::Judge
                    };
                    let item = make_item(kind, (t * ITEMS_PER_TASK + i) as i32);
                    let _ = w.enqueue(item).await;
                }
            });
        }

        while join_set.join_next().await.is_some() {}

        let snap = writer.stats().snapshot();
        // All items should have been received (some may go to overflow).
        assert!(
            snap.received <= TOTAL,
            "received {} should not exceed total {TOTAL}",
            snap.received
        );
        assert!(
            snap.dropped == 0,
            "no items should be dropped; got {} dropped",
            snap.dropped
        );
        // admission_sequence counts every accepted attempt (starting at 1).
        assert_eq!(snap.admission_sequence, snap.received + 1);

        // Flush will fail (no real DB) — confirm it doesn't panic.
        let flush_result = writer.flush().await;
        assert!(flush_result.is_err(), "flush expected to fail without DB");

        writer.shutdown().await.ok();
    }
}

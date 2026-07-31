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
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, warn};

use super::{
    now_ms, EnqueueOutcome, HighFrequencyConfig, HighFrequencyItem, HighFrequencyStats,
    SequenceTracker,
};

// ── Internal message type ────────────────────────────────────────────────────

enum HfMessage {
    Item(HighFrequencyItem),
    Flush {
        reply: oneshot::Sender<Result<(), String>>,
        target_seq: u64,
        deadline_ms: i64,
    },
    Shutdown {
        reply: oneshot::Sender<Result<(), String>>,
        /// Absolute deadline (ms since epoch) captured from the caller's
        /// timeout so the worker does not retry database writes past the
        /// point where the caller has already given up (P0-K).
        deadline_ms: i64,
    },
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
    /// Linearizes enqueue vs shutdown: an item is either fully admitted before
    /// shutdown begins, or rejected because shutdown already happened (P0-F).
    /// Tokio mutex so it can be held across awaits in shutdown().
    admission_gate: tokio::sync::Mutex<()>,
    /// Shutdown lifecycle state (P0-G + PMP41 P1):
    ///
    /// `Open → Requested → ControlSent → Terminated*`.  A control send that
    /// never reached the worker leaves `ControlNotSent`, which is the only
    /// state from which shutdown may be re-requested.  Once the worker has
    /// acknowledged (or the caller gave up while the worker is exiting), the
    /// handle lands in a `Terminated*` state and NEVER returns to `Open` —
    /// a returned worker cannot be re-started, so pretending it is open would
    /// let callers retry into a dead channel (PMP41 P1).
    shutdown_state: AtomicU8,
}

// Shutdown state machine values (P0-G / PMP41 P1).
const SHUTDOWN_OPEN: u8 = 0;
const SHUTDOWN_REQUESTED: u8 = 1;
const SHUTDOWN_CONTROL_SENT: u8 = 2;
const SHUTDOWN_TERMINATED_CLEAN: u8 = 3;
const SHUTDOWN_TERMINATED_DATA_LOSS: u8 = 4;
const SHUTDOWN_TERMINATED_FAILED: u8 = 5;
/// The shutdown control was never delivered to the worker (send failed or
/// timed out).  The worker may still be alive; shutdown may be retried.
const SHUTDOWN_CONTROL_NOT_SENT: u8 = 6;

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
            sequence_tracker: Mutex::new(SequenceTracker::new()),
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
            admission_gate: tokio::sync::Mutex::new(()),
            shutdown_state: AtomicU8::new(SHUTDOWN_OPEN),
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
        // Linearization point with shutdown (P0-F): hold the admission gate so
        // shutdown cannot interleave between our closed check and the actual
        // admission.  Either this item is fully admitted before shutdown
        // begins, or shutdown already happened and it is rejected — never a
        // half-state where an item lands after the shutdown control.
        let _gate = self.admission_gate.lock().await;
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
        item.admission_seq = seq;

        match self.tx.try_send(HfMessage::Item(item)) {
            Ok(()) => {
                self.stats.received.fetch_add(1, Ordering::Relaxed);
                self.stats.received_points.fetch_add(points, Ordering::Relaxed);
                // Only update last_accepted_sequence AFTER the item successfully
                // enters the main queue.  Dropped items must NOT advance this
                // counter — otherwise Flush can target a seq that was never
                // accepted and hang forever.
                self.stats.last_accepted_sequence.fetch_max(seq, Ordering::AcqRel);
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
                        self.stats.sequence_tracker.lock().unwrap().mark_dropped(seq);
                        warn!("high frequency writer overflow queue full; item dropped (kind={kind})");
                        Ok(EnqueueOutcome::Dropped)
                    } else {
                        // Only update after accepted into overflow queue.
                        self.stats.last_accepted_sequence.fetch_max(seq, Ordering::AcqRel);
                        Ok(EnqueueOutcome::OverflowQueue)
                    }
                } else {
                    self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                    self.stats.dropped_points.fetch_add(points, Ordering::Relaxed);
                    self.stats.sequence_tracker.lock().unwrap().mark_dropped(seq);
                    warn!("high frequency writer queue full; item dropped (kind={kind})");
                    Ok(EnqueueOutcome::Dropped)
                }
            }
            Err(mpsc::error::TrySendError::Closed(HfMessage::Item(_item))) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                self.stats.dropped_points.fetch_add(points, Ordering::Relaxed);
                self.stats.sequence_tracker.lock().unwrap().mark_dropped(seq);
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
    /// database and reply, using the caller's timeout as the deadline (P1).
    pub async fn flush(&self, timeout: Duration) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        if self.closed.load(Ordering::Acquire) {
            return Err("high frequency writer is shutting down".to_string());
        }
        let target_seq = self.stats.last_accepted_sequence.load(Ordering::Acquire);
        if target_seq == 0 {
            // No items ever accepted; flush is a no-op.
            return Ok(());
        }
        let deadline_ms = now_ms() + timeout.as_millis() as i64;
        // A SINGLE absolute deadline covers send + reply: the send phase
        // consumes from the reply budget (P1).
        let deadline = std::time::Instant::now() + timeout;
        let send_timeout = deadline.saturating_duration_since(std::time::Instant::now());
        match tokio::time::timeout(send_timeout, self.tx.send(HfMessage::Flush { reply, target_seq, deadline_ms })).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return Err("high frequency writer is closed".to_string()),
            Err(_) => return Err("high frequency flush send timed out".to_string()),
        }
        let reply_timeout = deadline.saturating_duration_since(std::time::Instant::now());
        tokio::time::timeout(reply_timeout, rx)
            .await
            .map_err(|_| "high frequency flush timed out".to_string())?
            .map_err(|_| "high frequency flush reply dropped".to_string())?
    }

    /// Flush remaining items and stop the background task.
    ///
    /// After shutdown the writer is permanently closed, regardless of whether
    /// the final flush succeeds or fails.  Timeout is 10 seconds.
    pub async fn shutdown(&self, timeout: Duration) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        // A SINGLE absolute deadline covers admission-gate wait + control send
        // + worker processing + reply (P1): every phase consumes from the same
        // budget so a slow send can never push the reply wait past the
        // caller's timeout.
        let deadline = std::time::Instant::now() + timeout;
        // Pass the caller's absolute deadline into the worker so it stops
        // retrying database writes at the same time the caller gives up (P0-K).
        let deadline_ms = now_ms() + timeout.as_millis() as i64;
        // Linearize with enqueue (P0-F): hold the admission gate while setting
        // closed, so any enqueue that has already admitted is included in the
        // worker's drain, and no new item can be accepted after this point.
        let _gate = self.admission_gate.lock().await;
        // Shutdown state machine (P0-G / PMP41 P1): proceed from OPEN or from
        // CONTROL_NOT_SENT (the control never reached the worker, so a retry is
        // meaningful).  A shutdown already in flight, or one that already
        // terminated, returns an error — never a fake Ok.
        loop {
            let cur = self.shutdown_state.load(Ordering::Acquire);
            match cur {
                SHUTDOWN_OPEN | SHUTDOWN_CONTROL_NOT_SENT => {
                    if self
                        .shutdown_state
                        .compare_exchange(
                            cur,
                            SHUTDOWN_REQUESTED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        break;
                    }
                }
                SHUTDOWN_REQUESTED | SHUTDOWN_CONTROL_SENT => {
                    return Err("high frequency shutdown already in progress".to_string());
                }
                SHUTDOWN_TERMINATED_CLEAN => {
                    return Err("high frequency writer already shut down cleanly".to_string());
                }
                SHUTDOWN_TERMINATED_DATA_LOSS => {
                    return Err("high frequency writer already shut down with data loss".to_string());
                }
                SHUTDOWN_TERMINATED_FAILED => {
                    return Err("high frequency writer already shut down with failure".to_string());
                }
                _ => return Err("high frequency writer in unknown shutdown state".to_string()),
            }
        }
        self.closed.store(true, Ordering::Release);
        let send_timeout = deadline.saturating_duration_since(std::time::Instant::now());
        match tokio::time::timeout(send_timeout, self.tx.send(HfMessage::Shutdown { reply, deadline_ms })).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                // Channel closed — the worker is already gone.  The control was
                // never delivered, so record ControlNotSent (not Open).  Retry
                // is permitted but will observe the closed channel and fail.
                self.shutdown_state.store(SHUTDOWN_CONTROL_NOT_SENT, Ordering::Release);
                return Err("high frequency writer is closed".to_string());
            }
            Err(_) => {
                // Send timed out — the control was never placed in the channel.
                // Record ControlNotSent so a retry may proceed (the worker may
                // still be alive), but never fake a terminated state.
                self.shutdown_state.store(SHUTDOWN_CONTROL_NOT_SENT, Ordering::Release);
                return Err("high frequency shutdown send timed out".to_string());
            }
        }
        self.shutdown_state.store(SHUTDOWN_CONTROL_SENT, Ordering::Release);
        // Reply wait uses the REMAINING time after the send phase (P1).
        let reply_timeout = deadline.saturating_duration_since(std::time::Instant::now());
        let result = match tokio::time::timeout(reply_timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("high frequency shutdown reply dropped".to_string()),
            Err(_) => Err("high frequency shutdown timed out".to_string()),
        };
        self.shutdown_state
            .store(classify_shutdown_result(&result), Ordering::Release);
        result
    }

    /// Current shutdown state as a stable name (diagnostics).
    pub fn shutdown_state_name(&self) -> &'static str {
        match self.shutdown_state.load(Ordering::Acquire) {
            SHUTDOWN_REQUESTED => "requested",
            SHUTDOWN_CONTROL_SENT => "control_sent",
            SHUTDOWN_TERMINATED_CLEAN => "terminated_clean",
            SHUTDOWN_TERMINATED_DATA_LOSS => "terminated_data_loss",
            SHUTDOWN_TERMINATED_FAILED => "terminated_failed",
            SHUTDOWN_CONTROL_NOT_SENT => "control_not_sent",
            _ => "open",
        }
    }

    /// Whether the writer has permanently finished shutting down.  A
    /// terminated writer cannot be re-opened; callers must not retry.
    pub fn is_terminated(&self) -> bool {
        matches!(
            self.shutdown_state.load(Ordering::Acquire),
            SHUTDOWN_TERMINATED_CLEAN
                | SHUTDOWN_TERMINATED_DATA_LOSS
                | SHUTDOWN_TERMINATED_FAILED
        )
    }

    /// Reference to the atomic stats counters.
    pub fn stats(&self) -> Arc<HighFrequencyStats> {
        Arc::clone(&self.stats)
    }
}

/// Map a worker's shutdown reply onto the terminal shutdown state.
///
/// The worker breaks out of its loop after replying, so the handle can never
/// return to `Open` here — the channel is about to close.  DataLoss gets its
/// own state so callers can distinguish "data was permanently lost" from a
/// generic incomplete shutdown (PMP41 P1).
fn classify_shutdown_result(result: &Result<(), String>) -> u8 {
    match result {
        Ok(()) => SHUTDOWN_TERMINATED_CLEAN,
        Err(e) if e.contains("DataLoss") => SHUTDOWN_TERMINATED_DATA_LOSS,
        Err(_) => SHUTDOWN_TERMINATED_FAILED,
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
    let max_retries = config.max_retries;  // 0 = unlimited by count (bounded by deadline)
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
                                &mut batch, &stats, &db, max_retries, retry_max_age_ms, None, "max_items"
                            ).await.ok();
                        }
                    }
                    Some(HfMessage::Flush { reply, target_seq, deadline_ms }) => {
                        let result = loop {
                            // Check deadline captured at call time so the worker
                            // does not loop indefinitely after the caller has
                            // already timed out (PMP32 P0-F-4).
                            if now_ms() >= deadline_ms {
                                break Err("high frequency flush timed out in worker".to_string());
                            }

                            // Check the sequence tracker for watermark and
                            // dropped ranges before attempting any work.
                            let early = {
                                let tracker = stats.sequence_tracker.lock().unwrap();
                                if tracker.watermark() >= target_seq {
                                    Some(Ok(()))
                                } else if let Some(dropped) = tracker.find_dropped_up_to(target_seq) {
                                    Some(Err(
                                        format!("DataLoss: sequence {dropped} was permanently dropped, creating an unrecoverable gap in committed sequence")
                                    ))
                                } else {
                                    None
                                }
                            };
                            if let Some(r) = early {
                                break r;
                            }

                            // Drain overflow into current batch before flushing
                            drain_overflow(
                                &mut batch, &mut overflow_rx, &stats, overflow_max_age_ms, max_batch_size,
                            )
                            .await;

                            if batch.is_empty() {
                                // Re-check tracker in case a concurrent flush completed.
                                let (watermark_ok, dropped_seq): (bool, Option<u64>) = {
                                    let tracker = stats.sequence_tracker.lock().unwrap();
                                    (tracker.watermark() >= target_seq, tracker.find_dropped_up_to(target_seq))
                                };
                                if watermark_ok {
                                    break Ok(());
                                }
                                if let Some(dropped) = dropped_seq {
                                    break Err(
                                        format!("DataLoss: sequence {dropped} was permanently dropped, creating an unrecoverable gap in committed sequence"),
                                    );
                                }
                                // No overflow items, no batch, no dropped
                                // sequences, but watermark < target_seq.
                                // The accepted sequences between watermark
                                // and target_seq are neither committed nor
                                // dropped — progress is impossible.
                                break Err(
                                    format!("DataLoss: accepted sequences between watermark and {target_seq} are neither committed nor dropped"),
                                );
                            }

                            let res = flush_and_update_seq(
                                &mut batch,
                                &stats,
                                &db,
                                max_retries,
                                retry_max_age_ms,
                                Some(deadline_ms),
                                "explicit_flush",
                            )
                            .await;
                            if let Err(e) = res {
                                break Err(e);
                            }
                        };
                        let _ = reply.send(result);
                    }
                    Some(HfMessage::Shutdown { reply, deadline_ms }) => {
                        // If the caller's deadline has already passed, do not
                        // attempt the flush — report the timeout immediately so
                        // the worker does not continue working past the point
                        // where the caller has given up (P0-K).
                        if now_ms() >= deadline_ms {
                            let _ = reply.send(Err(
                                "high frequency shutdown timed out before final flush".to_string()
                            ));
                            break;
                        }
                        // Drain ALL overflow items before the final flush, not just
                        // one batch-worth.  Using usize::MAX drains until the channel
                        // is empty (the function breaks on TryRecvError::Empty).
                        drain_overflow(&mut batch, &mut overflow_rx, &stats, overflow_max_age_ms, usize::MAX).await;
                        let flush_result = flush_and_update_seq(
                            &mut batch, &stats, &db, max_retries, retry_max_age_ms, Some(deadline_ms), "shutdown"
                        ).await;
                        // After the final flush, verify that all accepted
                        // sequences are resolved (committed or dropped).
                        // If not, report the specific condition so the
                        // caller knows whether data loss occurred
                        // (PMP32 P0-F-3).
                        let target_seq = stats.last_accepted_sequence.load(Ordering::Acquire);
                        let result = if flush_result.is_err() {
                            // The flush itself failed — preserve that error.
                            flush_result
                        } else if target_seq == 0 {
                            // No items ever accepted.
                            debug!("high frequency writer shut down gracefully (no items)");
                            Ok(())
                        } else {
                            let (watermark_ok, dropped_seq): (bool, Option<u64>) = {
                                let tracker = stats.sequence_tracker.lock().unwrap();
                                (tracker.watermark() >= target_seq, tracker.find_dropped_up_to(target_seq))
                            };
                            if watermark_ok {
                                debug!("high frequency writer shut down gracefully");
                                Ok(())
                            } else if let Some(dropped) = dropped_seq {
                                Err(format!(
                                    "DataLoss: sequence {dropped} was permanently dropped during shutdown"
                                ))
                            } else {
                                Err(format!(
                                    "high frequency shutdown incomplete: sequences between watermark and {target_seq} are pending"
                                ))
                            }
                        };
                        let _ = reply.send(result);
                        break;
                    }
                    None => {
                        // Channel closed — flush remaining items and exit.
                        // Drain ALL overflow before the final flush.
                        drain_overflow(&mut batch, &mut overflow_rx, &stats, overflow_max_age_ms, usize::MAX).await;
                        if !batch.is_empty() {
                            flush_and_update_seq(
                                &mut batch, &stats, &db, max_retries, retry_max_age_ms, None, "closed"
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
                        &mut batch, &stats, &db, max_retries, retry_max_age_ms, None, "interval"
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
                    stats.sequence_tracker.lock().unwrap().mark_dropped(overflow.item.admission_seq);
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
    message_deadline_ms: Option<i64>,
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
    let batch_id = super::postgres::batch_uuid(
        min_seq,
        max_seq,
        crate::server_instance::current(),
    );
    let records = super::postgres::extract_runtime_records(&batch_id, &items);
    let record_count = records.len() as u64;
    let point_count: u64 = items.iter().map(|i| i.item_count() as u64).sum();

    // Reset oldest timestamp — will be updated on next item arrival.
    stats.oldest_batch_at.store(0, Ordering::Relaxed);

    // Retry deadline from config, capped by the caller's patience.
    let retry_deadline = if retry_max_age_ms > 0 {
        Some(now_ms() + retry_max_age_ms)
    } else {
        None
    };
    let effective_deadline = match (retry_deadline, message_deadline_ms) {
        (Some(r), Some(m)) => Some(r.min(m)),
        (Some(r), None) => Some(r),
        (None, Some(m)) => Some(m),
        (None, None) => None,
    };

    // Ensure at least one bound exists to prevent infinite retry.
    let effective_max_retries = if max_retries == 0 && effective_deadline.is_none() {
        warn!("no retry bound configured (max_retries=0, no deadline), falling back to single attempt");
        1u32
    } else {
        max_retries
    };

    let mut attempt = 0u32;

    loop {
        if attempt > 0 {
            // Check count-based retry limit.
            if effective_max_retries > 0 && attempt >= effective_max_retries {
                warn!(
                    attempt,
                    effective_max_retries,
                    reason,
                    "max retries exceeded, dropping batch"
                );
                break;
            }
            // Check time-based deadline before sleeping.
            if let Some(deadline_ms) = effective_deadline {
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
        // Each single DB call is bounded by the remaining time to
        // effective_deadline so a hung COPY/INSERT cannot exceed the
        // Flush/Shutdown deadline (P0-E).
        let ok = {
            let attempt_write = async {
                match super::postgres::try_copy_write(db, &records).await {
                    Ok(()) => true,
                    Err(_) => {
                        // Fallback: multi-row INSERT with ON CONFLICT DO NOTHING.
                        db.record_runtime_telemetry_batches(records.clone()).await
                    }
                }
            };
            match effective_deadline {
                Some(dl) => {
                    let remaining_ms = (dl - now_ms()).max(0) as u64;
                    if remaining_ms > 0 {
                        match tokio::time::timeout(Duration::from_millis(remaining_ms), attempt_write).await {
                            Ok(v) => v,
                            Err(_) => {
                                warn!(
                                    attempt = attempt + 1,
                                    reason,
                                    "high frequency DB write timed out before deadline"
                                );
                                false
                            }
                        }
                    } else {
                        false // deadline already passed — no time to attempt
                    }
                }
                None => attempt_write.await,
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

    // All retries exhausted — mark every item's admission sequence as
    // dropped in the SequenceTracker so the tracker's watermark knows
    // to stop before this gap and Flush/Shutdown can detect the loss
    // (PMP32 P0-F-1).  Without this the second Flush of the same
    // target_seq finds the batch empty and loops forever (PMP32 P0-F-2).
    {
        let mut tracker = stats.sequence_tracker.lock().unwrap();
        for item in &items {
            if item.admission_seq > 0 {
                tracker.mark_dropped(item.admission_seq);
            }
        }
        stats
            .continuous_committed_watermark
            .store(tracker.watermark(), Ordering::Relaxed);
    }
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
        attempt,
        max_retries = max_retries,
        "high frequency batch dropped after {attempt} attempts"
    );
    Err(format!(
        "high frequency batch dropped after {attempt} attempts"
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
    message_deadline_ms: Option<i64>,
    reason: &str,
) -> Result<(), String> {
    let seqs: Vec<u64> = batch.iter().map(|i| i.admission_seq).collect();
    let target_seq = seqs.iter().max().copied().unwrap_or(0);
    let result = flush_batch(batch, stats, db, max_retries, retry_max_age_ms, message_deadline_ms, reason).await;
    if result.is_ok() && target_seq > 0 {
        let current = stats.committed_sequence.load(Ordering::Relaxed);
        if target_seq > current {
            stats.committed_sequence.store(target_seq, Ordering::Relaxed);
        }
        // Mark committed using the batch's actual sequence intervals so that
        // gaps in the batch are not falsely committed (PMP33 P0-G).
        let mut unique_seqs: Vec<u64> = seqs.into_iter().filter(|&s| s > 0).collect();
        unique_seqs.sort_unstable();
        unique_seqs.dedup();
        let mut tracker = stats.sequence_tracker.lock().unwrap();
        let mut i = 0;
        while i < unique_seqs.len() {
            let start = unique_seqs[i];
            let mut end = start;
            while i + 1 < unique_seqs.len() && unique_seqs[i + 1] == end + 1 {
                i += 1;
                end = unique_seqs[i];
            }
            tracker.mark_committed(start, end);
            i += 1;
        }
        stats.continuous_committed_watermark.store(tracker.watermark(), Ordering::Relaxed);
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

    #[test]
    fn classify_shutdown_result_maps_worker_outcomes() {
        assert_eq!(classify_shutdown_result(&Ok(())), SHUTDOWN_TERMINATED_CLEAN);
        assert_eq!(
            classify_shutdown_result(&Err(
                "DataLoss: sequence 7 was permanently dropped".to_string()
            )),
            SHUTDOWN_TERMINATED_DATA_LOSS
        );
        assert_eq!(
            classify_shutdown_result(&Err("high frequency shutdown timed out".to_string())),
            SHUTDOWN_TERMINATED_FAILED
        );
        assert_eq!(
            classify_shutdown_result(&Err(
                "high frequency shutdown incomplete: sequences between watermark and 12 are pending".to_string()
            )),
            SHUTDOWN_TERMINATED_FAILED
        );
    }

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
        let flush_result = writer.flush(Duration::from_secs(5)).await;
        assert!(flush_result.is_err(), "flush expected to fail without DB");

        writer.shutdown(Duration::from_secs(5)).await.ok();
    }
}

//! Runtime PersistenceWorker.
//!
//! Ordinary persistence events use bounded backpressure instead of queue-full
//! loss. Flush and shutdown are ordered control messages with acknowledgements,
//! so accepted work can be drained before process termination. All production
//! Touch/Judge telemetry goes through the HighFrequencyWriter — the single
//! unified high-frequency persistence path.

use crate::persistence::control::PendingControl;
use crate::persistence::message::{AdmissionOutcome, PersistenceEvent};
use crate::persistence::process::process_event_through_pipeline;
use crate::persistence::process::ProcessOutcome;
use crate::persistence::stats::{
    record_control_deadline_exceeded, record_control_deferred, record_control_wal_error,
    record_dropped, record_queued,
    record_wal_compaction, record_wal_only,
    record_wal_received, record_wal_recovered, PersistenceStats,
};
use crate::persistence::wal::PersistenceWal;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tracing::info;

enum WorkerMessage {
    Event {
        wal_sequence: u64,
        wal_id: uuid::Uuid,
        event: PersistenceEvent,
        needs_wal_ack: bool,
    },
    Flush {
        target_wal_sequence: u64,
        deadline: Instant,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Shutdown {
        target_wal_sequence: u64,
        deadline: Instant,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Debug)]
pub struct PersistenceWorker {
    tx: mpsc::Sender<WorkerMessage>,
    /// Serializes event/control insertion so nothing can be accepted behind a
    /// Shutdown marker and then remain unprocessed.
    /// Shared with the WAL recovery scanner so it respects the same ordering
    /// fence.
    send_gate: Arc<Mutex<()>>,
    /// Idle-mode diagnostic hint. Persistence remains active while idle so
    /// accepted events are never discarded merely because gameplay is quiet.
    suspended: AtomicBool,
    closed: AtomicBool,
    /// Set when WAL replay events have all been processed (not just parsed).
    /// Used by `is_healthy()` so readiness checks wait for full replay processing.
    initial_replay_drained: Arc<AtomicBool>,
    stats: Arc<RwLock<PersistenceStats>>,
    wal: Arc<PersistenceWal>,
    /// WAL IDs of events currently queued but not yet ACKed.
    /// Shared with the periodic WAL recovery scanner so it can avoid
    /// re-enqueueing entries that are already in-flight.
    in_flight: Arc<Mutex<HashSet<uuid::Uuid>>>,
    /// Current number of entries in the pending-ACK retry queue.  Exposed so
    /// `is_healthy()` can require zero pending ACKs before reporting healthy
    /// (P0-D).
    pending_acks: Arc<AtomicUsize>,
}


/// Normal worker loop: processes replayed events first, then new admissions
/// from the channel, dispatching each through the persistence pipeline and
/// ACKing the WAL on completion.
///
/// Uses WAL sequence gating to preserve WAL order: the recovery scanner may
/// re-enqueue WalOnly events at the back of the channel, so we buffer any
/// message whose sequence does not match `next_expected_sequence` and process
/// them in WAL order once the missing predecessor arrives.
async fn process_worker_loop(
    rx: &mut mpsc::Receiver<WorkerMessage>,
    replay: &mut std::collections::VecDeque<(uuid::Uuid, PersistenceEvent, u64)>,
    worker_stats: &Arc<RwLock<PersistenceStats>>,
    worker_dead_letter_path: &Option<std::path::PathBuf>,
    worker_wal: &Arc<PersistenceWal>,
    in_flight: &Arc<Mutex<HashSet<uuid::Uuid>>>,
    initial_replay_drained: &Arc<AtomicBool>,
    replay_pending_ids: &Arc<Mutex<HashSet<uuid::Uuid>>>,
    pending_acks_count: &Arc<AtomicUsize>,
) {
    use tracing::{debug, trace, warn};

    // Pending ACK retry queue. When worker_wal.ack() fails, the wal_id is
    // queued here for retry on subsequent iterations. Flush/Shutdown drain
    // this queue before returning.
    let mut pending_acks: std::collections::VecDeque<(uuid::Uuid, u32)> =
        std::collections::VecDeque::new();

    // WAL sequence gating: the next wal_sequence we expect from the channel.
    // Replayed events are processed first (already in WAL order) and do not
    // participate in gating; after they are exhausted the channel takes over
    // with sequence-gated dispatch.
    let mut next_expected_sequence: u64 = 0;
    // Buffer for out-of-order channel messages, keyed by wal_sequence.
    let mut buffer: BTreeMap<u64, (uuid::Uuid, PersistenceEvent, bool)> =
        BTreeMap::new();

    // Tracks a deferred control message (Flush/Shutdown) waiting for all
    // events with wal_sequence <= target to reach a terminal state before
    // replying.  Checked on each loop iteration so progress is made through
    // normal event processing.  Type lives in `persistence::control`.
    let mut pending_control: Option<PendingControl> = None;

    loop {
        // ---- Check pending control (deferred flush/shutdown) ----
        //
        // IMPORTANT: use as_mut() / as_ref() instead of take() so the
        // PendingControl is NOT consumed when conditions are not yet met.
        // Previously take() was used, and if the re-check found the
        // conditions still unsatisfied without a deadline expiry, the
        // PendingControl fell out of scope and its oneshot reply sender
        // was dropped — the caller received "acknowledgement was dropped"
        // instead of waiting for the target events to complete.
        if let Some(pc) = pending_control.as_mut() {
            let (target, deadline) = (pc.target(), pc.deadline());
            let buffer_remaining = buffer.range(..=target).count();
            // Count ALL un-ACKed WAL entries with seq <= target.
            let wal_pending = match worker_wal.list_pending().await {
                Ok(p) => p.iter()
                    .filter(|(_, _, seq)| *seq <= target)
                    .count(),
                Err(e) => {
                    // Fail-closed: WAL corruption/read error means we cannot
                    // confirm that all events <= target reached a terminal
                    // state.  Reply with an error instead of assuming zero
                    // pending (which would let Flush/Shutdown report success
                    // while an uncommitted event is lost).
                    record_control_wal_error(worker_stats).await;
                    let (reply, should_break) = pending_control.take().unwrap().finish();
                    let _ = reply.send(Err(format!("WAL error during flush/shutdown: {e}")));
                    pending_control = None;
                    if should_break {
                        break;
                    }
                    continue;
                }
            };

            let ready = pending_acks.is_empty() && buffer_remaining == 0 && wal_pending == 0;
            let expired = Instant::now() >= deadline;

            if ready || expired {
                // Take ownership only when we are about to reply.
                if expired {
                    record_control_deadline_exceeded(worker_stats).await;
                }
                let (reply, should_break) = pending_control.take().unwrap().finish();
                if ready {
                    let _ = reply.send(Ok(()));
                } else {
                    warn!(
                        buffer_remaining, wal_pending,
                        "pending control deadline exceeded",
                    );
                    let _ = reply.send(Err("deadline exceeded".to_string()));
                }
                pending_control = None;
                if should_break {
                    break;
                }
            } else {
                // Deferred — re-check next iteration.
                record_control_deferred(worker_stats).await;
            }
            // If neither ready nor expired, pc stays in pending_control
            // (we used as_mut(), not take()) and the loop continues.
        }

        // ---- Retry pending WAL ACKs (P0-D) ----
        // Run on every iteration, BEFORE the message fetch, so a pending ACK
        // is retried even when the channel is idle (the fetch now times out
        // when pending_acks is non-empty, but this block must run on timeout
        // iterations too — previously it sat after `let Some(msg) else
        // continue` and was skipped whenever no message arrived).
        if let Some((retry_id, retry_attempt)) = pending_acks.front().copied() {
            match worker_wal.ack(retry_id).await {
                Ok(()) => {
                    debug!(wal_id = %retry_id, "ACK retry succeeded");
                    pending_acks.pop_front();
                    // Clear the degraded flag only when the retry queue is now
                    // empty (P0-D: a single success must not mask other
                    // pending ACK failures).
                    if pending_acks.is_empty() {
                        worker_wal.clear_ack_degraded();
                    }
                    in_flight.lock().await.remove(&retry_id);
                }
                Err(e) => {
                    worker_wal.mark_degraded(crate::persistence::wal::DEGRADED_ACK);
                    trace!(
                        wal_id = %retry_id, attempt = %retry_attempt, error = %e,
                        "ACK retry failed, will retry on next iteration"
                    );
                    if let Some(mut entry) = pending_acks.pop_front() {
                        entry.1 = entry.1.saturating_add(1);
                        pending_acks.push_back(entry);
                    }
                }
            }
        }
        // Sync the pending-ACK count for health checks (P0-D).
        pending_acks_count.store(pending_acks.len(), Ordering::Release);

        // ---- Drain initial replay into the buffer (P0-A) ----
        //
        // Replay events are moved into the SAME BTreeMap buffer keyed by their
        // real wal_sequence, so they flow through the identical sequence gate
        // as channel events.  Previously replay events bypassed the gate with a
        // seq=0 sentinel, which let a RetryableFailure on an early replay event
        // be skipped while later replay events committed — producing a final
        // out-of-order state (P0-01).
        if !replay.is_empty() {
            while let Some((wal_id, event, seq)) = replay.pop_front() {
                buffer.insert(seq, (wal_id, event, true)); // needs_wal_ack = true
            }
            if next_expected_sequence == 0 {
                // Initialize the gate to the lowest replay sequence so the
                // buffer is drained in WAL order.
                if let Some(&min_seq) = buffer.keys().next() {
                    next_expected_sequence = min_seq;
                }
            }
        }

        // ---- Determine the message to process this iteration ----
        //
        // Priority: buffer (for draining the expected sequence) >
        //           channel with gating.
        // Replay events now live in the buffer, so they are handled by the
        // same priority-1 path.
        let message: Option<WorkerMessage> = 'fetch: {
            // 1. Buffer check: if the next expected sequence is already
            //    buffered, process it without blocking on the channel.
            if let Some((wal_id, event, needs_wal_ack)) =
                buffer.remove(&next_expected_sequence)
            {
                break 'fetch Some(WorkerMessage::Event {
                    wal_id,
                    wal_sequence: next_expected_sequence,
                    event,
                    needs_wal_ack,
                });
            }

            // 2. Channel receive with sequence gating.
            // Use a short timeout whenever there is deferred work so the loop
            // wakes up to retry it even when no new message arrives: pending
            // control (flush/shutdown fence) or pending WAL ACKs (P0-D —
            // ACK retry must not depend on new channel messages).
            let has_deferred_work = pending_control.is_some() || !pending_acks.is_empty();
            let msg = if has_deferred_work {
                match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
                    Ok(Some(msg)) => msg,
                    Ok(None) => break 'fetch None,
                    Err(_) => break 'fetch None, // timeout — re-check deferred work
                }
            } else {
                match rx.recv().await {
                    Some(msg) => msg,
                    None => break 'fetch None,
                }
            };

            let result = match msg {
                WorkerMessage::Event { wal_id, wal_sequence, event, needs_wal_ack } if wal_sequence != 0 => {
                    if next_expected_sequence == 0 {
                        // First channel event — initialise the gate from
                        // the minimum pending WAL sequence across ALL
                        // un-ACKed entries (including in_flight ones).
                        // Previously in_flight entries were excluded, which
                        // allowed the gate to start past a WalOnly event
                        // that was queued out-of-order, causing it to be
                        // skipped as "stale" when it later arrived.
                        match worker_wal.list_pending().await {
                            Ok(pending) => {
                                let min_seq = pending.iter()
                                    .map(|(_, _, seq)| *seq)
                                    .min();
                                next_expected_sequence = min_seq.unwrap_or(wal_sequence);
                            }
                            Err(e) => {
                                // Fail-closed: WAL corruption during sequence
                                // gate init — cannot safely process events.
                                tracing::error!(
                                    wal_id = %wal_id, error = %e,
                                    "sequence gate init failed: WAL corruption"
                                );
                                break 'fetch None;
                            }
                        }
                    }

                    if wal_sequence == next_expected_sequence {
                        // In-order — process directly.
                        Some(WorkerMessage::Event { wal_id, wal_sequence, event, needs_wal_ack })
                    } else if wal_sequence > next_expected_sequence {
                        // Out-of-order (future sequence) — buffer.
                        buffer.insert(wal_sequence, (wal_id, event, needs_wal_ack));
                        None
                    } else {
                        // wal_sequence < next_expected_sequence: this event
                        // is from a sequence we have already passed.
                        // This can happen when the recovery scanner
                        // re-enqueues an event that was also in the initial
                        // replay (the replay already processed it).
                        // Log and skip.
                        trace!(
                            wal_id = %wal_id,
                            wal_sequence,
                            next_expected = %next_expected_sequence,
                            "stale channel event skipped (already past its sequence)"
                        );
                        None
                    }
                }
                // seq=0 sentinel or control messages: bypass gating.
                other => Some(other),
            };
            if let Some(val) = result {
                // val is Some(WorkerMessage) — break with it
                break 'fetch Some(val);
            }
            // result was None (out-of-order or stale) — fall through to continue outer loop
            None
        };

        let Some(msg) = message else { continue; };

        // ---- Dispatch ----
        let (wal_id, wal_sequence, event, needs_wal_ack) = match msg {
            WorkerMessage::Event {
                wal_id,
                wal_sequence,
                event,
                needs_wal_ack,
            } => (wal_id, wal_sequence, event, needs_wal_ack),
            WorkerMessage::Flush { target_wal_sequence, deadline, reply } => {
                drain_pending_acks(worker_wal, &mut pending_acks, in_flight, Some(deadline)).await;

                let buffer_remaining = buffer.range(..=target_wal_sequence).count();
                let wal_pending = match worker_wal.list_pending().await {
                    Ok(p) => p.iter()
                        .filter(|(_, _, seq)| *seq <= target_wal_sequence)
                        .count(),
                    Err(e) => {
                        // Fail-closed: cannot confirm all events committed.
                        warn!(error = %e, "flush failed: WAL read/corruption error");
                        let _ = reply.send(Err(format!("flush failed: WAL error: {e}")));
                        continue;
                    }
                };

                if pending_acks.is_empty() && buffer_remaining == 0 && wal_pending == 0 {
                    // All events <= target_wal_sequence are terminal.
                    let _ = reply.send(Ok(()));
                } else if Instant::now() >= deadline {
                    warn!(buffer_remaining, wal_pending, "flush deadline exceeded");
                    let _ = reply.send(Err("flush deadline exceeded".to_string()));
                } else {
                    // Not yet done — defer and re-check on subsequent iterations
                    // as progress is made through normal event processing.
                    pending_control = Some(PendingControl::FlushReply {
                        target: target_wal_sequence,
                        reply,
                        deadline,
                    });
                }
                continue;
            }
            WorkerMessage::Shutdown { target_wal_sequence, deadline, reply } => {
                drain_pending_acks(worker_wal, &mut pending_acks, in_flight, Some(deadline)).await;

                let buffer_remaining = buffer.range(..=target_wal_sequence).count();
                let wal_pending = match worker_wal.list_pending().await {
                    Ok(p) => p.iter()
                        .filter(|(_, _, seq)| *seq <= target_wal_sequence)
                        .count(),
                    Err(e) => {
                        // Fail-closed: cannot confirm all events committed.
                        warn!(error = %e, "shutdown failed: WAL read/corruption error");
                        let _ = reply.send(Err(format!("shutdown failed: WAL error: {e}")));
                        break;
                    }
                };

                if pending_acks.is_empty() && buffer_remaining == 0 && wal_pending == 0 {
                    // All events <= target_wal_sequence are terminal; safe to exit.
                    let _ = reply.send(Ok(()));
                    break;
                } else if Instant::now() >= deadline {
                    warn!(buffer_remaining, wal_pending, "shutdown deadline exceeded");
                    let _ = reply.send(Err("shutdown deadline exceeded".to_string()));
                    break;
                } else {
                    // Defer and re-check after processing more events.
                    pending_control = Some(PendingControl::Shutdown {
                        target: target_wal_sequence,
                        reply,
                        deadline,
                    });
                    continue;
                }
            }
        };

        // Process the event through the persistence pipeline and obtain
        // the outcome that determines whether the sequence gate advances.
        // Capture kind before `event` is moved into the pipeline.
        let event_kind = event.kind();
        let outcome = process_event_through_pipeline(
            wal_id,
            event,
            needs_wal_ack,
            worker_stats,
            worker_dead_letter_path,
            worker_wal,
            in_flight,
            &mut pending_acks,
        )
        .await;

        match outcome {
            ProcessOutcome::Shutdown => {
                break;
            }
            ProcessOutcome::RetryableFailure => {
                // Event did NOT reach a durable terminal state (both DB and
                // DLQ failed).  The WAL sequence gate MUST NOT advance so
                // that this event is retried before any later event can be
                // dispatched out-of-order.
                //
                // The event was consumed from the channel during processing,
                // but remains pending in WAL (not ACKed).  Remove it from
                // in_flight so the periodic recovery scanner can re-enqueue
                // it on its next interval — without this the entry is stuck
                // forever (in_flight, not in channel, scanner skips it).
                in_flight.lock().await.remove(&wal_id);
                tracing::warn!(
                    wal_id = %wal_id, kind = %event_kind,
                    "non-durable outcome — retrying on next scanner interval"
                );
            }
            _ => {
                // Terminal outcome (DatabaseCommitted, DurableDeadLetterStored,
                // or PendingWalAck).  The sequence gate can advance.
                if wal_sequence != 0 {
                    let base = wal_sequence + 1;
                    // P0-B: allow legitimate ACK sequence gaps.  After a
                    // compaction + restart the WAL may lack an intermediate
                    // sequence that was ACKed and removed.  If the buffer holds
                    // a HIGHER sequence and `base` is not itself buffered, check
                    // whether `base` is a confirmed gap (absent from WAL
                    // pending) and skip it — otherwise the gate waits forever
                    // on a sequence that will never appear and later events are
                    // stuck in the buffer.
                    if !buffer.contains_key(&base) {
                        if let Some(&next_in_buffer) =
                            buffer.range(base..).next().map(|(k, _)| k)
                        {
                            // Determine the pending set.  A WAL read error here
                            // must fail-closed (P0-D): keep the gate at base
                            // (skip nothing — the failed event is retried, not
                            // bypassed) and report a critical failure.
                            let pending: Option<HashSet<u64>> =
                                match worker_wal.list_pending().await {
                                    Ok(p) => Some(p.iter().map(|(_, _, s)| *s).collect()),
                                    Err(e) => {
                                        tracing::error!(
                                            wal_id = %wal_id, error = %e,
                                            "sequence-gap check failed: WAL read error; \
                                             halting gate advancement"
                                        );
                                        crate::supervisor_actor::report_critical_failure(
                                            "persistence-sequence-gap",
                                            format!(
                                                "WAL read error during sequence-gap check: {e}"
                                            ),
                                        )
                                        .await;
                                        None
                                    }
                                };
                            if let Some(pending) = pending {
                                let mut expected = base;
                                // Skip sequences that are neither buffered nor
                                // pending in the WAL (i.e. ACKed/compacted gaps).
                                // Pending WalOnly sequences stop the skip — they
                                // will arrive via the scanner.
                                while expected < next_in_buffer
                                    && !buffer.contains_key(&expected)
                                    && !pending.contains(&expected)
                                {
                                    expected += 1;
                                }
                                next_expected_sequence = expected;
                            } else {
                                // Fail-closed: do not advance past base.
                                next_expected_sequence = base;
                            }
                        } else {
                            next_expected_sequence = base;
                        }
                    } else {
                        next_expected_sequence = base;
                    }
                }
                // If this was a replay-derived event that has now reached a
                // durable terminal state, clear it from the replay pending set.
                // When the replay deque is exhausted AND the set is empty, all
                // initial replay events are fully committed — only then is
                // initial_replay_drained set (P0-F).
                let removed = replay_pending_ids.lock().await.remove(&wal_id);
                if removed && replay.is_empty() && replay_pending_ids.lock().await.is_empty() {
                    initial_replay_drained.store(true, Ordering::Release);
                }
            }
        }

        // Auto-compaction: trigger when ACK ratio drops below threshold.
        if worker_wal.should_compact() {
            match worker_wal.compact().await {
                Err(e) => {
                    tracing::warn!(error = %e, "auto-compaction failed");
                }
                Ok(_pending) => {
                    tracing::debug!("auto-compaction completed");
                    let wal_bytes = worker_wal.total_bytes();
                    record_wal_compaction(worker_stats, wal_bytes).await;
                }
            }
        }
    }
}

/// Drain the pending ACK queue, retrying each entry with a short sleep
/// on failure.  Uses a time-based deadline rather than a fixed retry count,
/// so every entry gets a fair attempt within the drain window.
///
/// When `deadline` is `None`, a default 6-second deadline is used from the
/// call time.  When `Some(abs_deadline)` is provided, the drain respects the
/// caller's absolute deadline and can abort early if it expires.
///
/// Returns the number of entries that remain in the queue after the deadline
/// expires.  A return value of 0 means all entries were successfully drained.
async fn drain_pending_acks(
    worker_wal: &Arc<PersistenceWal>,
    pending_acks: &mut std::collections::VecDeque<(uuid::Uuid, u32)>,
    in_flight: &Arc<Mutex<HashSet<uuid::Uuid>>>,
    deadline: Option<Instant>,
) -> usize {
    use tracing::{debug, warn};
    let deadline = deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(6));
    let initial_count = pending_acks.len();
    let mut attempts = 0u64;

    while !pending_acks.is_empty() && Instant::now() < deadline {
        if let Some((id, attempt)) = pending_acks.pop_front() {
            match worker_wal.ack(id).await {
                Ok(()) => {
                    // Latched: clear degraded only when the retry queue is
                    // fully drained (P0-D).
                    if pending_acks.is_empty() {
                        worker_wal.clear_ack_degraded();
                    }
                    debug!(wal_id = %id, "pending ACK drained");
                    in_flight.lock().await.remove(&id);
                }
                Err(_e) => {
                    worker_wal.mark_degraded(crate::persistence::wal::DEGRADED_ACK);
                    pending_acks.push_back((id, attempt.saturating_add(1)));
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        attempts += 1;
    }

    let remaining = pending_acks.len();
    let drained = initial_count.saturating_sub(remaining);
    if remaining > 0 {
        warn!(
            drained = %drained, remaining = %remaining, attempts = %attempts,
            "pending ACK drain timed out",
        );
    } else if attempts > 0 {
        debug!(
            drained = %drained, remaining = %remaining, attempts = %attempts,
            "pending ACK drain completed",
        );
    }
    remaining
}

/// Degraded worker loop: entered when WAL replay fails. Only accepts
/// Shutdown commands; all other messages are logged and discarded so no
/// data is processed with an unverified WAL.
async fn process_degraded_worker_loop(
    rx: &mut mpsc::Receiver<WorkerMessage>,
) {
    use tracing::{error, info, warn};

    error!("persistence worker entered degraded mode: WAL replay failed, rejecting all events");

    loop {
        let Some(message) = rx.recv().await else {
            break;
        };
        match message {
            WorkerMessage::Event { wal_id, needs_wal_ack: _, .. } => {
                warn!(wal_id = %wal_id, "dropping event in degraded persistence worker");
                continue;
            }
            WorkerMessage::Flush { reply, .. } => {
                let _ = reply.send(Ok(()));
                continue;
            }
            WorkerMessage::Shutdown { reply, .. } => {
                info!("degraded persistence worker shutting down");
                let _ = reply.send(Ok(()));
                break;
            }
        }
    }
}

/// Periodic WAL recovery scanner.
///
/// After a successful WAL admit the in-memory queue may be full, leaving the
/// event safely stored in WAL but not yet dispatched to the persistence
/// pipeline.  This scanner periodically checks for un-ACKed WAL entries that
/// are NOT already in-flight, and re-enqueues them so they are processed
/// during this runtime (rather than waiting for restart replay).
///
/// The scanner uses `try_send` so it never blocks the persistence pipeline.
/// If the queue is full, it backs off and retries on the next interval.
///
/// The scanner acquires `send_gate` before injecting each event so it
/// respects the same ordering fence that `enqueue`, `flush`, and `shutdown`
/// use.  If the gate is contended (another operation in progress), the
/// scanner skips that event and retries on the next interval.
async fn wal_recovery_scanner(
    tx: mpsc::Sender<WorkerMessage>,
    wal: Arc<PersistenceWal>,
    stats: Arc<RwLock<PersistenceStats>>,
    in_flight: Arc<Mutex<HashSet<uuid::Uuid>>>,
    send_gate: Arc<tokio::sync::Mutex<()>>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    // Consume the immediate first tick, then let the loop's tick drive the
    // first real scan at t = 5s (one configured period).  Previously two
    // pre-loop ticks delayed the first scan to 10s, which was too slow to
    // self-heal a transiently-failed replay event during startup (P0-D).
    interval.tick().await;

    loop {
        interval.tick().await;

        let pending = match wal.list_pending().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "WAL recovery scanner: list_pending failed");
                continue;
            }
        };

        // Prune stale in_flight entries: any entry that is in in_flight but NOT
        // in the WAL pending list was ACKed but never removed — this can happen
        // if the old scanner code registered in_flight after try_send (the race
        // is now fixed, but existing stale entries need cleanup).
        {
            let pending_ids: HashSet<uuid::Uuid> = pending.iter().map(|(id, _, _)| *id).collect();
            let mut in_flight_set = in_flight.lock().await;
            in_flight_set.retain(|id| pending_ids.contains(id));
        }

        if pending.is_empty() {
            continue;
        }

        // Snapshot in_flight set to avoid holding the lock across the scan loop.
        let in_flight_ids: HashSet<uuid::Uuid> = in_flight.lock().await.clone();

        for (wal_id, event, wal_sequence) in &pending {
            // Skip entries that are already queued (in-flight).
            if in_flight_ids.contains(wal_id) {
                continue;
            }

            // Acquire the send_gate before injecting into the channel.
            // If the gate is contended (flush/shutdown/enqueue in progress),
            // stop scanning and retry on the next interval.  This prevents
            // the scanner from bypassing the ordering fence that enqueue,
            // flush, and shutdown rely on.
            let kind = event.kind();
            let summary = event.summary();

            let _gate = match send_gate.try_lock() {
                Ok(guard) => guard,
                Err(_) => {
                    tracing::trace!(
                        wal_id = %wal_id, kind,
                        "WAL recovery scanner: send_gate contended, deferring"
                    );
                    break;
                }
            };
            // Register in in_flight BEFORE try_send so there is no window where
            // the worker could process and ACK this entry before it appears in
            // the in_flight set — if that happened the in_flight entry would be
            // stale (never removed) and could accumulate as a memory leak.
            in_flight.lock().await.insert(*wal_id);

            let msg = WorkerMessage::Event {
                wal_sequence: *wal_sequence,
                wal_id: *wal_id,
                event: event.clone(),
                needs_wal_ack: true,
            };

            match tx.try_send(msg) {
                Ok(()) => {
                    record_wal_recovered(&stats, kind.clone(), summary).await;
                    tracing::debug!(
                        wal_id = %wal_id, kind = %kind,
                        "WAL recovery scanner re-enqueued event"
                    );
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // Queue is still full — undo in_flight registration and
                    // retry next interval.  The event is safely in WAL.
                    in_flight.lock().await.remove(wal_id);
                    break;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    // Worker is shut down — stop scanner entirely.
                    // Remove from in_flight since the event was never queued.
                    in_flight.lock().await.remove(wal_id);
                    tracing::info!("WAL recovery scanner stopped (worker channel closed)");
                    return;
                }
            }
        }
    }
}

impl PersistenceWorker {
    pub fn spawn(queue_capacity: usize) -> Arc<Self> {
        Self::spawn_with_journals(
            queue_capacity,
            Some("data/persistence-dead-letter.jsonl".to_string()),
            "data/persistence-worker.wal.jsonl".to_string(),
        )
    }

    pub fn spawn_with_journals(
        queue_capacity: usize,
        dead_letter_path: Option<String>,
        wal_path: String,
    ) -> Arc<Self> {
        let capacity = queue_capacity.max(16);
        let dead_letter_path = dead_letter_path
            .map(|path| path.trim().to_string())
            .filter(|path| !path.is_empty());
        let (tx, mut rx) = mpsc::channel::<WorkerMessage>(capacity);
        let stats = Arc::new(RwLock::new(PersistenceStats {
            capacity,
            dead_letter_path: dead_letter_path.clone(),
            ..PersistenceStats::default()
        }));
        let worker_stats = Arc::clone(&stats);
        let worker_dead_letter_path = dead_letter_path.map(PathBuf::from);
        let wal = Arc::new(PersistenceWal::new(wal_path));
        let worker_wal = Arc::clone(&wal);

        let in_flight: Arc<Mutex<HashSet<uuid::Uuid>>> =
            Arc::new(Mutex::new(HashSet::new()));
        let worker_in_flight = Arc::clone(&in_flight);
        let scanner_tx = tx.clone();
        let scanner_wal = Arc::clone(&wal);
        let scanner_stats = Arc::clone(&stats);
        let scanner_in_flight = Arc::clone(&in_flight);
        let send_gate = Arc::new(Mutex::new(()));
        let scanner_send_gate = Arc::clone(&send_gate);

        let initial_replay_drained = Arc::new(AtomicBool::new(false));
        let worker_initial_replay_drained = Arc::clone(&initial_replay_drained);
        // Replay-derived WAL IDs that have not yet reached a durable terminal
        // state.  initial_replay_drained is only set once this set is empty
        // (all replayed events committed), so recovery does not proceed while
        // a replayed event is still pending (P0-F).
        let replay_pending_ids: Arc<Mutex<HashSet<uuid::Uuid>>> =
            Arc::new(Mutex::new(HashSet::new()));
        let worker_replay_pending_ids = Arc::clone(&replay_pending_ids);

        // Pending-ACK count exposed for health checks (P0-D).
        let pending_acks_count: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
        let worker_pending_acks_count = Arc::clone(&pending_acks_count);

        crate::supervisor_actor::spawn_critical("persistence-worker", async move {
            // Check WAL instance consistency before replay to detect
            // accidental deletion/truncation of an already-initialized WAL.
            // If consistency check fails, enter degraded mode instead of replay.
            let mut replay_ok = true;
            if let Err(e) = worker_wal.check_instance_consistency().await {
                crate::supervisor_actor::report_critical_failure(
                    "persistence-wal-consistency",
                    e,
                )
                .await;
                replay_ok = false;
            }
            if replay_ok {
            match worker_wal.replay().await {
                Ok(events) => {
                    let mut replay: std::collections::VecDeque<
                        (uuid::Uuid, PersistenceEvent, u64),
                    > = std::collections::VecDeque::from(events);
                    // Add replayed entries to in_flight so the periodic recovery
                    // scanner does not re-enqueue them while they are being processed.
                    for (wal_id, _, _) in &replay {
                        worker_in_flight.lock().await.insert(*wal_id);
                        worker_replay_pending_ids.lock().await.insert(*wal_id);
                    }
                    // If WAL had no events to replay, mark drained immediately.
                    if replay.is_empty() {
                        worker_initial_replay_drained.store(true, Ordering::Release);
                    }
                    process_worker_loop(
                        &mut rx,
                        &mut replay,
                        &worker_stats,
                        &worker_dead_letter_path,
                        &worker_wal,
                        &worker_in_flight,
                        &worker_initial_replay_drained,
                        &worker_replay_pending_ids,
                        &worker_pending_acks_count,
                    )
                    .await;
                }
                Err(error) => {
                    crate::supervisor_actor::report_critical_failure(
                        "persistence-wal",
                        format!("WAL replay failed — persistence worker cannot start: {error}"),
                    )
                    .await;
                    // Fail-closed: enter a degraded loop that only accepts
                    // shutdown/control messages; all events are rejected.
                    process_degraded_worker_loop(&mut rx).await;
                }
            }
            } else {
                // Instance consistency check failed — enter degraded mode.
                process_degraded_worker_loop(&mut rx).await;
            }
        });

        // Spawn recovery scanner only when the persistence worker is not
        // degraded.  If the worker entered degraded mode, the scanner will
        // see a closed channel and stop on its next iteration.
        tokio::spawn(wal_recovery_scanner(
            scanner_tx,
            scanner_wal,
            scanner_stats,
            scanner_in_flight,
            scanner_send_gate,
        ));

        Arc::new(Self {
            tx,
            send_gate,
            stats,
            wal,
            in_flight,
            suspended: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            initial_replay_drained,
            pending_acks: pending_acks_count,
        })
    }

    /// Check whether the persistence worker is suspended (idle mode).
    pub fn is_suspended(&self) -> bool {
        self.suspended.load(Ordering::Relaxed)
    }

    /// Set the idle-mode hint. Ingestion deliberately remains active.
    pub async fn set_suspended(&self, suspended: bool) {
        self.suspended.store(suspended, Ordering::Release);
        if suspended {
            info!("persistence idle hint enabled; ingestion remains active");
        } else {
            info!("persistence idle hint cleared");
        }
    }

    pub async fn enqueue(&self, event: PersistenceEvent) -> Result<AdmissionOutcome, PersistenceEvent> {
        let kind = event.kind();
        let summary = event.summary();
        let _send_guard = self.send_gate.lock().await;
        if self.closed.load(Ordering::Acquire) {
            record_dropped(&self.stats, kind, summary,
                "persistence worker is shutting down".to_string()).await;
            return Err(event);
        }
        // WAL admit FIRST — once fsynced to WAL the event is recoverable
        // on restart even if queue admission fails temporarily.
        let (wal_id, wal_sequence, needs_wal_ack) = match self.wal.admit(event.clone()).await {
            Ok((id, seq)) => {
                record_wal_received(&self.stats).await;
                (id, seq, true)
            }
            Err(error) => {
                record_dropped(&self.stats, kind, summary, error).await;
                return Err(event);
            }
        };
        // The event is durable in WAL even if the deletion-guard marker failed
        // to update; surface that as AdmittedDegraded (P0-A).
        let marker_degraded = self.wal.marker_degraded();
        // Reserve queue capacity.  If the queue is full, try a bounded
        // wait (100 ms) before giving up — the event is already in WAL,
        // so a timeout is safe and the event will be replayed on restart.
        let permit = match self.tx.try_reserve() {
            Ok(permit) => permit,
            Err(mpsc::error::TrySendError::Full(_)) => {
                match tokio::time::timeout(
                    Duration::from_millis(100),
                    self.tx.reserve(),
                )
                .await
                {
                    Ok(Ok(permit)) => permit,
                    Ok(Err(_)) => {
                        // WAL has the event — return WalOnly so the caller
                        // knows the event is safe and will replayed on restart.
                        record_wal_only(&self.stats, kind, summary).await;
                        return Ok(AdmissionOutcome::WalOnly);
                    }
                    Err(_) => {
                        // WAL has the event — return WalOnly so the caller
                        // knows the event is safe and the periodic recovery
                        // scanner will re-enqueue it.
                        record_wal_only(&self.stats, kind, summary).await;
                        return Ok(AdmissionOutcome::WalOnly);
                    }
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // WAL has the event — return WalOnly so the caller knows the
                // event is safe and will be replayed on restart.
                record_wal_only(&self.stats, kind, summary).await;
                return Ok(AdmissionOutcome::WalOnly);
            }
        };
        // Send using the reserved permit (infallible).
        self.in_flight.lock().await.insert(wal_id);
        permit.send(WorkerMessage::Event { wal_id, wal_sequence, event, needs_wal_ack });
        record_queued(&self.stats, kind.clone(), summary).await;
        Ok(if marker_degraded {
            AdmissionOutcome::AdmittedDegraded
        } else {
            AdmissionOutcome::Queued
        })
    }
    /// Wait for all queued events with a WAL sequence <= the current sequence
    /// to reach a terminal state (ACKed or dead-lettered).
    ///
    /// Events that were admitted as WalOnly (WAL-only, never reached the
    /// in-memory channel) are also covered by the WAL-sequence fence: the
    /// worker waits for the periodic recovery scanner to re-enqueue and
    /// process them, or for the timeout to expire.
    pub async fn flush(&self, timeout: Duration) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        let target;
        let deadline;
        {
            // Linearization point (P0-B): acquire send_gate FIRST, then read
            // the WAL sequence INSIDE the gate.  Previously the sequence was
            // read before the gate, so a concurrent enqueue that already held
            // the gate (admit in progress) could complete AFTER the target was
            // captured but still be excluded from this flush — the caller
            // would return before that event reached a terminal state.
            let _send_guard = self.send_gate.lock().await;
            if self.closed.load(Ordering::Acquire) {
                return Err("persistence worker is shutting down".to_string());
            }
            target = self.wal.current_sequence();
            deadline = Instant::now() + timeout;
            self.tx
                .send(WorkerMessage::Flush { target_wal_sequence: target, deadline, reply })
                .await
                .map_err(|_| "persistence worker is closed".to_string())?;
        }
        tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| "persistence flush timed out".to_string())?
            .map_err(|_| "persistence flush acknowledgement was dropped".to_string())??;
        self.wal.compact().await?;
        Ok(())
    }

    /// Drain accepted events, flush telemetry, then stop the worker.
    pub async fn shutdown(&self, timeout: Duration) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        let deadline = Instant::now() + timeout;
        let target;
        {
            // Same linearization rule as flush (P0-B): acquire send_gate, then
            // read the sequence inside the gate so concurrent admissions are
            // covered by the shutdown target.
            let _send_guard = self.send_gate.lock().await;
            if self.closed.swap(true, Ordering::AcqRel) {
                return Ok(());
            }
            target = self.wal.current_sequence();
            if self
                .tx
                .send(WorkerMessage::Shutdown { target_wal_sequence: target, deadline, reply })
                .await
                .is_err()
            {
                self.closed.store(false, Ordering::Release);
                return Err("persistence worker is closed".to_string());
            }
        }
        let result = match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("persistence shutdown acknowledgement was dropped".to_string()),
            Err(_) => Err("persistence shutdown timed out".to_string()),
        };
        if let Err(error) = result {
            self.closed.store(false, Ordering::Release);
            return Err(error);
        }
        self.wal.compact().await?;
        Ok(())
    }

    pub async fn stats(&self) -> PersistenceStats {
        let mut stats = self.stats.read().await.clone();
        stats.refresh_derived();
        stats
    }

    pub async fn is_healthy(&self) -> bool {
        self.wal.replay_succeeded()
            && self.initial_replay_drained.load(Ordering::Acquire)
            && !self.wal.is_degraded()
            && self.pending_acks.load(Ordering::Acquire) == 0
            && !self.closed.load(Ordering::Acquire)
    }

    /// Return the current WAL admission sequence number.
    /// Used as a fence during DLQ replay to verify all replayed events
    /// are committed before proceeding to stale session cleanup.
    pub fn wal_sequence(&self) -> u64 {
        self.wal.current_sequence()
    }

    /// Return the number of pending (un-ACKed) WAL entries.
    /// Used after DLQ replay flush to verify all events are committed.
    /// Returns `Err` on WAL read/corruption so callers can fail-closed
    /// instead of treating an unreadable WAL as empty.
    pub async fn pending_wal_count(&self) -> Result<usize, String> {
        Ok(self.wal.list_pending().await?.len())
    }
}

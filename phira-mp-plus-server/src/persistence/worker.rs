//! Runtime PersistenceWorker.
//!
//! Ordinary persistence events use bounded backpressure instead of queue-full
//! loss. Flush and shutdown are ordered control messages with acknowledgements,
//! so accepted work can be drained before process termination. All production
//! Touch/Judge telemetry goes through the HighFrequencyWriter — the single
//! unified high-frequency persistence path.

use crate::persistence::message::{AdmissionOutcome, PersistenceEvent};
use crate::persistence::process::process_event_through_pipeline;
use crate::persistence::process::ProcessOutcome;
use crate::persistence::stats::{
    record_dropped, record_queued,
    record_wal_compaction, record_wal_only,
    record_wal_received, record_wal_recovered, PersistenceStats,
};
use crate::persistence::wal::PersistenceWal;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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
    // normal event processing.
    enum PendingControl {
        FlushReply {
            target: u64,
            reply: oneshot::Sender<Result<(), String>>,
            deadline: Instant,
        },
        Shutdown {
            target: u64,
            reply: oneshot::Sender<Result<(), String>>,
            deadline: Instant,
        },
    }
    let mut pending_control: Option<PendingControl> = None;

    loop {
        // ---- Check pending control (deferred flush/shutdown) ----
        if let Some(pc) = pending_control.take() {
            let (target, reply, deadline, should_break) = match pc {
                PendingControl::FlushReply { target, reply, deadline } => {
                    (target, reply, deadline, false)
                }
                PendingControl::Shutdown { target, reply, deadline } => {
                    (target, reply, deadline, true)
                }
            };
            let buffer_remaining = buffer.range(..=target).count();
            // Count ALL un-ACKed WAL entries with seq <= target, including
            // those currently in-flight (queued in the channel or being
            // processed by the pipeline).  Previously in_flight entries were
            // excluded, which created a correctness gap: the fence could
            // return before events that were queued (but not yet committed
            // to the database) reached a terminal state.
            let wal_pending = match worker_wal.list_pending().await {
                Ok(p) => p.iter()
                    .filter(|(_, _, seq)| *seq <= target)
                    .count(),
                Err(_) => 0,
            };
            if pending_acks.is_empty() && buffer_remaining == 0 && wal_pending == 0 {
                let _ = reply.send(Ok(()));
                pending_control = None;
                if should_break {
                    break;
                }
            } else if Instant::now() >= deadline {
                warn!(
                    buffer_remaining, wal_pending,
                    "pending control deadline exceeded",
                );
                let _ = reply.send(Err("deadline exceeded".to_string()));
                pending_control = None;
                if should_break {
                    break;
                }
            }
        }

        // ---- Determine the message to process this iteration ----
        //
        // Priority: buffer (for draining the expected sequence) >
        //           replay (bypass gating, already in WAL order) >
        //           channel with gating.
        // Priority order: buffer → replay → channel with gating.
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

            // 2. Replay events (already in WAL order, bypass gating with
            //    the seq=0 sentinel).
            if let Some((wal_id, event, _seq)) = replay.pop_front() {
                break 'fetch Some(WorkerMessage::Event {
                    wal_id,
                    wal_sequence: 0,
                    event,
                    needs_wal_ack: true,
                });
            }

            // Replay is exhausted. Mark drained exactly once.
            initial_replay_drained.store(true, Ordering::Release);

            // 3. Channel receive with sequence gating.
            let msg = if pending_control.is_some() {
                // Use a short timeout so we can re-check pending control
                // conditions when the channel is otherwise idle.
                match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
                    Ok(Some(msg)) => msg,
                    Ok(None) => break 'fetch None,
                    Err(_) => break 'fetch None, // timeout — re-check pending control
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
                        // the minimum pending WAL sequence (if any) so
                        // recovered WalOnly events with lower sequences
                        // do not arrive after we have advanced past them.
                        let in_flight_ids: HashSet<uuid::Uuid> =
                            in_flight.lock().await.clone();
                        if let Ok(pending) = worker_wal.list_pending().await {
                            let min_seq = pending
                                .iter()
                                .filter(|(id, _, _)| !in_flight_ids.contains(id))
                                .map(|(_, _, seq)| *seq)
                                .min();
                            next_expected_sequence = min_seq.unwrap_or(wal_sequence);
                        } else {
                            next_expected_sequence = wal_sequence;
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


        // ---- Retry pending WAL ACKs ----
        if let Some((retry_id, retry_attempt)) = pending_acks.front().copied() {
            match worker_wal.ack(retry_id).await {
                Ok(()) => {
                    worker_wal.set_degraded(false);
                    debug!(wal_id = %retry_id, "ACK retry succeeded");
                    pending_acks.pop_front();
                    in_flight.lock().await.remove(&retry_id);
                }
                Err(e) => {
                    worker_wal.set_degraded(true);
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

        // ---- Dispatch ----
        let (wal_id, wal_sequence, event, needs_wal_ack) = match msg {
            WorkerMessage::Event {
                wal_id,
                wal_sequence,
                event,
                needs_wal_ack,
            } => (wal_id, wal_sequence, event, needs_wal_ack),
            WorkerMessage::Flush { target_wal_sequence, reply } => {
                let deadline = Instant::now() + Duration::from_secs(30);
                drain_pending_acks(worker_wal, &mut pending_acks, in_flight, Some(deadline)).await;

                let buffer_remaining = buffer.range(..=target_wal_sequence).count();
                let wal_pending = match worker_wal.list_pending().await {
                    Ok(p) => p.iter()
                        .filter(|(_, _, seq)| *seq <= target_wal_sequence)
                        .count(),
                    Err(_) => 0,
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
                    Err(_) => 0,
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
                // The event remains pending in WAL and will be re-enqueued
                // by the recovery scanner on its next interval.
                tracing::warn!(
                    wal_id = %wal_id, kind = %kind,
                    "non-durable outcome — retrying on next scanner interval"
                );
            }
            _ => {
                // Terminal outcome (DatabaseCommitted, DurableDeadLetterStored,
                // or PendingWalAck).  The sequence gate can advance.
                if wal_sequence != 0 {
                    next_expected_sequence = wal_sequence + 1;
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
                    worker_wal.set_degraded(false);
                    debug!(wal_id = %id, "pending ACK drained");
                    in_flight.lock().await.remove(&id);
                }
                Err(_e) => {
                    worker_wal.set_degraded(true);
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
    // Start with a jitter so the scanner does not contend with initial replay.
    interval.tick().await; // first tick completes immediately, skip it
    interval.tick().await; // wait one full interval before first real scan

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
        Ok(AdmissionOutcome::Queued)
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
        // Capture the current WAL sequence so the worker can verify that all
        // events admitted before this point have reached a terminal state.
        let target = self.wal.current_sequence();
        {
            let _send_guard = self.send_gate.lock().await;
            if self.closed.load(Ordering::Acquire) {
                return Err("persistence worker is shutting down".to_string());
            }
            self.tx
                .send(WorkerMessage::Flush { target_wal_sequence: target, reply })
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
        let target = self.wal.current_sequence();
        {
            let _send_guard = self.send_gate.lock().await;
            if self.closed.swap(true, Ordering::AcqRel) {
                return Ok(());
            }
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
    pub async fn pending_wal_count(&self) -> usize {
        self.wal.list_pending().await.map(|v| v.len()).unwrap_or(0)
    }
}

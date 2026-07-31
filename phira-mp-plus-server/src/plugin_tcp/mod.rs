//! TCP connection management — WASM plugins connect/listen via WIT host API.
//!
//! Plugins get handles; host manages raw TCP sockets. No TLS.
//! (TLS was stripped because it was never production-ready.)
//!
//! # Per-connection mailbox (P0-E / P0-F / P0-G)
//!
//! Event delivery is routed through a **per-connection mailbox** instead of the
//! old plugin-level high/normal shared queues with peek-lock-pop:
//!
//! * every event (accept/connect/receive/error/disconnect) is appended to the
//!   FIFO mailbox of its `conn_handle`.  `tcp:accept` carries `conn_handle`;
//!   all other events carry `handle` — both are read by `extract_handle` so
//!   accept/connect enter the same connection ordering as receive/error/
//!   disconnect (P0-E).
//! * a worker claims a ready handle, takes its per-connection serialization
//!   lock and pops the *front* event, so same-connection events are strictly
//!   ordered while different connections are processed concurrently (bounded
//!   by `MAX_CONCURRENT_CALLBACKS`).
//! * the mailbox is removed **only after** the `disconnect` callback
//!   completes, so no residual receive can run concurrently with a disconnect
//!   (P0-E).  A `Close` from the plugin also removes the mailbox.
//! * byte/count accounting is guarded by a single queue-state mutex, making
//!   budget reservation and rollback atomic (P0-G).
//! * receive events are the droppable category; lifecycle events are admitted
//!   by evicting normal receives.  A lifecycle event is never silently dropped
//!   — when it cannot be admitted the caller gets `PushOutcome::Overflow`
//!   and must close the connection (or resync the plugin) (P0-F).

pub mod actor;
pub mod quota;
pub mod events;

pub use actor::PluginTcpActor;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex, Notify};

// ── Resource limits (PMP25 P5) ──────────────────────────────────────
pub(crate) use quota::{
    MAX_CONNECTIONS_PER_PLUGIN,
    MAX_LISTENERS_PER_PLUGIN,
    MAX_PENDING_EVENTS_PER_PLUGIN,
};

/// Handle used for events that carry no per-connection handle (should be
/// impossible in practice).  They still get their own FIFO mailbox.
const GLOBAL_HANDLE: u64 = 0;

/// Synchronous reply channel for WIT host functions — blocks the calling
/// WASM thread until the async TCP actor processes the command.
pub(crate) type SyncReply<T> = std::sync::mpsc::Sender<Result<T, String>>;

/// Internal events from accept loops back to the PluginTcpActor.
#[derive(Debug)]
pub(crate) enum PluginTcpInternal {
    Accepted {
        listener_handle: u64,
        conn_handle: u64,
        remote_addr: String,
        data_tx: mpsc::Sender<Vec<u8>>,
        close_tx: oneshot::Sender<()>,
        plugin_id_tx: oneshot::Sender<String>,
    },
    Disconnected {
        handle: u64,
        plugin_id: String,
        remote_addr: String,
    },
    /// Result of a spawned connect attempt — actor completes the registration.
    ConnectCompleted {
        handle: u64,
        plugin_id: String,
        addr: String,
        result: Result<(mpsc::Sender<Vec<u8>>, oneshot::Sender<()>), String>,
    },
}

/// Commands plugins send to the TCP actor.
#[derive(Debug)]
pub enum PluginTcpCommand {
    Connect {
        plugin_id: String,
        addr: String,
        reply: SyncReply<u64>,
    },
    Listen {
        plugin_id: String,
        addr: String,
        reply: SyncReply<u64>,
    },
    Send { plugin_id: String, handle: u64, bytes: Vec<u8> },
    Close { plugin_id: String, handle: u64 },
    Accept {
        plugin_id: String,
        listener_handle: u64,
        reply: SyncReply<Option<u64>>,
    },
    Recv {
        plugin_id: String,
        handle: u64,
        max_bytes: u32,
        reply: SyncReply<Option<Vec<u8>>>,
    },
    PeerAddr {
        plugin_id: String,
        handle: u64,
        reply: SyncReply<String>,
    },
    RemovePlugin {
        plugin_id: String,
        reply: SyncReply<()>,
    },
    /// Query aggregated per-plugin TCP metrics (pending/dropped events,
    /// buffered read bytes) for diagnostics.
    Stats {
        reply: SyncReply<serde_json::Value>,
    },
}

// Shared type aliases for connection/socket tracking, used by actor and events.
pub(crate) type ConnectionMap = Arc<Mutex<HashMap<u64, mpsc::Sender<Vec<u8>>>>>;
pub(crate) type CloseMap = Arc<Mutex<HashMap<u64, oneshot::Sender<()>>>>;
/// Shared buffer for received data (read task → recv command).
pub(crate) type ReadBufMap = Arc<Mutex<HashMap<u64, Vec<u8>>>>;
/// Per-handle buffered byte count, used for per-connection buffer accounting
/// at cleanup. Plugin totals are derived by summing per-handle values.
pub(crate) type HandleReadBytesMap = Arc<Mutex<HashMap<u64, usize>>>;

/// Approximate byte size of an event payload for byte-budget accounting.
/// Receive payloads are dominated by the `bytes` array; others are small JSON.
fn event_payload_bytes(event_type: &str, payload: &serde_json::Value) -> usize {
    if is_lifecycle(event_type) {
        serde_json::to_vec(payload).map(|v| v.len()).unwrap_or(0)
    } else {
        payload
            .get("bytes")
            .and_then(|b| b.as_array())
            .map(|a| a.len())
            .unwrap_or(0)
    }
}

/// Event types that are lifecycle-critical and must never be evicted by a
/// receive flood.
fn is_lifecycle(event_type: &str) -> bool {
    !event_type.eq_ignore_ascii_case("tcp:receive")
}

/// Extract the connection handle that routes an event to its per-connection
/// mailbox.  `tcp:accept` carries `conn_handle`; everything else carries
/// `handle`.  Reading both is what lets accept/connect enter the same ordering
/// as receive/error/disconnect (P0-E).
fn extract_handle(payload: &serde_json::Value) -> Option<u64> {
    payload
        .get("conn_handle")
        .and_then(|h| h.as_u64())
        .or_else(|| payload.get("handle").and_then(|h| h.as_u64()))
}

/// Result of a `PluginEventChannel::push` call.  Lets the actor close a
/// connection (or resync the plugin) when a lifecycle event cannot be admitted
/// instead of silently dropping it (P0-F).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PushOutcome {
    /// The event was queued (possibly merged into an existing receive).
    Accepted,
    /// A receive event was dropped (acceptable bounded loss under flood).
    Dropped,
    /// A lifecycle event could not be admitted.  The caller MUST close the
    /// connection or resync the plugin.
    Overflow { handle: u64, event_type: String },
}

/// A single queued event inside a per-connection mailbox.
pub(crate) struct QueuedEvent {
    pub(crate) event_type: String,
    pub(crate) payload: serde_json::Value,
}

/// Per-connection FIFO mailbox.
pub(crate) struct ConnectionMailbox {
    /// Serializes callback delivery — only one worker may process events for
    /// this connection at a time (P0-E single consumer).  The lock is
    /// `Arc`-shared so a worker can hold it across the (async) plugin callback
    /// even though the mailbox itself lives inside the channel's queue state.
    pub(crate) lock: Arc<TokioMutex<()>>,
    /// FIFO of queued events, in arrival order.
    pub(crate) events: VecDeque<QueuedEvent>,
    /// Sum of raw payload bytes queued for this connection.
    pub(crate) pending_bytes: usize,
}

impl ConnectionMailbox {
    fn new() -> Self {
        Self {
            lock: Arc::new(TokioMutex::new(())),
            events: VecDeque::new(),
            pending_bytes: 0,
        }
    }
}

/// Queue state guarded by a single mutex.  All byte/count accounting happens
/// under this lock, making budget reservation and rollback atomic (P0-G).
struct ChannelState {
    /// handle → per-connection mailbox.
    mailboxes: HashMap<u64, ConnectionMailbox>,
    /// Handles with at least one pending event, in arrival order.
    ready: VecDeque<u64>,
    /// Handles currently claimed by a worker (in-flight).
    active: HashSet<u64>,
    /// Total raw payload bytes pending across all mailboxes.
    total_bytes: usize,
    /// Raw payload bytes of lifecycle events pending (for the reserved budget).
    lifecycle_bytes: usize,
    /// Total number of pending events across all mailboxes.
    event_count: usize,
}

impl ChannelState {
    fn new() -> Self {
        Self {
            mailboxes: HashMap::new(),
            ready: VecDeque::new(),
            active: HashSet::new(),
            total_bytes: 0,
            lifecycle_bytes: 0,
            event_count: 0,
        }
    }
}

/// Per-plugin bounded event channel with per-connection mailboxes.
///
/// A single `Mutex<ChannelState>` guards the mailbox map, the ready/active
/// dispatch sets and the byte/count budgets, so a multi-connection push can
/// never overshoot a cap (P0-G).  Per-connection callback serialization uses a
/// tokio mutex so a worker can hold it across the (async) plugin callback.
pub(crate) struct PluginEventChannel {
    state: Mutex<ChannelState>,
    notify: Arc<Notify>,
    max_len: usize,
    /// Total events dropped because a cap was exceeded (metrics).
    dropped_count: std::sync::atomic::AtomicU64,
    /// Lifecycle events dropped (should be ~0 given eviction priority).
    dropped_lifecycle: std::sync::atomic::AtomicU64,
    /// Receive events dropped (the droppable category).
    dropped_receive: std::sync::atomic::AtomicU64,
    /// Bytes dropped because a byte budget was exceeded.
    dropped_bytes: std::sync::atomic::AtomicU64,
    /// Lifecycle events that could not be admitted at all (P0-F overflow).
    lifecycle_overflow: std::sync::atomic::AtomicU64,
}

impl PluginEventChannel {
    pub fn new(max_len: usize) -> Self {
        Self {
            state: Mutex::new(ChannelState::new()),
            notify: Arc::new(Notify::new()),
            max_len,
            dropped_count: std::sync::atomic::AtomicU64::new(0),
            dropped_lifecycle: std::sync::atomic::AtomicU64::new(0),
            dropped_receive: std::sync::atomic::AtomicU64::new(0),
            dropped_bytes: std::sync::atomic::AtomicU64::new(0),
            lifecycle_overflow: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Shared wakeup handle for the worker tasks.
    pub fn notify(&self) -> Arc<Notify> {
        Arc::clone(&self.notify)
    }

    fn bump_dropped(&self, is_lifecycle: bool, bytes: usize) {
        self.dropped_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.dropped_bytes.fetch_add(bytes as u64, std::sync::atomic::Ordering::Relaxed);
        if is_lifecycle {
            self.dropped_lifecycle.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        } else {
            self.dropped_receive.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Push an event into the owning connection's mailbox.
    ///
    /// Ordering guarantees (P0-E): events are appended to a per-connection
    /// FIFO in arrival order; the worker pops from the front, so
    /// accept/connect → receive chunks → error → disconnect is preserved for
    /// every connection regardless of event type.
    ///
    /// Budget guarantees:
    /// * the plugin-wide byte budget is reserved under the queue-state mutex,
    ///   so concurrent pushes cannot overshoot it (P0-G);
    /// * receive events are the droppable category; a lifecycle event is
    ///   admitted by evicting normal receives, and is never silently dropped —
    ///   if it cannot be admitted the caller receives `PushOutcome::Overflow`
    ///   (P0-F);
    /// * a receive merge is only performed when the merged event stays within
    ///   the per-event raw-byte bound, the per-connection pending bound and the
    ///   plugin total bound (P0-G).
    pub fn push(&self, event_type: String, payload: serde_json::Value) -> PushOutcome {
        let incoming_bytes = event_payload_bytes(&event_type, &payload);
        let is_lifecycle = is_lifecycle(&event_type);
        let handle = extract_handle(&payload).unwrap_or(GLOBAL_HANDLE);

        // P0-G: per-event raw bytes bound.  A single oversized event cannot be
        // admitted at all.  Lifecycle oversize means the connection must close.
        if incoming_bytes > crate::plugin_tcp::quota::MAX_EVENT_RAW_BYTES {
            self.bump_dropped(is_lifecycle, incoming_bytes);
            if is_lifecycle {
                tracing::warn!(
                    event_type, incoming_bytes, handle,
                    "plugin TCP lifecycle event exceeds MAX_EVENT_RAW_BYTES; connection must be closed"
                );
                return PushOutcome::Overflow { handle, event_type };
            }
            tracing::warn!(
                event_type, incoming_bytes, handle,
                "plugin TCP receive event exceeds MAX_EVENT_RAW_BYTES; dropped"
            );
            return PushOutcome::Dropped;
        }

        let mut state = self.state.lock().unwrap();

        // P0-G: merge receive chunks into the newest same-connection receive
        // when the merged event respects every budget.
        if !is_lifecycle {
            if let Some(outcome) = try_merge_receive(&mut state, handle, &payload, incoming_bytes) {
                drop(state);
                self.notify.notify_one();
                return outcome;
            }
        }

        // Would this push exceed the plugin-wide caps?
        let byte_over = state.total_bytes.saturating_add(incoming_bytes)
            > crate::plugin_tcp::quota::MAX_PENDING_EVENT_BYTES_PER_PLUGIN;
        let count_over = state.event_count >= self.max_len;

        if (byte_over || count_over) && !is_lifecycle {
            // Receive events are the droppable category (P0-F).
            drop(state);
            self.bump_dropped(false, incoming_bytes);
            return PushOutcome::Dropped;
        }

        if byte_over || count_over {
            // Lifecycle event: make room by evicting normal receive events
            // first (P0-F).  A lifecycle event is never silently dropped.
            let need_bytes = state
                .total_bytes
                .saturating_add(incoming_bytes)
                .saturating_sub(crate::plugin_tcp::quota::MAX_PENDING_EVENT_BYTES_PER_PLUGIN);
            let need_count = if count_over {
                state.event_count - self.max_len + 1
            } else {
                0
            };
            evict_receive_events(&mut state, need_bytes, need_count);
            let byte_ok = state.total_bytes.saturating_add(incoming_bytes)
                <= crate::plugin_tcp::quota::MAX_PENDING_EVENT_BYTES_PER_PLUGIN;
            let count_ok = state.event_count.saturating_add(1) <= self.max_len;
            if !byte_ok || !count_ok {
                drop(state);
                self.bump_dropped(true, incoming_bytes);
                self.lifecycle_overflow
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!(
                    event_type, incoming_bytes, handle,
                    "plugin TCP lifecycle event could not be admitted; connection must be closed or plugin resynced"
                );
                return PushOutcome::Overflow { handle, event_type };
            }
        }

        // P0-F: lifecycle events must also fit in the reserved lifecycle slice
        // of the budget.  Breach means the plugin is not draining — overflow so
        // the caller force-closes the connection.
        if is_lifecycle {
            let reserved = crate::plugin_tcp::quota::MAX_LIFECYCLE_RESERVED_BYTES;
            if state.lifecycle_bytes.saturating_add(incoming_bytes) > reserved {
                drop(state);
                self.bump_dropped(true, incoming_bytes);
                self.lifecycle_overflow
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!(
                    event_type, incoming_bytes, handle,
                    "plugin TCP lifecycle reserved budget exhausted; connection must be closed or plugin resynced"
                );
                return PushOutcome::Overflow { handle, event_type };
            }
        }

        // Route into the per-connection mailbox (creating it if this is the
        // first event for the connection, e.g. accept).
        let conn = state
            .mailboxes
            .entry(handle)
            .or_insert_with(ConnectionMailbox::new);
        conn.events.push_back(QueuedEvent {
            event_type,
            payload,
        });
        conn.pending_bytes += incoming_bytes;
        state.total_bytes += incoming_bytes;
        if is_lifecycle {
            state.lifecycle_bytes += incoming_bytes;
        }
        state.event_count += 1;

        // Make the handle claimable.  If a worker is already processing it the
        // worker's release step re-queues it once the current callback ends.
        if !state.active.contains(&handle) && !state.ready.contains(&handle) {
            state.ready.push_back(handle);
        }
        drop(state);
        self.notify.notify_one();
        PushOutcome::Accepted
    }

    /// Claim the next ready handle that is not already in-flight, returning it
    /// together with the connection's serialization lock.  The caller MUST
    /// acquire the returned lock before popping and call
    /// `PluginEventChannel::release_handle` (or
    /// `PluginEventChannel::remove_connection`) afterwards.
    pub fn claim_handle(&self) -> Option<(u64, Arc<TokioMutex<()>>)> {
        let mut state = self.state.lock().unwrap();
        let idx = state.ready.iter().position(|h| {
            !state.active.contains(h) && state.mailboxes.contains_key(h)
        })?;
        let handle = state.ready.remove(idx).unwrap();
        state.active.insert(handle);
        let conn = state.mailboxes.get(&handle).unwrap();
        Some((handle, Arc::clone(&conn.lock)))
    }

    /// Pop the front (oldest) event of a claimed connection's mailbox.  The
    /// caller must hold that mailbox's `lock` (single consumer) — this is not
    /// enforced here, but the dispatch protocol guarantees it.
    pub fn pop_event(&self, handle: u64) -> Option<QueuedEvent> {
        let mut state = self.state.lock().unwrap();
        let conn = state.mailboxes.get_mut(&handle)?;
        let evt = conn.events.pop_front()?;
        let freed = event_payload_bytes(&evt.event_type, &evt.payload);
        conn.pending_bytes = conn.pending_bytes.saturating_sub(freed);
        state.total_bytes = state.total_bytes.saturating_sub(freed);
        if is_lifecycle(&evt.event_type) {
            state.lifecycle_bytes = state.lifecycle_bytes.saturating_sub(freed);
        }
        state.event_count = state.event_count.saturating_sub(1);
        Some(evt)
    }

    /// Release a claimed handle after its event callback completed.  Re-queues
    /// the handle when its mailbox still has events.
    pub fn release_handle(&self, handle: u64) {
        let mut state = self.state.lock().unwrap();
        state.active.remove(&handle);
        let re_add = match state.mailboxes.get(&handle) {
            Some(conn) => !conn.events.is_empty(),
            None => false,
        };
        if re_add && !state.ready.contains(&handle) {
            state.ready.push_back(handle);
        }
    }

    /// Remove a connection's mailbox, dropping any queued events and releasing
    /// its byte/count reservations.  Called AFTER the disconnect callback
    /// completes (so no residual receive can run concurrently — P0-E) and when
    /// the plugin closes the connection.
    pub fn remove_connection(&self, handle: u64) {
        let mut state = self.state.lock().unwrap();
        state.active.remove(&handle);
        state.ready.retain(|h| *h != handle);
        if let Some(conn) = state.mailboxes.remove(&handle) {
            for evt in &conn.events {
                let freed = event_payload_bytes(&evt.event_type, &evt.payload);
                state.total_bytes = state.total_bytes.saturating_sub(freed);
                if is_lifecycle(&evt.event_type) {
                    state.lifecycle_bytes = state.lifecycle_bytes.saturating_sub(freed);
                }
            }
            state.event_count = state.event_count.saturating_sub(conn.events.len());
        }
    }

    // ── Metrics ───────────────────────────────────────────────────────

    /// Number of events dropped because a cap was exceeded.
    pub fn dropped_count(&self) -> u64 {
        self.dropped_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Lifecycle events dropped (should be ~0 given eviction priority).
    pub fn dropped_lifecycle(&self) -> u64 {
        self.dropped_lifecycle.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Receive events dropped (the droppable category).
    pub fn dropped_receive(&self) -> u64 {
        self.dropped_receive.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Lifecycle events that could not be admitted (must trigger a close).
    pub fn lifecycle_overflow(&self) -> u64 {
        self.lifecycle_overflow.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Total payload bytes currently buffered across all mailboxes.
    pub fn pending_bytes(&self) -> usize {
        self.state.lock().unwrap().total_bytes
    }

    /// Bytes dropped because a byte budget was exceeded.
    pub fn dropped_bytes(&self) -> u64 {
        self.dropped_bytes.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Number of events currently pending in the per-connection mailboxes.
    pub fn pending_count(&self) -> usize {
        self.state.lock().unwrap().event_count
    }
}

/// Try to merge an incoming `tcp:receive` payload into the newest queued
/// receive for the same connection (append bytes).  Returns `Some(outcome)`
/// when the event was fully handled (merged or no-op); `None` when no merge is
/// possible and the caller must push it as a new event.
///
/// A merge is only performed when the merged event stays within:
/// * `MAX_EVENT_RAW_BYTES` (per-event raw bound, P0-G);
/// * `MAX_PENDING_EVENT_BYTES_PER_CONNECTION` (per-handle pending bound, P0-G);
/// * `MAX_PENDING_EVENT_BYTES_PER_PLUGIN` (plugin total bound, P0-G).
fn try_merge_receive(
    state: &mut ChannelState,
    handle: u64,
    payload: &serde_json::Value,
    incoming_bytes: usize,
) -> Option<PushOutcome> {
    let new_bytes = payload.get("bytes").and_then(|b| b.as_array())?;
    if new_bytes.is_empty() {
        return Some(PushOutcome::Accepted); // nothing to add — treat as coalesced
    }
    // Read-only checks first.
    {
        let conn = state.mailboxes.get(&handle)?;
        let last = conn.events.back()?;
        if !last.event_type.eq_ignore_ascii_case("tcp:receive") {
            return None;
        }
        let last_bytes = last.payload.get("bytes").and_then(|b| b.as_array())?;
        let merged_len = last_bytes.len().saturating_add(new_bytes.len());
        if merged_len > crate::plugin_tcp::quota::MAX_EVENT_RAW_BYTES {
            return None;
        }
        if conn.pending_bytes.saturating_add(incoming_bytes)
            > crate::plugin_tcp::quota::MAX_PENDING_EVENT_BYTES_PER_CONNECTION
        {
            return None;
        }
        if state.total_bytes.saturating_add(incoming_bytes)
            > crate::plugin_tcp::quota::MAX_PENDING_EVENT_BYTES_PER_PLUGIN
        {
            return None;
        }
    }
    // Perform the merge.
    let conn = state.mailboxes.get_mut(&handle)?;
    let last = conn.events.back_mut()?;
    if let Some(bytes_arr) = last.payload.get_mut("bytes").and_then(|b| b.as_array_mut()) {
        bytes_arr.extend(new_bytes.iter().cloned());
    }
    conn.pending_bytes += incoming_bytes;
    state.total_bytes += incoming_bytes;
    Some(PushOutcome::Accepted)
}

/// Evict normal receive events (oldest-first within the largest mailbox) until
/// at least `need_bytes` bytes and `need_count` events are freed.  Returns the
/// amount actually freed.  Mailboxes currently claimed by a worker are skipped
/// so a live callback is never disturbed.  This is what keeps lifecycle events
/// admissible under a receive flood (P0-F).
fn evict_receive_events(state: &mut ChannelState, need_bytes: usize, need_count: usize) -> (usize, usize) {
    let mut freed_bytes = 0usize;
    let mut freed_count = 0usize;
    while freed_bytes < need_bytes || freed_count < need_count {
        let best = {
            let mut best: Option<(u64, usize)> = None;
            for (&h, conn) in state.mailboxes.iter() {
                if state.active.contains(&h) {
                    continue;
                }
                let Some(front) = conn.events.front() else { continue; };
                if !front.event_type.eq_ignore_ascii_case("tcp:receive") {
                    continue;
                }
                let size = front
                    .payload
                    .get("bytes")
                    .and_then(|b| b.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                if best.map_or(true, |(_, b)| size > b) {
                    best = Some((h, size));
                }
            }
            best
        };
        let Some((h, size)) = best else { break; };
        let is_empty;
        {
            let conn = state.mailboxes.get_mut(&h).unwrap();
            let evt = conn.events.pop_front().unwrap();
            debug_assert!(evt.event_type.eq_ignore_ascii_case("tcp:receive"));
            conn.pending_bytes = conn.pending_bytes.saturating_sub(size);
            state.total_bytes = state.total_bytes.saturating_sub(size);
            state.event_count = state.event_count.saturating_sub(1);
            is_empty = conn.events.is_empty();
        }
        freed_bytes += size;
        freed_count += 1;
        if is_empty {
            state.mailboxes.remove(&h);
            state.ready.retain(|x| *x != h);
        }
    }
    (freed_bytes, freed_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_tcp::quota::{MAX_EVENT_RAW_BYTES, MAX_PENDING_EVENT_BYTES_PER_PLUGIN};

    fn receive(handle: u64, bytes: Vec<u8>) -> (String, serde_json::Value) {
        (
            "tcp:receive".to_string(),
            serde_json::json!({"handle": handle, "plugin_id": "p", "bytes": bytes}),
        )
    }

    fn lifecycle(event_type: &str, handle: u64) -> (String, serde_json::Value) {
        (
            event_type.to_string(),
            serde_json::json!({"handle": handle, "plugin_id": "p"}),
        )
    }

    fn accept(conn_handle: u64) -> (String, serde_json::Value) {
        (
            "tcp:accept".to_string(),
            serde_json::json!({"listener_handle": 1, "conn_handle": conn_handle, "plugin_id": "p"}),
        )
    }

    /// Simulate a worker delivering the next event exactly like
    /// `PluginTcpActor::ensure_event_channel` does: claim → lock → pop →
    /// callback → release (or remove on disconnect).
    async fn next_event(ch: &PluginEventChannel) -> Option<(String, serde_json::Value, u64)> {
        let (handle, conn) = ch.claim_handle()?;
        let _guard = conn.lock().await;
        let evt = ch.pop_event(handle)?;
        let h = handle;
        if evt.event_type.eq_ignore_ascii_case("tcp:disconnect") {
            ch.remove_connection(h);
        }
        drop(_guard);
        ch.release_handle(h);
        Some((evt.event_type, evt.payload, h))
    }

    #[tokio::test]
    async fn per_connection_fifo_preserves_lifecycle_order() {
        let ch = PluginEventChannel::new(16);
        let h = 100u64;

        // accept uses `conn_handle`; everything else uses `handle`.
        let (et, pl) = accept(h);
        assert_eq!(ch.push(et, pl), PushOutcome::Accepted);
        let (et, pl) = receive(h, vec![1, 2]);
        assert_eq!(ch.push(et, pl), PushOutcome::Accepted);
        // A second receive on the same connection is merged (byte order kept).
        let (et, pl) = receive(h, vec![3]);
        assert_eq!(ch.push(et, pl), PushOutcome::Accepted);
        let (et, pl) = lifecycle("tcp:error", h);
        assert_eq!(ch.push(et, pl), PushOutcome::Accepted);
        let (et, pl) = lifecycle("tcp:disconnect", h);
        assert_eq!(ch.push(et, pl), PushOutcome::Accepted);

        let mut order = Vec::new();
        while let Some((et, payload, got_h)) = next_event(&ch).await {
            assert_eq!(got_h, h, "all events must stay on connection {h}");
            if et == "tcp:receive" {
                let b = payload.get("bytes").and_then(|v| v.as_array()).unwrap();
                let nums: Vec<u64> = b.iter().map(|v| v.as_u64().unwrap()).collect();
                assert_eq!(nums, vec![1, 2, 3], "receive chunks keep byte order");
            }
            order.push(et);
        }

        // The disconnect must be strictly last, the accept strictly first.
        assert_eq!(
            order,
            vec!["tcp:accept", "tcp:receive", "tcp:error", "tcp:disconnect"]
        );
        assert_eq!(ch.pending_count(), 0);
        assert_eq!(ch.pending_bytes(), 0);
    }

    #[tokio::test]
    async fn lifecycle_admitted_by_evicting_receives_under_flood() {
        // Deliberately tiny event-count budget.
        let ch = PluginEventChannel::new(4);

        for h in 1..=4u64 {
            let (et, pl) = receive(h, vec![7]);
            assert_eq!(ch.push(et, pl), PushOutcome::Accepted);
        }
        assert_eq!(ch.pending_count(), 4);

        // A further receive is the droppable category (P0-F).
        let (et, pl) = receive(5, vec![7]);
        assert_eq!(ch.push(et, pl), PushOutcome::Dropped);
        assert_eq!(ch.dropped_receive(), 1);

        // A lifecycle event is never silently dropped — it evicts a receive.
        let (et, pl) = lifecycle("tcp:disconnect", 5);
        assert_eq!(ch.push(et, pl), PushOutcome::Accepted);
        assert_eq!(ch.lifecycle_overflow(), 0);
        assert_eq!(ch.pending_count(), 4); // one receive evicted, disconnect added
        assert_eq!(ch.dropped_receive(), 1);

        // The disconnect for handle 5 is delivered eventually.
        let mut saw_disconnect_5 = false;
        for _ in 0..16 {
            let Some((handle, conn)) = ch.claim_handle() else { break; };
            let _guard = conn.lock().await;
            let Some(evt) = ch.pop_event(handle) else { break; };
            if handle == 5 && evt.event_type == "tcp:disconnect" {
                saw_disconnect_5 = true;
            }
            drop(_guard);
            if evt.event_type == "tcp:disconnect" {
                ch.remove_connection(handle);
            } else {
                ch.release_handle(handle);
            }
        }
        assert!(saw_disconnect_5, "disconnect must not be lost under a flood");
    }

    #[test]
    fn concurrent_push_never_overshoots_event_cap() {
        // All budget accounting is guarded by a single queue-state mutex
        // (P0-G), so concurrent pushes from many threads cannot overshoot.
        let ch = std::sync::Arc::new(PluginEventChannel::new(8));
        let mut handles = Vec::new();
        for t in 0..16u64 {
            let ch = Arc::clone(&ch);
            handles.push(std::thread::spawn(move || {
                for i in 0..2u64 {
                    let h = t * 100 + i;
                    let (et, pl) = receive(h, vec![1, 2, 3]);
                    let _ = ch.push(et, pl);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert!(ch.pending_count() <= 8, "event cap must be atomic under concurrency");
        assert!(ch.dropped_count() >= 24, "32 pushes, at most 8 fit");
    }

    #[tokio::test]
    async fn merge_refuses_when_single_event_would_exceed_per_event_bound() {
        let ch = PluginEventChannel::new(8);
        let h = 42u64;

        // MAX_EVENT_RAW_BYTES/2 + MAX_EVENT_RAW_BYTES/2 + 32 → merging would
        // exceed MAX_EVENT_RAW_BYTES, so the second chunk must become its own
        // event instead of a merged mega-event (P0-G).
        let big = vec![0u8; MAX_EVENT_RAW_BYTES / 2 + 16];
        let small = vec![0u8; MAX_EVENT_RAW_BYTES / 2 + 16];
        let (et, pl) = receive(h, big);
        assert_eq!(ch.push(et, pl), PushOutcome::Accepted);
        let (et, pl) = receive(h, small);
        assert_eq!(ch.push(et, pl), PushOutcome::Accepted);

        // Two separate receive events (not one merged mega-event) (P0-G).
        assert_eq!(ch.pending_count(), 2);
        assert!(ch.pending_bytes() <= MAX_PENDING_EVENT_BYTES_PER_PLUGIN);
    }

    #[test]
    fn byte_budget_drops_receive_but_admits_lifecycle() {
        let ch = PluginEventChannel::new(8);

        // A real receive event exists so a lifecycle push can evict it.
        let (et, pl) = receive(1, vec![0u8; 4096]);
        assert_eq!(ch.push(et, pl), PushOutcome::Accepted);
        // Simulate the rest of the plugin's pending data sitting just under the
        // 4 MiB cap (accounting is guarded by the same queue-state mutex, P0-G).
        {
            let mut state = ch.state.lock().unwrap();
            state.total_bytes = MAX_PENDING_EVENT_BYTES_PER_PLUGIN - 1;
        }

        // A further receive would push the total over the cap → dropped.
        let (et, pl) = receive(2, vec![0u8; 16]);
        assert_eq!(ch.push(et, pl), PushOutcome::Dropped);
        assert_eq!(ch.dropped_receive(), 1);

        // A lifecycle event is admitted by evicting the normal receive, never
        // silently dropped (P0-F).
        let (et, pl) = lifecycle("tcp:disconnect", 2);
        assert_eq!(ch.push(et, pl), PushOutcome::Accepted);
        assert_eq!(ch.lifecycle_overflow(), 0);
        assert!(ch.pending_bytes() <= MAX_PENDING_EVENT_BYTES_PER_PLUGIN);
    }
}

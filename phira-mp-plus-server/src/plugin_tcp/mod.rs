//! TCP connection management — WASM plugins connect/listen via WIT host API.
//!
//! Plugins get handles; host manages raw TCP sockets. No TLS.
//! (TLS was stripped because it was never production-ready.)

pub mod actor;
pub mod quota;
pub mod events;

pub use actor::PluginTcpActor;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot, Notify};

// ── Resource limits (PMP25 P5) ──────────────────────────────────────
pub(crate) use quota::{
    MAX_CONNECTIONS_PER_PLUGIN,
    MAX_LISTENERS_PER_PLUGIN,
    MAX_PENDING_EVENTS_PER_PLUGIN,
};

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
/// Per-plugin bounded event channel for TCP events.
///
/// Two queues: a high-priority queue for lifecycle events (accept/disconnect/
/// error/connect) and a normal queue for `tcp:receive`.  When a queue is full
/// the OLDEST event in that queue is dropped — a flood of receive events can
/// never evict a lifecycle event, so the plugin's connection state stays
/// accurate (PMP36 P1: lifecycle priority).
pub(crate) struct PluginEventChannel {
    high: Arc<Mutex<VecDeque<(String, serde_json::Value)>>>,
    normal: Arc<Mutex<VecDeque<(String, serde_json::Value)>>>,
    notify: Arc<Notify>,
    max_len: usize,
    /// Total events dropped because a queue was full (metrics).
    dropped_count: std::sync::atomic::AtomicU64,
    /// Lifecycle events dropped (should be ~0 given priority).
    dropped_lifecycle: std::sync::atomic::AtomicU64,
    /// Receive events dropped (the droppable category).
    dropped_receive: std::sync::atomic::AtomicU64,
    /// Total payload bytes currently buffered across both queues (P0-F).
    total_bytes: std::sync::atomic::AtomicUsize,
    /// Bytes dropped because the byte budget was exceeded (P0-F).
    dropped_bytes: std::sync::atomic::AtomicU64,
}

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

/// Merge an incoming `tcp:receive` payload into the newest queued receive for
/// the same handle (append bytes).  Returns `false` when no merge is possible
/// (queue empty, newest event is not a receive, different handle, or payload
/// shape unexpected) — the caller then falls back to dropping the oldest.
fn merge_receive(
    queue: &mut VecDeque<(String, serde_json::Value)>,
    incoming: &serde_json::Value,
) -> bool {
    let Some(handle) = incoming.get("handle").and_then(|h| h.as_u64()) else {
        return false;
    };
    let Some(incoming_bytes) = incoming.get("bytes").and_then(|b| b.as_array()) else {
        return false;
    };
    if incoming_bytes.is_empty() {
        return true; // nothing to add — treat as coalesced
    }
    let Some((last_type, last_payload)) = queue.back_mut() else {
        return false;
    };
    if !last_type.eq_ignore_ascii_case("tcp:receive") {
        return false;
    }
    if last_payload.get("handle").and_then(|h| h.as_u64()) != Some(handle) {
        return false;
    }
    if let Some(last_bytes) = last_payload.get_mut("bytes").and_then(|b| b.as_array_mut()) {
        last_bytes.extend(incoming_bytes.iter().cloned());
        return true;
    }
    false
}

impl PluginEventChannel {
    pub fn new(max_len: usize) -> Self {
        Self {
            high: Arc::new(Mutex::new(VecDeque::with_capacity(max_len / 2 + 1))),
            normal: Arc::new(Mutex::new(VecDeque::with_capacity(max_len))),
            notify: Arc::new(Notify::new()),
            max_len,
            dropped_count: std::sync::atomic::AtomicU64::new(0),
            dropped_lifecycle: std::sync::atomic::AtomicU64::new(0),
            dropped_receive: std::sync::atomic::AtomicU64::new(0),
            total_bytes: std::sync::atomic::AtomicUsize::new(0),
            dropped_bytes: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Push an event.  Lifecycle events go to the high-priority queue; receive
    /// events to the normal queue.  When a queue is full, a receive event is
    /// first MERGED with the newest receive for the same handle (bytes
    /// appended) and only dropped if no merge is possible; lifecycle events
    /// are dropped only when the high queue itself is full (never evicted by a
    /// receive flood).
    pub fn push(&self, event_type: String, payload: serde_json::Value) {
        let incoming_bytes = event_payload_bytes(&event_type, &payload);
        let is_lifecycle = is_lifecycle(&event_type);
        if is_lifecycle {
            let mut queue = self.high.lock().unwrap();
            if queue.len() >= self.max_len {
                if let Some((t, p)) = queue.pop_front() {
                    let freed = event_payload_bytes(&t, &p);
                    self.total_bytes.fetch_sub(freed, std::sync::atomic::Ordering::Relaxed);
                }
                self.dropped_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.dropped_lifecycle.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            // Byte budget (P0-F): reject the event if it would exceed the cap.
            if self.total_bytes.load(std::sync::atomic::Ordering::Relaxed)
                + incoming_bytes
                > crate::plugin_tcp::quota::MAX_PENDING_EVENT_BYTES_PER_PLUGIN
            {
                self.dropped_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.dropped_lifecycle.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.dropped_bytes.fetch_add(incoming_bytes as u64, std::sync::atomic::Ordering::Relaxed);
                return;
            }
            self.total_bytes.fetch_add(incoming_bytes, std::sync::atomic::Ordering::Relaxed);
            queue.push_back((event_type, payload));
        } else {
            let mut queue = self.normal.lock().unwrap();
            if queue.len() >= self.max_len {
                // Byte budget check applies to MERGE too (P0-D): do not append
                // merged bytes if they would exceed the cap.
                let would_exceed = self.total_bytes.load(std::sync::atomic::Ordering::Relaxed)
                    + incoming_bytes
                    > crate::plugin_tcp::quota::MAX_PENDING_EVENT_BYTES_PER_PLUGIN;
                if !would_exceed && merge_receive(&mut queue, &payload) {
                    // Merged bytes grow the buffered total.
                    self.total_bytes.fetch_add(incoming_bytes, std::sync::atomic::Ordering::Relaxed);
                    drop(queue);
                    self.notify.notify_one();
                    return;
                }
                if let Some((t, p)) = queue.pop_front() {
                    let freed = event_payload_bytes(&t, &p);
                    self.total_bytes.fetch_sub(freed, std::sync::atomic::Ordering::Relaxed);
                }
                self.dropped_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.dropped_receive.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            // Byte budget (P0-F): reject if adding would exceed the cap.
            if self.total_bytes.load(std::sync::atomic::Ordering::Relaxed)
                + incoming_bytes
                > crate::plugin_tcp::quota::MAX_PENDING_EVENT_BYTES_PER_PLUGIN
            {
                self.dropped_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.dropped_receive.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.dropped_bytes.fetch_add(incoming_bytes as u64, std::sync::atomic::Ordering::Relaxed);
                return;
            }
            self.total_bytes.fetch_add(incoming_bytes, std::sync::atomic::Ordering::Relaxed);
            queue.push_back((event_type, payload));
        }
        self.notify.notify_one();
    }

    /// Number of events dropped because a queue was full.
    pub fn dropped_count(&self) -> u64 {
        self.dropped_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Lifecycle events dropped (should be ~0 given priority).
    pub fn dropped_lifecycle(&self) -> u64 {
        self.dropped_lifecycle.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Receive events dropped (the droppable category).
    pub fn dropped_receive(&self) -> u64 {
        self.dropped_receive.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Pop the next event (high priority first).  Decrements total_bytes so
    /// the pending-byte budget accounts for consumption (P0-C).
    pub fn pop(&self) -> Option<(String, serde_json::Value)> {
        let event = {
            let mut high = self.high.lock().unwrap();
            let mut normal = self.normal.lock().unwrap();
            high.pop_front().or_else(|| normal.pop_front())
        };
        if let Some((t, p)) = &event {
            let freed = event_payload_bytes(t, p);
            self.total_bytes.fetch_sub(freed, std::sync::atomic::Ordering::Relaxed);
        }
        event
    }

    /// Total payload bytes currently buffered across both queues.
    pub fn pending_bytes(&self) -> usize {
        self.total_bytes.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Bytes dropped because the byte budget was exceeded.
    pub fn dropped_bytes(&self) -> u64 {
        self.dropped_bytes.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Number of events currently pending in the bounded queues.
    pub fn pending_count(&self) -> usize {
        self.high.lock().unwrap().len() + self.normal.lock().unwrap().len()
    }

    /// Shared queues and notify for worker task consumption.
    /// Workers must drain `high` before `normal` to honor lifecycle priority.
    pub fn shared(
        &self,
    ) -> (
        Arc<Mutex<VecDeque<(String, serde_json::Value)>>>,
        Arc<Mutex<VecDeque<(String, serde_json::Value)>>>,
        Arc<Notify>,
    ) {
        (
            Arc::clone(&self.high),
            Arc::clone(&self.normal),
            Arc::clone(&self.notify),
        )
    }
}

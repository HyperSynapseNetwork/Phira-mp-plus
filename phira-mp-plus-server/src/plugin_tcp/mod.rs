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
    /// Number of events dropped because the channel was full (metrics).
    dropped_count: std::sync::atomic::AtomicU64,
}

/// Event types that are lifecycle-critical and must never be evicted by a
/// receive flood.
fn is_lifecycle(event_type: &str) -> bool {
    !event_type.eq_ignore_ascii_case("tcp:receive")
}

impl PluginEventChannel {
    pub fn new(max_len: usize) -> Self {
        Self {
            high: Arc::new(Mutex::new(VecDeque::with_capacity(max_len / 2 + 1))),
            normal: Arc::new(Mutex::new(VecDeque::with_capacity(max_len))),
            notify: Arc::new(Notify::new()),
            max_len,
            dropped_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Push an event.  Lifecycle events go to the high-priority queue; receive
    /// events to the normal queue.  When a queue is full the oldest event in
    /// that queue is dropped (lifecycle events are never dropped by a receive
    /// flood).
    pub fn push(&self, event_type: String, payload: serde_json::Value) {
        let is_lifecycle = is_lifecycle(&event_type);
        let mut queue = if is_lifecycle {
            self.high.lock().unwrap()
        } else {
            self.normal.lock().unwrap()
        };
        if queue.len() >= self.max_len {
            queue.pop_front();
            self.dropped_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        queue.push_back((event_type, payload));
        drop(queue);
        self.notify.notify_one();
    }

    /// Number of events dropped because a queue was full.
    pub fn dropped_count(&self) -> u64 {
        self.dropped_count.load(std::sync::atomic::Ordering::Relaxed)
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

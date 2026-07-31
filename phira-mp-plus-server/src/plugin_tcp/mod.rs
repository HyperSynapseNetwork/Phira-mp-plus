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
/// When full, drops the oldest event.
pub(crate) struct PluginEventChannel {
    queue: Arc<Mutex<VecDeque<(String, serde_json::Value)>>>,
    notify: Arc<Notify>,
    max_len: usize,
    /// Number of events dropped because the channel was full (metrics).
    dropped_count: std::sync::atomic::AtomicU64,
}

impl PluginEventChannel {
    pub fn new(max_len: usize) -> Self {
        Self {
            queue: Arc::new(Mutex::new(VecDeque::with_capacity(max_len))),
            notify: Arc::new(Notify::new()),
            max_len,
            dropped_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Push an event. If the channel is full, drop the oldest event.
    pub fn push(&self, event_type: String, payload: serde_json::Value) {
        let mut queue = self.queue.lock().unwrap();
        if queue.len() >= self.max_len {
            queue.pop_front();
            self.dropped_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        queue.push_back((event_type, payload));
        self.notify.notify_one();
    }

    /// Number of events dropped because the queue was full.
    pub fn dropped_count(&self) -> u64 {
        self.dropped_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Number of events currently pending in the bounded queue.
    pub fn pending_count(&self) -> usize {
        self.queue.lock().unwrap().len()
    }

    /// Shared queue and notify for worker task consumption.
    pub fn shared(&self) -> (Arc<Mutex<VecDeque<(String, serde_json::Value)>>>, Arc<Notify>) {
        (Arc::clone(&self.queue), Arc::clone(&self.notify))
    }
}

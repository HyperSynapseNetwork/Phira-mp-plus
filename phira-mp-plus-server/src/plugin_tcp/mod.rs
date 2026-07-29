//! TCP connection management — WASM plugins connect/listen via WIT host API.
//!
//! Plugins get handles; host manages raw TCP sockets. No TLS.
//! (TLS was stripped because it was never production-ready.)

pub mod actor;
pub mod quota;
pub mod events;

pub use actor::PluginTcpActor;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

// ── Resource limits (PMP25 P5) ──────────────────────────────────────
pub(crate) use quota::{
    MAX_CONNECTIONS_PER_PLUGIN,
    MAX_LISTENERS_PER_PLUGIN,
    MAX_READ_BUF_PER_CONNECTION,
    MAX_READ_BUF_PER_PLUGIN,
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
}

// Shared type aliases for connection/socket tracking, used by actor and events.
pub(crate) type ConnectionMap = Arc<Mutex<HashMap<u64, mpsc::Sender<Vec<u8>>>>>;
pub(crate) type CloseMap = Arc<Mutex<HashMap<u64, oneshot::Sender<()>>>>;
/// Shared buffer for received data (read task → recv command).
pub(crate) type ReadBufMap = Arc<Mutex<HashMap<u64, Vec<u8>>>>;

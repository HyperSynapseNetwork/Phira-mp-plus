//! TCP connection management — WASM plugins connect/listen via WIT host API.
//!
//! Plugins get handles; host manages raw TCP sockets. No TLS.
//! (TLS was stripped because it was never production-ready.)

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info, warn};

// ── Resource limits (PMP25 P5) ──────────────────────────────────────
const MAX_CONNECTIONS_PER_PLUGIN: u32 = 32;
const MAX_LISTENERS_PER_PLUGIN: u32 = 8;
const MAX_READ_BUF_PER_CONNECTION: usize = 1_048_576;  // 1 MB
const MAX_READ_BUF_PER_PLUGIN: usize = 4_194_304;      // 4 MB

/// Synchronous reply channel for WIT host functions — blocks the calling
/// WASM thread until the async TCP actor processes the command.
pub(crate) type SyncReply<T> = std::sync::mpsc::Sender<Result<T, String>>;

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
}

/// Events from the TCP actor back to the plugin system.
#[derive(Debug, Clone)]
pub enum PluginTcpEvent {
    Accepted {
        listener_handle: u64,
        conn_handle: u64,
        remote_addr: String,
    },
    Received {
        handle: u64,
        bytes: Vec<u8>,
    },
    Disconnected {
        handle: u64,
        reason: String,
    },
    Error {
        handle: u64,
        error: String,
    },
}

type ConnectionMap = Arc<Mutex<HashMap<u64, mpsc::Sender<Vec<u8>>>>>;
type CloseMap = Arc<Mutex<HashMap<u64, oneshot::Sender<()>>>>;
/// Shared buffer for received data (read task → recv command).
type ReadBufMap = Arc<Mutex<HashMap<u64, Vec<u8>>>>;

struct Connection {
    remote_addr: String,
    close_tx: Option<oneshot::Sender<()>>,
}

struct Listener {
    addr: String,
    close_tx: Option<oneshot::Sender<()>>,
    /// Queue of accepted connections waiting for the plugin to call accept().
    pending_accepts: Vec<u64>,
}

/// TCP actor managing connection and listener handles.
pub struct PluginTcpActor {
    rx: mpsc::Receiver<PluginTcpCommand>,
    connections: HashMap<u64, Connection>,
    listeners: HashMap<u64, Listener>,
    next_handle: u64,
    conn_map: ConnectionMap,
    close_map: CloseMap,
    read_buf_map: ReadBufMap,
    event_callback: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
    // ── Resource ownership (PMP25 P5) ───────────────────────────────
    /// handle → owning plugin_id
    handle_owner: HashMap<u64, String>,
    /// plugin_id → connection count
    plugin_connections: HashMap<String, u32>,
    /// plugin_id → listener count
    plugin_listeners: HashMap<String, u32>,
    /// plugin_id → total buffered read bytes
    plugin_read_bytes: HashMap<String, usize>,
}

impl PluginTcpActor {
    pub fn new(rx: mpsc::Receiver<PluginTcpCommand>) -> Self {
        Self {
            rx,
            connections: HashMap::new(),
            listeners: HashMap::new(),
            next_handle: 1,
            conn_map: Arc::new(Mutex::new(HashMap::new())),
            close_map: Arc::new(Mutex::new(HashMap::new())),
            read_buf_map: Arc::new(Mutex::new(HashMap::new())),
            event_callback: None,
            handle_owner: HashMap::new(),
            plugin_connections: HashMap::new(),
            plugin_listeners: HashMap::new(),
            plugin_read_bytes: HashMap::new(),
        }
    }

    /// Return the event callback for wiring (used by PluginManager).
    pub fn event_callback(&self) -> Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>> {
        self.event_callback.clone()
    }

    pub fn set_event_callback(
        &mut self,
        cb: Arc<dyn Fn(String, serde_json::Value) + Send + Sync>,
    ) {
        self.event_callback = Some(cb);
    }

    /// Remove all handles owned by a plugin (for unload cleanup).
    pub fn remove_plugin_handles(&mut self, plugin_id: &str) {
        let handles: Vec<u64> = self.handle_owner.iter()
            .filter(|(_, owner)| owner.as_str() == plugin_id)
            .map(|(h, _)| *h)
            .collect();
        for h in handles {
            self.close_handle(h);
        }
        self.plugin_connections.remove(plugin_id);
        self.plugin_listeners.remove(plugin_id);
        self.plugin_read_bytes.remove(plugin_id);
    }

    fn check_owner(&self, handle: u64, plugin_id: &str) -> Result<(), String> {
        match self.handle_owner.get(&handle) {
            Some(owner) if owner == plugin_id => Ok(()),
            Some(owner) => Err(format!("handle {handle} owned by plugin '{owner}'")),
            None => Err(format!("handle {handle} not found")),
        }
    }

    fn close_handle(&mut self, handle: u64) {
        let _ = self.conn_map.lock().unwrap().remove(&handle);
        let _ = self.close_map.lock().unwrap().remove(&handle);
        let plugin_id = self.handle_owner.remove(&handle);
        if let Some(conn) = self.connections.remove(&handle) {
            if let Some(ref pid) = plugin_id {
                if let Some(cnt) = self.plugin_connections.get_mut(pid) {
                    *cnt = cnt.saturating_sub(1);
                    if *cnt == 0 { self.plugin_connections.remove(pid); }
                }
            }
            if let Some(tx) = conn.close_tx {
                let _ = tx.send(());
            }
            info!(%handle, addr = %conn.remote_addr, "tcp connection closed");
        } else if let Some(listener) = self.listeners.remove(&handle) {
            if let Some(ref pid) = plugin_id {
                if let Some(cnt) = self.plugin_listeners.get_mut(pid) {
                    *cnt = cnt.saturating_sub(1);
                    if *cnt == 0 { self.plugin_listeners.remove(pid); }
                }
            }
            if let Some(tx) = listener.close_tx {
                let _ = tx.send(());
            }
            info!(%handle, addr = %listener.addr, "tcp listener stopped");
        } else {
            warn!(%handle, "tcp close on unknown handle");
        }
    }

    fn alloc_handle(&mut self) -> u64 {
        let h = self.next_handle;
        self.next_handle += 1;
        h
    }

    fn emit_event(&self, event_type: &str, payload: serde_json::Value) {
        if let Some(cb) = &self.event_callback {
            cb(event_type.to_string(), payload);
        }
    }

    pub async fn run(&mut self) {
        info!("tcp actor started");

        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                PluginTcpCommand::Connect { plugin_id, addr, reply } => {
                    if self.count_plugin_connections(&plugin_id) >= MAX_CONNECTIONS_PER_PLUGIN {
                        let _ = reply.send(Err(format!("plugin '{plugin_id}' connection quota exceeded ({MAX_CONNECTIONS_PER_PLUGIN})")));
                        continue;
                    }
                    let handle = self.alloc_handle();
                    let cb = self.event_callback.clone();
                    let cm = Arc::clone(&self.conn_map);
                    let rbm = Arc::clone(&self.read_buf_map);

                    self.handle_owner.insert(handle, plugin_id.clone());
                    *self.plugin_connections.entry(plugin_id.clone()).or_insert(0) += 1;

                    match tcp_connect(&addr, handle, cb, cm, rbm).await {
                        Ok((_, close_tx)) => {
                            self.connections.insert(
                                handle,
                                Connection {
                                    remote_addr: addr.clone(),
                                    close_tx: Some(close_tx),
                                },
                            );
                            info!(%handle, %addr, "tcp connected");
                            let _ = reply.send(Ok(handle));
                        }
                        Err(e) => {
                            warn!(%handle, %addr, error = %e, "tcp connect failed");
                            let _ = reply.send(Err(e));
                        }
                    }
                }
                PluginTcpCommand::Listen { plugin_id, addr, reply } => {
                    if self.count_plugin_listeners(&plugin_id) >= MAX_LISTENERS_PER_PLUGIN {
                        let _ = reply.send(Err(format!("plugin '{plugin_id}' listener quota exceeded ({MAX_LISTENERS_PER_PLUGIN})")));
                        continue;
                    }
                    let handle = self.alloc_handle();
                    let cb = self.event_callback.clone();
                    let cm = Arc::clone(&self.conn_map);
                    let clm = Arc::clone(&self.close_map);
                    let rbm = Arc::clone(&self.read_buf_map);

                    self.handle_owner.insert(handle, plugin_id.clone());
                    *self.plugin_listeners.entry(plugin_id.clone()).or_insert(0) += 1;

                    match tcp_listen(&addr, handle, cb, cm, clm, rbm).await {
                        Ok(close_tx) => {
                            self.listeners.insert(
                                handle,
                                Listener {
                                    addr: addr.clone(),
                                    close_tx: Some(close_tx),
                                    pending_accepts: Vec::new(),
                                },
                            );
                            info!(%handle, %addr, "tcp listener started");
                            let _ = reply.send(Ok(handle));
                        }
                        Err(e) => {
                            warn!(%handle, %addr, error = %e, "tcp listen failed");
                            let _ = reply.send(Err(e));
                        }
                    }
                }
                PluginTcpCommand::Send { plugin_id, handle, bytes } => {
                    if let Err(e) = self.check_owner(handle, &plugin_id) { continue; }
                    let map = self.conn_map.lock().unwrap();
                    if let Some(tx) = map.get(&handle) {
                        if let Err(e) = tx.try_send(bytes) {
                            warn!(%handle, error = %e, "tcp send failed");
                            self.emit_event("tcp:error",
                                serde_json::json!({"handle": handle, "error": e.to_string()}));
                        }
                    } else {
                        warn!(%handle, "tcp send on unknown handle");
                        self.emit_event("tcp:error",
                            serde_json::json!({"handle": handle, "error": "unknown handle"}));
                    }
                }
                PluginTcpCommand::Close { plugin_id, handle } => {
                    if self.check_owner(handle, &plugin_id).is_ok() {
                        self.close_handle(handle);
                    }
                }
                PluginTcpCommand::Accept { plugin_id, listener_handle, reply } => {
                    let result = if let Err(e) = self.check_owner(listener_handle, &plugin_id) {
                        Err(e)
                    } else if let Some(listener) = self.listeners.get_mut(&listener_handle) {
                        if let Some(conn_handle) = listener.pending_accepts.pop() {
                            self.handle_owner.insert(conn_handle, plugin_id.clone());
                            *self.plugin_connections.entry(plugin_id.clone()).or_insert(0) += 1;
                            // Store a minimal connection record for peer_addr support.
                            self.connections.insert(conn_handle, Connection {
                                remote_addr: listener.addr.clone(),
                                close_tx: None,
                            });
                            Ok(Some(conn_handle))
                        } else {
                            Ok(None)
                        }
                    } else {
                        Err(format!("unknown listener handle: {listener_handle}"))
                    };
                    let _ = reply.send(result);
                }
                PluginTcpCommand::Recv { plugin_id, handle, max_bytes, reply } => {
                    let result = if let Err(e) = self.check_owner(handle, &plugin_id) {
                        Err(e)
                    } else {
                        let map = self.read_buf_map.lock().unwrap();
                        if let Some(buf) = map.get(&handle) {
                            if buf.is_empty() {
                                Ok(None)
                            } else {
                                let len = (max_bytes as usize).min(buf.len());
                                let data = buf[..len].to_vec();
                                drop(map);
                                if let Some(b) = self.read_buf_map.lock().unwrap().get_mut(&handle) {
                                    b.drain(..len);
                                }
                                Ok(Some(data))
                            }
                        } else {
                            Err(format!("unknown connection handle: {handle}"))
                        }
                    };
                    let _ = reply.send(result);
                }
                PluginTcpCommand::PeerAddr { plugin_id, handle, reply } => {
                    let result = if let Err(e) = self.check_owner(handle, &plugin_id) {
                        Err(e)
                    } else if let Some(conn) = self.connections.get(&handle) {
                        Ok(conn.remote_addr.clone())
                    } else {
                        Err(format!("unknown handle: {handle}"))
                    };
                    let _ = reply.send(result);
                }
            }
        }
        info!("tcp actor stopped");
    }
}

// ── Plain TCP helpers ───────────────────────────────────────────────

async fn tcp_connect(
    addr: &str,
    handle: u64,
    event_cb: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
    conn_map: ConnectionMap,
    read_buf_map: ReadBufMap,
) -> Result<(mpsc::Sender<Vec<u8>>, oneshot::Sender<()>), String> {
    let stream = TcpStream::connect(addr)
        .await
        .map_err(|e| format!("TCP connect to {addr}: {e}"))?;

    let (data_tx, data_rx) = mpsc::channel::<Vec<u8>>(64);
    let (close_tx, close_rx) = oneshot::channel();
    conn_map.lock().unwrap().insert(handle, data_tx.clone());
    read_buf_map.lock().unwrap().insert(handle, Vec::new());

    let remote = addr.to_string();
    let cm = Arc::clone(&conn_map);
    let rbm = Arc::clone(&read_buf_map);
    tokio::spawn(async move {
        tcp_read_task(stream, handle, data_rx, close_rx, event_cb, remote, rbm).await;
        cm.lock().unwrap().remove(&handle);
    });

    Ok((data_tx, close_tx))
}

async fn tcp_listen(
    addr: &str,
    listener_handle: u64,
    event_cb: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
    conn_map: ConnectionMap,
    close_map: CloseMap,
    read_buf_map: ReadBufMap,
) -> Result<oneshot::Sender<()>, String> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| format!("TCP bind {addr}: {e}"))?;

    let (close_tx, close_rx) = oneshot::channel();
    tokio::spawn(tcp_accept_loop(
        listener, listener_handle, close_rx, event_cb, conn_map, close_map, read_buf_map,
    ));
    Ok(close_tx)
}

async fn tcp_accept_loop(
    listener: TcpListener,
    listener_handle: u64,
    mut close_rx: oneshot::Receiver<()>,
    event_cb: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
    conn_map: ConnectionMap,
    close_map: CloseMap,
    read_buf_map: ReadBufMap,
) {
    let mut next_conn: u64 = 1;
    loop {
        tokio::select! {
            accept = listener.accept() => {
                match accept {
                    Ok((stream, addr)) => {
                        let peer = addr.to_string();
                        let conn_handle = (listener_handle << 32) | next_conn;
                        next_conn += 1;
                        let cb = event_cb.clone();
                        let cm_handle = Arc::clone(&conn_map);
                        let clm_handle = Arc::clone(&close_map);
                        let rbm = Arc::clone(&read_buf_map);
                        // Register read buffer for this connection.
                        rbm.lock().unwrap().insert(conn_handle, Vec::new());
                        // Notify plugin via on-api event.
                        if let Some(ref cb) = cb {
                            cb("tcp:accept".into(), serde_json::json!({
                                "listener_handle": listener_handle,
                                "conn_handle": conn_handle,
                                "remote_addr": peer,
                            }));
                        }
                        tokio::spawn(async move {
                            let (data_tx, data_rx) = mpsc::channel::<Vec<u8>>(64);
                            let (close_tx, close_rx) = oneshot::channel::<()>();
                            cm_handle.lock().unwrap().insert(conn_handle, data_tx);
                            clm_handle.lock().unwrap().insert(conn_handle, close_tx);
                            tcp_read_task(stream, conn_handle, data_rx, close_rx, cb, peer, rbm).await;
                            cm_handle.lock().unwrap().remove(&conn_handle);
                            clm_handle.lock().unwrap().remove(&conn_handle);
                        });
                    }
                    Err(e) => {
                        error!(error = %e, "accept failed");
                        continue;
                    }
                }
            }
            _ = &mut close_rx => {
                info!(%listener_handle, "tcp listener shutting down");
                break;
            }
        }
    }
}

async fn tcp_read_task(
    stream: TcpStream,
    handle: u64,
    mut data_rx: mpsc::Receiver<Vec<u8>>,
    mut close_rx: oneshot::Receiver<()>,
    event_cb: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
    _remote_addr: String,
    read_buf_map: ReadBufMap,
) {
    use tokio::io::AsyncWriteExt;

    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut buf = vec![0u8; 8192];
    let cb = event_cb.unwrap_or_else(|| Arc::new(|_, _| {}));

    loop {
        tokio::select! {
            data = data_rx.recv() => {
                match data {
                    Some(bytes) => {
                        if let Err(e) = writer.write_all(&bytes).await {
                            cb("tcp:error".into(), serde_json::json!({
                                "handle": handle, "error": format!("write: {e}"),
                            }));
                            break;
                        }
                    }
                    None => break,
                }
            }
            result = reader.read(&mut buf) => {
                match result {
                    Ok(0) => {
                        read_buf_map.lock().unwrap().remove(&handle);
                        cb("tcp:disconnect".into(), serde_json::json!({
                            "handle": handle, "reason": "remote peer closed connection",
                        }));
                        break;
                    }
                    Ok(n) => {
                        // Buffer for pull-based recv()
                        read_buf_map.lock().unwrap()
                            .entry(handle).or_default()
                            .extend_from_slice(&buf[..n]);
                        cb("tcp:receive".into(), serde_json::json!({
                            "handle": handle, "bytes": buf[..n].to_vec(),
                        }));
                    }
                    Err(e) => {
                        read_buf_map.lock().unwrap().remove(&handle);
                        cb("tcp:error".into(), serde_json::json!({
                            "handle": handle, "error": format!("read: {e}"),
                        }));
                        break;
                    }
                }
            }
            _ = &mut close_rx => break,
        }
    }
    cb("tcp:disconnect".into(), serde_json::json!({
        "handle": handle, "reason": "connection task exited",
    }));
}

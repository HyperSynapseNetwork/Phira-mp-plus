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

/// Commands plugins send to the TCP actor.
#[derive(Debug)]
pub enum PluginTcpCommand {
    Connect {
        addr: String,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    Listen {
        addr: String,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    Send { handle: u64, bytes: Vec<u8> },
    Close { handle: u64 },
    /// Accept a queued inbound connection (non-blocking).
    Accept {
        listener_handle: u64,
        reply: oneshot::Sender<Result<Option<u64>, String>>,
    },
    /// Read buffered data from a connection (non-blocking).
    Recv {
        handle: u64,
        max_bytes: u32,
        reply: oneshot::Sender<Result<Option<Vec<u8>>, String>>,
    },
    /// Get the remote address of a connection.
    PeerAddr {
        handle: u64,
        reply: oneshot::Sender<Result<String, String>>,
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
    /// Buffer for incoming data (bytes arriving before the plugin calls recv).
    read_buf: Vec<u8>,
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

    fn close_handle(&mut self, handle: u64) {
        let _ = self.conn_map.lock().unwrap().remove(&handle);
        let _ = self.close_map.lock().unwrap().remove(&handle);
        if let Some(conn) = self.connections.remove(&handle) {
            if let Some(tx) = conn.close_tx {
                let _ = tx.send(());
            }
            info!(%handle, addr = %conn.remote_addr, "tcp connection closed");
        } else if let Some(listener) = self.listeners.remove(&handle) {
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
                PluginTcpCommand::Connect { addr, reply } => {
                    let handle = self.alloc_handle();
                    let cb = self.event_callback.clone();
                    let cm = Arc::clone(&self.conn_map);
                    let rbm = Arc::clone(&self.read_buf_map);

                    match tcp_connect(&addr, handle, cb, cm, rbm).await {
                        Ok((_, close_tx)) => {
                            self.connections.insert(
                                handle,
                                Connection {
                                    remote_addr: addr.clone(),
                                    close_tx: Some(close_tx),
                                    read_buf: Vec::new(),
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
                PluginTcpCommand::Listen { addr, reply } => {
                    let handle = self.alloc_handle();
                    let cb = self.event_callback.clone();
                    let cm = Arc::clone(&self.conn_map);
                    let clm = Arc::clone(&self.close_map);
                    let rbm = Arc::clone(&self.read_buf_map);

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
                PluginTcpCommand::Send { handle, bytes } => {
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
                PluginTcpCommand::Close { handle } => {
                    self.close_handle(handle);
                }
                PluginTcpCommand::Accept { listener_handle, reply } => {
                    let result = if let Some(listener) = self.listeners.get_mut(&listener_handle) {
                        if listener.pending_accepts.is_empty() {
                            Ok(None)
                        } else {
                            Ok(Some(listener.pending_accepts.remove(0)))
                        }
                    } else {
                        Err(format!("unknown listener handle: {listener_handle}"))
                    };
                    let _ = reply.send(result);
                }
                PluginTcpCommand::Recv { handle, max_bytes, reply } => {
                    let result = {
                        let map = self.read_buf_map.lock().unwrap();
                        if let Some(buf) = map.get(&handle) {
                            if buf.is_empty() {
                                Ok(None)
                            } else {
                                let len = (max_bytes as usize).min(buf.len());
                                let data = buf[..len].to_vec();
                                // Drop from shared buf after read (done via drain in real usage)
                                drop(map);
                                self.read_buf_map.lock().unwrap().get_mut(&handle)
                                    .map(|b| { b.drain(..len); });
                                Ok(Some(data))
                            }
                        } else {
                            Err(format!("unknown connection handle: {handle}"))
                        }
                    };
                    let _ = reply.send(result);
                }
                PluginTcpCommand::PeerAddr { handle, reply } => {
                    let result = if let Some(conn) = self.connections.get(&handle) {
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

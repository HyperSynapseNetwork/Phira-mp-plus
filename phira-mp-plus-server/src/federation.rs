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
pub enum FederationCommand {
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
}

/// Events from the TCP actor back to the plugin system.
#[derive(Debug, Clone)]
pub enum FederationEvent {
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

struct Connection {
    remote_addr: String,
    close_tx: Option<oneshot::Sender<()>>,
}

struct Listener {
    addr: String,
    close_tx: Option<oneshot::Sender<()>>,
}

/// TCP actor managing connection and listener handles.
pub struct FederationActor {
    rx: mpsc::Receiver<FederationCommand>,
    connections: HashMap<u64, Connection>,
    listeners: HashMap<u64, Listener>,
    next_handle: u64,
    conn_map: ConnectionMap,
    close_map: CloseMap,
    event_callback: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
}

impl FederationActor {
    pub fn new(rx: mpsc::Receiver<FederationCommand>) -> Self {
        Self {
            rx,
            connections: HashMap::new(),
            listeners: HashMap::new(),
            next_handle: 1,
            conn_map: Arc::new(Mutex::new(HashMap::new())),
            close_map: Arc::new(Mutex::new(HashMap::new())),
            event_callback: None,
        }
    }

    pub fn set_event_callback(
        &mut self,
        cb: Arc<dyn Fn(String, serde_json::Value) + Send + Sync>,
    ) {
        self.event_callback = Some(cb);
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
                FederationCommand::Connect { addr, reply } => {
                    let handle = self.alloc_handle();
                    let cb = self.event_callback.clone();
                    let cm = Arc::clone(&self.conn_map);

                    match tcp_connect(&addr, handle, cb, cm).await {
                        Ok((data_tx, close_tx)) => {
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
                FederationCommand::Listen { addr, reply } => {
                    let handle = self.alloc_handle();
                    let cb = self.event_callback.clone();
                    let cm = Arc::clone(&self.conn_map);
                    let clm = Arc::clone(&self.close_map);

                    match tcp_listen(&addr, handle, cb, cm, clm).await {
                        Ok(close_tx) => {
                            self.listeners.insert(
                                handle,
                                Listener {
                                    addr: addr.clone(),
                                    close_tx: Some(close_tx),
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
                FederationCommand::Send { handle, bytes } => {
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
                FederationCommand::Close { handle } => {
                    let _ = self.conn_map.lock().unwrap().remove(&handle);
                    if let Some(close_tx) = self.close_map.lock().unwrap().remove(&handle) {
                        let _ = close_tx.send(());
                        info!(%handle, "tcp accepted connection closed");
                    } else if let Some(conn) = self.connections.remove(&handle) {
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
) -> Result<(mpsc::Sender<Vec<u8>>, oneshot::Sender<()>), String> {
    let stream = TcpStream::connect(addr)
        .await
        .map_err(|e| format!("TCP connect to {addr}: {e}"))?;

    let (data_tx, data_rx) = mpsc::channel::<Vec<u8>>(64);
    let (close_tx, close_rx) = oneshot::channel();
    conn_map.lock().unwrap().insert(handle, data_tx.clone());

    let remote = addr.to_string();
    let cm = Arc::clone(&conn_map);
    tokio::spawn(async move {
        tcp_read_task(stream, handle, data_rx, close_rx, event_cb, remote).await;
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
) -> Result<oneshot::Sender<()>, String> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| format!("TCP bind {addr}: {e}"))?;

    let (close_tx, close_rx) = oneshot::channel();
    tokio::spawn(tcp_accept_loop(
        listener, listener_handle, close_rx, event_cb, conn_map, close_map,
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
                        tokio::spawn(async move {
                            let (data_tx, data_rx) = mpsc::channel::<Vec<u8>>(64);
                            let (close_tx, close_rx) = oneshot::channel::<()>();
                            cm_handle.lock().unwrap().insert(conn_handle, data_tx);
                            clm_handle.lock().unwrap().insert(conn_handle, close_tx);
                            if let Some(ref cb) = cb {
                                cb("tcp:accept".into(), serde_json::json!({
                                    "listener_handle": listener_handle,
                                    "conn_handle": conn_handle,
                                    "remote_addr": peer,
                                }));
                            }
                            tcp_read_task(stream, conn_handle, data_rx, close_rx, cb, peer).await;
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
    mut stream: TcpStream,
    handle: u64,
    mut data_rx: mpsc::Receiver<Vec<u8>>,
    mut close_rx: oneshot::Receiver<()>,
    event_cb: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
    _remote_addr: String,
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
                        cb("tcp:disconnect".into(), serde_json::json!({
                            "handle": handle, "reason": "remote peer closed connection",
                        }));
                        break;
                    }
                    Ok(n) => {
                        cb("tcp:receive".into(), serde_json::json!({
                            "handle": handle, "bytes": buf[..n].to_vec(),
                        }));
                    }
                    Err(e) => {
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

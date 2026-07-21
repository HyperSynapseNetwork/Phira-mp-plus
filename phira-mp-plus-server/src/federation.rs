//! Federation actor — 联邦网络句柄管理与事件分发。
//!
//! Host 只提供连接句柄的分配、状态追踪和事件分发工具 API，
//! 不耦合具体 TCP/TLS 传输实现。具体传输由 WASM 插件通过
//! host API 控制句柄自行实现。
//!
//! Lifecycle:
//!   Plugin: connect(addr, tls_opts) → host alloc handle → handle returned
//!   Plugin: listen(addr, tls_opts) → host alloc handle → listener handle
//!   Plugin: send(handle, bytes) → host routes to plugin connection
//!   Plugin: close(handle) → host frees handle state

use tokio::sync::mpsc;

/// TLS configuration for a federated connection.
#[derive(Debug, Clone)]
pub struct FederationTlsOpts {
    /// Expected peer CA identifiers.
    pub expected_ca_ids: Vec<String>,
    /// Whether to verify peer certificate chain.
    pub verify_peer: bool,
    /// Optional local certificate chain (PEM).
    pub local_cert_chain: Option<String>,
    /// Optional local private key (PEM).
    pub local_private_key: Option<String>,
    /// Minimum TLS version (e.g. "1.2", "1.3").
    pub min_tls_version: Option<String>,
}

/// Commands plugins send to the federation actor.
#[derive(Debug)]
pub enum FederationCommand {
    /// Connect to a remote peer.
    Connect {
        addr: String,
        tls_opts: FederationTlsOpts,
        reply: tokio::sync::oneshot::Sender<Result<u64, String>>,
    },
    /// Start a TLS listener.
    Listen {
        addr: String,
        tls_opts: FederationTlsOpts,
        reply: tokio::sync::oneshot::Sender<Result<u64, String>>,
    },
    /// Send bytes on an established connection.
    Send { handle: u64, bytes: Vec<u8> },
    /// Set read timeout on a connection.
    SetReadTimeout { handle: u64, timeout_ms: u64 },
    /// Close a connection or stop a listener.
    Close { handle: u64 },
}

/// Events from the federation actor back to the plugin system.
#[derive(Debug, Clone)]
pub enum FederationEvent {
    Accepted {
        listener_handle: u64,
        conn_handle: u64,
        peer_pubkey: String,
        peer_ca_id: String,
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

/// Per-connection state tracked by the actor.
struct Connection {
    /// Channel to send data to the connection's read task.
    tx: mpsc::Sender<Vec<u8>>,
    /// Remote address for diagnostics.
    #[allow(dead_code)]
    remote_addr: String,
}

/// Per-listener state.
struct Listener {
    #[allow(dead_code)]
    addr: String,
    /// Channel to signal listener shutdown.
    #[allow(dead_code)]
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
}

/// Federation actor managing connection handles.
pub struct FederationActor {
    rx: mpsc::Receiver<FederationCommand>,
    connections: std::collections::HashMap<u64, Connection>,
    listeners: std::collections::HashMap<u64, Listener>,
    next_handle: u64,
    /// Channel to dispatch federation events back to plugins.
    event_tx: Option<mpsc::Sender<FederationEvent>>,
    /// Plugin callback for on_api dispatch.
    event_callback: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
}

use std::sync::Arc;

impl FederationActor {
    pub fn new(rx: mpsc::Receiver<FederationCommand>) -> Self {
        Self {
            rx,
            connections: std::collections::HashMap::new(),
            listeners: std::collections::HashMap::new(),
            next_handle: 1,
            event_tx: None,
            event_callback: None,
        }
    }

    /// Set the channel to dispatch federation events.
    pub fn set_event_tx(&mut self, tx: mpsc::Sender<FederationEvent>) {
        self.event_tx = Some(tx);
    }

    /// Set the plugin callback for on_api dispatch.
    pub fn set_event_callback(&mut self, cb: Arc<dyn Fn(String, serde_json::Value) + Send + Sync>) {
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
        use tracing::{info, warn};

        info!("federation actor started");

        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                FederationCommand::Connect {
                    addr,
                    tls_opts,
                    reply,
                } => {
                    let handle = self.alloc_handle();
                    let (tx, _rx) = mpsc::channel::<Vec<u8>>(64);
                    self.connections.insert(
                        handle,
                        Connection {
                            tx,
                            remote_addr: addr.clone(),
                        },
                    );
                    info!(%handle, %addr, ?tls_opts.verify_peer, "federation connect requested");
                    // Host 只提供句柄管理与事件分发。具体 TCP/TLS 传输
                    // 由插件通过 host API 控制连接句柄自行实现。
                    let _ = reply.send(Ok(handle));
                }
                FederationCommand::Listen {
                    addr,
                    tls_opts,
                    reply,
                } => {
                    let handle = self.alloc_handle();
                    let (shutdown_tx, _shutdown_rx) = tokio::sync::oneshot::channel::<()>();
                    self.listeners.insert(
                        handle,
                        Listener {
                            addr: addr.clone(),
                            shutdown_tx: Some(shutdown_tx),
                        },
                    );
                    info!(%handle, %addr, ?tls_opts.verify_peer, "federation listen requested");
                    // Host 只提供句柄管理与事件分发。具体 TCP/TLS 传输
                    // 由插件通过 host API 控制连接句柄自行实现。
                    let _ = reply.send(Ok(handle));
                }
                FederationCommand::Send { handle, bytes } => {
                    if let Some(conn) = self.connections.get(&handle) {
                        if let Err(e) = conn.tx.try_send(bytes) {
                            warn!(%handle, error = %e, "federation send failed");
                            self.emit_event(
                                "federation:error",
                                serde_json::json!({
                                    "handle": handle,
                                    "error": e.to_string(),
                                }),
                            );
                        }
                    } else {
                        warn!(%handle, "federation send on unknown handle");
                        self.emit_event("federation:error", serde_json::json!({
                            "handle": handle,
                            "error": "unknown handle",
                        }));
                    }
                }
                FederationCommand::SetReadTimeout { handle, timeout_ms } => {
                    info!(%handle, %timeout_ms, "federation set read timeout");
                    // 宿主记录超时配置，由插件在 read 时自行遵守。
                }
                FederationCommand::Close { handle } => {
                    if self.connections.remove(&handle).is_some() {
                        info!(%handle, "federation connection closed by plugin");
                    } else if self.listeners.remove(&handle).is_some() {
                        info!(%handle, "federation listener stopped by plugin");
                    } else {
                        warn!(%handle, "federation close on unknown handle");
                    }
                }
            }
        }

        info!("federation actor stopped");
    }
}

//! Federation actor — TCP-TLS connection management for plugin federation.
//!
//! Plugins control connection timing and parameters via host API handles.
//! Host owns: TLS handshake, cert validation, socket lifetimes, resource limits.
//!
//! Lifecycle:
//!   Plugin: connect(addr, tls_opts) → host TCP+TLS → handle returned
//!   Plugin: listen(addr, tls_opts) → host binds → listener handle
//!   Host → Plugin: on_api("federation:accept", {listener, conn, peer})
//!   Host → Plugin: on_api("federation:receive", {handle, bytes})
//!   Host → Plugin: on_api("federation:disconnect", {handle, reason})
//!   Host → Plugin: on_api("federation:error", {handle, error})

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
                    // TODO: actual TCP+TLS connect (requires tokio-tcp + rustls)
                    // Current implementation is a WIT ABI placeholder: allocates a handle,
                    // stores connection state, and returns success without establishing a
                    // real transport. Real TCP+TLS requires adding tokio-tcp and rustls
                    // dependencies. Once wired, replace the `reply.send(Ok(handle))` with
                    // actual TcpStream::connect + TLS handshake.
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
                    // TODO: actual TCP+TLS listener (requires tokio-tcp + rustls)
                    // Placeholder: allocates a handle and returns success without binding
                    // a real socket or accepting connections. Real implementation requires
                    // TcpListener::bind + accept loop + TLS handshake per connection.
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
                    // Placeholder: no-op until connection tasks exist. Real implementation
                    // applies the timeout to the connection task's read loop.
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

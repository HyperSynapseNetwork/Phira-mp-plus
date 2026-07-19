//! Federation actor — outbound TCP-TLS connections for plugin federation.
//!
//! Lifecycle:
//!   FederationActor manages connection handles. Plugins never touch the
//!   network stack directly; they operate on handles obtained via
//!   federation:accept / federation:connect callbacks.
//!
//! Events are delivered to plugins via on_api:
//!   "federation:accept"    — new inbound connection
//!   "federation:receive"   — data received on a handle
//!   "federation:disconnect" — connection dropped

use tokio::sync::mpsc;

/// Commands plugins can send to the federation actor.
#[derive(Debug)]
pub enum FederationCommand {
    /// Send bytes on an established connection handle.
    Send {
        handle: u64,
        bytes: Vec<u8>,
    },
    /// Close a connection handle gracefully.
    Close {
        handle: u64,
    },
}

/// Federation actor managing connection handles.
pub struct FederationActor {
    rx: mpsc::Receiver<FederationCommand>,
    /// Tracks active connections: handle → sender.
    connections: std::collections::HashMap<u64, mpsc::Sender<Vec<u8>>>,
    next_handle: u64,
    /// Channel to dispatch federation events back to plugins.
    event_tx: Option<mpsc::Sender<FederationEvent>>,
}

/// Events from the federation actor to the plugin system.
#[derive(Debug, Clone)]
pub enum FederationEvent {
    Accepted {
        handle: u64,
        peer_pubkey: String,
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
}

impl FederationActor {
    pub fn new(rx: mpsc::Receiver<FederationCommand>) -> Self {
        Self {
            rx,
            connections: std::collections::HashMap::new(),
            next_handle: 1,
            event_tx: None,
        }
    }

    /// Set the channel to dispatch federation events.
    pub fn set_event_tx(&mut self, tx: mpsc::Sender<FederationEvent>) {
        self.event_tx = Some(tx);
    }

    pub async fn run(&mut self) {
        use tracing::{error, info, warn};

        info!("federation actor started");

        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                FederationCommand::Send { handle, bytes } => {
                    if let Some(tx) = self.connections.get(&handle) {
                        if let Err(e) = tx.try_send(bytes) {
                            warn!(%handle, error = %e, "federation send failed");
                        }
                    } else {
                        warn!(%handle, "federation unknown handle");
                    }
                }
                FederationCommand::Close { handle } => {
                    if self.connections.remove(&handle).is_some() {
                        info!(%handle, "federation connection closed");
                    }
                }
            }
        }

        info!("federation actor stopped");
    }
}

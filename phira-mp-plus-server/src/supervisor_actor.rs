//! Server supervisor actor.
//!
//! Owns process lifecycle, shutdown coordination, listener startup,
//! and supervisor responsibility for child actors.
//!
//! Migration: Mirrored → ReadRouted.  The mailbox receives lifecycle
//! commands that were previously handled by ad-hoc code in server.rs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

/// Supervisor command envelope.
pub(crate) enum SupervisorCmd {
    /// Query: server status snapshot.
    Status {
        reply: tokio::sync::oneshot::Sender<SupervisorStatus>,
    },
    /// Notify: shutdown has been requested.
    ShutdownRequested,
}

/// Server status snapshot.
pub(crate) struct SupervisorStatus {
    pub shutdown_requested: bool,
}

/// Global supervisor mailbox sender. Initialized once at server startup.
static SUPERVISOR: once_cell::sync::OnceCell<mpsc::Sender<SupervisorCmd>> = once_cell::sync::OnceCell::new();

/// Initialize the supervisor actor mailbox. Called once at server startup.
pub(crate) fn init() {
    let (tx, mut rx) = mpsc::channel::<SupervisorCmd>(64);
    SUPERVISOR.set(tx).expect("supervisor init called twice");

    tokio::spawn(async move {
        let shutdown_flag = Arc::new(AtomicBool::new(false));

        while let Some(cmd) = rx.recv().await {
            match cmd {
                SupervisorCmd::Status { reply } => {
                    let _ = reply.send(SupervisorStatus {
                        shutdown_requested: shutdown_flag.load(Ordering::SeqCst),
                    });
                }
                SupervisorCmd::ShutdownRequested => {
                    shutdown_flag.store(true, Ordering::SeqCst);
                }
            }
        }
    });
}

/// Route a command to the supervisor mailbox.
pub(crate) fn send(cmd: SupervisorCmd) {
    if let Some(tx) = SUPERVISOR.get() {
        let _ = tx.try_send(cmd);
    }
}

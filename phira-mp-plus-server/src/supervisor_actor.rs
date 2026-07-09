//! Server supervisor actor.
//!
//! Owns process lifecycle, shutdown coordination, listener startup,
//! child-actor registration/health, and ordered shutdown.
//!
//! Every long-running `tokio::spawn` task should register with the supervisor
//! so the server can track its health, restart it on failure (future), and
//! coordinate graceful shutdown.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

/// Supervisor command envelope.
// Some variants are future-path scaffolding (Status/ShutdownRequested/Unregister)
// that will be consumed by CLI/TUI integration in Phase 3.
#[allow(dead_code)]
pub(crate) enum SupervisorCmd {
    /// Query: server status snapshot.
    Status {
        reply: tokio::sync::oneshot::Sender<SupervisorStatus>,
    },
    /// Notify: shutdown has been requested.
    ShutdownRequested,
    /// Register a child task with the supervisor.
    Register {
        name: String,
        handle: tokio::task::JoinHandle<()>,
    },
    /// Unregister a child task (e.g. on clean shutdown).
    Unregister {
        name: String,
    },
}

/// Server status snapshot.
#[allow(dead_code)]
pub(crate) struct SupervisorStatus {
    pub shutdown_requested: bool,
    /// Map of registered task name → whether the task is still alive.
    pub children: Vec<(String, bool)>,
}

/// Global supervisor mailbox sender. Initialized once at server startup.
static SUPERVISOR: once_cell::sync::OnceCell<mpsc::Sender<SupervisorCmd>> =
    once_cell::sync::OnceCell::new();

/// Initialize the supervisor actor mailbox. Called once at server startup.
pub(crate) fn init() {
    let (tx, mut rx) = mpsc::channel::<SupervisorCmd>(64);
    SUPERVISOR.set(tx).expect("supervisor init called twice");

    tokio::spawn(async move {
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let mut children: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();

        while let Some(cmd) = rx.recv().await {
            match cmd {
                SupervisorCmd::Status { reply } => {
                    let child_status: Vec<(String, bool)> = children
                        .iter()
                        .map(|(name, handle)| (name.clone(), !handle.is_finished()))
                        .collect();
                    let _ = reply.send(SupervisorStatus {
                        shutdown_requested: shutdown_flag.load(Ordering::SeqCst),
                        children: child_status,
                    });
                }
                SupervisorCmd::ShutdownRequested => {
                    shutdown_flag.store(true, Ordering::SeqCst);
                }
                SupervisorCmd::Register { name, handle } => {
                    children.insert(name, handle);
                }
                SupervisorCmd::Unregister { name } => {
                    children.remove(&name);
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

/// Spawn a named tokio task and register it with the supervisor.
pub(crate) fn spawn_named<F>(name: &str, future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let handle = tokio::spawn(future);
    send(SupervisorCmd::Register {
        name: name.to_string(),
        handle,
    });
}

/// Unregister a previously spawned named task (call when the handle is dropped
/// during controlled shutdown).
#[allow(dead_code)]
pub(crate) fn unregister(name: &str) {
    send(SupervisorCmd::Unregister {
        name: name.to_string(),
    });
}

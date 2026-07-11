//! Process task supervisor.
//!
//! Long-running background tasks register here so completed/panicked tasks are
//! observed instead of becoming detached. During shutdown all registered tasks
//! are aborted and joined within a single deadline. Restart policies are kept
//! explicit at the call site; this supervisor never silently restarts a task
//! whose side effects may not be idempotent.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info, warn};

struct ChildTask {
    name: String,
    handle: tokio::task::JoinHandle<()>,
}

#[allow(dead_code)]
pub(crate) enum SupervisorCmd {
    Status {
        reply: oneshot::Sender<SupervisorStatus>,
    },
    Register {
        id: u64,
        name: String,
        handle: tokio::task::JoinHandle<()>,
    },
    Shutdown {
        timeout: Duration,
        reply: oneshot::Sender<usize>,
    },
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct SupervisorStatus {
    pub shutdown_requested: bool,
    pub children: Vec<(String, bool)>,
}

static SUPERVISOR: once_cell::sync::OnceCell<mpsc::Sender<SupervisorCmd>> =
    once_cell::sync::OnceCell::new();
static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

/// Initialize the process-wide supervisor. Repeated calls within the same
/// server runtime are harmless.
pub fn init() {
    if SUPERVISOR.get().is_some() {
        return;
    }

    let (tx, rx) = mpsc::channel::<SupervisorCmd>(256);
    if SUPERVISOR.set(tx).is_err() {
        return;
    }
    tokio::spawn(run_supervisor(rx));
}

async fn run_supervisor(mut rx: mpsc::Receiver<SupervisorCmd>) {
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let mut children: HashMap<u64, ChildTask> = HashMap::new();
    let mut health_tick = tokio::time::interval(Duration::from_secs(1));
    health_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            command = rx.recv() => {
                let Some(command) = command else {
                    abort_and_join(&mut children, Duration::from_secs(5)).await;
                    break;
                };
                match command {
                    SupervisorCmd::Status { reply } => {
                        let mut status = children
                            .values()
                            .map(|child| (child.name.clone(), !child.handle.is_finished()))
                            .collect::<Vec<_>>();
                        status.sort_by(|a, b| a.0.cmp(&b.0));
                        let _ = reply.send(SupervisorStatus {
                            shutdown_requested: shutdown_flag.load(Ordering::Acquire),
                            children: status,
                        });
                    }
                    SupervisorCmd::Register { id, name, handle } => {
                        if shutdown_flag.load(Ordering::Acquire) {
                            handle.abort();
                            continue;
                        }
                        children.insert(id, ChildTask { name, handle });
                    }
                    SupervisorCmd::Shutdown { timeout, reply } => {
                        shutdown_flag.store(true, Ordering::Release);
                        let stopped = abort_and_join(&mut children, timeout).await;
                        let _ = reply.send(stopped);
                        break;
                    }
                }
            }
            _ = health_tick.tick() => {
                reap_finished(&mut children).await;
            }
        }
    }
}

async fn reap_finished(children: &mut HashMap<u64, ChildTask>) {
    let finished = children
        .iter()
        .filter_map(|(id, child)| child.handle.is_finished().then_some(*id))
        .collect::<Vec<_>>();

    for id in finished {
        let Some(child) = children.remove(&id) else {
            continue;
        };
        match child.handle.await {
            Ok(()) => info!(task = %child.name, "supervised task exited"),
            Err(err) if err.is_cancelled() => {
                info!(task = %child.name, "supervised task was cancelled")
            }
            Err(err) if err.is_panic() => {
                error!(task = %child.name, ?err, "supervised task panicked")
            }
            Err(err) => warn!(task = %child.name, ?err, "supervised task failed"),
        }
    }
}

async fn abort_and_join(children: &mut HashMap<u64, ChildTask>, timeout: Duration) -> usize {
    let mut tasks = children.drain().map(|(_, child)| child).collect::<Vec<_>>();
    let count = tasks.len();
    for child in &tasks {
        child.handle.abort();
    }

    let deadline = Instant::now() + timeout;
    for child in tasks.drain(..) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            warn!(task = %child.name, "supervisor shutdown deadline exhausted");
            break;
        }
        match tokio::time::timeout(remaining, child.handle).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) if err.is_cancelled() => {}
            Ok(Err(err)) => warn!(task = %child.name, ?err, "task failed during shutdown"),
            Err(_) => warn!(task = %child.name, "task did not stop before shutdown deadline"),
        }
    }
    count
}

/// Spawn and register a named task. Names are diagnostic labels rather than
/// unique keys, so per-room/per-session tasks may safely share a prefix.
pub fn spawn_named<F>(name: impl Into<String>, future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    init();
    let name = name.into();
    let id = NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed);
    let handle = tokio::spawn(future);
    let Some(tx) = SUPERVISOR.get().cloned() else {
        handle.abort();
        return;
    };
    let command = SupervisorCmd::Register { id, name, handle };
    match tx.try_send(command) {
        Ok(()) => {}
        Err(mpsc::error::TrySendError::Full(command)) => {
            tokio::spawn(async move {
                if let Err(err) = tx.send(command).await {
                    if let SupervisorCmd::Register { handle, .. } = err.0 {
                        handle.abort();
                    }
                }
            });
        }
        Err(mpsc::error::TrySendError::Closed(command)) => {
            if let SupervisorCmd::Register { handle, .. } = command {
                handle.abort();
            }
        }
    }
}

#[allow(dead_code)]
pub(crate) async fn status() -> Option<SupervisorStatus> {
    let tx = SUPERVISOR.get()?.clone();
    let (reply, rx) = oneshot::channel();
    tx.send(SupervisorCmd::Status { reply }).await.ok()?;
    rx.await.ok()
}

/// Stop all registered background tasks and wait for their JoinHandles.
pub async fn shutdown_all(timeout: Duration) -> usize {
    let Some(tx) = SUPERVISOR.get().cloned() else {
        return 0;
    };
    let (reply, rx) = oneshot::channel();
    if tx
        .send(SupervisorCmd::Shutdown { timeout, reply })
        .await
        .is_err()
    {
        return 0;
    }
    tokio::time::timeout(timeout + Duration::from_secs(1), rx)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(0)
}

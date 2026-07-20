//! Process task supervisor.
//!
//! Long-running background tasks register here so completion and panic are
//! observed instead of becoming detached. Tasks may be marked `Critical`;
//! unexpected completion of a critical task before the process enters its
//! shutdown phase moves the supervisor to a degraded state and records the
//! failure. The supervisor never silently restarts a task whose side effects
//! may not be idempotent.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info, warn};

const MAX_RECENT_FAILURES: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShutdownOrder {
    /// Network listeners (accept new connections). Stopped first.
    Listeners = 0,
    /// Active game sessions. Stopped second.
    Sessions = 1,
    /// Room actors and mailboxes. Stopped third.
    Rooms = 2,
    /// WASM plugin runtime. Stopped fourth.
    Plugins = 3,
    /// Persistence worker + WAL. Stopped fifth.
    Persistence = 4,
    /// Telemetry batcher. Stopped last.
    Telemetry = 5,
    /// No specific ordering (default).
    Unordered = 99,
}

impl ShutdownOrder {
    pub fn label(self) -> &'static str {
        match self {
            Self::Listeners => "listeners",
            Self::Sessions => "sessions",
            Self::Rooms => "rooms",
            Self::Plugins => "plugins",
            Self::Persistence => "persistence",
            Self::Telemetry => "telemetry",
            Self::Unordered => "unordered",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskCriticality {
    BestEffort,
    Critical,
}

impl TaskCriticality {
    fn is_critical(self) -> bool {
        matches!(self, Self::Critical)
    }
}

struct ChildTask {
    name: String,
    criticality: TaskCriticality,
    shutdown_order: ShutdownOrder,
    handle: tokio::task::JoinHandle<()>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct SupervisorFailure {
    pub task: String,
    pub critical: bool,
    pub outcome: String,
    pub observed_at_ms: u64,
}

#[allow(dead_code)]
pub(crate) enum SupervisorCmd {
    Status {
        reply: oneshot::Sender<SupervisorStatus>,
    },
    Register {
        id: u64,
        name: String,
        criticality: TaskCriticality,
        shutdown_order: ShutdownOrder,
        handle: tokio::task::JoinHandle<()>,
    },
    ReportFailure {
        task: String,
        critical: bool,
        outcome: String,
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
    pub degraded: bool,
    pub critical_failures: u64,
    pub children: Vec<(String, bool, bool)>,
    pub recent_failures: Vec<SupervisorFailure>,
}

#[derive(Clone)]
struct SupervisorHandle {
    generation: u64,
    tx: mpsc::Sender<SupervisorCmd>,
}

static SUPERVISOR: once_cell::sync::Lazy<std::sync::RwLock<Option<SupervisorHandle>>> =
    once_cell::sync::Lazy::new(|| std::sync::RwLock::new(None));
static NEXT_SUPERVISOR_GENERATION: AtomicU64 = AtomicU64::new(1);
static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);
static SHUTDOWN_PHASE: AtomicBool = AtomicBool::new(false);
static CRITICAL_FAILURES: AtomicU64 = AtomicU64::new(0);

fn current_sender() -> Option<mpsc::Sender<SupervisorCmd>> {
    SUPERVISOR
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map(|handle| handle.tx.clone())
}

fn clear_supervisor(generation: u64) {
    let mut guard = SUPERVISOR
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard
        .as_ref()
        .map(|handle| handle.generation == generation)
        .unwrap_or(false)
    {
        *guard = None;
    }
}

/// Initialize the process-wide supervisor. A completed supervisor can be
/// recreated in the same process, which keeps integration tests and embedded
/// lifecycle restarts from inheriting a permanently closed sender.
pub fn init() {
    let mut guard = SUPERVISOR
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if guard
        .as_ref()
        .map(|handle| !handle.tx.is_closed())
        .unwrap_or(false)
    {
        return;
    }

    let generation = NEXT_SUPERVISOR_GENERATION.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel::<SupervisorCmd>(256);
    *guard = Some(SupervisorHandle {
        generation,
        tx: tx.clone(),
    });
    SHUTDOWN_PHASE.store(false, Ordering::Release);
    CRITICAL_FAILURES.store(0, Ordering::Release);
    drop(guard);

    tokio::spawn(async move {
        run_supervisor(rx).await;
        clear_supervisor(generation);
    });
}

/// Mark the beginning of the ordered process shutdown phase. Critical tasks
/// that finish after this point are treated as expected shutdown completion.
pub fn begin_shutdown() {
    SHUTDOWN_PHASE.store(true, Ordering::Release);
}

/// Number of unexpected critical task exits observed in this process.
pub fn critical_failure_count() -> u64 {
    CRITICAL_FAILURES.load(Ordering::Acquire)
}

pub fn is_degraded() -> bool {
    critical_failure_count() > 0
}

async fn run_supervisor(mut rx: mpsc::Receiver<SupervisorCmd>) {
    let shutdown_flag = Arc::new(AtomicBool::new(false));
    let mut children: HashMap<u64, ChildTask> = HashMap::new();
    let mut recent_failures = VecDeque::<SupervisorFailure>::new();
    let mut health_tick = tokio::time::interval(Duration::from_secs(1));
    health_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            command = rx.recv() => {
                let Some(command) = command else {
                    begin_shutdown();
                    abort_and_join(&mut children, Duration::from_secs(5)).await;
                    break;
                };
                match command {
                    SupervisorCmd::Status { reply } => {
                        let mut status = children
                            .values()
                            .map(|child| (
                                child.name.clone(),
                                !child.handle.is_finished(),
                                child.criticality.is_critical(),
                            ))
                            .collect::<Vec<_>>();
                        status.sort_by(|a, b| a.0.cmp(&b.0));
                        let _ = reply.send(SupervisorStatus {
                            shutdown_requested: shutdown_flag.load(Ordering::Acquire)
                                || SHUTDOWN_PHASE.load(Ordering::Acquire),
                            degraded: is_degraded(),
                            critical_failures: critical_failure_count(),
                            children: status,
                            recent_failures: recent_failures.iter().cloned().collect(),
                        });
                    }
                    SupervisorCmd::Register { id, name, criticality, shutdown_order, handle } => {
                        if shutdown_flag.load(Ordering::Acquire)
                            || SHUTDOWN_PHASE.load(Ordering::Acquire)
                        {
                            handle.abort();
                            continue;
                        }
                        children.insert(id, ChildTask { name, criticality, shutdown_order, handle });
                    }
                    SupervisorCmd::ReportFailure { task, critical, outcome } => {
                        if !SHUTDOWN_PHASE.load(Ordering::Acquire) {
                            if critical {
                                CRITICAL_FAILURES.fetch_add(1, Ordering::AcqRel);
                            }
                            push_failure(
                                &mut recent_failures,
                                SupervisorFailure {
                                    task,
                                    critical,
                                    outcome,
                                    observed_at_ms: unix_time_ms(),
                                },
                            );
                        }
                    }
                    SupervisorCmd::Shutdown { timeout, reply } => {
                        begin_shutdown();
                        shutdown_flag.store(true, Ordering::Release);
                        let stopped = abort_and_join(&mut children, timeout).await;
                        let _ = reply.send(stopped);
                        break;
                    }
                }
            }
            _ = health_tick.tick() => {
                reap_finished(&mut children, &mut recent_failures).await;
            }
        }
    }
}

async fn reap_finished(
    children: &mut HashMap<u64, ChildTask>,
    recent_failures: &mut VecDeque<SupervisorFailure>,
) {
    let finished = children
        .iter()
        .filter_map(|(id, child)| child.handle.is_finished().then_some(*id))
        .collect::<Vec<_>>();

    for id in finished {
        let Some(child) = children.remove(&id) else {
            continue;
        };
        let expected_shutdown = SHUTDOWN_PHASE.load(Ordering::Acquire);
        let critical = child.criticality.is_critical();
        let outcome = match child.handle.await {
            Ok(()) => {
                info!(task = %child.name, critical, "supervised task exited");
                "exited".to_string()
            }
            Err(err) if err.is_cancelled() => {
                info!(task = %child.name, critical, "supervised task was cancelled");
                "cancelled".to_string()
            }
            Err(err) if err.is_panic() => {
                error!(task = %child.name, critical, ?err, "supervised task panicked");
                "panicked".to_string()
            }
            Err(err) => {
                warn!(task = %child.name, critical, ?err, "supervised task failed");
                format!("join_error:{err}")
            }
        };

        if !expected_shutdown && (critical || outcome == "panicked") {
            if critical {
                CRITICAL_FAILURES.fetch_add(1, Ordering::AcqRel);
            }
            push_failure(
                recent_failures,
                SupervisorFailure {
                    task: child.name,
                    critical,
                    outcome,
                    observed_at_ms: unix_time_ms(),
                },
            );
        }
    }
}

fn push_failure(failures: &mut VecDeque<SupervisorFailure>, failure: SupervisorFailure) {
    if failures.len() >= MAX_RECENT_FAILURES {
        failures.pop_front();
    }
    failures.push_back(failure);
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

async fn abort_and_join(children: &mut HashMap<u64, ChildTask>, timeout: Duration) -> usize {
    // Sort by shutdown order: lower order = shut down first (Listeners → Sessions → ...)
    let mut tasks = children.drain().map(|(_, child)| child).collect::<Vec<_>>();
    tasks.sort_by_key(|child| child.shutdown_order);
    let count = tasks.len();

    // Log the shutdown sequence for observability
    if !tasks.is_empty() {
        let order_str: Vec<String> = tasks
            .iter()
            .map(|t| format!("{}:{}", t.shutdown_order.label(), t.name))
            .collect();
        info!(sequence = %order_str.join(" → "), "shutting down tasks in order");
    }

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

fn spawn_with_criticality<F>(name: impl Into<String>, criticality: TaskCriticality, future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    init();
    let name = name.into();
    let id = NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed);
    let handle = tokio::spawn(future);
    if SHUTDOWN_PHASE.load(Ordering::Acquire) {
        handle.abort();
        return;
    }
    let Some(tx) = current_sender() else {
        handle.abort();
        return;
    };
    let command = SupervisorCmd::Register {
        id,
        name,
        criticality,
        shutdown_order: ShutdownOrder::Unordered,
        handle,
    };
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

/// Spawn and register a best-effort named task.
pub fn spawn_named<F>(name: impl Into<String>, future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    spawn_with_criticality(name, TaskCriticality::BestEffort, future);
}

/// Spawn a named task with an explicit shutdown order.
pub fn spawn_with_order<F>(name: impl Into<String>, shutdown_order: ShutdownOrder, future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let name = name.into();
    let criticality = TaskCriticality::BestEffort;
    let id = NEXT_TASK_ID.fetch_add(1, Ordering::SeqCst);
    let handle = tokio::spawn({
        let n = name.clone();
        async move {
            future.await;
            info!(task = %n, "supervisor registered task completed");
        }
    });
    let command = SupervisorCmd::Register {
        id,
        name: name.clone(),
        criticality,
        shutdown_order,
        handle,
    };
    let Some(tx) = current_sender() else {
        warn!("supervisor not initialized; task '{name}' is unregistered");
        return;
    };
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

/// Spawn and register a process-critical named task. Unexpected completion
/// before [`begin_shutdown`] marks the supervisor degraded.
pub fn spawn_critical<F>(name: impl Into<String>, future: F)
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    spawn_with_criticality(name, TaskCriticality::Critical, future);
}

/// Report a critical subsystem failure that did not terminate its task, such
/// as loss of both the database write and the local dead-letter journal.
pub async fn report_critical_failure(task: impl Into<String>, outcome: impl Into<String>) {
    init();
    let Some(tx) = current_sender() else {
        CRITICAL_FAILURES.fetch_add(1, Ordering::AcqRel);
        return;
    };
    if tx
        .send(SupervisorCmd::ReportFailure {
            task: task.into(),
            critical: true,
            outcome: outcome.into(),
        })
        .await
        .is_err()
    {
        // The supervisor channel itself is unavailable. Preserve the degraded
        // signal in the process-wide counter instead of silently discarding it.
        CRITICAL_FAILURES.fetch_add(1, Ordering::AcqRel);
    }
}

#[allow(dead_code)]
pub(crate) async fn status() -> Option<SupervisorStatus> {
    let tx = current_sender()?;
    let (reply, rx) = oneshot::channel();
    tx.send(SupervisorCmd::Status { reply }).await.ok()?;
    rx.await.ok()
}

/// Stop all registered background tasks and wait for their JoinHandles.
pub async fn shutdown_all(timeout: Duration) -> usize {
    begin_shutdown();
    let Some(tx) = current_sender() else {
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
    let stopped = tokio::time::timeout(timeout + Duration::from_secs(1), rx)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or(0);
    // Wait until the receiver is actually dropped so an immediate same-process
    // restart cannot observe the previous generation as still active.
    let _ = tokio::time::timeout(Duration::from_secs(1), tx.closed()).await;
    stopped
}

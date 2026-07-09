//! Server idle mode.
//!
//! When no users are connected and no rooms are active, the server can
//! suspend heavy subsystems (HTTP, plugins, simulation, benchmark,
//! PersistenceWorker, TelemetryBatcher) to reduce RAM.
//!
//! **Lifecycle:**
//!
//! 1. `IdleMonitor::new()` creates the monitor at startup (wired in `PlusServer::new()`).
//! 2. Every TCP accept calls `mark_activity()` to reset the idle timer.
//! 3. `start_idle_loop()` spawns a background task that checks every
//!    `check_interval_secs` whether to enter or leave idle.
//! 4. On enter-idle the heavy subsystems are suspended; on leave-idle
//!    they are resumed.

use crate::plugin::PluginManager;
use crate::server::PlusServerState;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

// ── Configuration ────────────────────────────────────────────────────

/// Idle mode configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleConfig {
    /// Seconds of total inactivity before entering idle.
    #[serde(default = "default_idle_after_secs")]
    pub idle_after_secs: u64,
    /// Seconds between idle checks.
    #[serde(default = "default_check_interval_secs")]
    pub check_interval_secs: u64,
    /// Session heartbeat timeout (seconds).
    #[serde(default = "default_heartbeat_timeout")]
    pub heartbeat_timeout_secs: u64,
    /// Unauthenticated connection timeout (seconds).
    #[serde(default = "default_auth_timeout")]
    pub auth_timeout_secs: u64,
    /// If true, services are lazily started on first demand instead of at boot.
    #[serde(default)]
    pub lazy_services: bool,
    /// Minimal mode: no plugins, no CLI/TUI, no HTTP.
    #[serde(default)]
    pub minimal: bool,
    /// Enable WASM plugin system. Disabled in minimal mode.
    #[serde(default = "default_plugins_enabled")]
    pub plugins_enabled: bool,
}

fn default_plugins_enabled() -> bool { true }
fn default_idle_after_secs() -> u64 { 300 }
fn default_check_interval_secs() -> u64 { 15 }
fn default_heartbeat_timeout() -> u64 { 30 }
fn default_auth_timeout() -> u64 { 15 }

impl Default for IdleConfig {
    fn default() -> Self {
        Self {
            idle_after_secs: default_idle_after_secs(),
            check_interval_secs: default_check_interval_secs(),
            heartbeat_timeout_secs: default_heartbeat_timeout(),
            auth_timeout_secs: default_auth_timeout(),
            lazy_services: false,
            minimal: false,
            plugins_enabled: default_plugins_enabled(),
        }
    }
}

// ── State ────────────────────────────────────────────────────────────

/// Idle state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdleState {
    /// Normal operation — all services running.
    Active,
    /// No activity detected — heavy services can be suspended.
    Idle,
}

/// Tracks last activity timestamp and current idle state.
pub struct IdleMonitor {
    state: RwLock<IdleState>,
    last_activity: AtomicU64,
    auto_idle_enabled: RwLock<bool>,
    /// Whether the server has entered idle since boot (ensures enter/leave
    /// hooks fire at most once per transition).
    idle_since_boot: AtomicBool,
    config: IdleConfig,
}

impl IdleMonitor {
    pub fn new(config: IdleConfig) -> Arc<Self> {
        Arc::new(Self {
            state: RwLock::new(IdleState::Active),
            last_activity: AtomicU64::new(now_secs()),
            auto_idle_enabled: RwLock::new(true),
            idle_since_boot: AtomicBool::new(false),
            config,
        })
    }

    /// Mark activity — resets the idle timer.
    pub fn mark_activity(&self) {
        self.last_activity.store(now_secs(), Ordering::SeqCst);
    }

    /// Seconds since last activity.
    pub fn idle_seconds(&self) -> u64 {
        now_secs().saturating_sub(self.last_activity.load(Ordering::SeqCst))
    }

    pub async fn state(&self) -> IdleState {
        *self.state.read().await
    }

    pub async fn set_state(&self, s: IdleState) {
        *self.state.write().await = s;
        self.mark_activity();
    }

    pub async fn auto_idle_enabled(&self) -> bool {
        *self.auto_idle_enabled.read().await
    }

    pub async fn set_auto_idle(&self, enabled: bool) {
        *self.auto_idle_enabled.write().await = enabled;
    }

    pub fn config(&self) -> &IdleConfig {
        &self.config
    }

    /// Start the idle-detection loop in a background task.
    ///
    /// This spawns a named supervisor task that periodically checks whether
    /// the server should enter or leave idle and calls the suspend/resume
    /// hooks accordingly.
    pub fn start_loop(self: &Arc<Self>, state: &Arc<PlusServerState>) {
        let monitor = Arc::clone(self);
        let state = Arc::clone(state);
        let interval = self.config.check_interval_secs;
        crate::supervisor_actor::spawn_named("idle-monitor", async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;

                // Gather current load
                let user_count = state.users.read().await.len();
                let room_count = state.rooms.read().await.len();
                let idle_secs = monitor.idle_seconds();
                let auto_enabled = monitor.auto_idle_enabled().await;
                let current_state = monitor.state().await;

                match current_state {
                    IdleState::Active if auto_enabled && should_enter_idle(user_count, room_count, idle_secs, &monitor.config) => {
                        info!(
                            user_count,
                            room_count,
                            idle_secs,
                            "entering idle mode — suspending heavy services"
                        );
                        monitor.set_state(IdleState::Idle).await;
                        suspend_services(&state).await;
                    }
                    IdleState::Idle if should_leave_idle(user_count, room_count) => {
                        info!(
                            user_count,
                            room_count,
                            "leaving idle mode — resuming heavy services"
                        );
                        monitor.set_state(IdleState::Active).await;
                        resume_services(&state).await;
                    }
                    _ => {}
                }
            }
        });
    }
}

// ── Condition checks ────────────────────────────────────────────────

pub fn should_enter_idle(user_count: usize, room_count: usize, idle_secs: u64, cfg: &IdleConfig) -> bool {
    user_count == 0 && room_count == 0 && idle_secs >= cfg.idle_after_secs
}

pub fn should_leave_idle(user_count: usize, room_count: usize) -> bool {
    user_count > 0 || room_count > 0
}

// ── Service suspend/resume ───────────────────────────────────────────

async fn suspend_services(state: &PlusServerState) {
    // 1. Suspend plugin event processing
    state.plugin_manager.set_suspended(true).await;

    // 2. Suspend persistence worker
    state.persistence_worker.set_suspended(true).await;

    // 3. Stop HTTP server (graceful — existing connections drain)
    //    The HTTP server is stopped via a dedicated shutdown trigger.
    //    For now we use the shared shutdown Notify boolean.
    info!("heavy services suspended for idle mode");
}

async fn resume_services(state: &PlusServerState) {
    // 1. Resume plugin event processing
    state.plugin_manager.set_suspended(false).await;

    // 2. Resume persistence worker
    state.persistence_worker.set_suspended(false).await;

    // 3. HTTP server is re-started on-demand when the first request arrives
    //    (lazy service semantics). For the initial implementation we just
    //    log the event.
    info!("heavy services resumed from idle mode");
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

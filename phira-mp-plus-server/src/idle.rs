//! Server idle mode.
//!
//! When no users are connected and no rooms are active, the server can
//! suspend heavy subsystems (HTTP, plugins, simulation, benchmark,
//! PersistenceWorker, TelemetryBatcher) to reduce RAM.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;

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
    last_activity: AtomicU64, // unix seconds
    auto_idle_enabled: RwLock<bool>,
}

impl IdleMonitor {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: RwLock::new(IdleState::Active),
            last_activity: AtomicU64::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            ),
            auto_idle_enabled: RwLock::new(true),
        })
    }

    /// Mark activity — resets the idle timer.
    pub fn mark_activity(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.last_activity.store(now, Ordering::SeqCst);
    }

    /// Seconds since last activity.
    pub fn idle_seconds(&self) -> u64 {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now.saturating_sub(self.last_activity.load(Ordering::SeqCst))
    }

    pub async fn state(&self) -> IdleState {
        *self.state.read().await
    }

    pub async fn set_state(&self, s: IdleState) {
        *self.state.write().await = s;
        // Mark activity on state transitions so the monitor doesn't
        // immediately flip back.
        self.mark_activity();
    }

    pub async fn auto_idle_enabled(&self) -> bool {
        *self.auto_idle_enabled.read().await
    }

    pub async fn set_auto_idle(&self, enabled: bool) {
        *self.auto_idle_enabled.write().await = enabled;
    }
}

/// Check whether the server should enter or leave idle.
#[allow(dead_code)]
pub fn should_enter_idle(user_count: usize, room_count: usize, idle_secs: u64, cfg: &IdleConfig) -> bool {
    user_count == 0 && room_count == 0 && idle_secs >= cfg.idle_after_secs
}

#[allow(dead_code)]
pub fn should_leave_idle(user_count: usize, room_count: usize) -> bool {
    user_count > 0 || room_count > 0
}

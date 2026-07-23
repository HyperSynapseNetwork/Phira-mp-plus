//! Event subscribers, publishers, and monitor routing.
//!
//! Extracted from the original `server.rs` to reduce complexity in the
//! orchestration layer.

use crate::benchmark_report::BenchmarkReport;
use crate::event_bus::MpEvent;
use crate::plugin::PluginEvent;
use crate::server::state::{PlusServerState, ServerStats};
use phira_mp_common::{RoomEvent, ServerCommand};
use std::sync::Arc;
use tracing::{trace, warn};

// ── Spawned event observers ──────────────────────────────────────────

pub fn spawn_runtime_event_observer(event_bus: Arc<crate::event_bus::EventBus>) {
    let mut rx = event_bus.subscribe();
    crate::supervisor_actor::spawn_named("runtime-event-observer", async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    trace!(kind = event.kind(), summary = %event.summary(), "runtime event observed");
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "runtime event observer lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Subscribe to EventBus events and drive real side effects.
pub fn spawn_event_subscribers(state: &Arc<PlusServerState>) {
    let mut rx = state.event_bus.subscribe();
    let state_clone = Arc::clone(state);
    crate::supervisor_actor::spawn_critical("event-subscribers", async move {
        loop {
            match rx.recv().await {
                Ok(event) => match &event {
                    MpEvent::SimulationStarted { .. } => {
                        state_clone
                            .broadcast_system_message(
                                "服务器正在进行性能测试，期间可能出现短暂卡顿。",
                            )
                            .await;
                    }
                    MpEvent::SimulationStopped { .. } => {
                        state_clone
                            .broadcast_system_message("性能测试已结束，感谢您的耐心等待。")
                            .await;
                    }
                    MpEvent::BenchmarkCompleted { report } => {
                        state_clone.benchmark_reports.record(report.clone());
                    }
                    _ => {}
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "event subscriber lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

// ── PlusServer impl (event helpers) ──────────────────────────────────

use super::state::PlusServer;

impl PlusServer {
    /// 触发插件事件
    pub async fn trigger_event(&self, event: &PluginEvent) {
        self.state.dispatch_plugin_event(event.clone()).await;
    }

    /// 获取服务器统计信息
    pub async fn stats(&self) -> ServerStats {
        let user_count = self
            .state
            .users
            .read()
            .await
            .values()
            .filter(|user| user.id > 0)
            .count();
        let room_count = self.state.rooms.read().await.len();
        let session_count = self.state.sessions.read().await.len();
        let plugin_count = self.state.plugin_manager.list_plugins().await.len();

        ServerStats {
            users_online: user_count,
            active_rooms: room_count,
            active_sessions: session_count,
            loaded_plugins: plugin_count,
            port: self.state.config.port,
        }
    }
}

// ── PlusServerState event helpers ────────────────────────────────────

impl PlusServerState {
    /// Canonical domain event dispatch: persistence → plugin → telemetry.
    ///
    /// This is the single entry point for all domain events during ordinary
    /// operation, replacing the older pattern of calling
    /// `publish_runtime_event()` + `dispatch_plugin_event()` separately.
    ///
    /// Migration: new code should call `canonical_event()` instead of the
    /// two-step pattern. The old functions remain for backward compatibility
    /// during the transition (Phase A → Phase B).
    pub async fn canonical_event(
        &self,
        event: crate::event_bus::MpEvent,
        plugin_event: Option<PluginEvent>,
    ) {
        // 1. EventBus (observational / diagnostic tracing)
        self.event_bus.publish(event);
        // 2. Plugin delivery (if applicable)
        if let Some(pe) = plugin_event {
            self.plugin_manager.dispatch_event(pe).await;
        }
        // 3. Persistence (future: canonical event pipeline integration)
        // 4. Telemetry (future: automatic telemetry event recording)
    }

    /// Publish a plugin event to the diagnostic bus and the reliable bounded
    /// plugin dispatcher. The bus is observational; delivery is owned by the
    /// dispatcher rather than a broadcast subscriber.
    pub async fn dispatch_plugin_event(&self, event: PluginEvent) {
        self.event_bus
            .publish(crate::event_bus::MpEvent::PluginEventDispatched(Arc::new(
                event.clone(),
            )));
        self.plugin_manager.dispatch_event(event).await;
    }

    pub async fn publish_user_connected(
        &self,
        user_id: i32,
        user_name: String,
        user_ip: String,
        user_language: String,
    ) {
        let mp_event = crate::event_bus::MpEvent::UserConnected {
            user_id,
            user_name: user_name.clone(),
            user_ip: user_ip.clone(),
            user_language,
        };
        let plugin_event = PluginEvent::UserConnect {
            user_id,
            user_name,
            user_ip,
        };
        self.canonical_event(mp_event, Some(plugin_event)).await;
    }

    pub async fn publish_user_disconnected(&self, user_id: i32, user_name: String) {
        self.canonical_event(
            crate::event_bus::MpEvent::UserDisconnected {
                user_id,
                user_name: user_name.clone(),
            },
            Some(PluginEvent::UserDisconnect { user_id, user_name }),
        )
        .await;
    }

    /// Publish a diagnostic/runtime event.
    ///
    /// Mandatory plugin and persistence side effects use their dedicated bounded
    /// dispatchers; the broadcast EventBus is an observation channel and may lag.
    pub fn publish_runtime_event(&self, event: crate::event_bus::MpEvent) -> usize {
        if let crate::event_bus::MpEvent::BenchmarkCompleted { report } = &event {
            self.benchmark_reports.record(report.clone());
        }
        self.event_bus.publish(event)
    }

    pub fn publish_benchmark_completed(&self, report: &BenchmarkReport) -> usize {
        self.publish_runtime_event(crate::event_bus::MpEvent::BenchmarkCompleted {
            report: report.clone(),
        })
    }

    /// Broadcast a system chat message to every currently connected normal user.
    ///
    /// This is intentionally small and side-effect-only. Runtime v2 background
    /// tasks use it for simulation lifecycle notices without reaching into the
    /// CLI handler. User Arcs are cloned before awaiting so the global users lock
    /// is never held across network sends.
    pub async fn broadcast_system_message(&self, message: &str) -> usize {
        let recipients = {
            let users = self.users.read().await;
            users.values().cloned().collect::<Vec<_>>()
        };
        let cmd = ServerCommand::Message(phira_mp_common::Message::Chat {
            user: 0,
            content: format!("[系统广播] {message}"),
        });
        let mut sent = 0usize;
        for user in recipients {
            user.try_send(cmd.clone()).await;
            sent += 1;
        }
        sent
    }

    /// Publish a room event to the SSE hub and the room monitor (if connected).
    pub async fn publish_room_event(&self, event: RoomEvent) {
        // Enqueue to PersistenceWorker (exclusive — no direct DB fallback)
        let _ = self
            .persistence_worker
            .enqueue(crate::persistence::message::PersistenceEvent::ServerEvent {
                kind: event.event_type().to_string(),
                payload: Arc::new(event.clone().inner()),
                simulation: false,
            })
            .await;
        self.events.publish_room_event(event.clone());
        if let Some(monitor) = self.get_room_monitor().await {
            monitor.try_send(ServerCommand::RoomEvent(event)).await;
        }
    }
}

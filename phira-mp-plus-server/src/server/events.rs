//! Event subscribers, publishers, and monitor routing.
//!
//! Extracted from the original `server.rs` to reduce complexity in the
//! orchestration layer.

use crate::benchmark::report::BenchmarkReport;
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
    let _state = Arc::clone(state);
    crate::supervisor_actor::spawn_critical("event-subscribers", async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let _ = &event;
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
    /// New code calls `canonical_event()` instead of the former two-step
    /// dispatch pattern. The older functions remain only for compatibility
    /// with callers that have not yet migrated.
    pub async fn canonical_event(
        &self,
        event: crate::event_bus::MpEvent,
        plugin_event: Option<PluginEvent>,
    ) {
        // 1. EventBus (observational / diagnostic tracing)
        // Clone event before publish since we need it for persistence lookup
        let persistence_event = Self::mp_event_to_persistence_event(&event);
        self.event_bus.publish(event);
        // 2. Plugin delivery (if applicable)
        if let Some(pe) = plugin_event {
            self.plugin_manager.dispatch_event(pe).await;
        }
        // 3. Persistence — enqueue server event for durable storage
        if let Some(pe) = persistence_event {
            if let Err(_e) = self.persistence_worker.enqueue(pe).await {
                tracing::warn!("canonical_event: failed to enqueue persistence event");
            }
        }
    }

    /// Convert an MpEvent to a PersistenceEvent for durable storage, if applicable.
    fn mp_event_to_persistence_event(
        event: &crate::event_bus::MpEvent,
    ) -> Option<crate::persistence::message::PersistenceEvent> {
        use crate::event_bus::MpEvent;
        match event {
            MpEvent::GameStarted { room_id, round_id } => Some(
                crate::persistence::message::PersistenceEvent::ServerEvent {
                    kind: "game_started".to_string(),
                    payload: Arc::new(serde_json::json!({
                        "room_id": room_id.to_string(),
                        "round_id": round_id,
                    })),
                },
            ),
            MpEvent::RoomStateChanged { room_id, state } => Some(
                crate::persistence::message::PersistenceEvent::ServerEvent {
                    kind: "room_state_changed".to_string(),
                    payload: Arc::new(serde_json::json!({
                        "room_id": room_id.to_string(),
                        "state": state,
                    })),
                },
            ),
            MpEvent::RoomJoined { room_id, user_id }
            | MpEvent::RoomLeft { room_id, user_id } => {
                let kind = if matches!(event, MpEvent::RoomJoined { .. }) {
                    "room_joined"
                } else {
                    "room_left"
                };
                Some(
                    crate::persistence::message::PersistenceEvent::ServerEvent {
                        kind: kind.to_string(),
                        payload: Arc::new(serde_json::json!({
                            "room_id": room_id.to_string(),
                            "user_id": user_id,
                        })),
                    },
                )
            }
            MpEvent::BenchmarkCompleted { report } => {
                Some(crate::persistence::message::PersistenceEvent::BenchmarkReport {
                    report: report.clone(),
                })
            }
            MpEvent::PluginEventDispatched(_)
            | MpEvent::UserConnected { .. }
            | MpEvent::UserDisconnected { .. }
            | MpEvent::ChatMessage { .. } => None,
            // Remaining MpEvent variants are not persisted (observational only)
            _ => None,
        }
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
        self.event_bus.publish(event)
    }

    pub fn publish_benchmark_completed(&self, report: &BenchmarkReport) -> usize {
        self.publish_runtime_event(crate::event_bus::MpEvent::BenchmarkCompleted {
            report: report.clone(),
        })
    }

    /// Broadcast a system chat message to every currently connected normal user.
    ///
    /// This is intentionally small and side-effect-only. User Arcs are cloned
    /// before awaiting so the global users lock is never held across network sends.
    pub async fn broadcast_system_message(&self, message: &str) -> usize {
        let recipients = {
            let users = self.users.read().await;
            users.values().cloned().collect::<Vec<_>>()
        };
        let mut sent = 0usize;
        for user in recipients {
            let empty_args = fluent::FluentArgs::new();
            let prefix = crate::l10n::translate_system(&user.lang, "system-broadcast-prefix", &empty_args);
            user.try_send(
                ServerCommand::Message(phira_mp_common::Message::Chat {
                    user: 0,
                    content: format!("{prefix} {message}"),
                }),
                // 系统广播消息非房间状态事件，cutover 不适用。
                None,
            )
            .await;
            sent += 1;
        }
        sent
    }

    /// Publish a room event to the SSE hub and the room monitor (if connected).
    pub async fn publish_room_event(&self, event: RoomEvent) {
        // Enqueue to PersistenceWorker (exclusive — no direct DB fallback)
        if let Err(e) = self
            .persistence_worker
            .enqueue(crate::persistence::message::PersistenceEvent::ServerEvent {
                kind: event.event_type().to_string(),
                payload: Arc::new(event.clone().inner()),
            })
            .await
        {
            warn!(kind = %e.kind(), "publish_room_event enqueue failed");
        }
        self.events.publish_room_event(event.clone());
        if let Some(monitor) = self.get_room_monitor().await {
            // 监控事件非客户端状态事件，cutover 不适用。
            monitor.try_send(ServerCommand::RoomEvent(event), None).await;
        }
    }
}

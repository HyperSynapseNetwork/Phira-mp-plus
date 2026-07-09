//! Event subscribers, publishers, and monitor routing.
//!
//! Extracted from the original `server.rs` to reduce complexity in the
//! orchestration layer.

use crate::event_bus::MpEvent;
use crate::server::PlusServerState;
use std::sync::Arc;
use tracing::{info, trace, warn};

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
///
/// This is the Runtime v2 event-driven subscriber that gradually replaces direct
/// calls in session.rs / runner.rs. Old paths remain intact (dual-write) until
/// the EventBus path is proven stable.
pub fn spawn_event_subscribers(state: &Arc<PlusServerState>) {
    let mut rx = state.event_bus.subscribe();
    let state_clone = Arc::clone(state);
    crate::supervisor_actor::spawn_named("event-subscribers", async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    match &event {
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
                            state_clone.benchmark_reports.push(report.clone()).await;
                        }
                        MpEvent::StateQuery { method, args, reply } => {
                            let result = (state_clone.server_state_query)(method, args);
                            let _ = reply.send(result);
                        }
                        MpEvent::UserConnected { user_id } => {
                            state_clone
                                .plugin_manager
                                .trigger(&crate::plugin::PluginEvent::UserJoin {
                                    user_id: *user_id,
                                })
                                .await;
                        }
                        MpEvent::UserDisconnected { user_id } => {
                            state_clone
                                .plugin_manager
                                .trigger(&crate::plugin::PluginEvent::UserLeave {
                                    user_id: *user_id,
                                })
                                .await;
                        }
                        _ => {}
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "event subscriber lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Subscribe to EventBus PluginEventDispatched messages and dispatch
/// them to the plugin manager.  This decouples plugin dispatch from
/// the legacy synchronous event path.
pub fn spawn_plugin_subscriber(state: &Arc<PlusServerState>) {
    let mut rx = state.event_bus.subscribe();
    let pm = Arc::clone(&state.plugin_manager);
    crate::supervisor_actor::spawn_named("plugin-subscriber", async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let MpEvent::PluginEventDispatched(plugin_event) = &event {
                        if !pm.has_plugins().await {
                            continue;
                        }
                        if !pm.try_spawn_trigger((**plugin_event).clone()) {
                            trace!(
                                kind = plugin_event.kind(),
                                "dropping plugin event because plugin event concurrency is saturated"
                            );
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "plugin subscriber lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

//! Runtime v2 room command handler — all commands write actor_state.
//!
//! execute_with_actor() handles all 17 commands by writing actor_state
//! first, then broadcasting via Room, then returning a typed payload.
//! The caller (execute_command in actor.rs) calls sync_to_room() after
//! to push actor_state → Room.
//!
//! Legacy `execute()` and all `_in_actor` methods have been removed.

use super::{
    command::RoomActorCommand, context::RoomCommandContext, RoomCommandDelivery,
    RoomCommandPayload, RoomCommandResult,
};
use crate::plugin::PluginEvent;
use phira_mp_common::{Message, PartialRoomData, RoomEvent, ServerCommand};
use serde_json::json;

fn typed_err(msg: &str) -> RoomCommandResult {
    RoomCommandResult::Err {
        delivery: RoomCommandDelivery::PerRoomMailbox,
        error: msg.to_string(),
    }
}

pub(super) struct RoomCommandHandler;

impl RoomCommandHandler {
    /// Execute a command against actor-owned state.
    /// Each handler writes actor_state first, then broadcasts via Room,
    /// then returns the typed payload.  The caller (execute_command)
    /// calls sync_to_room() afterward to push actor_state → Room.
    pub(super) async fn execute_with_actor(
        mut ctx: RoomCommandContext<'_>,
        command: &RoomActorCommand,
    ) -> RoomCommandResult {
        let room = ctx.room.clone();
        let state = ctx.state;
        let rid = || command.room_id().to_string();

        macro_rules! ok {
            ($payload:expr) => {
                RoomCommandResult::ok($payload, RoomCommandDelivery::PerRoomMailbox)
            };
        }

        match command {
            RoomActorCommand::SetLock { locked, actor_user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                as_.state.set_locked(*locked);
                if let Some(ref room) = room {
                    room.set_locked(*locked);
                    room.send(Message::LockRoom { lock: *locked }).await;
                    room.publish_update(PartialRoomData {
                        lock: Some(*locked), ..Default::default()
                    }).await;
                }
                state.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: *actor_user_id, room_id: rid(),
                    data: json!({"action":"lock","value":locked}).to_string(),
                }).await;
                ok!(RoomCommandPayload::LockChanged { room_id: rid(), locked: *locked })
            }

            RoomActorCommand::SetCycle { cycle, actor_user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                as_.state.set_cycle(*cycle);
                if let Some(ref room) = room {
                    room.set_cycle(*cycle);
                    room.send(Message::CycleRoom { cycle: *cycle }).await;
                    room.publish_update(PartialRoomData {
                        cycle: Some(*cycle), ..Default::default()
                    }).await;
                }
                state.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: *actor_user_id, room_id: rid(),
                    data: json!({"action":"cycle","value":cycle}).to_string(),
                }).await;
                ok!(RoomCommandPayload::CycleChanged { room_id: rid(), cycle: *cycle })
            }

            RoomActorCommand::SetHidden { hidden, .. } => {
                let as_ = ctx.expect_actor_state();
                as_.state.set_hidden(*hidden);
                if let Some(ref room) = room {
                    room.set_hidden(*hidden);
                }
                state.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: 0, room_id: rid(),
                    data: json!({"action":"hidden","value":hidden}).to_string(),
                }).await;
                ok!(RoomCommandPayload::HiddenChanged { room_id: rid(), hidden: *hidden })
            }

            // ── SetHost ─────────────────────────────────────────────────
            RoomActorCommand::SetHost { target_id, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = room.as_ref().ok_or_else(|| typed_err("no room"))?;
                let user_name = match target_id {
                    Some(uid) => {
                        for u in r.users().await {
                            if u.id == *uid {
                                let name = r.display_name(&u).await;
                                r.send(Message::Chat {
                                    user: 0, content: format!("房主已转移给 {name}"),
                                }).await;
                                r.set_host(Some(*uid), true).await.map_err(|e| e.to_string())?;
                                as_.state.control.host_id = Some(*uid);
                                as_.state.control.system_host = false;
                                name
                            }
                        }
                        uid.to_string()
                    }
                    None => {
                        r.send(Message::Chat {
                            user: 0, content: "房主已设为系统 ?".to_string(),
                        }).await;
                        r.set_host(None, true).await.map_err(|e| e.to_string())?;
                        as_.state.control.host_id = None;
                        as_.state.control.system_host = true;
                        "?".to_string()
                    }
                };
                ok!(RoomCommandPayload::HostChanged {
                    room_id: rid(), host: *target_id,
                    host_name: user_name, host_is_system: target_id.is_none(),
                })
            }

            // ── SetEndpoint ─────────────────────────────────────────────
            RoomActorCommand::SetEndpoint { endpoint, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = room.as_ref().ok_or_else(|| typed_err("no room"))?;
                r.set_phira_api_endpoint(endpoint.clone());
                as_.state.control.phira_api_endpoint = Some(endpoint.clone());
                ok!(RoomCommandPayload::EndpointChanged {
                    room_id: rid(), endpoint: endpoint.clone(),
                })
            }

            // ── CloseRoom ───────────────────────────────────────────────
            RoomActorCommand::CloseRoom { .. } => {
                let r = room.as_ref().ok_or_else(|| typed_err("no room"))?;
                let rid_s = r.id.to_string();
                r.send(Message::Chat {
                    user: 0, content: "房间已被管理员关闭".to_string(),
                }).await;
                for user in r.users().await {
                    *user.room.write().await = None;
                    user.try_send(ServerCommand::LeaveRoom(Ok(()))).await;
                    state.publish_room_event(RoomEvent::LeaveRoom {
                        room: rid_s.clone(), user: user.id,
                    }).await;
                }
                for monitor in r.monitors().await {
                    *monitor.room.write().await = None;
                    monitor.try_send(ServerCommand::LeaveRoom(Ok(()))).await;
                }
                state.rooms.write().await.remove(&rid_s);
                state.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: 0, room_id: rid_s.clone(),
                    data: json!({"action":"closed"}).to_string(),
                }).await;
                ok!(RoomCommandPayload::RoomClosed { room_id: rid_s })
            }

            // ── KickUser ────────────────────────────────────────────────
            RoomActorCommand::KickUser { target_id, .. } => {
                let r = room.as_ref().ok_or_else(|| typed_err("no room"))?;
                let users = r.users().await;
                let monitors = r.monitors().await;
                let user = users.into_iter().chain(monitors.into_iter())
                    .find(|u| u.id == *target_id)
                    .ok_or_else(|| typed_err("user not in room"))?;

                r.send(Message::Chat {
                    user: 0, content: format!("用户 {} 已被管理员踢出房间", user.name),
                }).await;
                let was_monitor = user.monitor.load(std::sync::atomic::Ordering::SeqCst);
                let should_drop = r.on_user_leave(&user).await;
                user.try_send(ServerCommand::LeaveRoom(Ok(()))).await;
                if should_drop {
                    state.rooms.write().await.remove(&r.id.to_string());
                }
                if !was_monitor {
                    state.publish_room_event(RoomEvent::LeaveRoom {
                        room: r.id.to_string(), user: *target_id,
                    }).await;
                }
                state.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: *target_id, room_id: rid(),
                    data: json!({"action":"kicked"}).to_string(),
                }).await;
                ok!(RoomCommandPayload::UserKicked {
                    room_id: rid(), user_id: *target_id,
                    user_name: user.name.clone(), room_dropped: should_drop,
                })
            }

            // ── StartRoom (admin start) ─────────────────────────────────
            RoomActorCommand::StartRoom { .. } => {
                let r = room.as_ref().ok_or_else(|| typed_err("no room"))?;
                r.begin_admin_start().await.map_err(|e| typed_err(&e.to_string()))?;
                state.dispatch_plugin_event(PluginEvent::GameStart {
                    user_id: 0, room_id: rid(),
                }).await;
                ok!(RoomCommandPayload::RoomStarted { room_id: rid() })
            }

            // ── CancelStart ─────────────────────────────────────────────
            RoomActorCommand::CancelStart { .. } => {
                let r = room.as_ref().ok_or_else(|| typed_err("no room"))?;
                let canceled = {
                    let mut room_state = r.state.write().await;
                    if matches!(&*room_state, crate::room::InternalRoomState::WaitForReady { .. }) {
                        *room_state = crate::room::InternalRoomState::SelectChart;
                        true
                    } else { false }
                };
                if canceled {
                    r.send(Message::CancelGame { user: 0 }).await;
                    r.finish_admin_start().await;
                    r.on_state_change().await;
                }
                ok!(RoomCommandPayload::CancelResult { room_id: rid(), canceled })
            }

            // ── SetChart ────────────────────────────────────────────────
            RoomActorCommand::SetChart { chart_id, chart_name, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = room.as_ref().ok_or_else(|| typed_err("no room"))?;
                {
                    let rs = r.state.read().await;
                    if !matches!(*rs, crate::room::InternalRoomState::SelectChart) {
                        return typed_err("cannot set chart outside SelectChart state");
                    }
                }
                *r.chart.write().await = Some(crate::server::Chart {
                    id: *chart_id, name: chart_name.clone(),
                });
                as_.state.chart = Some(*chart_id);
                r.send(Message::SelectChart {
                    user: 0, name: chart_name.clone(), id: *chart_id,
                }).await;
                r.on_state_change().await;
                r.publish_update(phira_mp_common::PartialRoomData {
                    chart: Some(*chart_id), ..Default::default()
                }).await;
                ok!(RoomCommandPayload::ChartSelected { room_id: rid(), chart_id: *chart_id })
            }

            // ── SetReady ────────────────────────────────────────────────
            RoomActorCommand::SetReady { user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = room.as_ref().ok_or_else(|| typed_err("no room"))?;
                {
                    let mut guard = r.state.write().await;
                    if let crate::room::InternalRoomState::WaitForReady { ref mut started, .. } = *guard {
                        if !started.insert(*user_id) {
                            return typed_err("already ready");
                        }
                        // Sync lifecycle back to actor_state
                        as_.state.lifecycle = guard.clone();
                    } else {
                        return typed_err("not in WaitForReady state");
                    }
                }
                r.send(Message::Ready { user: *user_id }).await;
                state.publish_runtime_event(crate::event_bus::MpEvent::PlayerReadyChanged {
                    room_id: rid(), user_id: *user_id, ready: true,
                });
                r.check_all_ready().await;
                ok!(RoomCommandPayload::UserReady { room_id: rid(), user_id: *user_id })
            }

            // ── CancelReady ─────────────────────────────────────────────
            RoomActorCommand::CancelReady { user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = room.as_ref().ok_or_else(|| typed_err("no room"))?;
                let was_host = r.control_snapshot().host_id == Some(*user_id);
                {
                    let mut guard = r.state.write().await;
                    if let crate::room::InternalRoomState::WaitForReady { ref mut started, .. } = *guard {
                        if !started.remove(user_id) {
                            return typed_err("not ready");
                        }
                        if was_host {
                            r.send(Message::CancelGame { user: *user_id }).await;
                            *guard = crate::room::InternalRoomState::SelectChart;
                            drop(guard);
                            r.finish_admin_start().await;
                            r.on_state_change().await;
                        } else {
                            r.send(Message::CancelReady { user: *user_id }).await;
                        }
                        // Re-read lifecycle into actor_state
                        as_.state.lifecycle = r.state.read().await.clone();
                    } else {
                        return typed_err("not in WaitForReady state");
                    }
                }
                state.publish_runtime_event(crate::event_bus::MpEvent::PlayerReadyChanged {
                    room_id: rid(), user_id: *user_id, ready: false,
                });
                ok!(RoomCommandPayload::UserNotReady { room_id: rid(), user_id: *user_id })
            }

            // ── SubmitResult ────────────────────────────────────────────
            RoomActorCommand::SubmitResult {
                user_id, score, accuracy, perfect, good, bad, miss, max_combo, full_combo, ..
            } => {
                let as_ = ctx.expect_actor_state();
                let r = room.as_ref().ok_or_else(|| typed_err("no room"))?;
                let record = crate::server::Record {
                    id: 0, player: *user_id, score: *score, perfect: *perfect,
                    good: *good, bad: *bad, miss: *miss, max_combo: *max_combo,
                    accuracy: *accuracy, full_combo: *full_combo, std: 0.0, std_score: 0.0,
                };
                {
                    let mut guard = r.state.write().await;
                    if let crate::room::InternalRoomState::Playing { results, aborted } = &mut *guard {
                        if aborted.contains(user_id) {
                            return typed_err("user aborted");
                        }
                        if results.insert(*user_id, record).is_some() {
                            return typed_err("already uploaded");
                        }
                        as_.state.lifecycle = guard.clone();
                    } else {
                        return typed_err("not in Playing state");
                    }
                }
                r.send(Message::Played {
                    user: *user_id, score: *score, accuracy: *accuracy, full_combo: *full_combo,
                }).await;
                r.check_all_ready().await;
                state.dispatch_plugin_event(PluginEvent::GameEnd {
                    user_id: *user_id, user_name: String::new(), room_id: rid(),
                    score: *score, accuracy: *accuracy, perfect: *perfect,
                    good: *good, bad: *bad, miss: *miss, max_combo: *max_combo,
                    full_combo: *full_combo,
                }).await;
                ok!(RoomCommandPayload::RoundResultSubmitted {
                    room_id: rid(), user_id: *user_id, score: *score,
                })
            }

            // ── AbortRound ──────────────────────────────────────────────
            RoomActorCommand::AbortRound { user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = room.as_ref().ok_or_else(|| typed_err("no room"))?;
                {
                    let mut guard = r.state.write().await;
                    if let crate::room::InternalRoomState::Playing { results, aborted } = &mut *guard {
                        if results.contains_key(user_id) {
                            return typed_err("already uploaded");
                        }
                        if !aborted.insert(*user_id) {
                            return typed_err("already aborted");
                        }
                        as_.state.lifecycle = guard.clone();
                    } else {
                        return typed_err("not in Playing state");
                    }
                }
                r.send(Message::Abort { user: *user_id }).await;
                r.check_all_ready().await;
                ok!(RoomCommandPayload::RoundAborted { room_id: rid(), user_id: *user_id })
            }

            // ── HostStart ───────────────────────────────────────────────
            RoomActorCommand::HostStart { user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = room.as_ref().ok_or_else(|| typed_err("no room"))?;
                if !matches!(&*r.state.read().await, crate::room::InternalRoomState::SelectChart) {
                    return typed_err("room is not selecting a chart");
                }
                if r.admin_start_pending() {
                    return typed_err("administrative start is already in progress");
                }
                if r.chart.read().await.is_none() {
                    return typed_err("no chart selected");
                }
                r.finish_admin_start().await;
                r.reset_game_time().await;
                r.send(Message::GameStart { user: *user_id }).await;
                *r.state.write().await = crate::room::InternalRoomState::WaitForReady {
                    started: std::iter::once(*user_id).collect(),
                    admin_started: false,
                };
                // Sync lifecycle into actor_state
                as_.state.lifecycle = r.state.read().await.clone();
                r.on_state_change().await;
                r.check_all_ready().await;
                state.dispatch_plugin_event(PluginEvent::GameStart {
                    user_id: *user_id, room_id: rid(),
                }).await;
                ok!(RoomCommandPayload::HostStarted { room_id: rid() })
            }

            // ── AddUser ─────────────────────────────────────────────────
            RoomActorCommand::AddUser { user_id, user_name, monitor, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = room.as_ref().ok_or_else(|| typed_err("no room"))?;
                let current_count = r.users().await.len();
                if current_count >= r.control_snapshot().max_users && !monitor {
                    return typed_err("room is full");
                }
                if !r.live.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    tracing::info!(room = %r.id, "room goes live via add_user");
                }
                // Sync live flag + members into actor_state
                as_.state.live = true;
                as_.state.members.users.push(*user_id);
                state.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: *user_id, room_id: rid(),
                    data: json!({"action": if *monitor { "monitor_join" } else { "join" }}).to_string(),
                }).await;
                ok!(RoomCommandPayload::UserAdded {
                    room_id: rid(), user_id: *user_id,
                    monitor: *monitor,
                    room_full: current_count + 1 >= r.control_snapshot().max_users,
                })
            }

            // ── RemoveUser ──────────────────────────────────────────────
            RoomActorCommand::RemoveUser { user_id, .. } => {
                let r = room.as_ref().ok_or_else(|| typed_err("no room"))?;
                let found = {
                    let users = r.users().await;
                    users.iter().find(|u| u.id == *user_id).cloned()
                        .or_else(|| {
                            let monitors = r.monitors().await;
                            monitors.iter().find(|u| u.id == *user_id).cloned()
                        })
                };
                match found {
                    Some(user) => {
                        let was_monitor = user.monitor.load(std::sync::atomic::Ordering::SeqCst);
                        let should_drop = r.on_user_leave(&user).await;
                        if should_drop {
                            state.rooms.write().await.remove(&r.id.to_string());
                        }
                        if !was_monitor {
                            state.publish_room_event(RoomEvent::LeaveRoom {
                                room: r.id.to_string(), user: *user_id,
                            }).await;
                        }
                        state.dispatch_plugin_event(PluginEvent::RoomModify {
                            user_id: *user_id, room_id: rid(),
                            data: json!({"action": "leave"}).to_string(),
                        }).await;
                        ok!(RoomCommandPayload::UserRemoved {
                            room_id: rid(), user_id: *user_id,
                            room_dropped: should_drop,
                        })
                    }
                    None => typed_err("user not found in room"),
                }
            }
        }
    }

    pub(super) fn should_stop_room_mailbox(
        command: &RoomActorCommand,
        result: &RoomCommandResult,
    ) -> bool {
        if command.kind().stops_room_mailbox_after_execution() {
            return true;
        }

        matches!(command, RoomActorCommand::KickUser { .. })
            && result
                .payload()
                .and_then(|value| value.get("room_dropped"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    fn dummy_reply() -> oneshot::Sender<RoomCommandResult> {
        let (tx, _rx) = oneshot::channel();
        tx
    }

    #[test]
    fn close_stops_room_mailbox_contract() {
        let command = RoomActorCommand::CloseRoom {
            room_id: "room-a".to_string(),
            reply: dummy_reply(),
        };
        let result = RoomCommandResult::from_untyped(
            Ok(serde_json::json!({"ok": true})),
            RoomCommandDelivery::PerRoomMailbox,
        );

        assert!(RoomCommandHandler::should_stop_room_mailbox(
            &command, &result
        ));
    }

    #[test]
    fn kick_only_stops_room_mailbox_when_room_dropped() {
        let keep_command = RoomActorCommand::KickUser {
            room_id: "room-a".to_string(),
            target_id: 42,
            reply: dummy_reply(),
        };
        let keep_result = RoomCommandResult::from_untyped(
            Ok(serde_json::json!({"ok": true, "room_dropped": false})),
            RoomCommandDelivery::PerRoomMailbox,
        );
        assert!(!RoomCommandHandler::should_stop_room_mailbox(
            &keep_command,
            &keep_result
        ));

        let drop_command = RoomActorCommand::KickUser {
            room_id: "room-a".to_string(),
            target_id: 42,
            reply: dummy_reply(),
        };
        let drop_result = RoomCommandResult::from_untyped(
            Ok(serde_json::json!({"ok": true, "room_dropped": true})),
            RoomCommandDelivery::PerRoomMailbox,
        );
        assert!(RoomCommandHandler::should_stop_room_mailbox(
            &drop_command,
            &drop_result
        ));
    }

    #[test]
    fn start_does_not_stop_room_mailbox_contract() {
        let command = RoomActorCommand::StartRoom {
            room_id: "room-a".to_string(),
            reply: dummy_reply(),
        };
        let result = RoomCommandResult::from_untyped(
            Ok(serde_json::json!({"ok": true})),
            RoomCommandDelivery::PerRoomMailbox,
        );

        assert!(!RoomCommandHandler::should_stop_room_mailbox(
            &command, &result
        ));
    }

    #[test]
    fn cancel_does_not_stop_room_mailbox_contract() {
        let command = RoomActorCommand::CancelStart {
            room_id: "room-b".to_string(),
            reply: dummy_reply(),
        };
        let result = RoomCommandResult::from_untyped(
            Ok(serde_json::json!({"canceled": true})),
            RoomCommandDelivery::PerRoomMailbox,
        );
        assert!(!RoomCommandHandler::should_stop_room_mailbox(
            &command, &result
        ));
    }

    #[test]
    fn set_lock_does_not_stop_room_mailbox() {
        let command = RoomActorCommand::SetLock {
            room_id: "room-c".to_string(),
            locked: true,
            actor_user_id: 0,
            reply: dummy_reply(),
        };
        let result = RoomCommandResult::from_untyped(
            Ok(serde_json::json!({"locked": true})),
            RoomCommandDelivery::PerRoomMailbox,
        );
        assert!(!RoomCommandHandler::should_stop_room_mailbox(
            &command, &result
        ));
    }

    #[test]
    fn set_cycle_does_not_stop_room_mailbox() {
        let command = RoomActorCommand::SetCycle {
            room_id: "room-d".to_string(),
            cycle: true,
            actor_user_id: 0,
            reply: dummy_reply(),
        };
        let result = RoomCommandResult::from_untyped(
            Ok(serde_json::json!({"cycle": true})),
            RoomCommandDelivery::PerRoomMailbox,
        );
        assert!(!RoomCommandHandler::should_stop_room_mailbox(
            &command, &result
        ));
    }

    #[test]
    fn set_host_does_not_stop_room_mailbox() {
        let command = RoomActorCommand::SetHost {
            room_id: "room-e".to_string(),
            target_id: Some(42),
            reply: dummy_reply(),
        };
        let result = RoomCommandResult::from_untyped(
            Ok(serde_json::json!({"host": 42})),
            RoomCommandDelivery::PerRoomMailbox,
        );
        assert!(!RoomCommandHandler::should_stop_room_mailbox(
            &command, &result
        ));
    }
}

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
use phira_mp_common::{Message, PartialRoomData, RoomEvent, RoomId, ServerCommand};
use serde_json::{json, Value};

/// Helper: build an error result.
fn err(msg: &str) -> RoomCommandResult {
    RoomCommandResult::Err {
        delivery: RoomCommandDelivery::PerRoomMailbox,
        error: msg.to_string(),
    }
}

/// Helper: build an ok result.
fn ok(payload: RoomCommandPayload) -> RoomCommandResult {
    RoomCommandResult::ok(payload, RoomCommandDelivery::PerRoomMailbox)
}

pub(super) struct RoomCommandHandler;

impl RoomCommandHandler {
    /// Execute a command against actor-owned state.
    pub(super) async fn execute_with_actor(
        mut ctx: RoomCommandContext<'_>,
        command: &RoomActorCommand,
    ) -> RoomCommandResult {
        let state = ctx.state;
        let room = ctx.room.clone();


        match command {
            RoomActorCommand::SetLock { room_id, locked, actor_user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                as_.state.set_locked(*locked);
                if let Some(ref r) = room {
                    r.set_locked(*locked);
                    r.send(Message::LockRoom { lock: *locked }).await;
                    r.publish_update(PartialRoomData { lock: Some(*locked), ..Default::default() }).await;
                }
                state.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: *actor_user_id, room_id: room_id.clone().to_string(),
                    data: json!({"action":"lock","value":locked}).to_string(),
                }).await;
                ok(RoomCommandPayload::LockChanged { room_id: room_id.clone().to_string(), locked: *locked })
            }

            RoomActorCommand::SetCycle { room_id, cycle, actor_user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                as_.state.set_cycle(*cycle);
                if let Some(ref r) = room {
                    r.set_cycle(*cycle);
                    r.send(Message::CycleRoom { cycle: *cycle }).await;
                    r.publish_update(PartialRoomData { cycle: Some(*cycle), ..Default::default() }).await;
                }
                state.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: *actor_user_id, room_id: room_id.clone().to_string(),
                    data: json!({"action":"cycle","value":cycle}).to_string(),
                }).await;
                ok(RoomCommandPayload::CycleChanged { room_id: room_id.clone().to_string(), cycle: *cycle })
            }

            RoomActorCommand::SetHidden { room_id, hidden, .. } => {
                let as_ = ctx.expect_actor_state();
                as_.state.set_hidden(*hidden);
                if let Some(ref r) = room {
                    r.set_hidden(*hidden);
                }
                state.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: 0, room_id: room_id.clone().to_string(),
                    data: json!({"action":"hidden","value":hidden}).to_string(),
                }).await;
                ok(RoomCommandPayload::HiddenChanged { room_id: room_id.clone().to_string(), hidden: *hidden })
            }

            RoomActorCommand::SetHost { room_id, target_id, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = match room { Some(ref r) => r, None => return err("no room") };
                let (host_id, host_name, system_host) = match target_id {
                    Some(uid) => {
                        let name = {
                            let users = r.users().await;
                            let mut name = uid.to_string();
                            for u in &users {
                                if u.id == *uid {
                                    name = r.display_name(u).await;
                                    break;
                                }
                            }
                            name
                        };
                        r.send(Message::Chat { user: 0, content: format!("房主已转移给 {name}") }).await;
                        r.set_host(Some(*uid), true).await.map_err(|e| e.to_string()).unwrap_or(());
                        (Some(*uid), name, false)
                    }
                    None => {
                        r.send(Message::Chat { user: 0, content: "房主已设为系统 ?".to_string() }).await;
                        r.set_host(None, true).await.map_err(|e| e.to_string()).unwrap_or(());
                        (None, "?".to_string(), true)
                    }
                };
                as_.state.control.host_id = host_id;
                as_.state.control.system_host = system_host;
                ok(RoomCommandPayload::HostChanged {
                    room_id: room_id.clone().to_string(), host: *target_id, host_name, host_is_system: system_host,
                })
            }

            RoomActorCommand::SetEndpoint { room_id, endpoint, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = match room { Some(ref r) => r, None => return err("no room") };
                let endpoint = endpoint.clone();
                r.set_phira_api_endpoint_override(endpoint.clone()).await;
                as_.state.control.phira_api_endpoint = endpoint.clone();
                ok(RoomCommandPayload::EndpointChanged {
                    room_id: room_id.clone().to_string(), endpoint: endpoint.clone().unwrap_or_default(), endpoint_override: endpoint.clone(), using_room_override: false,
                })
            }

            RoomActorCommand::CloseRoom { room_id, .. } => {
                let r = match room { Some(ref r) => r, None => return err("no room") };
                r.send(Message::Chat { user: 0, content: "房间已被管理员关闭".to_string() }).await;
                for user in r.users().await {
                    *user.room.write().await = None;
                    user.try_send(ServerCommand::LeaveRoom(Ok(()))).await;
                    state.publish_room_event(RoomEvent::LeaveRoom { room: r.id.clone(), user: user.id }).await;
                }
                for monitor in r.monitors().await {
                    *monitor.room.write().await = None;
                    monitor.try_send(ServerCommand::LeaveRoom(Ok(()))).await;
                }
                state.rooms.write().await.remove(&r.id);
                state.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: 0, room_id: r.id.to_string(),
                    data: json!({"action":"closed"}).to_string(),
                }).await;
                ok(RoomCommandPayload::RoomClosed { room_id: r.id.to_string() })
            }

            RoomActorCommand::KickUser { room_id, target_id, .. } => {
                let r = match room { Some(ref r) => r, None => return err("no room") };
                let users = r.users().await;
                let monitors = r.monitors().await;
                let user = match users.into_iter().chain(monitors.into_iter()).find(|u| u.id == *target_id) {
                    Some(u) => u, None => return err("user not in room"),
                };
                r.send(Message::Chat { user: 0, content: format!("用户 {} 已被管理员踢出房间", user.name) }).await;
                let was_monitor = user.monitor.load(std::sync::atomic::Ordering::SeqCst);
                let should_drop = r.on_user_leave(&user).await;
                user.try_send(ServerCommand::LeaveRoom(Ok(()))).await;
                if should_drop { state.rooms.write().await.remove(&r.id); }
                if !was_monitor {
                    state.publish_room_event(RoomEvent::LeaveRoom { room: r.id.clone(), user: *target_id }).await;
                }
                state.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: *target_id, room_id: room_id.clone().to_string(),
                    data: json!({"action":"kicked"}).to_string(),
                }).await;
                ok(RoomCommandPayload::UserKicked {
                    room_id: room_id.clone().to_string(), user_id: *target_id,
                    user_name: user.name.clone(), room_dropped: should_drop,
                })
            }

            RoomActorCommand::StartRoom { room_id, .. } => {
                let r = match room { Some(ref r) => r, None => return err("no room") };
                if let Err(e) = r.begin_admin_start().await { return err(&e.to_string()); }
                state.dispatch_plugin_event(PluginEvent::GameStart { user_id: 0, room_id: room_id.clone().to_string() }).await;
                ok(RoomCommandPayload::RoomStarted { room_id: room_id.clone().to_string() })
            }

            RoomActorCommand::CancelStart { room_id, .. } => {
                let r = match room { Some(ref r) => r, None => return err("no room") };
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
                ok(RoomCommandPayload::CancelResult { room_id: room_id.clone().to_string(), canceled })
            }

            RoomActorCommand::SetChart { room_id, chart_id, chart_name, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = match room { Some(ref r) => r, None => return err("no room") };
                if !matches!(&*r.state.read().await, crate::room::InternalRoomState::SelectChart) {
                    return err("cannot set chart outside SelectChart state");
                }
                *r.chart.write().await = Some(crate::server::Chart { id: *chart_id, name: chart_name.clone() });
                as_.state.chart = Some(*chart_id);
                r.send(Message::SelectChart { user: 0, name: chart_name.clone(), id: *chart_id }).await;
                r.on_state_change().await;
                r.publish_update(phira_mp_common::PartialRoomData { chart: Some(*chart_id), ..Default::default() }).await;
                ok(RoomCommandPayload::ChartSelected { room_id: room_id.clone().to_string(), chart_id: *chart_id })
            }

            RoomActorCommand::SetReady { room_id, user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = match room { Some(ref r) => r, None => return err("no room") };
                {
                    let mut guard = r.state.write().await;
                    match &mut *guard {
                        crate::room::InternalRoomState::WaitForReady { ref mut started, .. } => {
                            if !started.insert(*user_id) { return err("already ready"); }
                            as_.state.lifecycle = guard.clone();
                        }
                        _ => return err("not in WaitForReady state"),
                    }
                }
                r.send(Message::Ready { user: *user_id }).await;
                state.publish_runtime_event(crate::event_bus::MpEvent::PlayerReadyChanged {
                    room_id: room_id.clone().try_into().unwrap(), user_id: *user_id, ready: true,
                });
                r.check_all_ready().await;
                ok(RoomCommandPayload::UserReady { room_id: room_id.clone().to_string(), user_id: *user_id })
            }

            RoomActorCommand::CancelReady { room_id, user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = match room { Some(ref r) => r, None => return err("no room") };
                let was_host = r.control_snapshot().host_id == Some(*user_id);
                {
                    let mut guard = r.state.write().await;
                    match &mut *guard {
                        crate::room::InternalRoomState::WaitForReady { ref mut started, .. } => {
                            if !started.remove(user_id) { return err("not ready"); }
                            if was_host {
                                r.send(Message::CancelGame { user: *user_id }).await;
                                *guard = crate::room::InternalRoomState::SelectChart;
                                drop(guard);
                                r.finish_admin_start().await;
                                r.on_state_change().await;
                            } else {
                                r.send(Message::CancelReady { user: *user_id }).await;
                            }
                            as_.state.lifecycle = r.state.read().await.clone();
                        }
                        _ => return err("not in WaitForReady state"),
                    }
                }
                state.publish_runtime_event(crate::event_bus::MpEvent::PlayerReadyChanged {
                    room_id: room_id.clone().try_into().unwrap(), user_id: *user_id, ready: false,
                });
                ok(RoomCommandPayload::UserNotReady { room_id: room_id.clone().to_string(), user_id: *user_id })
            }

            RoomActorCommand::SubmitResult { room_id, user_id, score, accuracy, perfect, good, bad, miss, max_combo, full_combo, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = match room { Some(ref r) => r, None => return err("no room") };
                let record = crate::server::Record {
                    id: 0, player: *user_id, score: *score, perfect: *perfect,
                    good: *good, bad: *bad, miss: *miss, max_combo: *max_combo,
                    accuracy: *accuracy, full_combo: *full_combo, std: 0.0, std_score: 0.0,
                };
                {
                    let mut guard = r.state.write().await;
                    match &mut *guard {
                        crate::room::InternalRoomState::Playing { results, aborted } => {
                            if aborted.contains(user_id) { return err("user aborted"); }
                            if results.insert(*user_id, record).is_some() { return err("already uploaded"); }
                            as_.state.lifecycle = guard.clone();
                        }
                        _ => return err("not in Playing state"),
                    }
                }
                r.send(Message::Played { user: *user_id, score: *score, accuracy: *accuracy, full_combo: *full_combo }).await;
                r.check_all_ready().await;
                state.dispatch_plugin_event(PluginEvent::GameEnd {
                    user_id: *user_id, user_name: String::new(), room_id: room_id.clone().to_string(),
                    score: *score, accuracy: *accuracy, perfect: *perfect,
                    good: *good, bad: *bad, miss: *miss, max_combo: *max_combo, full_combo: *full_combo,
                }).await;
                ok(RoomCommandPayload::RoundResultSubmitted { room_id: room_id.clone().to_string(), user_id: *user_id, score: *score })
            }

            RoomActorCommand::AbortRound { room_id, user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = match room { Some(ref r) => r, None => return err("no room") };
                {
                    let mut guard = r.state.write().await;
                    match &mut *guard {
                        crate::room::InternalRoomState::Playing { results, aborted } => {
                            if results.contains_key(user_id) { return err("already uploaded"); }
                            if !aborted.insert(*user_id) { return err("already aborted"); }
                            as_.state.lifecycle = guard.clone();
                        }
                        _ => return err("not in Playing state"),
                    }
                }
                r.send(Message::Abort { user: *user_id }).await;
                r.check_all_ready().await;
                ok(RoomCommandPayload::RoundAborted { room_id: room_id.clone().to_string(), user_id: *user_id })
            }

            RoomActorCommand::HostStart { room_id, user_id, .. } => {
                let r = match room { Some(ref r) => r, None => return err("no room") };
                if !matches!(&*r.state.read().await, crate::room::InternalRoomState::SelectChart) {
                    return err("room is not selecting a chart");
                }
                if r.admin_start_pending() { return err("administrative start is already in progress"); }
                if r.chart.read().await.is_none() { return err("no chart selected"); }
                r.finish_admin_start().await;
                r.reset_game_time().await;
                r.send(Message::GameStart { user: *user_id }).await;
                *r.state.write().await = crate::room::InternalRoomState::WaitForReady {
                    started: std::iter::once(*user_id).collect(), admin_started: false,
                };
                if let Some(as_) = ctx.actor_state() {
                    as_.state.lifecycle = r.state.read().await.clone();
                }
                r.on_state_change().await;
                r.check_all_ready().await;
                state.dispatch_plugin_event(PluginEvent::GameStart { user_id: *user_id, room_id: room_id.clone().to_string() }).await;
                ok(RoomCommandPayload::HostStarted { room_id: room_id.clone().to_string() })
            }

            RoomActorCommand::AddUser { room_id, user_id, user_name, monitor, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = match room { Some(ref r) => r, None => return err("no room") };
                let current_count = r.users().await.len();
                if current_count >= r.control_snapshot().max_users && !monitor {
                    return err("room is full");
                }
                if !r.live.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    tracing::info!(room = %r.id, "room goes live via add_user");
                }
                as_.state.live = true;
                as_.state.members.users.push(*user_id);
                state.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: *user_id, room_id: room_id.clone().to_string(),
                    data: json!({"action": if *monitor { "monitor_join" } else { "join" }}).to_string(),
                }).await;
                ok(RoomCommandPayload::UserAdded {
                    room_id: room_id.clone().to_string(), user_id: *user_id,
                    monitor: *monitor,
                    room_full: current_count + 1 >= r.control_snapshot().max_users,
                })
            }

            RoomActorCommand::RemoveUser { room_id, user_id, .. } => {
                let r = match room { Some(ref r) => r, None => return err("no room") };
                let user = {
                    let users = r.users().await;
                    let monitors = r.monitors().await;
                    users.iter().find(|u| u.id == *user_id).cloned()
                        .or_else(|| monitors.iter().find(|u| u.id == *user_id).cloned())
                };
                match user {
                    Some(user) => {
                        let was_monitor = user.monitor.load(std::sync::atomic::Ordering::SeqCst);
                        let should_drop = r.on_user_leave(&user).await;
                        if should_drop { state.rooms.write().await.remove(&r.id); }
                        if !was_monitor {
                            state.publish_room_event(RoomEvent::LeaveRoom { room: r.id.clone(), user: *user_id }).await;
                        }
                        state.dispatch_plugin_event(PluginEvent::RoomModify {
                            user_id: *user_id, room_id: room_id.clone().to_string(),
                            data: json!({"action": "leave"}).to_string(),
                        }).await;
                        ok(RoomCommandPayload::UserRemoved {
                            room_id: room_id.clone().to_string(), user_id: *user_id, room_dropped: should_drop,
                        })
                    }
                    None => err("user not found in room"),
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

//! Runtime v2 room command handler — all commands write actor_state.
//!
//! execute_with_actor() handles all commands by writing actor_state
//! first, then broadcasting via Room (pure broadcast bus), then
//! returning a typed payload. The caller (execute_command in actor.rs)
//! updates the snapshot cache after execution.
//!
//! After Phase 2 Work C, Room no longer holds mutable state. All state
//! is actor-owned via `RoomActorState`. Room is used only for:
//! - `send()` / `broadcast*()` — message dispatch
//! - `publish_update()` — infrastructure notification
//! - `users()` / `monitors()` / `on_user_leave()` — user management

use super::{
    command::RoomActorCommand, context::RoomCommandContext, RoomCommandDelivery,
    RoomCommandPayload, RoomCommandResult,
};
use crate::plugin::PluginEvent;
use crate::room::InternalRoomState;
use phira_mp_common::{Message, PartialRoomData, RoomEvent, ServerCommand};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use tracing::{debug, info, warn};

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

/// Helper: broadcast a state change via `on_state_change`.
async fn broadcast_state_change(r: &crate::room::Room, state: &InternalRoomState, chart: Option<i32>) {
    let room_state = state.to_client(chart);
    r.broadcast(ServerCommand::ChangeState(room_state)).await;
    let stripped = state.stripped();
    let state_desc = match stripped {
        phira_mp_common::StrippedRoomState::SelectingChart => "selecting_chart",
        phira_mp_common::StrippedRoomState::WaitingForReady => "waiting_for_ready",
        phira_mp_common::StrippedRoomState::Playing => "playing",
    };
    r.publish_update(PartialRoomData {
        state: Some(stripped),
        ..Default::default()
    })
    .await;
    if let Some(server) = r.server.upgrade() {
        server.publish_runtime_event(crate::event_bus::MpEvent::RoomStateChanged {
            room_id: r.id.clone(),
            state: state_desc.to_string(),
        });
    }
}

/// Reset game time for all users in the room.
async fn reset_game_time(r: &crate::room::Room) {
    for user in r.users().await {
        user.game_time
            .store(f32::NEG_INFINITY.to_bits(), Ordering::Relaxed);
    }
}

/// Save round history and produce a RoundData event.
/// Returns Some(RoundData) if there was a Playing round to save.
async fn save_round_history(
    r: &crate::room::Room,
    lifecycle: &mut InternalRoomState,
    current_round_id: &mut Option<uuid::Uuid>,
    chart: Option<i32>,
    chart_name: Option<&str>,
    display_names: &HashMap<i32, String>,
) -> Option<phira_mp_common::RoundData> {
    let round_id = current_round_id.unwrap_or(uuid::Uuid::nil());
    let (chart_id, chart_name_str, results, aborted) = {
        match &*lifecycle {
            InternalRoomState::Playing { results, aborted } => {
                let (cid, cn) = match chart {
                    Some(cid) => (cid, chart_name.unwrap_or("?").to_string()),
                    None => return None,
                };
                let results = results.clone();
                let aborted = aborted.clone();
                (cid, cn, results, aborted)
            }
            _ => return None,
        }
    };

    // 收集用户名
    let mut users_map: HashMap<i32, String> = HashMap::new();
    for u in r.users().await {
        let name = display_names.get(&u.id).cloned().unwrap_or_else(|| u.name.clone());
        users_map.insert(u.id, name);
    }

    let mut play_results = Vec::new();
    for (uid, rec) in &results {
        play_results.push(crate::room::PlayResult {
            user_id: *uid,
            user_name: users_map
                .get(uid)
                .cloned()
                .unwrap_or_else(|| format!("{}", uid)),
            score: rec.score,
            accuracy: rec.accuracy,
            perfect: rec.perfect,
            good: rec.good,
            bad: rec.bad,
            miss: rec.miss,
            max_combo: rec.max_combo,
            full_combo: rec.full_combo,
            aborted: false,
            std_score: rec.std_score,
        });
    }
    for uid in &aborted {
        if !results.contains_key(uid) {
            play_results.push(crate::room::PlayResult {
                user_id: *uid,
                user_name: users_map
                    .get(uid)
                    .cloned()
                    .unwrap_or_else(|| format!("{}", uid)),
                score: 0,
                accuracy: 0.0,
                perfect: 0,
                good: 0,
                bad: 0,
                miss: 0,
                max_combo: 0,
                full_combo: false,
                aborted: true,
                std_score: 0.0,
            });
        }
    }

    let round = crate::room::PlayRound {
        round_id,
        chart_id,
        chart_name: chart_name_str,
        results: play_results,
    };

    if let Some(db) = crate::internal_hooks::DB.get() {
        for result in &round.results {
            if !db
                .record_round_result(&round.round_id.to_string(), &r.id.to_string(), result)
                .await
            {
                warn!(
                    room = %r.id,
                    round_id = %round.round_id,
                    user_id = result.user_id,
                    "failed to persist round result"
                );
            }
        }
    }
    let event = crate::room::protocol_round(&round);
    r.play_history.push(round, &r.uuid).await;
    let total = r.play_history.len().await;
    info!(
        room = r.id.to_string(),
        "saved play round history (total {})", total
    );
    Some(event)
}

/// Check if all users are ready (transition to Playing) or all have finished (transition to SelectChart).
async fn check_all_ready(
    r: &crate::room::Room,
    as_: &mut crate::room_actor::actor::RoomActorState,
    _state:     state: &crate::server::PlusServerState,crate::server::PlusServerState,
) {
    // Clone the lifecycle to check state
    let lifecycle = as_.state.lifecycle.clone();
    match &lifecycle {
        InternalRoomState::WaitForReady { started, admin_started } => {
            if r.users().await.into_iter().chain(r.monitors().await.into_iter())
                .all(|it| started.contains(&it.id))
            {
                // All ready — transition to Playing
                if *admin_started {
                    // Finish admin start
                    as_.state.control.admin_start_pending = false;
                    if let Some(host) = r.users().await.iter().find(|u| as_.state.control.host_id == Some(u.id)) {
                        host.try_send(ServerCommand::ChangeHost(true)).await;
                    }
                }
                let round_id = uuid::Uuid::new_v4();
                as_.state.round.round_id = Some(round_id);
                info!(room = r.id.to_string(), round = %round_id, "game start");
                if let Some(server) = r.server.upgrade() {
                    server.publish_runtime_event(crate::event_bus::MpEvent::GameStarted {
                        room_id: r.id.clone(),
                        round_id: round_id.to_string(),
                    });
                }
                r.send(Message::StartPlaying).await;
                reset_game_time(r).await;
                as_.state.lifecycle = InternalRoomState::Playing {
                    results: HashMap::new(),
                    aborted: HashSet::new(),
                };
                broadcast_state_change(r, &as_.state.lifecycle, as_.state.chart).await;

                // 打开轮次数据存储
                let rid = round_id.to_string();
                let cid = as_.state.chart.unwrap_or(0);
                let players: Vec<i32> = r.users().await.into_iter().map(|u| u.id).collect();
                if let Some(rs) = &r.round_store {
                    let meta = crate::round_store::RoundMeta {
                        round_uuid: rid,
                        chart_id: cid,
                        chart_name: as_.state.control.phira_api_endpoint.clone().unwrap_or_default(),
                        room_id: r.id.to_string(),
                        players: players.clone(),
                        started_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as i64)
                            .unwrap_or(0),
                        finished_at: None,
                    };
                    if let Err(e) = rs.open_round(&meta).await {
                        warn!("round store: failed to open round: {e}");
                    }
                }
            }
        }
        InternalRoomState::Playing { results, aborted } => {
            if r.users().await.into_iter()
                .all(|it| results.contains_key(&it.id) || aborted.contains(&it.id))
            {
                let rid = as_.state.round.round_id;
                let completed_round = save_round_history(
                    r,
                    &mut as_.state.lifecycle,
                    &mut as_.state.round.round_id,
                    as_.state.chart,
                    None,
                    &as_.display_names,
                ).await;
                if let (Some(server), Some(round)) = (r.server.upgrade(), completed_round) {
                    server
                        .publish_room_event(RoomEvent::NewRound {
                            room: r.id.clone(),
                            round,
                        })
                        .await;
                }

                // 关闭轮次数据存储
                if let Some(rid) = rid {
                    info!("round complete: {}", rid);
                    if let Some(rs) = &r.round_store {
                        rs.close_round(&rid.to_string()).await;
                    }
                }

                // 触发 RoundComplete 事件
                if let Some(pm) = &r.plugin_manager {
                    pm.dispatch_event(PluginEvent::RoundComplete {
                        room_id: r.id.to_string(),
                        chart_id: as_.state.chart.unwrap_or(0),
                        chart_name: as_.state.control.phira_api_endpoint.clone().unwrap_or_default(),
                    })
                    .await;
                }

                // Domain event for round completion
                if let Some(server) = r.server.upgrade() {
                    if let Some(round_uuid) = rid {
                        server.publish_runtime_event(crate::event_bus::MpEvent::RoundCompleted {
                            room_id: r.id.clone(),
                            round_id: round_uuid.to_string(),
                        });
                    }
                }

                // 发送结算排行
                {
                    if let Some(last) = r.play_history.last().await {
                        let mut sorted = last.results.clone();
                        sorted.sort_by(|a, b| b.score.cmp(&a.score));
                        let mut lines: Vec<String> =
                            vec![format!("▸ {} 排行", last.chart_name)];
                        for (i, rr) in sorted.iter().enumerate() {
                            let status = if rr.aborted { " 放弃" } else { "" };
                            let fc = if rr.full_combo { " FC" } else { "" };
                            lines.push(format!(
                                "#{}. {:<12} {:>8}分  准确率 {:.2}%  误差 ±{:.2}{}{}",
                                i + 1,
                                rr.user_name,
                                rr.score,
                                rr.accuracy * 100.0,
                                rr.std_score,
                                fc,
                                status
                            ));
                            lines.push(format!(
                                "    Perfect:{}  Good:{}  Bad:{}  Miss:{}  MaxCombo:{}",
                                rr.perfect, rr.good, rr.bad, rr.miss, rr.max_combo
                            ));
                        }
                        for line in &lines {
                            r.send(Message::Chat {
                                user: 0,
                                content: line.clone(),
                            })
                            .await;
                        }
                    }
                }
                r.send(Message::GameEnd).await;
                as_.state.round.round_id = None;
                as_.state.lifecycle = InternalRoomState::SelectChart;
                if as_.state.control.cycle && !as_.state.control.system_host {
                    debug!(room = r.id.to_string(), "cycling");
                    let users = r.users().await;
                    let host_id = as_.state.control.host_id;
                    let new_host = {
                        if users.is_empty() {
                            None
                        } else {
                            let index = users
                                .iter()
                                .position(|it| Some(it.id) == host_id)
                                .map(|it| (it + 1) % users.len())
                                .unwrap_or_default();
                            users.into_iter().nth(index)
                        }
                    };
                    if let Some(new_host) = new_host {
                        let old_id = as_.state.control.host_id;
                        as_.state.control.host_id = Some(new_host.id);
                        as_.state.control.system_host = false;
                        r.send(Message::NewHost { user: new_host.id }).await;
                        if let Some(old_uid) = old_id {
                            if let Some(old) = r.users().await.iter().find(|u| u.id == old_uid) {
                                old.try_send(ServerCommand::ChangeHost(false)).await;
                            }
                        }
                        new_host.try_send(ServerCommand::ChangeHost(true)).await;
                        r.publish_update(PartialRoomData {
                            host: Some(new_host.id),
                            ..Default::default()
                        })
                        .await;
                    }
                }
                broadcast_state_change(r, &as_.state.lifecycle, as_.state.chart).await;
            }
        }
        _ => {}
    }
}

pub(super) struct RoomCommandHandler;

impl RoomCommandHandler {
    /// Execute a command against actor-owned state.
    /// Room is used only for broadcast/send and user management.
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
                    r.send(Message::LockRoom { lock: *locked }).await;
                    r.publish_update(PartialRoomData { lock: Some(*locked), ..Default::default() }).await;
                }
                state.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: *actor_user_id, room_id: room_id.clone().to_string(),
                    data: json!({"action":"lock","value":locked}).to_string(),
                }).await;
                state.publish_runtime_event(crate::event_bus::MpEvent::RoomLocked {
                    room_id: room_id.clone().try_into().unwrap(),
                    locked: *locked,
                });
                ok(RoomCommandPayload::LockChanged { room_id: room_id.clone().to_string(), locked: *locked })
            }

            RoomActorCommand::SetCycle { room_id, cycle, actor_user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                as_.state.set_cycle(*cycle);
                if let Some(ref r) = room {
                    r.send(Message::CycleRoom { cycle: *cycle }).await;
                    r.publish_update(PartialRoomData { cycle: Some(*cycle), ..Default::default() }).await;
                }
                state.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: *actor_user_id, room_id: room_id.clone().to_string(),
                    data: json!({"action":"cycle","value":cycle}).to_string(),
                }).await;
                state.publish_runtime_event(crate::event_bus::MpEvent::RoomCycled {
                    room_id: room_id.clone().try_into().unwrap(),
                    cycle: *cycle,
                });
                ok(RoomCommandPayload::CycleChanged { room_id: room_id.clone().to_string(), cycle: *cycle })
            }

            RoomActorCommand::SetHidden { room_id, hidden, .. } => {
                let as_ = ctx.expect_actor_state();
                as_.state.set_hidden(*hidden);
                state.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: 0, room_id: room_id.clone().to_string(),
                    data: json!({"action":"hidden","value":hidden}).to_string(),
                }).await;
                ok(RoomCommandPayload::HiddenChanged { room_id: room_id.clone().to_string(), hidden: *hidden })
            }

            RoomActorCommand::SetHost { room_id, target_id, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = match room { Some(ref r) => r, None => return err("no room") };
                // Find the target user (if any) and get display name from actor_state
                let (_host_id, host_name, system_host) = match target_id {
                    Some(uid) => {
                        let fallback_name = {
                            let users = r.users().await;
                            users.iter().find(|u| u.id == *uid).map(|u| u.name.clone())
                        };
                        let name = as_.display_names.get(uid)
                            .cloned()
                            .or_else(|| fallback_name)
                            .unwrap_or_else(|| uid.to_string());
                        // Send messages directly via Room broadcast
                        r.send(Message::Chat { user: 0, content: format!("房主已转移给 {name}") }).await;
                        // Notify old host
                        if let Some(old_uid) = as_.state.control.host_id {
                            if old_uid != *uid {
                                if let Some(old) = r.users().await.iter().find(|u| u.id == old_uid) {
                                    old.try_send(ServerCommand::ChangeHost(false)).await;
                                }
                            }
                        }
                        // Set new host in actor state
                        as_.state.control.host_id = Some(*uid);
                        as_.state.control.system_host = false;
                        // Announce
                        r.send(Message::NewHost { user: *uid }).await;
                        if let Some(u) = r.users().await.iter().find(|u| u.id == *uid) {
                            u.try_send(ServerCommand::ChangeHost(true)).await;
                        }
                        r.publish_update(PartialRoomData {
                            host: Some(*uid),
                            ..Default::default()
                        }).await;
                        (Some(*uid), name, false)
                    }
                    None => {
                        r.send(Message::Chat { user: 0, content: "房主已设为系统 ?".to_string() }).await;
                        // Notify old host
                        if let Some(old_uid) = as_.state.control.host_id {
                            if let Some(old) = r.users().await.iter().find(|u| u.id == old_uid) {
                                old.try_send(ServerCommand::ChangeHost(false)).await;
                            }
                        }
                        as_.state.control.host_id = None;
                        as_.state.control.system_host = true;
                        r.send(Message::NewHost { user: -1 }).await;
                        r.publish_update(PartialRoomData {
                            host: Some(-1),
                            ..Default::default()
                        }).await;
                        (None, "?".to_string(), true)
                    }
                };
                state.publish_runtime_event(crate::event_bus::MpEvent::HostChanged {
                    room_id: room_id.clone().try_into().unwrap(),
                    host: *target_id,
                });
                ok(RoomCommandPayload::HostChanged {
                    room_id: room_id.clone().to_string(), host: *target_id, host_name, host_is_system: system_host,
                })
            }

            RoomActorCommand::SetEndpoint { room_id, endpoint, .. } => {
                let as_ = ctx.expect_actor_state();
                let endpoint = endpoint.clone();
                as_.state.control.phira_api_endpoint = endpoint.clone();
                ok(RoomCommandPayload::EndpointChanged {
                    room_id: room_id.clone().to_string(), endpoint: endpoint.clone().unwrap_or_default(), endpoint_override: endpoint.clone(), using_room_override: false,
                })
            }

            RoomActorCommand::CloseRoom { room_id: _, .. } => {
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
                let as_ = ctx.expect_actor_state();
                let r = match room { Some(ref r) => r, None => return err("no room") };
                // Inline begin_admin_start using actor_state
                if as_.state.control.admin_start_pending {
                    return err("administrative start is already in progress");
                }
                if !matches!(&as_.state.lifecycle, InternalRoomState::SelectChart) {
                    return err("room is not selecting a chart");
                }
                if as_.state.chart.is_none() {
                    return err("no chart selected");
                }

                as_.state.control.admin_start_pending = true;
                broadcast_state_change(r, &as_.state.lifecycle, as_.state.chart).await;

                // Temporarily remove host privileges
                if let Some(host) = r.users().await.iter().find(|u| as_.state.control.host_id == Some(u.id)) {
                    host.try_send(ServerCommand::ChangeHost(false)).await;
                }

                reset_game_time(r).await;
                r.send(Message::GameStart { user: 0 }).await;
                r.send(Message::Chat {
                    user: 0,
                    content: "服务器已发起游戏，请加载谱面并点击准备".to_string(),
                }).await;
                as_.state.lifecycle = InternalRoomState::WaitForReady {
                    started: HashSet::new(),
                    admin_started: true,
                };
                broadcast_state_change(r, &as_.state.lifecycle, as_.state.chart).await;
                check_all_ready(r, as_, state).await;
                state.dispatch_plugin_event(PluginEvent::GameStart { user_id: 0, room_id: room_id.clone().to_string() }).await;
                ok(RoomCommandPayload::RoomStarted { room_id: room_id.clone().to_string() })
            }

            RoomActorCommand::CancelStart { room_id, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = match room { Some(ref r) => r, None => return err("no room") };
                let canceled = matches!(&as_.state.lifecycle, InternalRoomState::WaitForReady { .. });
                if canceled {
                    // Restore host privileges if admin_started
                    if let InternalRoomState::WaitForReady { admin_started, .. } = &as_.state.lifecycle {
                        if *admin_started {
                            if let Some(host) = r.users().await.iter().find(|u| as_.state.control.host_id == Some(u.id)) {
                                host.try_send(ServerCommand::ChangeHost(true)).await;
                            }
                        }
                    }
                    as_.state.control.admin_start_pending = false;
                    as_.state.lifecycle = InternalRoomState::SelectChart;
                    r.send(Message::CancelGame { user: 0 }).await;
                    broadcast_state_change(r, &as_.state.lifecycle, as_.state.chart).await;
                }
                ok(RoomCommandPayload::CancelResult { room_id: room_id.clone().to_string(), canceled })
            }

            RoomActorCommand::SetChart { room_id, chart_id, chart_name, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = match room { Some(ref r) => r, None => return err("no room") };
                if !matches!(&as_.state.lifecycle, InternalRoomState::SelectChart) {
                    return err("cannot set chart outside SelectChart state");
                }
                as_.state.chart = Some(*chart_id);
                r.send(Message::SelectChart { user: 0, name: chart_name.clone(), id: *chart_id }).await;
                broadcast_state_change(r, &as_.state.lifecycle, as_.state.chart).await;
                r.publish_update(phira_mp_common::PartialRoomData { chart: Some(*chart_id), ..Default::default() }).await;
                state.publish_runtime_event(crate::event_bus::MpEvent::ChartSelected {
                    room_id: room_id.clone().try_into().unwrap(),
                    chart_id: *chart_id,
                });
                ok(RoomCommandPayload::ChartSelected { room_id: room_id.clone().to_string(), chart_id: *chart_id })
            }

            RoomActorCommand::SetReady { room_id, user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = match room { Some(ref r) => r, None => return err("no room") };
                match &mut as_.state.lifecycle {
                    InternalRoomState::WaitForReady { ref mut started, .. } => {
                        if !started.insert(*user_id) { return err("already ready"); }
                    }
                    _ => return err("not in WaitForReady state"),
                }
                r.send(Message::Ready { user: *user_id }).await;
                state.publish_runtime_event(crate::event_bus::MpEvent::PlayerReadyChanged {
                    room_id: room_id.clone().try_into().unwrap(), user_id: *user_id, ready: true,
                });
                check_all_ready(r, as_, state).await;
                ok(RoomCommandPayload::UserReady { room_id: room_id.clone().to_string(), user_id: *user_id })
            }

            RoomActorCommand::CancelReady { room_id, user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = match room { Some(ref r) => r, None => return err("no room") };
                let was_host = as_.state.control.host_id == Some(*user_id);
                match &mut as_.state.lifecycle {
                    InternalRoomState::WaitForReady { ref mut started, .. } => {
                        if !started.remove(user_id) { return err("not ready"); }
                        if was_host {
                            // All users' host cancels the game
                            if let InternalRoomState::WaitForReady { admin_started, .. } = &as_.state.lifecycle {
                                if *admin_started {
                                    if let Some(host) = r.users().await.iter().find(|u| as_.state.control.host_id == Some(u.id)) {
                                        host.try_send(ServerCommand::ChangeHost(true)).await;
                                    }
                                }
                            }
                            as_.state.control.admin_start_pending = false;
                            r.send(Message::CancelGame { user: *user_id }).await;
                            as_.state.lifecycle = InternalRoomState::SelectChart;
                            broadcast_state_change(r, &as_.state.lifecycle, as_.state.chart).await;
                        } else {
                            r.send(Message::CancelReady { user: *user_id }).await;
                        }
                    }
                    _ => return err("not in WaitForReady state"),
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
                match &mut as_.state.lifecycle {
                    InternalRoomState::Playing { results, aborted } => {
                        if aborted.contains(user_id) { return err("user aborted"); }
                        if results.insert(*user_id, record).is_some() { return err("already uploaded"); }
                    }
                    _ => return err("not in Playing state"),
                }
                r.send(Message::Played { user: *user_id, score: *score, accuracy: *accuracy, full_combo: *full_combo }).await;
                check_all_ready(r, as_, state).await;
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
                match &mut as_.state.lifecycle {
                    InternalRoomState::Playing { results, aborted } => {
                        if results.contains_key(user_id) { return err("already uploaded"); }
                        if !aborted.insert(*user_id) { return err("already aborted"); }
                    }
                    _ => return err("not in Playing state"),
                }
                r.send(Message::Abort { user: *user_id }).await;
                check_all_ready(r, as_, state).await;
                ok(RoomCommandPayload::RoundAborted { room_id: room_id.clone().to_string(), user_id: *user_id })
            }

            RoomActorCommand::HostStart { room_id, user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = match room { Some(ref r) => r, None => return err("no room") };
                if !matches!(&as_.state.lifecycle, InternalRoomState::SelectChart) {
                    return err("room is not selecting a chart");
                }
                if as_.state.control.admin_start_pending { return err("administrative start is already in progress"); }
                if as_.state.chart.is_none() { return err("no chart selected"); }
                reset_game_time(r).await;
                r.send(Message::GameStart { user: *user_id }).await;
                as_.state.lifecycle = InternalRoomState::WaitForReady {
                    started: std::iter::once(*user_id).collect(), admin_started: false,
                };
                broadcast_state_change(r, &as_.state.lifecycle, as_.state.chart).await;
                check_all_ready(r, as_, state).await;
                state.dispatch_plugin_event(PluginEvent::GameStart { user_id: *user_id, room_id: room_id.clone().to_string() }).await;
                ok(RoomCommandPayload::HostStarted { room_id: room_id.clone().to_string() })
            }

            RoomActorCommand::AddUser { room_id, user_id, user_name: _, monitor, .. } => {
                let as_ = ctx.expect_actor_state();
                let r = match room { Some(ref r) => r, None => return err("no room") };
                let current_count = r.users().await.len();
                if current_count >= as_.state.control.max_users && !monitor {
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
                    room_full: current_count + 1 >= as_.state.control.max_users,
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

            RoomActorCommand::AddTouches { room_id, user_id, touches, .. } => {
                let as_ = ctx.expect_actor_state();
                let entry = as_.player_data.entry(*user_id).or_default();
                entry.push_touches(touches);
                ok(RoomCommandPayload::TouchesCached {
                    room_id: room_id.clone().to_string(), user_id: *user_id,
                })
            }

            RoomActorCommand::AddJudges { room_id, user_id, judges, .. } => {
                let as_ = ctx.expect_actor_state();
                let entry = as_.player_data.entry(*user_id).or_default();
                entry.push_judges(judges);
                ok(RoomCommandPayload::JudgesCached {
                    room_id: room_id.clone().to_string(), user_id: *user_id,
                })
            }

            RoomActorCommand::SetDisplayName { room_id, user_id, name, .. } => {
                let as_ = ctx.expect_actor_state();
                as_.display_names.insert(*user_id, name.clone());
                ok(RoomCommandPayload::DisplayNameSet {
                    room_id: room_id.clone().to_string(), user_id: *user_id, name: name.clone(),
                })
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

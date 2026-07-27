//! Runtime room command handler — all commands write actor_state.
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
    command::RoomActorCommand, context::RoomCommandContext, lifecycle::RoomLifecycle,
    RoomCommandDelivery, RoomCommandPayload, RoomCommandResult,
};
use crate::plugin::PluginEvent;
use crate::room::InternalRoomState;
use phira_mp_common::{Message, PartialRoomData, RoomEvent, ServerCommand};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use tracing::{debug, info, warn};

/// Calculate and set the playing timeout deadline based on chart duration + offset.
/// Falls back to a default if no cached duration is available.
fn set_playing_deadline(
    as_: &mut crate::room_actor::actor::RoomActorState,
    state: &crate::server::PlusServerState,
) {
    let offset_secs = state.config.playing_timeout_offset_secs;
    if offset_secs == 0 {
        as_.state.playing_timeout_deadline = None;
        return; // 超时未启用
    }
    let chart_id = as_.state.chart.unwrap_or(0);
    let duration_secs = state
        .chart_duration_cache
        .try_read()
        .ok()
        .and_then(|c| c.get(&chart_id).copied())
        .unwrap_or(120.0); // fallback: 120s
    let total_ms = (duration_secs + offset_secs as f64) * 1000.0;
    let deadline = now_ms() + total_ms as i64;
    as_.state.playing_timeout_deadline = Some(deadline);
    debug!(
        chart = chart_id, duration = %duration_secs, offset = offset_secs,
        deadline_ms = deadline, "playing timeout set"
    );
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

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
async fn broadcast_state_change(lc: &dyn RoomLifecycle, state: &InternalRoomState, chart: Option<i32>) {
    let room_state = state.to_client(chart);
    lc.broadcast(ServerCommand::ChangeState(room_state)).await;
    let stripped = state.stripped();
    let state_desc = match stripped {
        phira_mp_common::StrippedRoomState::SelectingChart => "selecting_chart",
        phira_mp_common::StrippedRoomState::WaitingForReady => "waiting_for_ready",
        phira_mp_common::StrippedRoomState::Playing => "playing",
    };
    lc.publish_update(PartialRoomData {
        state: Some(stripped),
        ..Default::default()
    })
    .await;
    lc.publish_runtime_event(crate::event_bus::MpEvent::RoomStateChanged {
        room_id: lc.room().id.clone(),
        state: state_desc.to_string(),
    });
}

/// Save round history and produce a RoundData event.
/// Returns Some(RoundData) if there was a Playing round to save.
async fn save_round_history(
    lc: &dyn RoomLifecycle,
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
    let room_ref = lc.room();
    for u in lc.users().await {
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
            std: rec.std,
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
                std: 0.0,
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
                .record_round_result(&round.round_id.to_string(), &room_ref.id.to_string(), result)
                .await
            {
                warn!(
                    room = %room_ref.id,
                    round_id = %round.round_id,
                    user_id = result.user_id,
                    "failed to persist round result"
                );
            }
        }
    }
    let event = crate::room::protocol_round(&round);
    room_ref.play_history.push(round, &room_ref.uuid).await;
    let total = room_ref.play_history.len().await;
    info!(
        room = room_ref.id.to_string(),
        "saved play round history (total {})", total
    );
    Some(event)
}

/// Check if all users are ready (transition to Playing) or all have finished (transition to SelectChart).
async fn check_all_ready(
    lc: &dyn RoomLifecycle,
    as_: &mut crate::room_actor::actor::RoomActorState,
) {
    // Clone the lifecycle to check state
    let lifecycle = as_.state.lifecycle.clone();
    match &lifecycle {
        InternalRoomState::WaitForReady { started, admin_started } => {
            let total: Vec<_> = lc.users().await.into_iter().chain(lc.monitors().await).collect();
            let ready_count = total.iter().filter(|it| started.contains(&it.id)).count();
            if ready_count < total.len() {
                debug!(
                    room = %lc.room().id, ready = ready_count, total = total.len(),
                    "waiting for ready"
                );
            }
            // Admin start (force start) skips the per-user ready check — all
            // players are moved directly into the game without waiting for Ready.
            if *admin_started
                || total.iter().all(|it| started.contains(&it.id))
            {
                // All ready — transition to Playing
                as_.state.ready_countdown_started_at = None;
                if *admin_started {
                    // Finish admin start
                    as_.state.control.admin_start_pending = false;
                    if let Some(host) = lc.users().await.iter().find(|u| as_.state.control.host_id == Some(u.id)) {
                        host.try_send(ServerCommand::ChangeHost(true)).await;
                    }
                }
                let round_id = uuid::Uuid::new_v4();
                as_.state.round.round_id = Some(round_id);
                info!(room = lc.room().id.to_string(), round = %round_id, "game start");
                lc.publish_runtime_event(crate::event_bus::MpEvent::GameStarted {
                    room_id: lc.room().id.clone(),
                    round_id: round_id.to_string(),
                });
                lc.send_msg(Message::StartPlaying).await;
                lc.reset_game_time().await;
                as_.state.lifecycle = InternalRoomState::Playing {
                    results: HashMap::new(),
                    aborted: HashSet::new(),
                };
                set_playing_deadline(as_, lc.server_state());
                broadcast_state_change(lc, &as_.state.lifecycle, as_.state.chart).await;

                // 打开轮次数据存储
                let rid = round_id.to_string();
                let cid = as_.state.chart.unwrap_or(0);
                let players: Vec<i32> = lc.users().await.into_iter().map(|u| u.id).collect();
                if let Some(rs) = &lc.room().round_store {
                    let meta = crate::round_store::RoundMeta {
                        round_uuid: rid,
                        chart_id: cid,
                        chart_name: as_.state.chart_name.clone().unwrap_or_default(),
                        room_id: lc.room().id.to_string(),
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
            if lc.users().await.into_iter()
                .all(|it| results.contains_key(&it.id) || aborted.contains(&it.id))
            {
                let rid = as_.state.round.round_id;
                let completed_round = save_round_history(
                    lc,
                    &mut as_.state.lifecycle,
                    &mut as_.state.round.round_id,
                    as_.state.chart,
                    as_.state.chart_name.as_deref(),
                    &as_.display_names,
                ).await;
                if let Some(round) = completed_round {
                    lc.publish_room_event(RoomEvent::NewRound {
                        room: lc.room().id.clone(),
                        round,
                    }).await;
                }

                // 关闭轮次数据存储
                if let Some(rid) = rid {
                    info!("round complete: {}", rid);
                    if let Some(rs) = &lc.room().round_store {
                        rs.close_round(&rid.to_string()).await;
                    }
                }

                // 触发 RoundComplete 事件
                if let Some(pm) = &lc.room().plugin_manager {
                    pm.dispatch_event(PluginEvent::RoundComplete {
                        room_id: lc.room().id.to_string(),
                        chart_id: as_.state.chart.unwrap_or(0),
                        chart_name: as_.state.chart_name.clone().unwrap_or_default(),
                    })
                    .await;
                }

                // Domain event for round completion
                if let Some(round_uuid) = rid {
                    lc.publish_runtime_event(crate::event_bus::MpEvent::RoundCompleted {
                        room_id: lc.room().id.clone(),
                        round_id: round_uuid.to_string(),
                    });
                }

                // 发送结算排行（本地化）
                {
                    if let Some(last) = lc.room().play_history.last().await {
                        let mut sorted = last.results.clone();
                        sorted.sort_by(|a, b| b.score.cmp(&a.score));
                        for user in lc.users().await.into_iter().chain(lc.monitors().await) {
                            let lang = user.lang.clone();
                            // 标题行
                            {
                                let mut args = fluent::FluentArgs::new();
                                args.set("chart_name", &last.chart_name);
                                let content = crate::l10n::translate_system(&lang, "result-ranking-title", &args);
                                user.try_send(ServerCommand::Message(Message::Chat { user: 0, content })).await;
                            }
                            // 每位玩家两行
                            for (i, rr) in sorted.iter().enumerate() {
                                let status_str = if rr.aborted {
                                    crate::l10n::translate_system(&lang, "result-aborted", &fluent::FluentArgs::new())
                                } else { String::new() };
                                let fc_str = if rr.full_combo {
                                    crate::l10n::translate_system(&lang, "result-fc", &fluent::FluentArgs::new())
                                } else { String::new() };
                                let mut args = fluent::FluentArgs::new();
                                args.set("rank", (i + 1) as i64);
                                args.set("name", &rr.user_name);
                                args.set("score", rr.score);
                                args.set("accuracy", format!("{:.2}", rr.accuracy * 100.0));
                                args.set("std", format!("{:.1}", rr.std * 1000.0));
                                args.set("fc", &fc_str);
                                args.set("status", &status_str);
                                let content = crate::l10n::translate_system(&lang, "result-player-line", &args);
                                user.try_send(ServerCommand::Message(Message::Chat { user: 0, content })).await;
                                let mut args2 = fluent::FluentArgs::new();
                                args2.set("perfect", rr.perfect);
                                args2.set("good", rr.good);
                                args2.set("bad", rr.bad);
                                args2.set("miss", rr.miss);
                                args2.set("max_combo", rr.max_combo);
                                let content = crate::l10n::translate_system(&lang, "result-detail-line", &args2);
                                user.try_send(ServerCommand::Message(Message::Chat { user: 0, content })).await;
                            }
                        }
                    }
                }
                lc.send_msg(Message::GameEnd).await;
                as_.state.round.round_id = None;
                as_.state.ready_countdown_started_at = None;
                as_.state.playing_timeout_deadline = None;
                as_.state.lifecycle = InternalRoomState::SelectChart;
                if as_.state.control.cycle && !as_.state.control.system_host {
                    debug!(room = lc.room().id.to_string(), "cycling");
                    let users = lc.users().await;
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
                        lc.send_msg(Message::NewHost { user: new_host.id }).await;
                        if let Some(old_uid) = old_id {
                            if let Some(old) = lc.users().await.iter().find(|u| u.id == old_uid) {
                                old.try_send(ServerCommand::ChangeHost(false)).await;
                            }
                        }
                        new_host.try_send(ServerCommand::ChangeHost(true)).await;
                        lc.publish_update(PartialRoomData {
                            host: Some(new_host.id),
                            ..Default::default()
                        })
                        .await;
                    }
                }
                broadcast_state_change(lc, &as_.state.lifecycle, as_.state.chart).await;
            }
        }
        _ => {}
    }
}

/// Transition to Playing — unready players are marked aborted.
/// Used by the ready countdown timer and can be reused for other auto-start paths.
pub(super) async fn force_start_playing(
    lc: &dyn RoomLifecycle,
    state: &mut crate::room_actor::actor::RoomState,
) {
    if !matches!(&state.lifecycle, InternalRoomState::WaitForReady { .. }) {
        return;
    }
    state.ready_countdown_started_at = None;

    // Collect unready players to abort
    let unready: HashSet<i32> = {
        let users = lc.users().await;
        let ready = match &state.lifecycle {
            InternalRoomState::WaitForReady { started, .. } => started.clone(),
            _ => HashSet::new(),
        };
        users.iter().map(|u| u.id).filter(|id| !ready.contains(id)).collect()
    };

    // If admin_started, restore host
    if let InternalRoomState::WaitForReady { admin_started, .. } = &state.lifecycle {
        if *admin_started {
            state.control.admin_start_pending = false;
            if let Some(host) = lc.users().await.iter().find(|u| state.control.host_id == Some(u.id)) {
                host.try_send(ServerCommand::ChangeHost(true)).await;
            }
        }
    }

    for id in &unready {
        info!(user = id, room = %lc.room().id, "auto-aborted (ready timeout)");
    }

    let round_id = uuid::Uuid::new_v4();
    state.round.round_id = Some(round_id);
    info!(room = lc.room().id.to_string(), round = %round_id, "game start (ready timeout)");

    lc.publish_runtime_event(crate::event_bus::MpEvent::GameStarted {
        room_id: lc.room().id.clone(),
        round_id: round_id.to_string(),
    });

    lc.send_msg(Message::StartPlaying).await;
    lc.reset_game_time().await;

    let results = HashMap::new();
    let mut aborted = HashSet::new();
    for id in unready {
        aborted.insert(id);
    }
    state.lifecycle = InternalRoomState::Playing { results, aborted };
    // Set playing timeout
    let server_state = lc.server_state();
    {
        let offset_secs = server_state.config.playing_timeout_offset_secs;
        if offset_secs > 0 {
            let chart_id = state.chart.unwrap_or(0);
            let duration_secs = server_state
                .chart_duration_cache
                .try_read()
                .ok()
                .and_then(|c| c.get(&chart_id).copied())
                .unwrap_or(120.0);
            let total_ms = (duration_secs + offset_secs as f64) * 1000.0;
            state.playing_timeout_deadline = Some(now_ms() + total_ms as i64);
            debug!(
                chart = chart_id, duration = %duration_secs, offset = offset_secs,
                "playing timeout set (force)"
            );
        }
    }
    broadcast_state_change(lc, &state.lifecycle, state.chart).await;

    let rid = round_id.to_string();
    let cid = state.chart.unwrap_or(0);
    let players: Vec<i32> = lc.users().await.into_iter().map(|u| u.id).collect();
    if let Some(rs) = &lc.room().round_store {
        let meta = crate::round_store::RoundMeta {
            round_uuid: rid,
            chart_id: cid,
            chart_name: state.chart_name.clone().unwrap_or_default(),
            room_id: lc.room().id.to_string(),
            players,
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
    lc.dispatch_plugin_event(PluginEvent::GameStart {
        user_id: 0,
        room_id: lc.room().id.to_string(),
    }).await;
}

/// End the playing phase due to timeout. Unfinished players are marked aborted,
/// then the round is saved and transitioned back to SelectChart.
pub(super) async fn force_end_playing(
    lc: &dyn RoomLifecycle,
    state: &mut crate::room_actor::actor::RoomState,
) {
    if !matches!(&state.lifecycle, InternalRoomState::Playing { .. }) {
        return;
    }
    // Remove unfinished and un-aborted players by adding them to aborted
    if let InternalRoomState::Playing { ref mut results, ref mut aborted } = &mut state.lifecycle {
        let users = lc.users().await;
        for u in &users {
            if !results.contains_key(&u.id) {
                aborted.insert(u.id);
                info!(user = u.id, room = %lc.room().id, "aborted by playing timeout");
            }
        }
    }
    // Now check_all_ready will see all as finished/aborted and transition
    // But we need actor_state for this — clone the lifecycle and check
    let all_done = match &state.lifecycle {
        InternalRoomState::Playing { results, aborted } => {
            let users = lc.users().await;
            users.iter().all(|u| results.contains_key(&u.id) || aborted.contains(&u.id))
        }
        _ => true,
    };
    if all_done {
        // Save round history directly
        let rid = state.round.round_id;
        if let InternalRoomState::Playing { .. } = &state.lifecycle {
            let completed_round = save_round_history(
                lc,
                &mut state.lifecycle,
                &mut state.round.round_id,
                state.chart,
                state.chart_name.as_deref(),
                &std::collections::HashMap::new(), // display_names not available here
            ).await;
            if let Some(round) = completed_round {
                lc.publish_room_event(RoomEvent::NewRound {
                    room: lc.room().id.clone(),
                    round,
                }).await;
            }
        }
        if let Some(rid) = rid {
            if let Some(rs) = &lc.room().round_store {
                rs.close_round(&rid.to_string()).await;
            }
        }
        state.round.round_id = None;
        state.ready_countdown_started_at = None;
        state.playing_timeout_deadline = None;
        lc.send_msg(Message::GameEnd).await;
        state.lifecycle = InternalRoomState::SelectChart;
        broadcast_state_change(lc, &state.lifecycle, state.chart).await;
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
        let lc: &dyn RoomLifecycle = ctx.lc;

        match command {
            RoomActorCommand::SetLock { room_id, locked, actor_user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                as_.state.set_locked(*locked);
                lc.publish_update(PartialRoomData { lock: Some(*locked), ..Default::default() }).await;
                lc.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: *actor_user_id, room_id: room_id.clone().to_string(),
                    data: json!({"action":"lock","value":locked}).to_string(),
                }).await;
                lc.publish_runtime_event(crate::event_bus::MpEvent::RoomLocked {
                    room_id: room_id.clone().try_into().unwrap(),
                    locked: *locked,
                });
                ok(RoomCommandPayload::LockChanged { room_id: room_id.clone().to_string(), locked: *locked })
            }

            RoomActorCommand::SetCycle { room_id, cycle, actor_user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                as_.state.set_cycle(*cycle);
                lc.publish_update(PartialRoomData { cycle: Some(*cycle), ..Default::default() }).await;
                lc.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: *actor_user_id, room_id: room_id.clone().to_string(),
                    data: json!({"action":"cycle","value":cycle}).to_string(),
                }).await;
                lc.publish_runtime_event(crate::event_bus::MpEvent::RoomCycled {
                    room_id: room_id.clone().try_into().unwrap(),
                    cycle: *cycle,
                });
                ok(RoomCommandPayload::CycleChanged { room_id: room_id.clone().to_string(), cycle: *cycle })
            }

            RoomActorCommand::SetHidden { room_id, hidden, .. } => {
                let as_ = ctx.expect_actor_state();
                as_.state.set_hidden(*hidden);
                lc.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: 0, room_id: room_id.clone().to_string(),
                    data: json!({"action":"hidden","value":hidden}).to_string(),
                }).await;
                ok(RoomCommandPayload::HiddenChanged { room_id: room_id.clone().to_string(), hidden: *hidden })
            }

            RoomActorCommand::SetHost { room_id, target_id, .. } => {
                let as_ = ctx.expect_actor_state();
                // Find the target user (if any) and get display name from actor_state
                let (_host_id, host_name, system_host) = match target_id {
                    Some(uid) => {
                        let fallback_name = {
                            let users = lc.users().await;
                            users.iter().find(|u| u.id == *uid).map(|u| u.name.clone())
                        };
                        let name = as_.display_names.get(uid)
                            .cloned()
                            .or(fallback_name)
                            .unwrap_or_else(|| uid.to_string());
                        // Send messages directly via Room broadcast
                        let name_clone = name.clone();
                        if as_.state.control.host_id.is_some() {
                            lc.room().send_system_msg(
                                &|lang| {
                                    let mut a = fluent::FluentArgs::new();
                                    a.set("name", &name_clone);
                                    crate::l10n::translate_system(lang, "host-transferred-to", &a)
                                },
                            ).await;
                        } else {
                            lc.room().send_system_msg(
                                &|lang| {
                                    let mut a = fluent::FluentArgs::new();
                                    a.set("name", &name_clone);
                                    crate::l10n::translate_system(lang, "user-became-host", &a)
                                },
                            ).await;
                        }
                        // Notify old host
                        if let Some(old_uid) = as_.state.control.host_id {
                            if old_uid != *uid {
                                if let Some(old) = lc.users().await.iter().find(|u| u.id == old_uid) {
                                    old.try_send(ServerCommand::ChangeHost(false)).await;
                                }
                            }
                        }
                        // Set new host in actor state
                        as_.state.control.host_id = Some(*uid);
                        as_.state.control.system_host = false;
                        // Announce
                        lc.send_msg(Message::NewHost { user: *uid }).await;
                        if let Some(u) = lc.users().await.iter().find(|u| u.id == *uid) {
                            u.try_send(ServerCommand::ChangeHost(true)).await;
                        }
                        lc.publish_update(PartialRoomData {
                            host: Some(*uid),
                            ..Default::default()
                        }).await;
                        (Some(*uid), name, false)
                    }
                    None => {
                        lc.room().send_system_msg_simple("host-set-to-system").await;
                        // Notify old host
                        if let Some(old_uid) = as_.state.control.host_id {
                            if let Some(old) = lc.users().await.iter().find(|u| u.id == old_uid) {
                                old.try_send(ServerCommand::ChangeHost(false)).await;
                            }
                        }
                        as_.state.control.host_id = None;
                        as_.state.control.system_host = true;
                        lc.send_msg(Message::NewHost { user: -1 }).await;
                        lc.publish_update(PartialRoomData {
                            host: Some(-1),
                            ..Default::default()
                        }).await;
                        (None, "?".to_string(), true)
                    }
                };
                lc.publish_runtime_event(crate::event_bus::MpEvent::HostChanged {
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
                lc.room().send_system_msg_simple("room-closed-by-admin").await;
                for user in lc.users().await {
                    *user.room.write().await = None;
                    user.try_send(ServerCommand::LeaveRoom(Ok(()))).await;
                    lc.publish_room_event(RoomEvent::LeaveRoom { room: lc.room().id.clone(), user: user.id }).await;
                }
                for monitor in lc.monitors().await {
                    *monitor.room.write().await = None;
                    monitor.try_send(ServerCommand::LeaveRoom(Ok(()))).await;
                }
                lc.remove_room(&lc.room().id).await;
                lc.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: 0, room_id: lc.room().id.to_string(),
                    data: json!({"action":"closed"}).to_string(),
                }).await;
                ok(RoomCommandPayload::RoomClosed { room_id: lc.room().id.to_string() })
            }

            RoomActorCommand::KickUser { room_id, target_id, .. } => {
                let users = lc.users().await;
                let monitors = lc.monitors().await;
                let user = match users.into_iter().chain(monitors.into_iter()).find(|u| u.id == *target_id) {
                    Some(u) => u, None => return err("user not in room"),
                };
                let name = user.name.clone();
                lc.room().send_system_msg(
                    &|lang| {
                        let mut a = fluent::FluentArgs::new();
                        a.set("name", &name);
                        crate::l10n::translate_system(lang, "user-kicked-from-room", &a)
                    },
                ).await;
                let was_monitor = user.monitor.load(std::sync::atomic::Ordering::SeqCst);
                let should_drop = lc.on_user_leave(&user).await;
                user.try_send(ServerCommand::LeaveRoom(Ok(()))).await;
                if should_drop { lc.remove_room(&lc.room().id).await; }
                if !was_monitor {
                    lc.publish_room_event(RoomEvent::LeaveRoom { room: lc.room().id.clone(), user: *target_id }).await;
                }
                // Clean up cached player data and display names for the kicked user.
                let as_ = ctx.expect_actor_state();
                as_.player_data.remove(target_id);
                as_.display_names.remove(target_id);
                lc.dispatch_plugin_event(PluginEvent::RoomModify {
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
                broadcast_state_change(lc, &as_.state.lifecycle, as_.state.chart).await;

                // Temporarily remove host privileges
                if let Some(host) = lc.users().await.iter().find(|u| as_.state.control.host_id == Some(u.id)) {
                    host.try_send(ServerCommand::ChangeHost(false)).await;
                }

                lc.reset_game_time().await;
                lc.send_msg(Message::GameStart { user: 0 }).await;
                lc.room().send_system_msg_simple("admin-started-game").await;
                as_.state.lifecycle = InternalRoomState::WaitForReady {
                    started: HashSet::new(),
                    admin_started: true,
                };
                as_.state.ready_countdown_started_at = Some(now_ms());
                broadcast_state_change(lc, &as_.state.lifecycle, as_.state.chart).await;
                check_all_ready(lc, as_).await;
                lc.dispatch_plugin_event(PluginEvent::GameStart { user_id: 0, room_id: room_id.clone().to_string() }).await;
                ok(RoomCommandPayload::RoomStarted { room_id: room_id.clone().to_string() })
            }

            RoomActorCommand::CancelStart { room_id, .. } => {
                let as_ = ctx.expect_actor_state();
                let canceled = matches!(&as_.state.lifecycle, InternalRoomState::WaitForReady { .. });
                if canceled {
                    // Restore host privileges if admin_started
                    if let InternalRoomState::WaitForReady { admin_started, .. } = &as_.state.lifecycle {
                        if *admin_started {
                            if let Some(host) = lc.users().await.iter().find(|u| as_.state.control.host_id == Some(u.id)) {
                                host.try_send(ServerCommand::ChangeHost(true)).await;
                            }
                        }
                    }
                    as_.state.control.admin_start_pending = false;
                    as_.state.ready_countdown_started_at = None;
                    as_.state.lifecycle = InternalRoomState::SelectChart;
                    lc.send_msg(Message::CancelGame { user: 0 }).await;
                    broadcast_state_change(lc, &as_.state.lifecycle, as_.state.chart).await;
                }
                ok(RoomCommandPayload::CancelResult { room_id: room_id.clone().to_string(), canceled })
            }

            RoomActorCommand::SetChart { room_id, chart_id, chart_name, actor_user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                if !matches!(&as_.state.lifecycle, InternalRoomState::SelectChart) {
                    return err("cannot set chart outside SelectChart state");
                }
                as_.state.chart = Some(*chart_id);
                as_.state.chart_name = Some(chart_name.clone());
                lc.send_msg(Message::SelectChart { user: *actor_user_id, name: chart_name.clone(), id: *chart_id }).await;
                broadcast_state_change(lc, &as_.state.lifecycle, as_.state.chart).await;
                lc.publish_update(phira_mp_common::PartialRoomData { chart: Some(*chart_id), ..Default::default() }).await;
                lc.publish_runtime_event(crate::event_bus::MpEvent::ChartSelected {
                    room_id: room_id.clone().try_into().unwrap(),
                    chart_id: *chart_id,
                });
                ok(RoomCommandPayload::ChartSelected { room_id: room_id.clone().to_string(), chart_id: *chart_id })
            }

            RoomActorCommand::SetReady { room_id, user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                match &mut as_.state.lifecycle {
                    InternalRoomState::WaitForReady { ref mut started, .. } => {
                        if !started.insert(*user_id) { return err("already ready"); }
                    }
                    _ => return err("not in WaitForReady state"),
                }
                lc.send_msg(Message::Ready { user: *user_id }).await;
                lc.publish_runtime_event(crate::event_bus::MpEvent::PlayerReadyChanged {
                    room_id: room_id.clone().try_into().unwrap(), user_id: *user_id, ready: true,
                });
                check_all_ready(lc, as_).await;
                ok(RoomCommandPayload::UserReady { room_id: room_id.clone().to_string(), user_id: *user_id })
            }

            RoomActorCommand::CancelReady { room_id, user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                let was_host = as_.state.control.host_id == Some(*user_id);
                match &mut as_.state.lifecycle {
                    InternalRoomState::WaitForReady { ref mut started, .. } => {
                        if !started.remove(user_id) { return err("not ready"); }
                        if was_host {
                            // All users' host cancels the game
                            if let InternalRoomState::WaitForReady { admin_started, .. } = &as_.state.lifecycle {
                                if *admin_started {
                                    if let Some(host) = lc.users().await.iter().find(|u| as_.state.control.host_id == Some(u.id)) {
                                        host.try_send(ServerCommand::ChangeHost(true)).await;
                                    }
                                }
                            }
                            as_.state.control.admin_start_pending = false;
                            as_.state.ready_countdown_started_at = None;
                            lc.send_msg(Message::CancelGame { user: *user_id }).await;
                            as_.state.lifecycle = InternalRoomState::SelectChart;
                            broadcast_state_change(lc, &as_.state.lifecycle, as_.state.chart).await;
                        } else {
                            lc.send_msg(Message::CancelReady { user: *user_id }).await;
                        }
                    }
                    _ => return err("not in WaitForReady state"),
                }
                lc.publish_runtime_event(crate::event_bus::MpEvent::PlayerReadyChanged {
                    room_id: room_id.clone().try_into().unwrap(), user_id: *user_id, ready: false,
                });
                ok(RoomCommandPayload::UserNotReady { room_id: room_id.clone().to_string(), user_id: *user_id })
            }

            RoomActorCommand::SubmitResult { room_id, user_id, score, accuracy, perfect, good, bad, miss, max_combo, full_combo, std, std_score, .. } => {
                let as_ = ctx.expect_actor_state();
                let record = crate::server::Record {
                    id: 0, player: *user_id, score: *score, perfect: *perfect,
                    good: *good, bad: *bad, miss: *miss, max_combo: *max_combo,
                    accuracy: *accuracy, full_combo: *full_combo, std: *std, std_score: *std_score,
                };
                match &mut as_.state.lifecycle {
                    InternalRoomState::Playing { results, aborted } => {
                        if aborted.contains(user_id) { return err("user aborted"); }
                        if results.insert(*user_id, record).is_some() { return err("already uploaded"); }
                    }
                    _ => return err("not in Playing state"),
                }
                // 首个完成者出现后延长对局超时（给其他玩家追赶时间）
                if let InternalRoomState::Playing { results, .. } = &as_.state.lifecycle {
                    let users = lc.users().await;
                    let finished = results.len();
                    let total = users.len();
                    if finished == 1 && total > 1 {
                        // 第一个完成，延长截止时间
                        let offset = (lc.server_state().config.playing_timeout_offset_secs as f64) * 1000.0;
                        if offset > 0.0 {
                            as_.state.playing_timeout_deadline = as_.state.playing_timeout_deadline.map(|d| d + offset as i64);
                            debug!("playing timeout extended by {}ms after first finish", offset as i64);
                        }
                    }
                }
                lc.send_msg(Message::Played { user: *user_id, score: *score, accuracy: *accuracy, full_combo: *full_combo, perfect: *perfect, good: *good, bad: *bad, miss: *miss, max_combo: *max_combo }).await;
                check_all_ready(lc, as_).await;
                lc.dispatch_plugin_event(PluginEvent::GameEnd {
                    user_id: *user_id, user_name: String::new(), room_id: room_id.clone().to_string(),
                    score: *score, accuracy: *accuracy, perfect: *perfect,
                    good: *good, bad: *bad, miss: *miss, max_combo: *max_combo, full_combo: *full_combo,
                }).await;
                ok(RoomCommandPayload::RoundResultSubmitted { room_id: room_id.clone().to_string(), user_id: *user_id, score: *score })
            }

            RoomActorCommand::AbortRound { room_id, user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                match &mut as_.state.lifecycle {
                    InternalRoomState::Playing { results, aborted } => {
                        if results.contains_key(user_id) { return err("already uploaded"); }
                        if !aborted.insert(*user_id) { return err("already aborted"); }
                    }
                    _ => return err("not in Playing state"),
                }
                lc.send_msg(Message::Abort { user: *user_id }).await;
                check_all_ready(lc, as_).await;
                ok(RoomCommandPayload::RoundAborted { room_id: room_id.clone().to_string(), user_id: *user_id })
            }

            RoomActorCommand::HostStart { room_id, user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                if !matches!(&as_.state.lifecycle, InternalRoomState::SelectChart) {
                    return err("room is not selecting a chart");
                }
                if as_.state.control.admin_start_pending { return err("administrative start is already in progress"); }
                if as_.state.chart.is_none() { return err("no chart selected"); }
                lc.reset_game_time().await;
                lc.send_msg(Message::GameStart { user: *user_id }).await;
                as_.state.lifecycle = InternalRoomState::WaitForReady {
                    started: std::iter::once(*user_id).collect(), admin_started: false,
                };
                as_.state.ready_countdown_started_at = Some(now_ms());
                broadcast_state_change(lc, &as_.state.lifecycle, as_.state.chart).await;
                check_all_ready(lc, as_).await;
                lc.dispatch_plugin_event(PluginEvent::GameStart { user_id: *user_id, room_id: room_id.clone().to_string() }).await;
                ok(RoomCommandPayload::HostStarted { room_id: room_id.clone().to_string() })
            }

            RoomActorCommand::AddUser { room_id, user_id, user_name: _, monitor, .. } => {
                let as_ = ctx.expect_actor_state();
                let current_count = lc.users().await.len();
                if current_count >= as_.state.control.max_users && !monitor {
                    return err("room is full");
                }
                if !as_.state.live {
                    as_.state.live = true;
                    tracing::info!(room = %lc.room().id, "room goes live via add_user");
                }
                if *monitor {
                    as_.state.members.monitors.push(*user_id);
                } else {
                    as_.state.members.users.push(*user_id);
                    // First non-monitor user becomes host, but only for
                    // player-created rooms (those with a creator_id).
                    // Server-created empty rooms keep host=None until CLI sets it.
                    if as_.state.control.host_id.is_none() && lc.room().creator_id.is_some() {
                        as_.state.control.host_id = Some(*user_id);
                    }
                }
                lc.dispatch_plugin_event(PluginEvent::RoomModify {
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
                let user = {
                    let users = lc.users().await;
                    let monitors = lc.monitors().await;
                    users.iter().find(|u| u.id == *user_id).cloned()
                        .or_else(|| monitors.iter().find(|u| u.id == *user_id).cloned())
                };
                match user {
                    Some(user) => {
                        let was_monitor = user.monitor.load(std::sync::atomic::Ordering::SeqCst);
                        let should_drop = lc.on_user_leave(&user).await;
                        if should_drop { lc.remove_room(&lc.room().id).await; }
                        if !was_monitor {
                            lc.publish_room_event(RoomEvent::LeaveRoom { room: lc.room().id.clone(), user: *user_id }).await;
                        }
                        // Clean up cached player data and display names for the removed user,
                        // and remove from authoritative members list.
                        let as_ = ctx.expect_actor_state();
                        as_.player_data.remove(user_id);
                        as_.display_names.remove(user_id);
                        as_.state.members.users.retain(|id| *id != *user_id);
                        as_.state.members.monitors.retain(|id| *id != *user_id);
                        lc.dispatch_plugin_event(PluginEvent::RoomModify {
                            user_id: *user_id, room_id: room_id.clone().to_string(),
                            data: json!({"action": "leave"}).to_string(),
                        }).await;
                        // Trigger check_all_ready in case the leaving user was in-game.
                        check_all_ready(lc, as_).await;
                        ok(RoomCommandPayload::UserRemoved {
                            room_id: room_id.clone().to_string(), user_id: *user_id, room_dropped: should_drop,
                        })
                    }
                    None => err("user not found in room"),
                }
            }

            RoomActorCommand::SetLive { room_id, live, .. } => {
                let as_ = ctx.expect_actor_state();
                let changed = as_.state.live != *live;
                as_.state.live = *live;
                if changed && *live {
                    tracing::info!(room = %room_id, "room goes live via set_live");
                }
                ok(RoomCommandPayload::LiveChanged {
                    room_id: room_id.clone().to_string(), live: *live,
                })
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

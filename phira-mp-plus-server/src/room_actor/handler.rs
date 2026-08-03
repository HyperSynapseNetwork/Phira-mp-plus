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
    command::{RoomActorCommand, RoomOrigin}, context::RoomCommandContext,
    lifecycle::RoomLifecycle, BindAndSnapshotData, BindAndSnapshotUser, RoomCommandDelivery,
    RoomCommandPayload, RoomCommandResult,
};
use crate::official_client_compat::protocol_trace::ProtocolTrace;
use crate::plugin::PluginEvent;
use crate::room::InternalRoomState;
use phira_mp_common::{Message, PartialRoomData, RoomEvent, ServerCommand, UserInfo};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
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
    let duration_secs = as_.state.chart_duration.unwrap_or(120.0);
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

/// PMP47 B: 该房间当前权威状态事件序号。Room 广播总线上每次 `bump_room_seq`
/// 都会把新序号写入 `Room::last_room_seq`；Handler 的直发（`try_send`）在
/// **事件产生时**读取它给 `SnapshotCovered` 事件打戳（`room_seq`），随出站
/// 条目携带，而非出站消费时读共享镜像——避免 N+1 over-stamp（audit §7.5）。
fn room_seq(lc: &dyn RoomLifecycle) -> Option<u64> {
    Some(lc.room().last_room_seq.load(std::sync::atomic::Ordering::Relaxed))
}

/// PMP46 Blocker 2 / PMP47 B: 权威状态事件序号递增 + 写入 Room 广播总线的
/// `last_room_seq` 镜像。每次权威状态变更前调用：`room_event_seq` 递增后立即
/// 把新值写入 `Room::last_room_seq`（emit-time binding）。此后该命令发出的
/// 广播/直发事件，其 Gate 条目都会以本事件真实序号打戳——认证激活时
/// `room_seq <= snapshot_seq` 才剔除（快照已包含），快照点之后的事件绝不误删
/// （audit §7.5）。`BindAndSnapshot` 不调用（只读）。
async fn bump_room_seq(
    lc: &dyn RoomLifecycle,
    state: &mut crate::room_actor::actor::RoomState,
) -> u64 {
    let seq = state.bump_room_event_seq();
    lc.room()
        .last_room_seq
        .store(seq, std::sync::atomic::Ordering::Relaxed);
    seq
}

/// Refuse a room command whose absolute actor deadline has passed (P0-C/P0-G).
///
/// Counts the refusal as a blocked late commit and returns the matching error
/// result so the caller answers the client with the corresponding `Err`
/// instead of silently dropping or committing after the deadline.
fn deadline_refused(deadline: std::time::Instant) -> RoomCommandResult {
    crate::official_client_compat::protocol_trace::ProtocolTrace::get()
        .late_commit
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    warn!(?deadline, "room command arrived after deadline; refusing to commit");
    err("command deadline elapsed")
}

/// Pure staleness decision for an origin token vs a user's current binding
/// snapshot. A `None` origin (non-session caller — CLI/admin/recovery) is never
/// stale. A session origin is stale when the binding generation moved OR the
/// bound session id no longer matches the origin session id (PMP44 P0-C).
fn origin_token_stale(
    origin: &RoomOrigin,
    binding_generation: u64,
    bound_session_id: Option<uuid::Uuid>,
) -> bool {
    let Some((session_id, generation)) = origin else {
        return false; // non-session callers (CLI/admin/recovery) are never stale
    };
    // Stale when either the generation moved OR the bound session no longer
    // matches the origin session id.
    binding_generation != *generation || bound_session_id != Some(*session_id)
}

/// PMP44 P0-C: re-validate the originating Session's generation against the
/// user's CURRENT binding at the room-actor commit point. A reconnect while the
/// command waited in the room mailbox bumps the generation, so a stale origin
/// (old session + old generation) must never mutate authoritative room state.
async fn origin_stale(lc: &dyn RoomLifecycle, origin: &RoomOrigin, user_id: i32) -> bool {
    // CLI/admin/recovery callers carry no session origin — never stale.
    if origin.is_none() {
        return false;
    }
    let state = lc.server_state();
    let user = {
        let users = state.users.read().await;
        users.get(&user_id).map(Arc::clone)
    };
    let Some(user) = user else {
        return true; // user no longer registered → stale
    };
    let binding = user.binding.read().await;
    let bound_session_id = binding
        .session
        .as_ref()
        .and_then(std::sync::Weak::upgrade)
        .map(|s| s.id);
    origin_token_stale(origin, binding.generation, bound_session_id)
}

/// Refuse a room command whose originating Session was superseded while the
/// command waited in the room mailbox (PMP44 P0-C). Counts the refusal so CI
/// can assert it stays 0 under normal operation.
fn refuse_stale_origin() -> RoomCommandResult {
    ProtocolTrace::get()
        .stale_commit_prevented
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    warn!("room command origin session superseded; refusing to commit");
    err("stale session origin; command refused")
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

/// Save round history and produce the in-memory PlayRound for this round.
/// Returns Some(PlayRound) if there was a Playing round to save.
///
/// The returned PlayRound is the authoritative in-memory record of the round
/// that just completed — the settlement ranking must render THIS round's
/// results, not a re-read of `play_history.last()`, so a slow/failed WAL
/// admission can never make the ranking fall back to an earlier round.
async fn save_round_history(
    lc: &dyn RoomLifecycle,
    lifecycle: &mut InternalRoomState,
    current_round_id: &mut Option<uuid::Uuid>,
    chart: Option<i32>,
    chart_name: Option<&str>,
    display_names: &HashMap<i32, String>,
) -> Option<crate::room::PlayRound> {
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

    // --- Persist round results via PersistenceWorker (WAL-backed) ---
    // Route through PersistenceWorker instead of direct SQL to ensure
    // write-ahead logging, retry, and dead-letter durability.
    //
    // All results are batched into a single RoundCompleted event so that
    // partial admission is impossible — either the whole round persists
    // or none of it does.
    let persistence_worker = &lc.server_state().persistence_worker;
    let finished_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let event = crate::persistence::PersistenceEvent::RoundCompleted {
        round_uuid: round_id.to_string(),
        room_id: room_ref.id.to_string(),
        event_id: uuid::Uuid::new_v4().to_string(),
        results: play_results.clone(),
        finished_at,
        aborted_users: aborted.iter().copied().collect(),
    };
    let any_failed = persistence_worker.enqueue(event).await.is_err();
    if any_failed {
        warn!(
            room = %room_ref.id,
            round_id = %round_id,
            "failed to enqueue RoundCompleted to persistence worker"
        );
    }

    let persistence_status = if any_failed {
        crate::room::PersistenceStatus::PendingAdmission
    } else {
        crate::room::PersistenceStatus::WalAdmitted
    };

    let round = crate::room::PlayRound {
        round_id,
        chart_id,
        chart_name: chart_name_str,
        results: play_results,
        persistence_status,
    };

    // In-memory play_history is the settlement/display cache and must always
    // reflect the round that just completed. The durable store is the WAL/DB
    // (via the RoundCompleted event above) — a failed admission must NOT leave
    // the settlement ranking pointing at an earlier round. PersistenceStatus
    // still records the admission outcome so callers can observe durability.
    room_ref.play_history.push(round.clone()).await;
    if any_failed {
        warn!(
            room = %room_ref.id,
            round_id = %round_id,
            "round not durably persisted (PendingAdmission); kept in-memory for settlement"
        );
    }
    Some(round)
}

/// PMP44 P0-N: Ready 检查结果——区分等待、成功开始、开始失败。
enum ReadyCheckOutcome {
    /// 尚未全员 Ready（或不在 WaitForReady），没有新的开赛尝试。
    Waiting,
    /// 全员 Ready（或 admin 强开）且持久化 open_round 成功，房间已进入 Playing。
    Started,
    /// 全员 Ready 但持久化 open_round 失败——房间退回 WaitingForReady，此前
    /// 已 Ready 的客户端必须收到 CancelReady 以收敛本地 Ready 状态（audit §18）。
    StartFailed,
}

/// Check if all users are ready (transition to Playing) or all have finished (transition to SelectChart).
///
/// `deadline` is the absolute actor deadline of the command that triggered the
/// check (P0-F). The durable round-open write is bounded by the remaining
/// budget so a stalled database cannot wedge the room actor forever.
///
/// PMP44 P0-N: 返回 [`ReadyCheckOutcome`] 供调用方区分等待/成功/失败——round
/// open 失败时此前已 Ready 的用户会收到 CancelReady，避免客户端与服务器
/// Ready 状态发散。
async fn check_all_ready(
    lc: &dyn RoomLifecycle,
    as_: &mut crate::room_actor::actor::RoomActorState,
    deadline: std::time::Instant,
) -> ReadyCheckOutcome {
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
                let prev_ready_countdown = as_.state.ready_countdown_started_at.take();
                let prev_admin_pending = as_.state.control.admin_start_pending;
                if *admin_started {
                    // Finish admin start
                    as_.state.control.admin_start_pending = false;
                    if let Some(host) = lc.users().await.iter().find(|u| as_.state.control.host_id == Some(u.id)) {
                        host.try_send(ServerCommand::ChangeHost(true), room_seq(lc)).await;
                    }
                }
                let round_id = uuid::Uuid::new_v4();
                as_.state.round.round_id = Some(round_id);

                // --- FIX P0-D(1): open_round FIRST (durable admission) ---
                // If open_round fails, revert state and return to WaitForReady
                // instead of transitioning to Playing without a persisted round.
                if let Some(rs) = &lc.room().round_store {
                    let rid = round_id.to_string();
                    let cid = as_.state.chart.unwrap_or(0);
                    let players: Vec<i32> = lc.users().await.into_iter().map(|u| u.id).collect();
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
                    // P0-F: bound the durable DB write by the remaining deadline
                    // so a stalled database cannot wedge the room actor forever.
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    let open_outcome = if remaining.is_zero() {
                        Err("round open deadline elapsed".to_string())
                    } else {
                        match tokio::time::timeout(remaining, rs.open_round(&meta)).await {
                            Ok(Ok(())) => Ok(()),
                            Ok(Err(e)) => Err(e.to_string()),
                            Err(_) => Err("round open timed out".to_string()),
                        }
                    };
                    if let Err(e) = open_outcome {
                        warn!(
                            room = %lc.room().id, round = %round_id,
                            "round store: failed to open round, aborting game start: {e}"
                        );
                        // PMP44 P0-N: 记录本次开赛尝试前已 Ready 的用户——round
                        // open 失败后这些用户的服务端 Ready 状态将被清除，必须向
                        // 客户端发送 CancelReady 收敛本地状态。
                        let ready_before: Vec<i32> = {
                            if let InternalRoomState::WaitForReady { started, .. } =
                                &as_.state.lifecycle
                            {
                                started.iter().copied().collect()
                            } else {
                                Vec::new()
                            }
                        };
                        // P0-F: clear the ready set so the room returns to an
                        // explicitly retryable WaitingForReady instead of a
                        // full-ready dead state where no further start can occur.
                        if let InternalRoomState::WaitForReady { started, admin_started } =
                            &mut as_.state.lifecycle
                        {
                            started.clear();
                            *admin_started = false;
                        }
                        as_.state.round.round_id = None;
                        as_.state.control.admin_start_pending = prev_admin_pending;
                        as_.state.ready_countdown_started_at = prev_ready_countdown;
                        lc.room().send_system_msg_simple("game-start-failed-retry").await;
                        broadcast_state_change(lc, &as_.state.lifecycle, as_.state.chart).await;
                        // PMP44 P0-N: 向此前所有已 Ready 的用户发送官方 CancelReady，
                        // 使客户端本地 Ready 状态与服务器收敛（服务器 started 已清空）。
                        for uid in ready_before {
                            if let Some(u) = lc.users().await.into_iter().find(|u| u.id == uid) {
                                u.try_send(ServerCommand::Message(Message::CancelReady { user: uid }), room_seq(lc)).await;
                            }
                        }
                        return ReadyCheckOutcome::StartFailed;
                    }
                }

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
                return ReadyCheckOutcome::Started;
            }
            // 未满员（或非 admin 强开）——仍处于 WaitForReady。
            ReadyCheckOutcome::Waiting
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
                if let Some(round) = &completed_round {
                    lc.publish_room_event(RoomEvent::StartRound {
                        room: lc.room().id.clone(),
                        round: crate::room::protocol_round(round),
                    }).await;
                }

                // Round close is now part of the atomic commit_round_completed
                // transaction inside the PersistenceWorker — no separate
                // close_round call needed here.

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

                // 发送结算排行（本地化）——以刚结算的 round 为准，不依赖
                // play_history 缓存（结算显示的是当前局真实成绩，绝不被旧轮覆盖）。
                {
                    if let Some(last) = &completed_round {
                        let mut sorted = last.results.clone();
                        sorted.sort_by(|a, b| b.score.cmp(&a.score));
                        for user in lc.users().await.into_iter().chain(lc.monitors().await) {
                            let lang = user.lang.clone();
                            // 标题行
                            {
                                let mut args = fluent::FluentArgs::new();
                                args.set("chart_name", &last.chart_name);
                                let content = crate::l10n::translate_system(&lang, "result-ranking-title", &args);
                                user.try_send(ServerCommand::Message(Message::Chat { user: 0, content }), room_seq(lc)).await;
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
                                user.try_send(ServerCommand::Message(Message::Chat { user: 0, content }), room_seq(lc)).await;
                                let mut args2 = fluent::FluentArgs::new();
                                args2.set("perfect", rr.perfect);
                                args2.set("good", rr.good);
                                args2.set("bad", rr.bad);
                                args2.set("miss", rr.miss);
                                args2.set("max_combo", rr.max_combo);
                                let content = crate::l10n::translate_system(&lang, "result-detail-line", &args2);
                                user.try_send(ServerCommand::Message(Message::Chat { user: 0, content }), room_seq(lc)).await;
                            }
                        }
                    }
                }
                lc.send_msg(Message::GameEnd).await;
                as_.state.round.round_id = None;
                as_.state.ready_countdown_started_at = None;
                as_.state.playing_timeout_deadline = None;
                as_.state.chart_duration = None;
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
                                old.try_send(ServerCommand::ChangeHost(false), room_seq(lc)).await;
                            }
                        }
                        new_host.try_send(ServerCommand::ChangeHost(true), room_seq(lc)).await;
                        lc.publish_update(PartialRoomData {
                            host: Some(new_host.id),
                            ..Default::default()
                        })
                        .await;
                    }
                }
                broadcast_state_change(lc, &as_.state.lifecycle, as_.state.chart).await;
            }
            // Playing 分支的完成结算不是一次“开赛”，视为 Waiting（无新的 Started）。
            ReadyCheckOutcome::Waiting
        }
        _ => ReadyCheckOutcome::Waiting,
    }
}

/// Transition to Playing — unready players are marked aborted.
/// Used by the ready countdown timer and can be reused for other auto-start paths.
///
/// `deadline` bounds the durable round-open write (P0-F) so a stalled database
/// cannot wedge the room actor. Non-session callers (e.g. the ready-countdown
/// tick in the mailbox) pass `Instant::now() + 30s`.
pub(super) async fn force_start_playing(
    lc: &dyn RoomLifecycle,
    state: &mut crate::room_actor::actor::RoomState,
    deadline: std::time::Instant,
) {
    if !matches!(state.lifecycle, InternalRoomState::WaitForReady { .. }) {
        return;
    }
    // PMP46 Blocker 2: 强制开赛推进 lifecycle，递增序号保证其广播的 ChangeState
    // 不会被认证 cutover 误删（audit §7.5）。
    let _seq = bump_room_seq(lc, &mut *state).await;
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
                host.try_send(ServerCommand::ChangeHost(true), room_seq(lc)).await;
            }
        }
    }

    for id in &unready {
        info!(user = id, room = %lc.room().id, "auto-aborted (ready timeout)");
    }

    let round_id = uuid::Uuid::new_v4();
    state.round.round_id = Some(round_id);
    info!(room = lc.room().id.to_string(), round = %round_id, "game start (ready timeout)");

    // --- FIX P0-D(1): open_round FIRST (durable admission) ---
    // If open_round fails, revert state and return instead of
    // transitioning to Playing without a persisted round.
    if let Some(rs) = &lc.room().round_store {
        let rid = round_id.to_string();
        let cid = state.chart.unwrap_or(0);
        let players: Vec<i32> = lc.users().await.into_iter().map(|u| u.id).collect();
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
        // P0-F: bound the durable DB write by the remaining deadline.
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let open_outcome = if remaining.is_zero() {
            Err("round open deadline elapsed".to_string())
        } else {
            match tokio::time::timeout(remaining, rs.open_round(&meta)).await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e.to_string()),
                Err(_) => Err("round open timed out".to_string()),
            }
        };
        if let Err(e) = open_outcome {
            warn!(
                room = %lc.room().id, round = %round_id,
                "round store: failed to open round, aborting force start: {e}"
            );
            // PMP44 P0-N: 记录强制开赛尝试前已 Ready 的用户——round open 失败后
            // 这些用户的服务端 Ready 状态将被清除，必须向客户端发送 CancelReady 收敛。
            let ready_before: Vec<i32> = {
                if let InternalRoomState::WaitForReady { started, .. } = &state.lifecycle {
                    started.iter().copied().collect()
                } else {
                    Vec::new()
                }
            };
            // P0-F: clear the ready set so the room returns to an explicitly
            // retryable WaitingForReady instead of a full-ready dead state.
            if let InternalRoomState::WaitForReady { started, admin_started } = &mut state.lifecycle {
                started.clear();
                *admin_started = false;
            }
            state.round.round_id = None;
            state.ready_countdown_started_at = None;
            lc.room().send_system_msg_simple("game-start-failed-retry").await;
            broadcast_state_change(lc, &state.lifecycle, state.chart).await;
            // PMP44 P0-N: 向此前已 Ready 的用户发送官方 CancelReady，收敛客户端状态。
            for uid in ready_before {
                if let Some(u) = lc.users().await.into_iter().find(|u| u.id == uid) {
                    u.try_send(ServerCommand::Message(Message::CancelReady { user: uid }), room_seq(lc)).await;
                }
            }
            return;
        }
    }

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
            let duration_secs = state.chart_duration.unwrap_or(120.0);
            let total_ms = (duration_secs + offset_secs as f64) * 1000.0;
            state.playing_timeout_deadline = Some(now_ms() + total_ms as i64);
            debug!(
                chart = chart_id, duration = %duration_secs, offset = offset_secs,
                "playing timeout set (force)"
            );
        }
    }
    broadcast_state_change(lc, &state.lifecycle, state.chart).await;

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
    if !matches!(state.lifecycle, InternalRoomState::Playing { .. }) {
        return;
    }
    // PMP46 Blocker 2: 强制结算推进 lifecycle，递增序号保证其广播的 ChangeState
    // 不会被认证 cutover 误删（audit §7.5）。
    let _seq = bump_room_seq(lc, &mut *state).await;
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
        if let InternalRoomState::Playing { .. } = &state.lifecycle {
            let completed_round = save_round_history(
                lc,
                &mut state.lifecycle,
                &mut state.round.round_id,
                state.chart,
                state.chart_name.as_deref(),
                &std::collections::HashMap::new(), // display_names not available here
            ).await;
            if let Some(round) = &completed_round {
                lc.publish_room_event(RoomEvent::StartRound {
                    room: lc.room().id.clone(),
                    round: crate::room::protocol_round(round),
                }).await;
            }
        }
        // Round close is now part of the atomic commit_round_completed
        // transaction inside the PersistenceWorker — no separate
        // close_round call needed here.
        state.round.round_id = None;
        state.ready_countdown_started_at = None;
        state.playing_timeout_deadline = None;
        state.chart_duration = None;
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
            RoomActorCommand::SetLock { room_id, locked, actor_user_id, deadline, origin, .. } => {
                let as_ = ctx.expect_actor_state();
                // P0-C/P0-G: never mutate lock state after the absolute actor deadline.
                if crate::official_client_compat::timing::deadline_expired(*deadline) {
                    return deadline_refused(*deadline);
                }
                // PMP44 P0-C: the originating session was superseded while the
                // command was queued — refuse the commit.
                if origin_stale(lc, origin, *actor_user_id).await {
                    return refuse_stale_origin();
                }
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
                as_.state.set_locked(*locked);
                lc.publish_update(PartialRoomData { lock: Some(*locked), ..Default::default() }).await;
                lc.publish_runtime_event(crate::event_bus::MpEvent::RoomLocked {
                    room_id: room_id.clone().try_into().unwrap(),
                    locked: *locked,
                });
                // PMP44 P0-M: 插件事件是 response-after——权威状态提交 + 官方消息
                // 同步返回后，插件回调绝不阻塞 Actor reply。
                let srv = lc.server_state_arc();
                let plugin_room_id = room_id.to_string();
                let plugin_user_id = *actor_user_id;
                let plugin_locked = *locked;
                crate::supervisor_actor::spawn_named(
                    format!("room-modify-lock-{plugin_room_id}-{plugin_user_id}"),
                    async move {
                        srv.dispatch_plugin_event(PluginEvent::RoomModify {
                            user_id: plugin_user_id,
                            room_id: plugin_room_id,
                            data: json!({"action":"lock","value":plugin_locked}).to_string(),
                        })
                        .await;
                    },
                );
                ok(RoomCommandPayload::LockChanged { room_id: room_id.clone().to_string(), locked: *locked })
            }

            RoomActorCommand::SetCycle { room_id, cycle, actor_user_id, deadline, origin, .. } => {
                let as_ = ctx.expect_actor_state();
                // P0-C/P0-G: never mutate cycle state after the absolute actor deadline.
                if crate::official_client_compat::timing::deadline_expired(*deadline) {
                    return deadline_refused(*deadline);
                }
                // PMP44 P0-C: the originating session was superseded while the
                // command was queued — refuse the commit.
                if origin_stale(lc, origin, *actor_user_id).await {
                    return refuse_stale_origin();
                }
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
                as_.state.set_cycle(*cycle);
                lc.publish_update(PartialRoomData { cycle: Some(*cycle), ..Default::default() }).await;
                lc.publish_runtime_event(crate::event_bus::MpEvent::RoomCycled {
                    room_id: room_id.clone().try_into().unwrap(),
                    cycle: *cycle,
                });
                // PMP44 P0-M: 插件事件是 response-after——绝不阻塞 Actor reply。
                let srv = lc.server_state_arc();
                let plugin_room_id = room_id.to_string();
                let plugin_user_id = *actor_user_id;
                let plugin_cycle = *cycle;
                crate::supervisor_actor::spawn_named(
                    format!("room-modify-cycle-{plugin_room_id}-{plugin_user_id}"),
                    async move {
                        srv.dispatch_plugin_event(PluginEvent::RoomModify {
                            user_id: plugin_user_id,
                            room_id: plugin_room_id,
                            data: json!({"action":"cycle","value":plugin_cycle}).to_string(),
                        })
                        .await;
                    },
                );
                ok(RoomCommandPayload::CycleChanged { room_id: room_id.clone().to_string(), cycle: *cycle })
            }

            RoomActorCommand::SetHidden { room_id, hidden, .. } => {
                let as_ = ctx.expect_actor_state();
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
                as_.state.set_hidden(*hidden);
                lc.dispatch_plugin_event(PluginEvent::RoomModify {
                    user_id: 0, room_id: room_id.clone().to_string(),
                    data: json!({"action":"hidden","value":hidden}).to_string(),
                }).await;
                ok(RoomCommandPayload::HiddenChanged { room_id: room_id.clone().to_string(), hidden: *hidden })
            }

            RoomActorCommand::SetHost { room_id, target_id, .. } => {
                let as_ = ctx.expect_actor_state();
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
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
                                    old.try_send(ServerCommand::ChangeHost(false), room_seq(lc)).await;
                                }
                            }
                        }
                        // Set new host in actor state
                        as_.state.control.host_id = Some(*uid);
                        as_.state.control.system_host = false;
                        // Announce
                        lc.send_msg(Message::NewHost { user: *uid }).await;
                        if let Some(u) = lc.users().await.iter().find(|u| u.id == *uid) {
                            u.try_send(ServerCommand::ChangeHost(true), room_seq(lc)).await;
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
                                old.try_send(ServerCommand::ChangeHost(false), room_seq(lc)).await;
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
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
                let endpoint = endpoint.clone();
                as_.state.control.phira_api_endpoint = endpoint.clone();
                ok(RoomCommandPayload::EndpointChanged {
                    room_id: room_id.clone().to_string(), endpoint: endpoint.clone().unwrap_or_default(), endpoint_override: endpoint.clone(), using_room_override: false,
                })
            }

            RoomActorCommand::CloseRoom { room_id: _, .. } => {
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                {
                    let as_ = ctx.expect_actor_state();
                    let _seq = bump_room_seq(lc, &mut as_.state).await;
                }
                lc.room().send_system_msg_simple("room-closed-by-admin").await;
                for user in lc.users().await {
                    *user.room.write().await = None;
                    user.try_send(ServerCommand::LeaveRoom(Ok(())), room_seq(lc)).await;
                    lc.publish_room_event(RoomEvent::LeaveRoom { room: lc.room().id.clone(), user: user.id }).await;
                }
                for monitor in lc.monitors().await {
                    *monitor.room.write().await = None;
                    monitor.try_send(ServerCommand::LeaveRoom(Ok(())), room_seq(lc)).await;
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
                let user = match users.into_iter().chain(monitors).find(|u| u.id == *target_id) {
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
                // PMP46 Blocker 2: 权威成员移除前递增序号（audit §7.5）。
                {
                    let as_ = ctx.expect_actor_state();
                    let _seq = bump_room_seq(lc, &mut as_.state).await;
                }
                let was_monitor = user.monitor.load(std::sync::atomic::Ordering::SeqCst);
                let should_drop = lc.on_user_leave(&user).await
                    && !lc.room().control_snapshot().persistent_empty;
                user.try_send(ServerCommand::LeaveRoom(Ok(())), room_seq(lc)).await;
                if should_drop { lc.remove_room(&lc.room().id).await; }
                if !was_monitor {
                    lc.publish_room_event(RoomEvent::LeaveRoom { room: lc.room().id.clone(), user: *target_id }).await;
                }
                // Clean up cached player data and display names for the kicked user.
                let as_ = ctx.expect_actor_state();
                as_.player_data.remove(target_id);
                as_.display_names.remove(target_id);
                // Host transfer when the kicked user was the host (same
                // choke-point rule as RemoveUser): hand the host to the next
                // remaining user, or revert to the system host for an empty
                // persistent room.
                if !should_drop && as_.state.control.host_id == Some(*target_id) {
                    let remaining = lc.users().await;
                    if let Some(next) = remaining.into_iter().next() {
                        let _seq = bump_room_seq(lc, &mut as_.state).await;
                        as_.state.control.host_id = Some(next.id);
                        as_.state.control.system_host = false;
                        let next_name = next.name.clone();
                        lc.room().send_system_msg(
                            &|lang| {
                                let mut a = fluent::FluentArgs::new();
                                a.set("name", &next_name);
                                crate::l10n::translate_system(lang, "host-transferred-to", &a)
                            },
                        ).await;
                        lc.send_msg(Message::NewHost { user: next.id }).await;
                        next.try_send(ServerCommand::ChangeHost(true), room_seq(lc)).await;
                        lc.publish_update(PartialRoomData {
                            host: Some(next.id),
                            ..Default::default()
                        }).await;
                    } else if as_.state.control.persistent_empty {
                        let _seq = bump_room_seq(lc, &mut as_.state).await;
                        as_.state.control.host_id = None;
                        as_.state.control.system_host = true;
                        lc.send_msg(Message::NewHost { user: -1 }).await;
                        lc.publish_update(PartialRoomData {
                            host: Some(-1),
                            ..Default::default()
                        }).await;
                    }
                }
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
                if !matches!(as_.state.lifecycle, InternalRoomState::SelectChart) {
                    return err("room is not selecting a chart");
                }
                if as_.state.chart.is_none() {
                    return err("no chart selected");
                }

                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
                as_.state.control.admin_start_pending = true;
                broadcast_state_change(lc, &as_.state.lifecycle, as_.state.chart).await;

                // Temporarily remove host privileges
                if let Some(host) = lc.users().await.iter().find(|u| as_.state.control.host_id == Some(u.id)) {
                    host.try_send(ServerCommand::ChangeHost(false), room_seq(lc)).await;
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
                let _ = check_all_ready(lc, as_, std::time::Instant::now() + std::time::Duration::from_secs(30)).await;
                lc.dispatch_plugin_event(PluginEvent::GameStart { user_id: 0, room_id: room_id.clone().to_string() }).await;
                ok(RoomCommandPayload::RoomStarted { room_id: room_id.clone().to_string() })
            }

            RoomActorCommand::EnterReadyPhase { room_id, .. } => {
                let as_ = ctx.expect_actor_state();
                // 进入准备阶段：仅 SelectChart 可进入；与 StartRoom（admin 强开）
                // 不同，这里 `admin_started=false`，不跳过玩家准备检查。
                if !matches!(as_.state.lifecycle, InternalRoomState::SelectChart) {
                    return err("room is not selecting a chart");
                }
                if as_.state.control.admin_start_pending {
                    return err("administrative start is already in progress");
                }
                if as_.state.chart.is_none() {
                    return err("no chart selected");
                }
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
                // 官方 RequestStart 核心序列（P0-D）：reset_game_time →
                // Message::GameStart → WaitForReady → on_state_change。PMP
                // 扩展（ready_countdown）紧随其后；不调用 check_all_ready——
                // 单人/空房不应在无玩家 ready 时立即开赛，由 SetReady / 倒计时
                // 超时（run_lifecycle_maintenance）驱动后续开赛。
                lc.reset_game_time().await;
                lc.send_msg(Message::GameStart { user: 0 }).await;
                // 官方 RequestStart 把发起者（host）加入 started（session.rs
                // `started: once(host)`）——host 发起开赛时自己天然 ready。PMP
                // 的 EnterReadyPhase 是 CLI 命令，若 started 为空，广播
                // ChangeState(WaitingForReady) 后房主客户端 is_ready=is_host=true
                // 但服务器不知道房主已 ready，check_all_ready 判房主未 ready 而
                // Waiting，只能靠倒计时强开。这里把当前房主加入 started，与官方
                // host 发起 RequestStart 的语义对齐（非 admin 强开，跳过
                // admin_started 检查）。
                let mut started = HashSet::new();
                if let Some(host_id) = as_.state.control.host_id {
                    started.insert(host_id);
                    lc.send_msg(Message::Ready { user: host_id }).await;
                }
                as_.state.lifecycle = InternalRoomState::WaitForReady {
                    started,
                    admin_started: false,
                };
                as_.state.ready_countdown_started_at = Some(now_ms());
                broadcast_state_change(lc, &as_.state.lifecycle, as_.state.chart).await;
                // PMP44 P0-M: GameStart 插件事件是 response-after——绝不阻塞 Actor reply。
                let srv = lc.server_state_arc();
                let plugin_room_id = room_id.to_string();
                crate::supervisor_actor::spawn_named(
                    format!("room-gamestart-{plugin_room_id}-admin-ready"),
                    async move {
                        srv.dispatch_plugin_event(PluginEvent::GameStart {
                            user_id: 0,
                            room_id: plugin_room_id,
                        })
                        .await;
                    },
                );
                ok(RoomCommandPayload::RoomStarted { room_id: room_id.clone().to_string() })
            }

            RoomActorCommand::CancelStart { room_id, .. } => {
                let as_ = ctx.expect_actor_state();
                let canceled = matches!(as_.state.lifecycle, InternalRoomState::WaitForReady { .. });
                if canceled {
                    // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                    let _seq = bump_room_seq(lc, &mut as_.state).await;
                    // Restore host privileges if admin_started
                    if let InternalRoomState::WaitForReady { admin_started, .. } = &as_.state.lifecycle {
                        if *admin_started {
                            if let Some(host) = lc.users().await.iter().find(|u| as_.state.control.host_id == Some(u.id)) {
                                host.try_send(ServerCommand::ChangeHost(true), room_seq(lc)).await;
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

            RoomActorCommand::SetChart { room_id, chart_id, chart_name, actor_user_id, deadline, origin, .. } => {
                let as_ = ctx.expect_actor_state();
                if !matches!(as_.state.lifecycle, InternalRoomState::SelectChart) {
                    return err("cannot set chart outside SelectChart state");
                }
                // P0-C/P0-G: never mutate the selected chart after the absolute
                // actor deadline.
                if crate::official_client_compat::timing::deadline_expired(*deadline) {
                    return deadline_refused(*deadline);
                }
                // PMP44 P0-C: the originating session was superseded while the
                // command was queued — refuse the commit.
                if origin_stale(lc, origin, *actor_user_id).await {
                    return refuse_stale_origin();
                }
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
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

            RoomActorCommand::SetChartDuration { room_id, duration, .. } => {
                let as_ = ctx.expect_actor_state();
                debug!(room = %room_id, duration = ?duration, "chart duration set");
                as_.state.chart_duration = *duration;
                ok(RoomCommandPayload::ChartDurationSet)
            }

            RoomActorCommand::SetReady { room_id, user_id, deadline, origin, .. } => {
                let as_ = ctx.expect_actor_state();
                // P0-G: never write Ready after the absolute actor deadline.
                if crate::official_client_compat::timing::deadline_expired(*deadline) {
                    return deadline_refused(*deadline);
                }
                // PMP44 P0-C: the originating session was superseded while the
                // command was queued — refuse the commit.
                if origin_stale(lc, origin, *user_id).await {
                    return refuse_stale_origin();
                }
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
                match &mut as_.state.lifecycle {
                    InternalRoomState::WaitForReady { ref mut started, .. } => {
                        if !started.insert(*user_id) { return err("already ready"); }
                        lc.send_msg(Message::Ready { user: *user_id }).await;
                        lc.publish_runtime_event(crate::event_bus::MpEvent::PlayerReadyChanged {
                            room_id: room_id.clone().try_into().unwrap(), user_id: *user_id, ready: true,
                        });
                        match check_all_ready(lc, as_, *deadline).await {
                            ReadyCheckOutcome::Started | ReadyCheckOutcome::Waiting => {
                                ok(RoomCommandPayload::UserReady { room_id: room_id.clone().to_string(), user_id: *user_id })
                            }
                            ReadyCheckOutcome::StartFailed => {
                                // PMP45 P0-L: round open 失败——服务器已清空 started，
                                // 触发用户不得收到 Ready(Ok)（否则官方客户端本地
                                // is_ready=true 与服务器分叉，audit §20）。返回错误，
                                // 客户端据此撤销本地 Ready 状态。
                                err("round start failed")
                            }
                        }
                    }
                    // P0-D: official phira-mp returns Ok (silent no-op) for Ready
                    // outside WaitForReady — NOT an error. Replicate the official
                    // server's observable behavior.
                    _ => ok(RoomCommandPayload::UserReady { room_id: room_id.clone().to_string(), user_id: *user_id }),
                }
            }

            RoomActorCommand::CancelReady { room_id, user_id, deadline, origin, .. } => {
                let as_ = ctx.expect_actor_state();
                // P0-G: never mutate CancelReady state after the absolute deadline.
                if crate::official_client_compat::timing::deadline_expired(*deadline) {
                    return deadline_refused(*deadline);
                }
                // PMP44 P0-C: the originating session was superseded while the
                // command was queued — refuse the commit.
                if origin_stale(lc, origin, *user_id).await {
                    return refuse_stale_origin();
                }
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
                let was_host = as_.state.control.host_id == Some(*user_id);
                match &mut as_.state.lifecycle {
                    InternalRoomState::WaitForReady { ref mut started, .. } => {
                        if !started.remove(user_id) { return err("not ready"); }
                        if was_host {
                            // All users' host cancels the game. Official core
                            // sequence: CancelGame → SelectChart → state change.
                            let admin_started = matches!(
                                &as_.state.lifecycle,
                                InternalRoomState::WaitForReady { admin_started: true, .. }
                            );
                            as_.state.control.admin_start_pending = false;
                            as_.state.ready_countdown_started_at = None;
                            lc.send_msg(Message::CancelGame { user: *user_id }).await;
                            as_.state.lifecycle = InternalRoomState::SelectChart;
                            broadcast_state_change(lc, &as_.state.lifecycle, as_.state.chart).await;
                            // P0-D: PMP extension — restore host privileges AFTER
                            // the official core sequence, never interleaved.
                            if admin_started {
                                if let Some(host) = lc.users().await.iter().find(|u| as_.state.control.host_id == Some(u.id)) {
                                    host.try_send(ServerCommand::ChangeHost(true), room_seq(lc)).await;
                                }
                            }
                        } else {
                            lc.send_msg(Message::CancelReady { user: *user_id }).await;
                        }
                        lc.publish_runtime_event(crate::event_bus::MpEvent::PlayerReadyChanged {
                            room_id: room_id.clone().try_into().unwrap(), user_id: *user_id, ready: false,
                        });
                        ok(RoomCommandPayload::UserNotReady { room_id: room_id.clone().to_string(), user_id: *user_id })
                    }
                    // P0-D: official phira-mp returns Ok (silent no-op) for
                    // CancelReady outside WaitForReady — NOT an error.
                    _ => ok(RoomCommandPayload::UserNotReady { room_id: room_id.clone().to_string(), user_id: *user_id }),
                }
            }

            RoomActorCommand::SubmitResult { room_id, user_id, score, accuracy, perfect, good, bad, miss, max_combo, full_combo, std, std_score, deadline, origin, .. } => {
                // P0-C/P0-G: never insert a result after the absolute actor deadline.
                if crate::official_client_compat::timing::deadline_expired(*deadline) {
                    // 迟到的成绩被拒绝（返回 deadline 错误给客户端），但绝不能因此
                    // 卡死房间：result 缺失会让 check_all_ready 的 `all(已提交||aborted)`
                    // 永不满足 → 房间停留在 Playing。把该玩家标记 aborted（成绩作废、
                    // 结算继续），并触发 check_all_ready 推进结算。
                    let mut mark = false;
                    {
                        let as_ = ctx.expect_actor_state();
                        if let InternalRoomState::Playing { results, aborted } = &mut as_.state.lifecycle {
                            if !results.contains_key(user_id) && !aborted.contains(user_id) {
                                aborted.insert(*user_id);
                                mark = true;
                            }
                        }
                    }
                    if mark {
                        let as_ = ctx.expect_actor_state();
                        let _seq = bump_room_seq(lc, &mut as_.state).await;
                        lc.send_msg(Message::Abort { user: *user_id }).await;
                        let _ = check_all_ready(lc, as_, *deadline).await;
                    }
                    return deadline_refused(*deadline);
                }
                let as_ = ctx.expect_actor_state();
                // PMP44 P0-C: the originating session was superseded while the
                // command was queued — refuse the commit.
                if origin_stale(lc, origin, *user_id).await {
                    return refuse_stale_origin();
                }
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
                let record = crate::server::Record {
                    id: 0, player: *user_id, score: *score, perfect: *perfect,
                    good: *good, bad: *bad, miss: *miss, max_combo: *max_combo,
                    accuracy: *accuracy, full_combo: *full_combo, std: *std, std_score: *std_score,
                };
                // player_score 域事件用协议 Record（字段与 server::Record 一致）。
                let event_record = phira_mp_common::Record {
                    id: record.id, player: record.player, score: record.score,
                    perfect: record.perfect, good: record.good, bad: record.bad,
                    miss: record.miss, max_combo: record.max_combo,
                    accuracy: record.accuracy, full_combo: record.full_combo,
                    std: record.std, std_score: record.std_score,
                };
                match &mut as_.state.lifecycle {
                    InternalRoomState::Playing { results, aborted } => {
                        if aborted.contains(user_id) { return err("user aborted"); }
                        if results.insert(*user_id, record).is_some() { return err("already uploaded"); }
                    }
                    _ => return err("not in Playing state"),
                }
                lc.publish_room_event(RoomEvent::PlayerScore {
                    room: lc.room().id.clone(),
                    record: event_record,
                }).await;
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
                let _ = check_all_ready(lc, as_, *deadline).await;
                // PMP45 P0-O: GameEnd 插件事件是 response-after——插件回调（WASM）
                // 绝不阻塞 Actor reply（audit §26）。权威提交（results 插入 +
                // Played 消息 + check_all_ready）保持同步。
                let srv = lc.server_state_arc();
                let plugin_room_id = room_id.to_string();
                let plugin_user_id = *user_id;
                let plugin_score = *score;
                let plugin_accuracy = *accuracy;
                let plugin_perfect = *perfect;
                let plugin_good = *good;
                let plugin_bad = *bad;
                let plugin_miss = *miss;
                let plugin_max_combo = *max_combo;
                let plugin_full_combo = *full_combo;
                crate::supervisor_actor::spawn_named(
                    format!("room-gameend-{plugin_room_id}-{plugin_user_id}"),
                    async move {
                        srv.dispatch_plugin_event(PluginEvent::GameEnd {
                            user_id: plugin_user_id,
                            user_name: String::new(),
                            room_id: plugin_room_id,
                            score: plugin_score,
                            accuracy: plugin_accuracy,
                            perfect: plugin_perfect,
                            good: plugin_good,
                            bad: plugin_bad,
                            miss: plugin_miss,
                            max_combo: plugin_max_combo,
                            full_combo: plugin_full_combo,
                        })
                        .await;
                    },
                );
                ok(RoomCommandPayload::RoundResultSubmitted { room_id: room_id.clone().to_string(), user_id: *user_id, score: *score })
            }

            RoomActorCommand::AbortRound { room_id, user_id, deadline, origin, .. } => {
                let as_ = ctx.expect_actor_state();
                // P0-C/P0-G: never insert an abort after the absolute actor deadline.
                if crate::official_client_compat::timing::deadline_expired(*deadline) {
                    return deadline_refused(*deadline);
                }
                // PMP44 P0-C: the originating session was superseded while the
                // command was queued — refuse the commit.
                if origin_stale(lc, origin, *user_id).await {
                    return refuse_stale_origin();
                }
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
                match &mut as_.state.lifecycle {
                    InternalRoomState::Playing { results, aborted } => {
                        if results.contains_key(user_id) { return err("already uploaded"); }
                        if !aborted.insert(*user_id) { return err("already aborted"); }
                    }
                    _ => return err("not in Playing state"),
                }
                lc.send_msg(Message::Abort { user: *user_id }).await;
                let _ = check_all_ready(lc, as_, *deadline).await;
                ok(RoomCommandPayload::RoundAborted { room_id: room_id.clone().to_string(), user_id: *user_id })
            }

            RoomActorCommand::HostStart { room_id, user_id, deadline, origin, .. } => {
                let as_ = ctx.expect_actor_state();
                if !matches!(as_.state.lifecycle, InternalRoomState::SelectChart) {
                    return err("room is not selecting a chart");
                }
                if as_.state.control.admin_start_pending { return err("administrative start is already in progress"); }
                if as_.state.chart.is_none() { return err("no chart selected"); }
                // P0-G: never transition to WaitForReady after the absolute
                // actor deadline.
                if crate::official_client_compat::timing::deadline_expired(*deadline) {
                    return deadline_refused(*deadline);
                }
                // PMP44 P0-C: the originating session was superseded while the
                // command was queued — refuse the commit.
                if origin_stale(lc, origin, *user_id).await {
                    return refuse_stale_origin();
                }
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
                // Official RequestStart core sequence (P0-D): reset_game_time →
                // Message::GameStart → WaitForReady → on_state_change →
                // check_all_ready. PMP extensions (ready_countdown_started_at,
                // plugin event) follow the official core.
                lc.reset_game_time().await;
                lc.send_msg(Message::GameStart { user: *user_id }).await;
                as_.state.lifecycle = InternalRoomState::WaitForReady {
                    started: std::iter::once(*user_id).collect(), admin_started: false,
                };
                as_.state.ready_countdown_started_at = Some(now_ms());
                broadcast_state_change(lc, &as_.state.lifecycle, as_.state.chart).await;
                let start_outcome = check_all_ready(lc, as_, *deadline).await;
                // PMP45 P0-L: round open 失败时返回 Err——发起者不得收到
                // HostStarted(Ok)（否则客户端认为开赛成功而服务器已退回
                // WaitForReady 并清空 started，Ready 状态分叉，audit §21）。
                if matches!(&start_outcome, ReadyCheckOutcome::StartFailed) {
                    return err("round start failed");
                }
                if let ReadyCheckOutcome::Waiting = &start_outcome {
                    // 全员未满但 transition 已发生? HostStart 总是把发起者加入
                    // started 并立即 check；正常情况下应是 Started 或 StartFailed。
                    // Waiting 属异常，保守返回成功但记录。
                    tracing::warn!("HostStart: unexpected Waiting outcome");
                }
                // PMP44 P0-M: GameStart 插件事件是 response-after——绝不阻塞 Actor reply。
                let srv = lc.server_state_arc();
                let plugin_room_id = room_id.to_string();
                let plugin_user_id = *user_id;
                crate::supervisor_actor::spawn_named(
                    format!("room-gamestart-{plugin_room_id}-{plugin_user_id}"),
                    async move {
                        srv.dispatch_plugin_event(PluginEvent::GameStart {
                            user_id: plugin_user_id,
                            room_id: plugin_room_id,
                        })
                        .await;
                    },
                );
                ok(RoomCommandPayload::HostStarted { room_id: room_id.clone().to_string() })
            }

            RoomActorCommand::AddUser { room_id, user_id, user_name: _, monitor, deadline, origin, .. } => {
                let as_ = ctx.expect_actor_state();
                // PMP45 P0-K: 房间处于 degraded（Join 补偿失败遗留 Ghost member，
                // 成员状态不确定）——在操作员 / 未来 reconcile 清空之前拒绝新的
                // AddUser（Join），避免在未知成员集合上继续提交。
                if as_.state.degraded {
                    return err("room join reconciliation pending");
                }
                // P0-F/P0-G: never commit a late Join after the absolute actor
                // deadline — the client may already have timed out and retried,
                // and a second Join would then observe "already in room".
                if crate::official_client_compat::timing::deadline_expired(*deadline) {
                    return deadline_refused(*deadline);
                }
                // PMP44 P0-C: the originating session was superseded while the
                // command was queued — a stale Join must never add a member.
                if origin_stale(lc, origin, *user_id).await {
                    return refuse_stale_origin();
                }
                let current_count = lc.users().await.len();
                if current_count >= as_.state.control.max_users && !monitor {
                    return err("room is full");
                }
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
                if !as_.state.live {
                    as_.state.live = true;
                    tracing::info!(room = %lc.room().id, "room goes live via add_user");
                }
                if *monitor {
                    as_.state.members.monitors.push(*user_id);
                } else {
                    as_.state.members.users.push(*user_id);
                    // Host is NEVER auto-assigned to the first joiner here.
                    // Player-created rooms get their host at actor init (from
                    // `creator_id`); server-created empty rooms and rooms
                    // explicitly set to the system host (`room host <id> ?`)
                    // keep `host_id = None` so they report host -1. A joiner
                    // must never silently take over a system-hosted room, and
                    // joining an empty room must not make the joiner host.
                }
                // PMP44 P0-M: 插件事件是 response-after——成员变更（join）是权威
                // 状态提交，插件回调绝不阻塞 Actor reply。
                let srv = lc.server_state_arc();
                let plugin_room_id = room_id.to_string();
                let plugin_user_id = *user_id;
                let plugin_action = if *monitor { "monitor_join" } else { "join" };
                crate::supervisor_actor::spawn_named(
                    format!("room-modify-add-{plugin_room_id}-{plugin_user_id}"),
                    async move {
                        srv.dispatch_plugin_event(PluginEvent::RoomModify {
                            user_id: plugin_user_id,
                            room_id: plugin_room_id,
                            data: json!({"action": plugin_action}).to_string(),
                        })
                        .await;
                    },
                );
                ok(RoomCommandPayload::UserAdded {
                    room_id: room_id.clone().to_string(), user_id: *user_id,
                    monitor: *monitor,
                    room_full: current_count + 1 >= as_.state.control.max_users,
                })
            }

            RoomActorCommand::RemoveUser { room_id, user_id, deadline, origin, .. } => {
                // P0-C/P0-G: never mutate membership after the absolute actor deadline.
                if crate::official_client_compat::timing::deadline_expired(*deadline) {
                    return deadline_refused(*deadline);
                }
                // PMP44 P0-C: the originating session was superseded while the
                // command was queued — refuse the commit.
                if origin_stale(lc, origin, *user_id).await {
                    return refuse_stale_origin();
                }
                let user = {
                    let users = lc.users().await;
                    let monitors = lc.monitors().await;
                    users.iter().find(|u| u.id == *user_id).cloned()
                        .or_else(|| monitors.iter().find(|u| u.id == *user_id).cloned())
                };
                // PMP46 Blocker 2: 权威成员移除前递增序号（audit §7.5）。
                {
                    let as_ = ctx.expect_actor_state();
                    let _seq = bump_room_seq(lc, &mut as_.state).await;
                }
                match user {
                    Some(user) => {
                        let was_monitor = user.monitor.load(std::sync::atomic::Ordering::SeqCst);
                        let should_drop = lc.on_user_leave(&user).await
                            && !lc.room().control_snapshot().persistent_empty;
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
                        // Host transfer on host leave.  This is the single choke
                        // point for ALL removal paths (explicit LeaveRoom, host
                        // disconnect, dangle-grace) — every one funnels through
                        // RemoveUser — so the host is transferred here, not in
                        // session_room.rs.  Previously the reassignment lived in
                        // session_room.rs but only for the explicit-leave path:
                        // a disconnecting host was never reassigned, and even the
                        // leave path was broken because `host_id` was never
                        // cleared (assign_room_host_if_missing bails when
                        // `host_id.is_some()`).
                        if !should_drop && as_.state.control.host_id == Some(*user_id) {
                            let remaining = lc.users().await;
                            if let Some(next) = remaining.into_iter().next() {
                                // Transfer host to the next remaining user.  The
                                // old host has already left the room, so no
                                // ChangeHost(false) to them.
                                let _seq = bump_room_seq(lc, &mut as_.state).await;
                                as_.state.control.host_id = Some(next.id);
                                as_.state.control.system_host = false;
                                let next_name = next.name.clone();
                                lc.room().send_system_msg(
                                    &|lang| {
                                        let mut a = fluent::FluentArgs::new();
                                        a.set("name", &next_name);
                                        crate::l10n::translate_system(lang, "host-transferred-to", &a)
                                    },
                                ).await;
                                lc.send_msg(Message::NewHost { user: next.id }).await;
                                next.try_send(ServerCommand::ChangeHost(true), room_seq(lc)).await;
                                lc.publish_update(PartialRoomData {
                                    host: Some(next.id),
                                    ..Default::default()
                                }).await;
                            } else if as_.state.control.persistent_empty {
                                // Room became empty but is persistent — revert to
                                // the system host (-1) so the room stays joinable
                                // as an empty room with host "?".
                                let _seq = bump_room_seq(lc, &mut as_.state).await;
                                as_.state.control.host_id = None;
                                as_.state.control.system_host = true;
                                lc.send_msg(Message::NewHost { user: -1 }).await;
                                lc.publish_update(PartialRoomData {
                                    host: Some(-1),
                                    ..Default::default()
                                }).await;
                            }
                        }
                        // PMP45 P0-O: 插件回调是 response-after（spawn，不经过 room
                        // mailbox），权威成员移除、on_user_leave、LeaveRoom 广播与
                        // check_all_ready 保持同步（官方可见效果）。
                        //
                        // 注意：check_all_ready 必须**同步**执行，不能经 room mailbox
                        // 重入——`execute_with_actor` 的 opaque future 通过
                        // `room_mailbox_sender` 的 worker 闭包间接引用自身，形成 E0391
                        // 类型循环。check_all_ready 本身就是 Actor 排序点内的调用，
                        // 同步执行即满足串行性（audit §26 的阻塞点是 DB 轮次持久化，
                        // 其由 round_store 的 remaining-timeout 限制，不会无限阻塞）。
                        let _ = check_all_ready(lc, as_, *deadline).await;
                        let srv = lc.server_state_arc();
                        let plugin_room_id = room_id.to_string();
                        let plugin_user_id = *user_id;
                        let leave_data = json!({"action": "leave"}).to_string();
                        crate::supervisor_actor::spawn_named(
                            format!("room-modify-leave-{plugin_room_id}-{plugin_user_id}"),
                            async move {
                                srv.dispatch_plugin_event(PluginEvent::RoomModify {
                                    user_id: plugin_user_id,
                                    room_id: plugin_room_id,
                                    data: leave_data,
                                })
                                .await;
                            },
                        );
                        ok(RoomCommandPayload::UserRemoved {
                            room_id: room_id.clone().to_string(), user_id: *user_id, room_dropped: should_drop,
                        })
                    }
                    None => {
                        // PMP44 P0-L: 连接注册表找不到该用户，但 Actor 成员可能已由
                        // AddUser 提交（连接注册表拒绝的 Ghost member，例如并发加入
                        // 恰好把房间撑满）。此时仍要清理 Actor 成员，撤销半提交的
                        // AddUser，使 join_room 的补偿 remove_user 成功（结果确定，
                        // 不产生 Ghost member）。Ghost member 没有连接注册条目，故不
                        // 广播 LeaveRoom（房间内无人可见该成员），也不删除房间。
                        let as_ = ctx.expect_actor_state();
                        if as_.state.members.users.contains(user_id)
                            || as_.state.members.monitors.contains(user_id)
                        {
                            as_.player_data.remove(user_id);
                            as_.display_names.remove(user_id);
                            as_.state.members.users.retain(|id| *id != *user_id);
                            as_.state.members.monitors.retain(|id| *id != *user_id);
                            ok(RoomCommandPayload::UserRemoved {
                                room_id: room_id.clone().to_string(),
                                user_id: *user_id,
                                room_dropped: false,
                            })
                        } else {
                            err("user not found in room")
                        }
                    }
                }
            }

            RoomActorCommand::SetLive { room_id, live, .. } => {
                let as_ = ctx.expect_actor_state();
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
                let changed = as_.state.live != *live;
                as_.state.live = *live;
                if changed && *live {
                    tracing::info!(room = %room_id, "room goes live via set_live");
                }
                ok(RoomCommandPayload::LiveChanged {
                    room_id: room_id.clone().to_string(), live: *live,
                })
            }

            RoomActorCommand::SetDegraded { room_id, degraded, .. } => {
                let as_ = ctx.expect_actor_state();
                // PMP45 P0-K: 设置房间 degraded 标志——Join 补偿失败时置 true
                //（AddUser 将拒绝新的 Join，直到操作员 / 未来的 reconcile 清空），
                // 也可由操作员显式置回 false 恢复 Join。
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
                let changed = as_.state.degraded != *degraded;
                as_.state.degraded = *degraded;
                if changed {
                    tracing::warn!(
                        room = %room_id,
                        degraded = *degraded,
                        "room degraded flag changed via set_degraded"
                    );
                }
                ok(RoomCommandPayload::Empty)
            }

            RoomActorCommand::SetDisplayName { room_id, user_id, name, .. } => {
                let as_ = ctx.expect_actor_state();
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
                as_.display_names.insert(*user_id, name.clone());
                ok(RoomCommandPayload::DisplayNameSet {
                    room_id: room_id.clone().to_string(), user_id: *user_id, name: name.clone(),
                })
            }

            RoomActorCommand::SetPersistentEmpty { room_id, persistent_empty, .. } => {
                let as_ = ctx.expect_actor_state();
                // PMP46 Blocker 2: 权威状态变更前递增序号（audit §7.5）。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
                as_.state.control.persistent_empty = *persistent_empty;
                ok(RoomCommandPayload::PersistentEmptyChanged {
                    room_id: room_id.clone().to_string(),
                    persistent_empty: *persistent_empty,
                })
            }

            RoomActorCommand::BindAndSnapshot { room_id, user_id, .. } => {
                let as_ = ctx.expect_actor_state();
                // PMP45 P0-F: 原子快照——state/lock/cycle/host/chart/live/ready
                // 全部从 actor 权威状态在同一排序点派生，绝不跨多次独立读取混用
                // 不同时间点（P0-09）。成员列表以 `actor_state.members` 为主，
                // 再并入 room 连接注册表（`lc.users()`/`lc.monitors()`）以覆盖
                // 未走 AddUser 的创建者（`Room::new` 直接加入注册表、从不进
                // actor members 的 pre-existing 差异）。两者都在本命令执行点
                // 读取，仍保持单点一致。
                let state = as_.state.lifecycle.to_client(as_.state.chart);
                let is_ready = matches!(
                    &as_.state.lifecycle,
                    InternalRoomState::WaitForReady { started, .. } if started.contains(user_id)
                );
                let mut users: HashMap<i32, UserInfo> = HashMap::new();
                {
                    let server = lc.server_state();
                    let users_guard = server.users.read().await;
                    for id in &as_.state.members.users {
                        let name = as_.display_names.get(id).cloned()
                            .or_else(|| users_guard.get(id).map(|u| u.name.clone()))
                            .unwrap_or_else(|| id.to_string());
                        users.insert(*id, UserInfo { id: *id, name, monitor: false });
                    }
                    for id in &as_.state.members.monitors {
                        let name = as_.display_names.get(id).cloned()
                            .or_else(|| users_guard.get(id).map(|u| u.name.clone()))
                            .unwrap_or_else(|| id.to_string());
                        users.insert(*id, UserInfo { id: *id, name, monitor: true });
                    }
                }
                // 并入连接注册表（覆盖创建者等未走 AddUser 的成员）。
                // 用 `entry().or_insert_with` 避免 contains_key+insert 双查（map_entry）。
                for u in lc.users().await {
                    users.entry(u.id).or_insert_with(|| {
                        let name = as_.display_names
                            .get(&u.id)
                            .cloned()
                            .unwrap_or_else(|| u.name.clone());
                        UserInfo {
                            id: u.id,
                            name,
                            monitor: u.monitor.load(std::sync::atomic::Ordering::SeqCst),
                        }
                    });
                }
                for u in lc.monitors().await {
                    users.entry(u.id).or_insert_with(|| {
                        let name = as_.display_names
                            .get(&u.id)
                            .cloned()
                            .unwrap_or_else(|| u.name.clone());
                        UserInfo {
                            id: u.id,
                            name,
                            monitor: true,
                        }
                    });
                }
                // `execute_with_actor` 返回 `RoomCommandResult`（非 `Result`），
                // 不能使用 `?`——显式 match 处理非法 room id。
                let rid: phira_mp_common::RoomId = match room_id.clone().try_into() {
                    Ok(rid) => rid,
                    Err(_) => return err("invalid room id"),
                };
                let client_state = phira_mp_common::ClientRoomState {
                    id: rid,
                    state,
                    live: as_.state.live,
                    locked: as_.state.control.locked,
                    cycle: as_.state.control.cycle,
                    is_host: as_.state.control.host_id == Some(*user_id),
                    is_ready,
                    users,
                };
                // cutover token：网关 command_seq。快照反映所有
                // `command_id <= token` 命令提交后的权威状态。
                let token = lc.server_state().room_commands.command_seq();
                // PMP46 Blocker 2: 快照时刻的权威状态事件序号——认证路径以它
                // 对齐 Gate cutover，绝不使用 Gate 自身序号（两者无关，audit §7.5）。
                let snapshot_seq = as_.state.room_event_seq;
                ok(RoomCommandPayload::BindAndSnapshot(BindAndSnapshotData {
                    room_id: room_id.to_string(),
                    state: as_.state.lifecycle.stripped(),
                    chart: as_.state.chart,
                    live: as_.state.live,
                    locked: as_.state.control.locked,
                    cycle: as_.state.control.cycle,
                    is_host: as_.state.control.host_id == Some(*user_id),
                    is_ready,
                    users: client_state
                        .users
                        .into_iter()
                        .map(|(id, info)| BindAndSnapshotUser {
                            id,
                            name: info.name,
                            monitor: info.monitor,
                        })
                        .collect(),
                    token,
                    snapshot_seq,
                }))
            }
            // 审计 P0: Telemetry fire-and-forget variants are handled by
            // execute_telemetry on the fast path; they should not arrive here.
            RoomActorCommand::TelemetryTouches { .. } | RoomActorCommand::TelemetryJudges { .. } => {
                ok(RoomCommandPayload::TouchesCached {
                    room_id: String::new(), user_id: 0,
                })
            }
            // AddTouches/AddJudges are no-op here — telemetry is now handled
            // by the execute_telemetry fast path in the actor.
            RoomActorCommand::AddTouches { .. } | RoomActorCommand::AddJudges { .. } => {
                ok(RoomCommandPayload::TouchesCached {
                    room_id: String::new(), user_id: 0,
                })
            }
            // PMP45 P0-O: 内部响应后检查（RemoveUser 触发，fire-and-forget）。
            // 在 Actor 排序点执行 check_all_ready，与其它命令串行；发起方丢弃
            // reply 接收端，无客户端等待。
            RoomActorCommand::CheckAllReady { deadline, .. } => {
                let as_ = ctx.expect_actor_state();
                // PMP46 Blocker 2: check_all_ready 可能推进 lifecycle（开赛/结算），
                // 递增序号保证其广播的 ChangeState 不会被认证 cutover 误删。
                let _seq = bump_room_seq(lc, &mut as_.state).await;
                let _outcome = check_all_ready(lc, as_, *deadline).await;
                ok(RoomCommandPayload::Empty)
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

    #[test]
    fn origin_token_stale_pure_predicate() {
        // Non-session callers (None origin) are never stale — even when the
        // binding generation moved or the bound session is unknown.
        assert!(!origin_token_stale(&None, 0, None));
        assert!(!origin_token_stale(&None, 7, Some(uuid::Uuid::nil())));

        let sid = uuid::Uuid::new_v4();
        // Matching generation AND session id => current.
        assert!(!origin_token_stale(&Some((sid, 3)), 3, Some(sid)));
        // Generation moved (reconnect) => stale.
        assert!(origin_token_stale(&Some((sid, 3)), 4, Some(sid)));
        // Bound session id no longer matches => stale.
        assert!(origin_token_stale(&Some((sid, 3)), 3, Some(uuid::Uuid::new_v4())));
        // No bound session => stale.
        assert!(origin_token_stale(&Some((sid, 3)), 3, None));
    }

    #[test]
    fn deadline_refused_returns_matching_error() {
        let result = deadline_refused(std::time::Instant::now() - std::time::Duration::from_secs(1));
        assert!(!result.is_ok(), "deadline refusal must be an error result");
        assert_eq!(
            result.error_message().as_deref(),
            Some("command deadline elapsed")
        );
    }

    #[test]
    fn deadline_refused_never_commits() {
        // A future deadline is NOT refused; a past deadline IS refused. This
        // pins the P0-G precondition used at the SetReady/HostStart commit
        // points.
        let future = crate::official_client_compat::timing::deadline_expired(
            std::time::Instant::now() + std::time::Duration::from_secs(1),
        );
        assert!(!future);
        let past = crate::official_client_compat::timing::deadline_expired(
            std::time::Instant::now() - std::time::Duration::from_millis(1),
        );
        assert!(past);
    }
}

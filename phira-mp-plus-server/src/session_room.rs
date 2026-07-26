//! Room lifecycle and room gameplay command handlers for client sessions.
//!
//! This module is intentionally kept free of socket/authentication details so
//! `session.rs` can become a thin dispatcher before the real Session Actor split.
//!
//! After Phase 2 Work C, Room is a pure broadcast interface and no longer
//! holds mutable state. All state queries route through the actor snapshot
//! cache (server.room_snapshot()) or Room::control_snapshot(). All state
//! mutations route through RoomCommandGateway.

use crate::phira_client::PhiraRetryNoticeTarget;
use crate::plugin::PluginEvent;
use crate::session::{SessionCategory, User};
use crate::session_auth::resolve_phira_api_endpoint;
use crate::tl;
use anyhow::{anyhow, bail, Result};

/// Translate known English error strings from room command results.
fn tr(e: String) -> String {
    match e.as_str() {
        "already ready" => tl!("already-ready"),
        "already uploaded" => tl!("already-uploaded"),
        "not ready" => tl!("not-ready"),
        "user aborted" => tl!("aborted"),
        "no chart selected" => tl!("start-no-chart-selected"),
        "room is full" => tl!("join-room-full"),
        "administrative start is already in progress" => tl!("admin-start-in-progress"),
        "room is not selecting a chart" | "cannot set chart outside SelectChart state" => tl!("invalid-state"),
        "not in WaitForReady state" => tl!("invalid-state"),
        "not in Playing state" => tl!("invalid-state"),
        _ => e,
    }
}
use phira_mp_common::{
    JoinRoomResponse, Message, RoomEvent, RoomId, ServerCommand,
};
use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc},
};
use tracing::{debug, debug_span, info, trace, Instrument};

pub fn decode_admin_room_command(input: &str) -> String {
    // Phira's room-name input box may not allow spaces. For the in-game admin
    // shortcut, the leading `_` is the command prefix and underscores after it
    // are treated as CLI spaces: `_room_list` => `room list`. A doubled
    // underscore escapes a literal underscore: `_room_info_my__room` =>
    // `room info my_room`.
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '_' {
            if matches!(chars.peek(), Some('_')) {
                chars.next();
                out.push('_');
            } else {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
    }
    out.trim().to_string()
}

/// Check if `user` is the room's host (via actor snapshot).
async fn is_host_for_room(room: &Arc<crate::room::Room>, user: &User) -> bool {
    let host = room.control_snapshot().host_id;
    host == Some(user.id)
}

async fn current_room(user: &Arc<User>) -> Result<Arc<crate::room::Room>> {
    user.room
        .read()
        .await
        .as_ref()
        .map(Arc::clone)
        .ok_or_else(|| anyhow!("{}", tl!("no-room")))
}

async fn current_room_in_select_chart(user: &Arc<User>) -> Result<Arc<crate::room::Room>> {
    let room = current_room(user).await?;
    // Read room lifecycle state from actor snapshot cache.
    if let Some(snap) = user.server.room_snapshot(&room.id.to_string()) {
        if !matches!(snap.stripped, phira_mp_common::StrippedRoomState::SelectingChart) {
            bail!("{}", tl!("invalid-state"));
        }
    } else {
        // No snapshot yet — fall back to assuming SelectChart for new rooms.
    }
    Ok(room)
}

/// Build a ClientRoomState from the actor snapshot and room user list.
pub(crate) async fn build_client_room_state(
    room: &crate::room::Room,
    user: &User,
) -> phira_mp_common::ClientRoomState {
    let control = room.control_snapshot();
    let snap = if let Some(server) = room.server.upgrade() {
        server.room_snapshot(&room.id.to_string())
    } else {
        None
    };
    let is_ready = snap.as_ref().and_then(|s| {
        s.ready_set.as_ref().map(|ready| ready.contains(&user.id))
    }).unwrap_or(false);
    let state = if let Some(ref snap) = snap {
        match snap.stripped {
            phira_mp_common::StrippedRoomState::SelectingChart =>
                phira_mp_common::RoomState::SelectChart(snap.chart),
            phira_mp_common::StrippedRoomState::WaitingForReady =>
                phira_mp_common::RoomState::WaitingForReady,
            phira_mp_common::StrippedRoomState::Playing =>
                phira_mp_common::RoomState::Playing,
        }
    } else {
        phira_mp_common::RoomState::SelectChart(None)
    };

    let users = room.users().await.into_iter()
        .chain(room.monitors().await)
        .map(|u| (u.id, u.to_info()))
        .collect();

    phira_mp_common::ClientRoomState {
        id: room.id.clone(),
        state,
        live: room.is_live(),
        locked: control.locked,
        cycle: control.cycle,
        is_host: control.host_id == Some(user.id),
        is_ready,
        users,
    }
}

/// Build a RoomData from the actor snapshot and room state.
pub(crate) async fn build_room_data(room: &crate::room::Room) -> phira_mp_common::RoomData {
    let control = room.control_snapshot();
    let snap = if let Some(server) = room.server.upgrade() {
        server.room_snapshot(&room.id.to_string())
    } else {
        None
    };
    let host = if control.system_host {
        -1
    } else {
        control.host_id.unwrap_or(-1)
    };
    let users: Vec<i32> = room.users().await.into_iter().map(|u| u.id).collect();
    let chart = snap.as_ref().and_then(|s| s.chart);
    let state = snap.as_ref().map_or(phira_mp_common::StrippedRoomState::SelectingChart, |s| s.stripped);
    let rounds = room.play_history.all().await.iter()
        .map(|r| crate::room::protocol_round(r))
        .collect();
    phira_mp_common::RoomData {
        host,
        users,
        lock: control.locked,
        cycle: control.cycle,
        chart,
        state,
        rounds,
    }
}

pub async fn create_room(user: Arc<User>, id: RoomId) -> Result<()> {
    let id_text = id.to_string();
    if let Some(command) = id_text.strip_prefix('_') {
        if user.server.is_admin_id(user.id).await {
            let command = decode_admin_room_command(command);
            let command = {
                let mut pending = user.admin_cli_pending.lock().await;
                match crate::cli::collect_cli_continuation(&mut *pending, command) {
                    Ok(Some(command)) => command,
                    Ok(None) => {
                        user.try_send(ServerCommand::Message(Message::Chat {
                            user: 0,
                            content: "[CLI] 已暂存续行；下一条命令需以 -- 开头".to_string(),
                        }))
                        .await;
                        bail!("admin CLI command pending");
                    }
                    Err(err) => {
                        user.try_send(ServerCommand::Message(Message::Chat {
                            user: 0,
                            content: format!("[CLI] {err}"),
                        }))
                        .await;
                        bail!("admin CLI continuation error");
                    }
                }
            };
            if command.is_empty() {
                user.try_send(ServerCommand::Message(Message::Chat {
                    user: 0,
                    content: "[CLI] 空命令".to_string(),
                }))
                .await;
                bail!("empty admin command");
            }
            let lines =
                crate::cli::execute_cli_once(Arc::clone(&user.server), command.clone()).await;
            user.try_send(ServerCommand::Message(Message::Chat {
                user: 0,
                content: format!("[CLI] > {command}"),
            }))
            .await;
            for line in lines {
                user.try_send(ServerCommand::Message(Message::Chat {
                    user: 0,
                    content: format!("[CLI] {line}"),
                }))
                .await;
            }
            bail!("admin CLI command executed");
        }
    }

    let mut room_guard = user.room.write().await;
    if room_guard.is_some() {
        bail!("{}", tl!("already-in-room"));
    }

    let mut map_guard = user.server.rooms.write().await;
    if map_guard.contains_key(&id) {
        bail!("{}", tl!("create-id-occupied"));
    }
    if let Some(limit) = user.server.config.max_rooms {
        if map_guard.len() >= limit {
            bail!("{}", tl!("server-room-limit-reached", limit => limit.to_string()));
        }
    }
    if !user.server.live_config.read().await.room_creation_enabled && !user.server.is_admin_id(user.id).await {
        bail!("{}", tl!("room-creation-disabled"));
    }
    let max_users = user.server.config.max_users_per_room.unwrap_or(100);
    let room = Arc::new(crate::room::Room::new(
        id.clone(),
        Arc::downgrade(&user),
        Some(Arc::clone(&user.server.plugin_manager)),
        Arc::downgrade(&user.server),
        max_users,
        Some(Arc::clone(&user.server.round_store)),
        Some(user.id),
    ));
    map_guard.insert(id.clone(), Arc::clone(&room));
    let room_uuid = room.uuid;
    // Drop write lock so subsequent reads don't hang.
    drop(map_guard);
    // NOTE: Room::new() already adds the creator as a user (users: vec![host]).
    // Do NOT call room.add_user() here or the creator will be double-counted
    // in room.users(), causing a 2-person room to appear as 3 or the welcome
    // message to report 2 users when there is only 1.
    *room_guard = Some(Arc::clone(&room));
    // Host is assigned by the first AddUser command (for monitors) or by
    // assign_room_host_if_missing (for non-monitors) in join_room.
    // CreateRoom(Ok) establishes client room state; do not emit a room event to
    // the creator before that response.
    user.server
        .publish_room_event(RoomEvent::CreateRoom {
            room: id.clone(),
            data: build_room_data(&room).await,
        })
        .await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    user.server
        .record_user_room_history(user.id, id.to_string(), room_uuid.to_string(), now)
        .await;

    info!(user = user.id, room = id.to_string(), room_uuid = %room_uuid, "user create room");
    info!("房间 '{}' 唯一标识: {}", id, room_uuid);

    user.server
        .dispatch_plugin_event(PluginEvent::RoomCreate {
            user_id: user.id,
            room_id: id.to_string(),
        })
        .await;

    user.server
        .publish_runtime_event(crate::event_bus::MpEvent::RoomCreated {
            room_id: id.clone(),
            room_uuid,
        });

    crate::internal_hooks::playtime_room_enter(user.id);
    Ok(())
}

pub async fn join_room(
    user: Arc<User>,
    category: SessionCategory,
    id: RoomId,
    monitor: bool,
) -> Result<JoinRoomResponse> {
    crate::internal_hooks::playtime_room_enter(user.id);
    let mut room_guard = user.room.write().await;
    if room_guard.is_some() {
        bail!("{}", tl!("already-in-room"));
    }
    let room = user.server.rooms.read().await.get(&id).map(Arc::clone);
    let Some(room) = room else {
        bail!("{}", tl!("room-not-found"))
    };
    // Monitors bypass player-only lock/ban/game-state gates.
    // No whitelist check — authentication at connection time is sufficient.
    let mut late_join = false;
    let mut need_abort = false;
    if !monitor {
        // Use control_snapshot for lock check (actor-authoritative)
        let control = room.control_snapshot();
        if control.locked {
            bail!("{}", tl!("join-room-locked"));
        }
        if user
            .server
            .ban_manager
            .is_room_banned(&room.uuid.to_string(), user.id)
            .await
        {
            bail!("{}", tl!("join-room-banned"));
        }
        // Read room lifecycle from actor snapshot for game state check.
        let stripped = if let Some(server) = room.server.upgrade() {
            server.room_snapshot(&room.id.to_string())
                .map(|s| s.stripped)
        } else {
            None
        };
        match stripped {
            Some(phira_mp_common::StrippedRoomState::SelectingChart) | None => {}
            Some(phira_mp_common::StrippedRoomState::Playing) => {
                let mut pending = user.join_pending_game.write().await;
                if pending.as_ref().map(|s| s.as_str()) == Some(id.to_string().as_str()) {
                    pending.take();
                    late_join = true;
                    need_abort = true;
                } else {
                    *pending = Some(id.to_string());
                    let _ = user
                        .try_send(ServerCommand::Message(Message::Chat {
                            user: 0,
                            content: tl!("join-game-ongoing-warning"),
                        }))
                        .await;
                    bail!("{}", tl!("join-game-ongoing"));
                }
            }
            _ => bail!("{}", tl!("join-game-ongoing")),
        }
        if need_abort {
            // Route the abort through the actor mailbox.
            user.server
                .room_commands
                .abort_round(&user.server, &room.id.to_string(), user.id)
                .await
                .ok();
        }
    }
    if monitor {
        // Route monitor add through mailbox for actor_state.members update.
        user.server
            .room_commands
            .add_user(&user.server, &id.to_string(), user.id, &user.name, true)
            .await
            .map_err(|e| anyhow!("{}", tr(e)))?;
        // Also add to Room connection mapping (immediate, direct).
        if !room.add_user(Arc::downgrade(&user), true).await {
            bail!("{}", tl!("join-room-full"));
        }
    } else if !room.add_user(Arc::downgrade(&user), false).await {
        bail!("{}", tl!("join-room-full"));
    }
    info!(
        user = user.id,
        room = id.to_string(),
        monitor,
        "user join room"
    );
    user.monitor.store(monitor, Ordering::SeqCst);
    // Route SetLive(true) through mailbox for actor-authoritative live flag.
    let _ = user.server
        .room_commands
        .set_live(&user.server, &id.to_string(), true)
        .await;
    // Register display name so result screens show names, not IDs.
    user.server
        .room_commands
        .set_display_name(&user.server, &id.to_string(), user.id, &user.name)
        .await
        .ok();
    user.server
        .assign_room_host_if_missing(&room, &user, monitor, false)
        .await;
    *room_guard = Some(Arc::clone(&room));
    // 清除进行中游戏加入确认标记
    user.join_pending_game.write().await.take();
    let joined_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    user.server
        .record_user_room_history(user.id, id.to_string(), room.uuid.to_string(), joined_at)
        .await;
    drop(room_guard);

    user.server.refresh_room_display_metadata_background(&room);
    let join = ServerCommand::OnJoinRoom(user.to_info());
    let message = ServerCommand::Message(Message::JoinRoom {
        user: user.id,
        name: user.name.clone(),
    });
    if monitor {
        room.broadcast_players(join).await;
        room.broadcast_players(message).await;
    } else {
        // The joining client first receives JoinRoom(Ok), which already contains
        // the full user snapshot. Only existing room members need incremental events.
        room.broadcast_except(user.id, join).await;
        room.broadcast_except(user.id, message).await;
        user.server
            .publish_room_event(RoomEvent::JoinRoom {
                room: id.clone(),
                user: user.id,
            })
            .await;
    }

    // Protocol-only game monitors are not exposed as players to plugins.
    // 插件事件可能执行 WASM/HTTP 逻辑，不能阻塞 JoinRoom 响应。
    if category == SessionCategory::Normal {
        user.server
            .dispatch_plugin_event(PluginEvent::RoomJoin {
                user_id: user.id,
                room_id: id.to_string(),
                is_monitor: monitor,
            })
            .await;
        user.server
            .publish_runtime_event(crate::event_bus::MpEvent::RoomJoined {
                room_id: room.id.clone(),
                user_id: user.id,
            });
    }

    let mut users = room.users().await;
    if category != SessionCategory::GameMonitor {
        users.extend(room.monitors().await);
    }

    let room_state = if late_join {
        // Read chart from actor snapshot.
        let chart = if let Some(server) = room.server.upgrade() {
            server.room_snapshot(&room.id.to_string())
                .and_then(|s| s.chart)
        } else {
            None
        };
        phira_mp_common::RoomState::SelectChart(chart)
    } else {
        build_client_room_state(&room, &user).await.state
    };

    // 发送房间最近的消息缓冲区给新加入用户
    let buf = room.message_buffer.read().await;
    for cmd in buf.iter() {
        user.try_send(cmd.clone()).await;
    }
    drop(buf);

    Ok(JoinRoomResponse {
        state: room_state,
        users: users.into_iter().map(|user| user.to_info()).collect(),
        live: room.is_live(),
    })
}

pub async fn leave_room(user: Arc<User>, category: SessionCategory) -> Result<()> {
    crate::internal_hooks::playtime_room_leave(user.id);
    user.join_pending_game.write().await.take();
    let room = current_room(&user).await?;
    let room_id = room.id.clone();
    info!(
        user = user.id,
        room = room.id.to_string(),
        "user leave room"
    );
    let was_monitor = user.monitor.load(Ordering::SeqCst);
    // Route through mailbox for actor_state.members update and Room cleanup.
    let result = user.server
        .room_commands
        .remove_user(&user.server, &room.id.to_string(), user.id)
        .await;
    let room_dropped = result.as_ref().ok()
        .and_then(|v| v.get("room_dropped"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // Fallback: if mailbox command failed, do direct cleanup.
    if result.is_err() {
        let room_empty = room.on_user_leave(&user).await;
        if room_empty {
            user.server.rooms.write().await.remove(&room.id);
        }
    }
    if !room_dropped && !was_monitor {
        // Reassign host to a random remaining user if host leaves.
        let remaining = room.users().await;
        if !remaining.is_empty() {
            let idx = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as usize) % remaining.len();
            if let Some(next) = remaining.get(idx).cloned() {
                user.server.assign_room_host_if_missing(&room, &next, false, false).await;
            }
        }
    }
    if category == SessionCategory::Normal && !was_monitor && !room_dropped {
        user.server
            .publish_room_event(RoomEvent::LeaveRoom {
                room: room.id.clone(),
                user: user.id,
            })
            .await;
    }

    if category == SessionCategory::Normal {
        user.server
            .dispatch_plugin_event(PluginEvent::RoomLeave {
                user_id: user.id,
                room_id: room_id.to_string(),
            })
            .await;
        user.server
            .publish_runtime_event(crate::event_bus::MpEvent::RoomLeft {
                room_id: room.id.clone(),
                user_id: user.id,
            });
    }

    Ok(())
}

pub async fn lock_room(user: Arc<User>, lock: bool) -> Result<()> {
    let room = current_room(&user).await?;
    if !is_host_for_room(&room, &user).await {
        bail!("{}", tl!("only-host-can-do"));
    }
    info!(
        user = user.id,
        room = room.id.to_string(),
        lock,
        "lock room"
    );
    user.server
        .room_commands
        .set_lock_as(&user.server, &room.id.to_string(), lock, user.id)
        .await
        .map_err(|e| anyhow!("{}", tr(e)))?;
    // Broadcast to all users including the sender (host). The host's client
    // needs Message::LockRoom to update its local lock state in the UI.
    // The old send_except approach (recent commit) broke this: the host saw
    // the command succeed but the lock icon never changed in their own client.
    room.send(Message::LockRoom { lock }).await;
    Ok(())
}

pub async fn cycle_room(user: Arc<User>, cycle: bool) -> Result<()> {
    let room = current_room(&user).await?;
    if !is_host_for_room(&room, &user).await {
        bail!("{}", tl!("only-host-can-do"));
    }
    info!(
        user = user.id,
        room = room.id.to_string(),
        cycle,
        "cycle room"
    );
    user.server
        .room_commands
        .set_cycle_as(&user.server, &room.id.to_string(), cycle, user.id)
        .await
        .map_err(|e| anyhow!("{}", tr(e)))?;
    // See lock_room comment — the host's client needs the event too.
    room.send(Message::CycleRoom { cycle }).await;
    Ok(())
}

pub async fn select_chart(user: Arc<User>, id: i32) -> Result<()> {
    let room = current_room_in_select_chart(&user).await?;
    if !is_host_for_room(&room, &user).await {
        bail!("{}", tl!("only-host-can-do"));
    }
    let span = debug_span!(
        "select chart",
        user = user.id,
        room = room.id.to_string(),
        chart = id
    );
    async move {
        trace!("fetch");
        // Use live_config endpoint first, falling back to config file.
        let endpoint = resolve_phira_api_endpoint(&user.server).await;
        let chart_name: String = match user
            .server
            .phira_client
            .get_json::<crate::server::Chart>(
                &endpoint,
                None,
                &format!("/chart/{id}"),
                None,
                crate::phira_client::PhiraRetryNoticeTarget::Silent,
            )
            .await
        {
            Ok(chart) => chart.name,
            Err(_) => {
                tracing::warn!("failed to fetch chart {id} from Phira API; using ID as name");
                format!("#{id}")
            }
        };
        debug!("chart name: {chart_name}");
        // Route state mutation through RoomActor mailbox for serialized access.
        user.server
            .room_commands
            .set_chart(&user.server, &room.id.to_string(), id, &chart_name, user.id)
            .await
            .map_err(|e| anyhow!("{}", tr(e)))?;
        Ok(())
    }
    .instrument(span)
    .await
}

pub async fn request_start(user: Arc<User>) -> Result<()> {
    let room = current_room_in_select_chart(&user).await?;
    if !is_host_for_room(&room, &user).await {
        bail!("{}", tl!("only-host-can-do"));
    }
    // Check admin_start_pending via snapshot.
    let control = room.control_snapshot();
    if control.admin_start_pending {
        bail!("{}", tl!("admin-start-in-progress"));
    }
    // Check chart from snapshot.
    let has_chart = if let Some(server) = room.server.upgrade() {
        server.room_snapshot(&room.id.to_string())
            .map(|s| s.chart.is_some())
            .unwrap_or(false)
    } else {
        false
    };
    if !has_chart {
        bail!("{}", tl!("start-no-chart-selected"));
    }
    debug!(room = room.id.to_string(), "room wait for ready");
    // Route through per-room mailbox for serialized state mutation.
    user.server
        .room_commands
        .host_start(&user.server, &room.id.to_string(), user.id)
        .await
        .map_err(|e| anyhow!("{}", tr(e)))?;
    Ok(())
}

pub async fn ready(user: Arc<User>) -> Result<()> {
    let room = current_room(&user).await?;
    user.server
        .room_commands
        .set_ready(&user.server, &room.id.to_string(), user.id)
        .await
        .map_err(|e| anyhow!("{}", tr(e)))?;
    Ok(())
}

pub async fn cancel_ready(user: Arc<User>) -> Result<()> {
    let room = current_room(&user).await?;
    user.server
        .room_commands
        .cancel_ready(&user.server, &room.id.to_string(), user.id)
        .await
        .map_err(|e| anyhow!("{}", tr(e)))?;
    Ok(())
}

pub async fn played(user: Arc<User>, id: i32) -> Result<()> {
    let room = current_room(&user).await?;
    // Use live_config endpoint first, falling back to config file.
    let endpoint = resolve_phira_api_endpoint(&user.server).await;
    let res: crate::server::Record = user
        .server
        .phira_client
        .get_json(
            &endpoint,
            None,
            &format!("/record/{id}"),
            None,
            PhiraRetryNoticeTarget::User(user.as_ref()),
        )
        .await?;
    if res.player != user.id {
        bail!("{}", tl!("invalid-record"));
    }
    debug!(
        room = room.id.to_string(),
        user = user.id,
        "user played: {res:?}"
    );
    // Route state mutation through RoomActor mailbox.
    user.server
        .room_commands
        .submit_result(
            &user.server, &room.id.to_string(), user.id,
            res.score, res.accuracy, res.perfect, res.good,
            res.bad, res.miss, res.max_combo, res.full_combo,
            res.std, res.std_score,
        )
        .await
        .map_err(|e| anyhow!("{}", tr(e)))?;
    Ok(())
}

pub async fn abort(user: Arc<User>) -> Result<()> {
    let room = current_room(&user).await?;
    user.server
        .room_commands
        .abort_round(&user.server, &room.id.to_string(), user.id)
        .await
        .map_err(|e| anyhow!("{}", tr(e)))?;
    Ok(())
}

pub async fn query_room_info(user: Arc<User>) -> Result<ServerCommand> {
    let rooms_guard = user.server.rooms.read().await;
    let mut info: HashMap<phira_mp_common::RoomId, phira_mp_common::RoomData> = HashMap::new();
    let mut user_room_map: HashMap<i32, phira_mp_common::RoomId> = HashMap::new();
    for (id, room) in rooms_guard.iter() {
        for u in room.users().await {
            user_room_map.insert(u.id, id.clone());
        }
        info.insert(id.clone(), build_room_data(room).await);
    }
    drop(rooms_guard);
    Ok(ServerCommand::RoomResponse(Ok((info, user_room_map))))
}

//! Room lifecycle and room gameplay command handlers for client sessions.
//!
//! This module is intentionally kept free of socket/authentication details so
//! `session.rs` can become a thin dispatcher before the real Session Actor split.

use crate::phira_client::PhiraRetryNoticeTarget;
use crate::plugin::PluginEvent;
use crate::session::{SessionCategory, User};
use crate::tl;
use anyhow::{anyhow, bail, Result};
use phira_mp_common::{
    JoinRoomResponse, Message, PartialRoomData, RoomEvent, RoomId, ServerCommand,
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
    if !matches!(
        &*room.state.read().await,
        crate::room::InternalRoomState::SelectChart
    ) {
        bail!("{}", tl!("invalid-state"));
    }
    Ok(room)
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
    let max_users = user.server.config.max_users_per_room.unwrap_or(100);
    let room = Arc::new(crate::room::Room::new(
        id.clone(),
        Arc::downgrade(&user),
        Some(Arc::clone(&user.server.plugin_manager)),
        Arc::downgrade(&user.server),
        max_users,
        Some(Arc::clone(&user.server.round_store)),
    ));
    match map_guard.entry(id.clone()) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(Arc::clone(&room));
        }
        std::collections::hash_map::Entry::Occupied(_) => {
            bail!("{}", tl!("create-id-occupied"));
        }
    }
    let room_uuid = room.uuid;
    room.set_display_name(user.id, user.name.clone()).await;
    room.send(Message::CreateRoom { user: user.id }).await;
    drop(map_guard);
    user.server
        .publish_room_event(RoomEvent::CreateRoom {
            room: id.clone(),
            data: crate::room::Room::into_data(&room).await,
        })
        .await;
    *room_guard = Some(Arc::clone(&room));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    user.server
        .user_room_history
        .write()
        .await
        .entry(user.id)
        .or_default()
        .push((id.to_string(), room_uuid.to_string(), now));
    if let Some(db) = crate::internal_hooks::DB.get() {
        db.record_user_room_history_sync(user.id, id.to_string(), room_uuid.to_string(), now);
    }

    info!(user = user.id, room = id.to_string(), room_uuid = %room_uuid, "user create room");
    info!("房间 '{}' 唯一标识: {}", id, room_uuid);

    user.server
        .plugin_manager
        .trigger(&PluginEvent::RoomCreate {
            user_id: user.id,
            room_id: id.to_string(),
        })
        .await;

    Ok(())
}

pub async fn join_room(
    user: Arc<User>,
    category: SessionCategory,
    id: RoomId,
    monitor: bool,
) -> Result<JoinRoomResponse> {
    let mut room_guard = user.room.write().await;
    if room_guard.is_some() {
        bail!("{}", tl!("already-in-room"));
    }
    let room = user.server.rooms.read().await.get(&id).map(Arc::clone);
    let Some(room) = room else {
        bail!("{}", tl!("room-not-found"))
    };
    if room.locked.load(Ordering::SeqCst) {
        bail!("{}", tl!("join-room-locked"));
    }
    // 检查房间黑名单（按 UUID 绑定，不按房间名）
    if user
        .server
        .ban_manager
        .is_room_banned(&room.uuid.to_string(), user.id)
        .await
    {
        bail!("{}", tl!("join-room-banned"));
    }
    {
        let state = room.state.read().await;
        match &*state {
            crate::room::InternalRoomState::SelectChart => {}
            crate::room::InternalRoomState::Playing { .. } => {
                // 进行中的游戏：第一次请求发警告，第二次直接加入
                let mut pending = user.join_pending_game.write().await;
                if pending.as_ref().map(|s| s.as_str()) == Some(id.to_string().as_str()) {
                    // 用户已确认，放行
                    pending.take();
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
            _ => {
                bail!("{}", tl!("join-game-ongoing"));
            }
        }
    }
    // GameMonitor 会话（user.id < 0）可以旁观任意房间
    if monitor && !user.can_monitor() && user.id > 0 {
        bail!("{}", tl!("join-cant-monitor"));
    }
    if !room.add_user(Arc::downgrade(&user), monitor).await {
        bail!("{}", tl!("join-room-full"));
    }
    info!(
        user = user.id,
        room = id.to_string(),
        monitor,
        "user join room"
    );
    user.monitor.store(monitor, Ordering::SeqCst);
    if monitor && !room.live.fetch_or(true, Ordering::SeqCst) {
        info!(room = id.to_string(), "room goes live");
    }
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
        .user_room_history
        .write()
        .await
        .entry(user.id)
        .or_default()
        .push((id.to_string(), room.uuid.to_string(), joined_at));
    if let Some(db) = crate::internal_hooks::DB.get() {
        db.record_user_room_history_sync(user.id, id.to_string(), room.uuid.to_string(), joined_at);
    }
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
        room.broadcast(join).await;
        room.broadcast(message).await;
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
        let plugin_manager = Arc::clone(&user.server.plugin_manager);
        let room_id = id.to_string();
        let user_id = user.id;
        tokio::spawn(async move {
            plugin_manager
                .trigger(&PluginEvent::RoomJoin {
                    user_id,
                    room_id,
                    is_monitor: monitor,
                })
                .await;
        });
    }

    let mut users = room.users().await;
    if category != SessionCategory::GameMonitor {
        users.extend(room.monitors().await);
    }

    Ok(JoinRoomResponse {
        state: room.client_room_state().await,
        users: users.into_iter().map(|user| user.to_info()).collect(),
        live: room.is_live(),
    })
}

pub async fn leave_room(user: Arc<User>, category: SessionCategory) -> Result<()> {
    user.join_pending_game.write().await.take();
    let room = current_room(&user).await?;
    let room_id = room.id.clone();
    info!(
        user = user.id,
        room = room.id.to_string(),
        "user leave room"
    );
    let was_monitor = user.monitor.load(Ordering::SeqCst);
    if room.on_user_leave(&user).await {
        user.server.rooms.write().await.remove(&room.id);
    }
    if category == SessionCategory::Normal && !was_monitor {
        user.server
            .publish_room_event(RoomEvent::LeaveRoom {
                room: room.id.clone(),
                user: user.id,
            })
            .await;
    }

    if category == SessionCategory::Normal {
        user.server
            .plugin_manager
            .trigger(&PluginEvent::RoomLeave {
                user_id: user.id,
                room_id: room_id.to_string(),
            })
            .await;
    }

    Ok(())
}

pub async fn lock_room(user: Arc<User>, lock: bool) -> Result<()> {
    let room = current_room(&user).await?;
    room.check_host(&user).await?;
    info!(
        user = user.id,
        room = room.id.to_string(),
        lock,
        "lock room"
    );
    room.locked.store(lock, Ordering::SeqCst);
    room.send(Message::LockRoom { lock }).await;
    room.publish_update(PartialRoomData {
        lock: Some(lock),
        ..Default::default()
    })
    .await;

    user.server
        .plugin_manager
        .trigger(&PluginEvent::RoomModify {
            user_id: user.id,
            room_id: room.id.to_string(),
            data: serde_json::json!({"action":"lock", "value": lock}).to_string(),
        })
        .await;

    Ok(())
}

pub async fn cycle_room(user: Arc<User>, cycle: bool) -> Result<()> {
    let room = current_room(&user).await?;
    room.check_host(&user).await?;
    info!(
        user = user.id,
        room = room.id.to_string(),
        cycle,
        "cycle room"
    );
    room.cycle.store(cycle, Ordering::SeqCst);
    room.send(Message::CycleRoom { cycle }).await;
    room.publish_update(PartialRoomData {
        cycle: Some(cycle),
        ..Default::default()
    })
    .await;

    user.server
        .plugin_manager
        .trigger(&PluginEvent::RoomModify {
            user_id: user.id,
            room_id: room.id.to_string(),
            data: serde_json::json!({"action":"cycle", "value": cycle}).to_string(),
        })
        .await;

    Ok(())
}

pub async fn select_chart(user: Arc<User>, id: i32) -> Result<()> {
    let room = current_room_in_select_chart(&user).await?;
    room.check_host(&user).await?;
    let span = debug_span!(
        "select chart",
        user = user.id,
        room = room.id.to_string(),
        chart = id
    );
    async move {
        trace!("fetch");
        let endpoint = room.effective_phira_api_endpoint(&user.server).await;
        let res: crate::server::Chart = user
            .server
            .phira_client
            .get_json(
                &user.server.config.phira_api_endpoint,
                Some(endpoint.as_str()),
                &format!("/chart/{id}"),
                None,
                PhiraRetryNoticeTarget::User(user.as_ref()),
            )
            .await?;
        debug!("chart is {res:?}");
        room.send(Message::SelectChart {
            user: user.id,
            name: res.name.clone(),
            id: res.id,
        })
        .await;
        *room.chart.write().await = Some(res);
        room.on_state_change().await;
        room.publish_update(PartialRoomData {
            chart: Some(id),
            ..Default::default()
        })
        .await;
        Ok(())
    }
    .instrument(span)
    .await
}

pub async fn request_start(user: Arc<User>) -> Result<()> {
    let room = current_room_in_select_chart(&user).await?;
    room.check_host(&user).await?;
    if room.admin_start_pending() {
        bail!("administrative start is already in progress");
    }
    if room.chart.read().await.is_none() {
        bail!("{}", tl!("start-no-chart-selected"));
    }
    debug!(room = room.id.to_string(), "room wait for ready");
    room.finish_admin_start().await;
    room.reset_game_time().await;
    room.send(Message::GameStart { user: user.id }).await;
    *room.state.write().await = crate::room::InternalRoomState::WaitForReady {
        started: std::iter::once(user.id).collect(),
        admin_started: false,
    };
    room.on_state_change().await;
    room.check_all_ready().await;

    user.server
        .plugin_manager
        .trigger(&PluginEvent::GameStart {
            user_id: user.id,
            room_id: room.id.to_string(),
        })
        .await;

    Ok(())
}

pub async fn ready(user: Arc<User>) -> Result<()> {
    let room = current_room(&user).await?;
    let mut guard = room.state.write().await;
    if let crate::room::InternalRoomState::WaitForReady { started, .. } = &mut *guard {
        if !started.insert(user.id) {
            bail!("{}", tl!("already-ready"));
        }
        room.send(Message::Ready { user: user.id }).await;
        user.server
            .publish_runtime_event(crate::event_bus::MpEvent::PlayerReadyChanged {
                room_id: room.id.clone(),
                user_id: user.id,
                ready: true,
            });
        drop(guard);
        room.check_all_ready().await;
    }
    Ok(())
}

pub async fn cancel_ready(user: Arc<User>) -> Result<()> {
    let room = current_room(&user).await?;
    let mut guard = room.state.write().await;
    if let crate::room::InternalRoomState::WaitForReady { started, .. } = &mut *guard {
        if !started.remove(&user.id) {
            bail!("{}", tl!("not-ready"));
        }
        if room.check_host(&user).await.is_ok() {
            room.send(Message::CancelGame { user: user.id }).await;
            *guard = crate::room::InternalRoomState::SelectChart;
            drop(guard);
            room.finish_admin_start().await;
            room.on_state_change().await;
        } else {
            room.send(Message::CancelReady { user: user.id }).await;
        }
        user.server
            .publish_runtime_event(crate::event_bus::MpEvent::PlayerReadyChanged {
                room_id: room.id.clone(),
                user_id: user.id,
                ready: false,
            });
    }
    Ok(())
}

pub async fn played(user: Arc<User>, id: i32) -> Result<()> {
    let room = current_room(&user).await?;
    let endpoint = room.effective_phira_api_endpoint(&user.server).await;
    let res: crate::server::Record = user
        .server
        .phira_client
        .get_json(
            &user.server.config.phira_api_endpoint,
            Some(endpoint.as_str()),
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
    room.send(Message::Played {
        user: user.id,
        score: res.score,
        accuracy: res.accuracy,
        full_combo: res.full_combo,
    })
    .await;
    let score = res.score;
    let accuracy = res.accuracy;
    let perfect = res.perfect;
    let good = res.good;
    let bad = res.bad;
    let miss = res.miss;
    let max_combo = res.max_combo;
    let full_combo = res.full_combo;
    let mut guard = room.state.write().await;
    if let crate::room::InternalRoomState::Playing { results, aborted } = &mut *guard {
        if aborted.contains(&user.id) {
            bail!("aborted");
        }
        if results.insert(user.id, res).is_some() {
            bail!("{}", tl!("already-uploaded"));
        }
        drop(guard);
        room.check_all_ready().await;

        user.server
            .plugin_manager
            .trigger(&PluginEvent::GameEnd {
                user_id: user.id,
                user_name: user.name.clone(),
                room_id: room.id.to_string(),
                score,
                accuracy,
                perfect,
                good,
                bad,
                miss,
                max_combo,
                full_combo,
            })
            .await;
    }
    Ok(())
}

pub async fn abort(user: Arc<User>) -> Result<()> {
    let room = current_room(&user).await?;
    let mut guard = room.state.write().await;
    if let crate::room::InternalRoomState::Playing { results, aborted } = &mut *guard {
        if results.contains_key(&user.id) {
            bail!("{}", tl!("already-uploaded"));
        }
        if !aborted.insert(user.id) {
            bail!("{}", tl!("aborted"));
        }
        drop(guard);
        room.send(Message::Abort { user: user.id }).await;
        room.check_all_ready().await;
    }
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
        info.insert(id.clone(), crate::room::Room::into_data(room).await);
    }
    drop(rooms_guard);
    Ok(ServerCommand::RoomResponse(Ok((info, user_room_map))))
}

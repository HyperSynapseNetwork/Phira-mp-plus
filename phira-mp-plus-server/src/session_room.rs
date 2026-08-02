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
use crate::session::{CommandOrigin, SessionCategory, User};
use crate::session_auth::resolve_phira_api_endpoint;
use crate::tl;
use anyhow::{anyhow, bail, Result};

/// Translate known English error strings from room command results.
/// Falls back to the English string if no LANGUAGE scope is set.
fn tr(e: String) -> String {
    let lang = crate::l10n::current_language();
    let id = match e.as_str() {
        "already ready" => Some("already-ready"),
        "already uploaded" => Some("already-uploaded"),
        "not ready" => Some("not-ready"),
        "user aborted" => Some("aborted"),
        "no chart selected" => Some("start-no-chart-selected"),
        "room is full" => Some("join-room-full"),
        "administrative start is already in progress" => Some("admin-start-in-progress"),
        "room is not selecting a chart" | "cannot set chart outside SelectChart state" => Some("invalid-state"),
        "not in WaitForReady state" => Some("invalid-state"),
        "not in Playing state" => Some("invalid-state"),
        _ => None,
    };
    match id {
        Some(id) => crate::l10n::try_translate(&lang.0, id),
        None => e,
    }
}
use phira_mp_common::{
    JoinRoomResponse, Message, RoomEvent, RoomId, ServerCommand,
};
use std::{
    collections::HashMap,
    sync::{atomic::Ordering, Arc},
    time::Instant,
};
use tracing::{debug, debug_span, info, trace, warn, Instrument};

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

pub async fn create_room(
    user: Arc<User>,
    id: RoomId,
    deadline: Instant,
    origin: &CommandOrigin,
) -> Result<()> {
    // PMP44 P0-J: 绝对预算已耗尽时拒绝创建——不得在客户端已超时之后
    // 拿到 user.room 写锁或写入 rooms 注册表。
    if crate::official_client_compat::timing::deadline_expired(deadline) {
        crate::official_client_compat::protocol_trace::ProtocolTrace::get()
            .late_commit
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        bail!("create room timed out");
    }
    let id_text = id.to_string();
    if let Some(command) = id_text.strip_prefix('_') {
        if user.server.is_admin_id(user.id).await {
            let command = decode_admin_room_command(command);
            let command = {
                let mut pending = user.admin_cli_pending.lock().await;
                match crate::cli::collect_cli_continuation(&mut *pending, command) {
                    Ok(Some(command)) => command,
                    Ok(None) => {
                        user.try_send(
                            ServerCommand::Message(Message::Chat {
                                user: 0,
                                content: "[CLI] 已暂存续行；下一条命令需以 -- 开头".to_string(),
                            }),
                            None,
                        )
                        .await;
                        bail!("admin CLI command pending");
                    }
                    Err(err) => {
                        user.try_send(
                            ServerCommand::Message(Message::Chat {
                                user: 0,
                                content: format!("[CLI] {err}"),
                            }),
                            None,
                        )
                        .await;
                        bail!("admin CLI continuation error");
                    }
                }
            };
            if command.is_empty() {
                user.try_send(
                    ServerCommand::Message(Message::Chat {
                        user: 0,
                        content: "[CLI] 空命令".to_string(),
                    }),
                    None,
                )
                .await;
                bail!("empty admin command");
            }
            let lines =
                crate::cli::execute_cli_once(Arc::clone(&user.server), command.clone()).await;
            user.try_send(
                ServerCommand::Message(Message::Chat {
                    user: 0,
                    content: format!("[CLI] > {command}"),
                }),
                None,
            )
            .await;
            for line in lines {
                user.try_send(
                    ServerCommand::Message(Message::Chat {
                        user: 0,
                        content: format!("[CLI] {line}"),
                    }),
                    None,
                )
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
    // Host is set at actor init from creator_id (player-created rooms) or via
    // SetHost (admin). Server-created empty rooms keep host_id = None and
    // report host -1 (system host); joining an empty room never makes the
    // joiner host.
    // CreateRoom(Ok) establishes client room state; do not emit a room event to
    // the creator before that response.
    drop(room_guard);

    // P0-H: the official CreateRoom(Ok) + Message::CreateRoom sequence must not
    // wait on PMP extension work. Room-event persistence, room history, plugin
    // events, runtime telemetry and mailbox pre-creation are all moved to a
    // response-after task bound to the create origin. The authoritative commit
    // (Room::new + registry insert + user.room) above stays synchronous because
    // the response and subsequent client commands depend on it.
    let origin = origin.clone();
    let server = Arc::clone(&user.server);
    let room_id = id.clone();
    let room_arc = Arc::clone(&room);
    let uid = user.id;
    crate::supervisor_actor::spawn_named(format!("create-room-post-{uid}"), async move {
        if !origin.is_current().await {
            tracing::debug!(
                user = uid,
                "post-create extension work runs for a superseded session"
            );
        }
        server
            .publish_room_event(RoomEvent::CreateRoom {
                room: room_id.clone(),
                data: build_room_data(&room_arc).await,
            })
            .await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        server
            .record_user_room_history(uid, room_id.to_string(), room_uuid.to_string(), now)
            .await;
        tracing::info!(user = uid, room = room_id.to_string(), room_uuid = %room_uuid, "user create room");
        tracing::info!("房间 '{}' 唯一标识: {}", room_id, room_uuid);
        server
            .dispatch_plugin_event(PluginEvent::RoomCreate {
                user_id: uid,
                room_id: room_id.to_string(),
            })
            .await;
        server
            .publish_runtime_event(crate::event_bus::MpEvent::RoomCreated {
                room_id: room_id.clone(),
                room_uuid,
            });
        // Pre-create the mailbox so the first join doesn't pay creation latency.
        let _ = server
            .room_commands
            .set_live(&server, &room_id.to_string(), true)
            .await;
    });

    Ok(())
}

/// PMP45 P0-K: Join 补偿守护——future 被取消（run_or_deadline 超时）时在 Drop
/// 中派发补偿 remove_user，杜绝 Ghost member（audit §19）。在连接映射成功
/// 后 disarm，避免双重补偿。
///
/// async Drop 不可用，因此 Drop 体通过 `supervisor_actor::spawn_named` 派发一个
/// best-effort 任务执行补偿；若补偿也失败，房间置 degraded，阻塞后续 Join 直到
/// 操作员 / 未来 reconcile 清空。
struct JoinCompensationGuard {
    server: Arc<crate::server::PlusServerState>,
    room_id: String,
    user_id: i32,
    /// 补偿 remove_user 携带的 room-actor origin token。保留原始 origin 使补偿
    /// 与 join 同源：若会话仍绑定则该补偿通过 P0-C stale 检查；若会话已被
    /// reconnect 取代则被拒绝 → 房间置 degraded（安全兜底）。
    room_origin: crate::room_actor::command::RoomOrigin,
    /// Actor AddUser 已提交成员——只有从此刻起的取消才需要补偿。
    committed: bool,
    /// 连接映射成功（或补偿已同步完成）——撤销 Drop 补偿，避免双重补偿。
    disarmed: bool,
}

impl JoinCompensationGuard {
    fn new(
        server: Arc<crate::server::PlusServerState>,
        room_id: String,
        user_id: i32,
        room_origin: crate::room_actor::command::RoomOrigin,
    ) -> Self {
        Self {
            server,
            room_id,
            user_id,
            room_origin,
            committed: false,
            disarmed: false,
        }
    }

    /// Actor AddUser 已提交成员——从现在起，任何提前取消都必须补偿。
    fn mark_committed(&mut self) {
        self.committed = true;
    }

    /// 连接映射成功（或补偿已同步完成）——撤销 Drop 补偿，避免双重补偿。
    fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for JoinCompensationGuard {
    fn drop(&mut self) {
        if !self.committed || self.disarmed {
            return;
        }
        let server = Arc::clone(&self.server);
        let room_id = self.room_id.clone();
        let user_id = self.user_id;
        let room_origin = self.room_origin;
        warn!(
            user = user_id,
            room = %room_id,
            "join_room cancelled mid-flight; spawning compensating remove_user"
        );
        crate::supervisor_actor::spawn_named(
            format!("join-compensate-{user_id}-{room_id}"),
            async move {
                // PMP45 P0-K: 补偿使用「内部清理 deadline」（200ms）而非命令原始
                // deadline——补偿必须在 handler 内完整跑完，绝不能被响应预算或外层
                // run_or_deadline 超时取消（取消会留下 Ghost member，audit §16.2）。
                let cleanup_deadline =
                    Instant::now() + std::time::Duration::from_millis(200);
                let compensation = server
                    .room_commands
                    .remove_user(
                        &server,
                        &room_id,
                        user_id,
                        Some(cleanup_deadline),
                        room_origin,
                    )
                    .await;
                if compensation.is_err() {
                    // 补偿也失败：Ghost member 遗留——房间置 degraded，阻塞后续
                    // Join，直到操作员 / 未来 reconcile 清空。
                    warn!(
                        user = user_id,
                        room = %room_id,
                        "drop-guard compensating remove_user failed; marking room degraded"
                    );
                    let _ = server
                        .room_commands
                        .set_degraded(&server, &room_id, true)
                        .await;
                }
            },
        );
    }
}

pub async fn join_room(
    user: Arc<User>,
    category: SessionCategory,
    id: RoomId,
    monitor: bool,
    deadline: Instant,
    response_deadline: Instant,
    received_at: Instant,
    origin: &CommandOrigin,
) -> Result<()> {
    // PMP45 P0-I: `deadline` 是 COMMIT deadline（= response deadline 减去
    // `commit_response_reserve_ms`）——所有权威状态提交（add_user 等）必须在它
    // 之前完成；`response_deadline` 是 RESPONSE deadline，供最低响应时延等待与
    // JoinRoom(Ok) flush 使用（响应预算独立于提交预算，audit §17）。
    // P0-E: a late Join must never mutate authoritative room state. Every
    // pre-check step re-tests the commit deadline; an expired deadline bails
    // with a deterministic error response (no commit, no connection close).
    macro_rules! check_deadline {
        () => {
            if crate::official_client_compat::timing::deadline_expired(deadline) {
                bail!("join request timed out");
            }
        };
    }
    check_deadline!();

    let mut room_guard = user.room.write().await;
    if room_guard.is_some() {
        bail!("{}", tl!("already-in-room"));
    }
    check_deadline!();
    let room = user.server.rooms.read().await.get(&id).map(Arc::clone);
    let Some(room) = room else {
        bail!("{}", tl!("room-not-found"))
    };
    check_deadline!();
    // Compute effective_monitor from category to prevent Normal/Console sessions
    // from bypassing lock/ban/game-state gates by sending monitor=true.
    let effective_monitor = match category {
        SessionCategory::RoomMonitor | SessionCategory::GameMonitor => true,
        SessionCategory::Normal | SessionCategory::Console => false,
    };
    if monitor && !effective_monitor {
        bail!("monitor access requires dedicated monitor authentication");
    }
    check_deadline!();

    // Monitors bypass player-only lock/ban/game-state gates.
    // No whitelist check — authentication at connection time is sufficient.
    let mut late_join = false;
    let mut need_abort = false;
    if !effective_monitor {
        // Use control_snapshot for lock check (actor-authoritative)
        let control = room.control_snapshot();
        if control.locked {
            bail!("{}", tl!("join-room-locked"));
        }
        check_deadline!();
        if user
            .server
            .ban_manager
            .is_room_banned(&room.uuid.to_string(), user.id)
            .await
        {
            bail!("{}", tl!("join-room-banned"));
        }
        check_deadline!();
        // Read room lifecycle from actor snapshot for game state check.
        let stripped = if let Some(server) = room.server.upgrade() {
            server.room_snapshot(&room.id.to_string())
                .map(|s| s.stripped)
        } else {
            None
        };
        match stripped {
            Some(phira_mp_common::StrippedRoomState::SelectingChart) | None => {}
            Some(phira_mp_common::StrippedRoomState::WaitingForReady) => {
                // ProtocolHack: 断线重连进 WaitForReady 时客户端不知道谱面 ID，
                // 先以 SelectChart 响应让客户端拿到谱面，再异步切回 WaitForReady。
                // 这样客户端能正常显示谱面信息，同时知道需要准备。
                if category != SessionCategory::Normal {
                    bail!("{}", tl!("join-game-ongoing"));
                }
            }
            Some(phira_mp_common::StrippedRoomState::Playing) => {
                let mut pending = user.join_pending_game.write().await;
                if pending.as_ref().map(|s| s.as_str()) == Some(id.to_string().as_str()) {
                    pending.take();
                    late_join = true;
                    need_abort = true;
                } else {
                    *pending = Some(id.to_string());
                    let _ = origin
                        .try_send(ServerCommand::Message(Message::Chat {
                            user: 0,
                            content: tl!("join-game-ongoing-warning"),
                        }))
                        .await;
                    bail!("{}", tl!("join-game-ongoing"));
                }
            }
        }
        check_deadline!();
        if need_abort {
            // Route the abort through the actor mailbox (best-effort; the abort
            // of a stale game must not block the join or outlive its deadline).
            user.server
                .room_commands
                .abort_round(
                    &user.server,
                    &room.id.to_string(),
                    user.id,
                    Some(deadline),
                    origin.to_room_origin(),
                )
                .await
                .ok();
        }
    }
    check_deadline!();

    // P0-E: verify the origin is still current before the AddUser commit point.
    // A superseded session (reconnect bumped the generation) must never mutate
    // authoritative room state — bail with a deterministic error.
    if !origin.is_current().await {
        bail!("stale session origin; refusing to join");
    }
    check_deadline!();

    // P0-E: room-full pre-check BEFORE the actor AddUser so a full room bails
    // deterministically instead of running actor AddUser + registry rollback.
    if !effective_monitor {
        let max_users = room.control_snapshot().max_users;
        if room.users().await.len() >= max_users {
            bail!("{}", tl!("join-room-full"));
        }
    }
    check_deadline!();

    // PMP45 P0-K: Join 补偿守护——arm 于 Actor AddUser 提交之前。若
    // `run_or_deadline` 在 AddUser 提交后、连接映射完成前取消本 future，Drop 会
    // 派发补偿 remove_user（audit §19：AddUser 与连接映射是两个操作，取消发生在
    // 两者之间时原有补偿代码不运行，会留下 Ghost member）。连接映射成功后 disarm。
    let mut join_guard = JoinCompensationGuard::new(
        Arc::clone(&user.server),
        id.to_string(),
        user.id,
        origin.to_room_origin(),
    );

    // Route user/monitor add through mailbox for actor_state.members tracking.
    // This is the authoritative source — Room.users is derived for broadcast only.
    user.server
        .room_commands
        .add_user(
            &user.server,
            &id.to_string(),
            user.id,
            &user.name,
            monitor,
            deadline,
            origin.to_room_origin(),
        )
        .await
        .map_err(|e| anyhow!("{}", tr(e)))?;
    // Actor AddUser 已提交成员——从现在起，任何提前取消都必须触发 Drop 补偿。
    join_guard.mark_committed();
    // NOTE: after the actor AddUser commits, `deadline`（commit）不再是预检查。
    // 后续 flush 使用 `response_deadline`（PMP45 P0-I）的 remaining-budget
    // 超时 → close_uncertain + bail（P0-D uncertain-after-commit），绝不普通
    // bail——那会让用户已提交而客户端被误导。

    // Also add to Room connection mapping (immediate, direct).
    if !room.add_user(Arc::downgrade(&user), monitor).await {
        // PMP44 P0-L: Actor AddUser 已提交成员但连接注册表拒绝该用户——在官方
        // Join 广播（OnJoinRoom/Message::JoinRoom）发出前执行补偿，撤销 Actor
        // 成员，避免 Ghost member（audit §16：actor 有成员但 user.room 为空、
        // 注册表为空）。
        warn!(
            user = user.id,
            room = %id,
            "room.add_user failed after actor AddUser; compensating actor remove_user"
        );
        // PMP45 P0-J/P0-14（audit §16.2/§18）：补偿使用「内部清理 deadline」
        // （`Instant::now() + 200ms`），而不是命令的原始 deadline——补偿必须
        // 在 handler 内完整跑完，绝不能被响应预算或外层 `run_or_deadline` 超时
        // 取消（取消会留下 Ghost member）。200ms 远小于 response budget
        //（默认 1000ms），处于安全范围内。
        let cleanup_deadline =
            Instant::now() + std::time::Duration::from_millis(200);
        let compensation = user
            .server
            .room_commands
            .remove_user(
                &user.server,
                &id.to_string(),
                user.id,
                Some(cleanup_deadline),
                origin.to_room_origin(),
            )
            .await;
        if compensation.is_err() {
            // PMP45 P0-K: 补偿也失败——Ghost member 遗留，房间进入 degraded，
            // 不再接受新的 Join，直到操作员 / 未来 reconcile 清空。结果不确定
            //（actor 成员可能仍在）——关闭 origin 传输，走 lost-connection 路径，
            // 客户端 reconnect Authenticate 恢复权威状态。
            warn!(
                user = user.id,
                room = %id,
                "compensating remove_user also failed; marking room degraded and closing origin transport"
            );
            let _ = user
                .server
                .room_commands
                .set_degraded(&user.server, &id.to_string(), true)
                .await;
            join_guard.disarm();
            origin.close_uncertain().await;
            bail!("failed to register user connection");
        }
        // 补偿成功：结果确定（未提交成员），撤销 Drop 补偿，发送错误给客户端，
        // 客户端可重试 Join。
        join_guard.disarm();
        bail!("failed to register user connection");
    }
    // 连接映射成功——actor 成员与连接注册表齐备，撤销 Drop 补偿。
    join_guard.disarm();

    info!(
        user = user.id,
        room = id.to_string(),
        monitor,
        "user join room"
    );
    user.monitor.store(monitor, Ordering::SeqCst);

    // P0-I: the actor AddUser promotes the first non-monitor joiner to host
    // WITHOUT broadcasting ChangeHost(true) (§15). Detect the promotion here so
    // the ChangeHost(true) packet can be deferred to the post-response compat
    // queue — never delivered before JoinRoom(Ok) and never to a new session.
    let became_host = user
        .server
        .assign_room_host_if_missing(&room, &user, monitor, false)
        .await;

    *room_guard = Some(Arc::clone(&room));
    // 清除进行中游戏加入确认标记
    user.join_pending_game.write().await.take();
    drop(room_guard);

    let join = ServerCommand::OnJoinRoom(user.to_info());
    let message = ServerCommand::Message(Message::JoinRoom {
        user: user.id,
        name: user.name.clone(),
    });
    // P0-D: official phira-mp broadcasts OnJoinRoom then Message::JoinRoom to ALL
    // room members (including the joiner) BEFORE returning JoinRoom(Ok). The
    // joiner's client updates its roster from these packets and the response
    // carries the full snapshot, but the packet ORDER must match the official
    // server. broadcast_except would drop these two packets for the joiner.
    room.broadcast(join).await;
    room.broadcast(message).await;

    let mut users = room.users().await;
    if category != SessionCategory::GameMonitor {
        users.extend(room.monitors().await);
    }

    // ProtocolHack: 断线重连时，如果房间在 WaitForReady，先以 SelectChart
    // 响应让客户端拿到谱面 ID，再异步切回 WaitingForReady（Phira 客户端在
    // WaitingForReady 状态下不直接包含谱面 ID）。
    let (room_state, deferred_wfr) = if late_join {
        let chart = if let Some(server) = room.server.upgrade() {
            server.room_snapshot(&room.id.to_string())
                .and_then(|s| s.chart)
        } else {
            None
        };
        (phira_mp_common::RoomState::SelectChart(chart), false)
    } else {
        let client_state = build_client_room_state(&room, &user).await;
        let is_waiting = matches!(client_state.state, phira_mp_common::RoomState::WaitingForReady);
        (client_state.state, is_waiting)
    };

    // 先发送 JoinRoom(Ok) 响应，确保客户端先拿到完整快照。
    let response = JoinRoomResponse {
        state: room_state,
        users: users.into_iter().map(|user| user.to_info()).collect(),
        live: room.is_live(),
    };
    // P0-F: JoinRoom(Ok) must not arrive before the minimum response latency
    // window (the official client installs its callback after send).
    // PMP45 P0-I: 最低响应时延等待与 JoinRoom(Ok) flush 共用 RESPONSE deadline
    //（`response_deadline`，完整预算）——权威提交已用 `deadline`（commit），
    // 响应预算独立保留给本 flush，避免「服务端已提交、客户端已超时」（audit §17）。
    crate::official_client_compat::timing::CompatTiming::from_config(&user.server.config)
        .wait_until_minimum_bounded(received_at, Some(response_deadline))
        .await;

    // P0-E/P0-C: the JoinRoom(Ok) flush uses the RESPONSE deadline — remaining
    // budget only, never an independent 5s constant (§9). The flush is bound to
    // the ORIGIN session, so after a reconnect it can never reach (or close)
    // the new session.
    let remaining = response_deadline.saturating_duration_since(Instant::now());
    let flushed = match tokio::time::timeout(
        remaining,
        origin.send_and_flush(ServerCommand::JoinRoom(Ok(response))),
    )
    .await
    {
        Ok(Ok(())) => {
            let trace = crate::official_client_compat::protocol_trace::ProtocolTrace::get();
            trace.response_queued.fetch_add(1, Ordering::Relaxed);
            trace.response_flushed.fetch_add(1, Ordering::Relaxed);
            trace.record_response_latency(received_at);
            true
        }
        _ => {
            // P0-D/P0-E: after the official join sequence was already broadcast,
            // a flush failure or timeout is UNCERTAIN. Never run an incomplete
            // transaction rollback (remove_user would broadcast Leave to a
            // client that may already have installed the room). Close the origin
            // transport and bail; the reconnect Authenticate restores state.
            warn!(
                user = user.id,
                room = %id,
                "JoinRoom send_and_flush failed within deadline; closing origin transport"
            );
            origin.close_uncertain().await;
            bail!("failed to deliver JoinRoom response to client");
        }
    };
    let _ = flushed;

    // ── After the official response is proven flushed ────────────────────
    // P0-I/P0-H: compensations and extension work are response-after. Same
    // origin compensations are emitted in fixed PostResponseKind order
    // (ChangeHost before ChangeState) by the compat queue.
    let mut compensations = Vec::new();
    if became_host {
        compensations.push(
            crate::official_client_compat::post_response::PostResponseItem::to_origin(
                origin.clone(),
                crate::official_client_compat::post_response::PostResponseKind::ChangeHost,
                ServerCommand::ChangeHost(true),
                "first-user-host",
            ),
        );
    }
    if deferred_wfr {
        // ProtocolHack (P1): 客户端刚收到 SelectChart 快照，需在官方响应 flush
        // 之后发送 GameStart 让客户端切换到 WaitingForReady 并显示准备按钮。
        compensations.push(
            crate::official_client_compat::post_response::PostResponseItem::to_origin(
                origin.clone(),
                crate::official_client_compat::post_response::PostResponseKind::ChangeState,
                ServerCommand::Message(Message::GameStart { user: 0 }),
                "join-reconnect-wait-for-ready",
            ),
        );
    }
    if !compensations.is_empty() {
        crate::official_client_compat::post_response::schedule_post_response(
            &user.server.config,
            compensations,
        );
    }

    // 再发送聊天历史（仅 Chat 消息），让客户端在完整快照后接收增量消息。
    // Delivery is bound to the join's origin, never the user's current session.
    {
        let history = room.chat_history.read().await;
        for msg in history.iter() {
            if let Message::Chat { user: chat_user, content } = msg {
                let _ = origin
                    .try_send(ServerCommand::Message(Message::Chat {
                        user: *chat_user,
                        content: content.clone(),
                    }))
                    .await;
            }
        }
    }

    // P0-H: room event / plugin / runtime telemetry / history / metadata / live /
    // display-name are extension work — never block the JoinRoom(Ok) flush or
    // the actor task. All of it runs in a response-after task.
    let server = Arc::clone(&user.server);
    let room_id = id.clone();
    let room_arc = Arc::clone(&room);
    let uid = user.id;
    let uname = user.name.clone();
    let is_normal = category == SessionCategory::Normal;
    let joined_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    crate::supervisor_actor::spawn_named(format!("join-post-{uid}"), async move {
        if !monitor {
            server
                .publish_room_event(RoomEvent::JoinRoom {
                    room: room_id.clone(),
                    user: uid,
                })
                .await;
        }
        // Protocol-only game monitors are not exposed as players to plugins.
        if is_normal {
            server
                .dispatch_plugin_event(PluginEvent::RoomJoin {
                    user_id: uid,
                    room_id: room_id.to_string(),
                    is_monitor: monitor,
                })
                .await;
            server
                .publish_runtime_event(crate::event_bus::MpEvent::RoomJoined {
                    room_id: room_id.clone(),
                    user_id: uid,
                });
        }
        server
            .record_user_room_history(uid, room_id.to_string(), room_arc.uuid.to_string(), joined_at)
            .await;
        server.refresh_room_display_metadata_background(&room_arc);
        // Route SetLive(true) and set_display_name through mailbox — fire-and-forget.
        let _ = server.room_commands.set_live(&server, &room_id.to_string(), true).await;
        let _ = server.room_commands.set_display_name(&server, &room_id.to_string(), uid, &uname).await;
    });

    Ok(())
}

pub async fn leave_room(
    user: Arc<User>,
    category: SessionCategory,
    deadline: Instant,
    origin: &CommandOrigin,
) -> Result<()> {
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
        .remove_user(
            &user.server,
            &room.id.to_string(),
            user.id,
            Some(deadline),
            origin.to_room_origin(),
        )
        .await;
    let room_dropped = result.as_ref().ok()
        .and_then(|v| v.get("room_dropped"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    // 审计 P3/PMP25 P4: Actor RemoveUser 失败时返回 Err，不再走 direct fallback。
    if result.is_err() {
        let err_msg = format!("Actor RemoveUser failed for user {}", user.id);
        warn!("{}", err_msg);
        return Err(anyhow::anyhow!(err_msg));
    }
    // Only attempt host reassignment when the room actually has a real host to
    // transfer. Empty rooms that keep the system host (`host_id = None`) must
    // not get a random host assigned when a member leaves — the host stays -1.
    if !room_dropped && !was_monitor && room.control_snapshot().host_id.is_some() {
        // Reassign host to a random remaining user if host leaves.
        let remaining = room.users().await;
        if !remaining.is_empty() {
            let idx = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as usize) % remaining.len();
            if let Some(next) = remaining.get(idx).cloned() {
                // announce=true: reassigning the host after a leave is NOT the
                // join-first-host path, so set_host broadcasts the ChangeHost
                // packet immediately (there is no pending JoinRoom(Ok) here).
                user.server.assign_room_host_if_missing(&room, &next, false, true).await;
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

pub async fn lock_room(
    user: Arc<User>,
    lock: bool,
    deadline: Instant,
    origin: &CommandOrigin,
) -> Result<()> {
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
        .set_lock_as(
            &user.server,
            &room.id.to_string(),
            lock,
            user.id,
            Some(deadline),
            origin.to_room_origin(),
        )
        .await
        .map_err(|e| anyhow!("{}", tr(e)))?;
    // Broadcast to all users including the sender (host): the host's client
    // needs Message::LockRoom to update its local lock state in the UI.
    room.send(Message::LockRoom { lock }).await;
    Ok(())
}

pub async fn cycle_room(
    user: Arc<User>,
    cycle: bool,
    deadline: Instant,
    origin: &CommandOrigin,
) -> Result<()> {
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
        .set_cycle_as(
            &user.server,
            &room.id.to_string(),
            cycle,
            user.id,
            Some(deadline),
            origin.to_room_origin(),
        )
        .await
        .map_err(|e| anyhow!("{}", tr(e)))?;
    // See lock_room comment — the host's client needs the event too.
    room.send(Message::CycleRoom { cycle }).await;
    Ok(())
}

pub async fn select_chart(
    user: Arc<User>,
    id: i32,
    deadline: Instant,
    origin: &CommandOrigin,
) -> Result<()> {
    let room = current_room_in_select_chart(&user).await?;
    if !is_host_for_room(&room, &user).await {
        bail!("{}", tl!("only-host-can-do"));
    }
    if crate::official_client_compat::timing::deadline_expired(deadline) {
        bail!("select chart timed out");
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
        // P0-H: the external Phira API fetch is bounded by the command's
        // remaining absolute deadline — a slow/blocked API must never let a
        // SelectChart commit after the client already timed out.
        let fetch_budget = deadline.saturating_duration_since(Instant::now());
        let (chart_name, chart_meta): (String, Option<(String, String, String, Option<f32>, Option<String>)>) =
            match tokio::time::timeout(
                fetch_budget,
                user.server.phira_client.get_json::<crate::server::Chart>(
                    &endpoint,
                    None,
                    &format!("/chart/{id}"),
                    None,
                    crate::phira_client::PhiraRetryNoticeTarget::Silent,
                    None,
                ),
            )
            .await
            {
                Ok(Ok(chart)) => (
                    chart.name,
                    Some((
                        chart.charter,
                        chart.composer,
                        chart.level,
                        chart.rating,
                        chart.chart_updated,
                    )),
                ),
                Ok(Err(_)) => {
                    tracing::warn!("failed to fetch chart {id} from Phira API; using ID as name");
                    (format!("#{id}"), None)
                }
                Err(_) => {
                    // Deadline exhausted before the API returned — the client has
                    // already timed out. Do not commit the chart.
                    bail!("select chart timed out fetching chart metadata");
                }
            };
        debug!("chart name: {chart_name}");

        // 异步获取谱面时长（只下 info.txt，不拖整个 zip）
        if !user.server.chart_duration_cache.read().await.contains_key(&id) {
            // 先从 API 拿 chart 元数据（含 file 下载链接）
            let file_url = user.server.phira_client
                .fetch_chart_by_id(&endpoint, id)
                .await
                .and_then(|c| c.file);
            if let Some(url) = file_url {
                let state = Arc::clone(&user.server);
                let cid = id;
                tokio::spawn(async move {
                    if let Some(duration) = state.phira_client.fetch_chart_duration(&url).await {
                        state.chart_duration_cache.write().await.insert(cid, duration);
                        debug!(chart = cid, duration, "chart duration cached");
                    }
                });
            }
        }

        // P0-C: refuse a late commit — the client has already timed out.
        if crate::official_client_compat::timing::deadline_expired(deadline) {
            bail!("select chart timed out");
        }
        // Route state mutation through RoomActor mailbox for serialized access.
        user.server
            .room_commands
            .set_chart(
                &user.server,
                &room.id.to_string(),
                id,
                &chart_name,
                user.id,
                Some(deadline),
                origin.to_room_origin(),
            )
            .await
            .map_err(|e| anyhow!("{}", tr(e)))?;
        // 广播谱面信息（谱师/曲师/难度/评分）
        if let Some((charter, composer, level, rating, chart_updated)) = chart_meta {
            if !charter.is_empty() || !composer.is_empty() {
                let rating_part = rating.map(|r| format!("    评分: {r:.3}")).unwrap_or_default();
                let updated_part = chart_updated
                    .as_deref()
                    .map(|s| format!("    谱面更新: {}", &s[..s.len().min(10)]))
                    .unwrap_or_default();
                let content = format!(
                    "谱师:{}    曲师:{}    难度: {}{}{}",
                    charter, composer, level, rating_part, updated_part
                );
                room.broadcast(ServerCommand::Message(Message::Chat { user: 0, content }))
                    .await;
            }
        }
        Ok(())
    }
    .instrument(span)
    .await
}

pub async fn request_start(
    user: Arc<User>,
    deadline: Instant,
    origin: &CommandOrigin,
) -> Result<()> {
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
        .host_start(
            &user.server,
            &room.id.to_string(),
            user.id,
            deadline,
            origin.to_room_origin(),
        )
        .await
        .map_err(|e| anyhow!("{}", tr(e)))?;
    Ok(())
}

pub async fn ready(user: Arc<User>, deadline: Instant, origin: &CommandOrigin) -> Result<()> {
    let room = current_room(&user).await?;
    user.server
        .room_commands
        .set_ready(
            &user.server,
            &room.id.to_string(),
            user.id,
            deadline,
            origin.to_room_origin(),
        )
        .await
        .map_err(|e| anyhow!("{}", tr(e)))?;
    Ok(())
}

pub async fn cancel_ready(
    user: Arc<User>,
    deadline: Instant,
    origin: &CommandOrigin,
) -> Result<()> {
    let room = current_room(&user).await?;
    user.server
        .room_commands
        .cancel_ready(
            &user.server,
            &room.id.to_string(),
            user.id,
            deadline,
            origin.to_room_origin(),
        )
        .await
        .map_err(|e| anyhow!("{}", tr(e)))?;
    Ok(())
}

pub async fn played(
    user: Arc<User>,
    id: i32,
    deadline: Instant,
    origin: &CommandOrigin,
) -> Result<()> {
    let room = current_room(&user).await?;
    // Use live_config endpoint first, falling back to config file.
    let endpoint = resolve_phira_api_endpoint(&user.server).await;
    // P0-H: the remote record fetch is bounded by the command's remaining
    // absolute deadline — a slow/blocked Phira API must never let Played commit
    // after the client already timed out.
    let fetch_budget = deadline.saturating_duration_since(Instant::now());
    let res: crate::server::Record = tokio::time::timeout(
        fetch_budget,
        user.server.phira_client.get_json(
            &endpoint,
            None,
            &format!("/record/{id}"),
            None,
            PhiraRetryNoticeTarget::User(user.as_ref()),
            None,
        ),
    )
    .await
    .map_err(|_| anyhow!("played record fetch timed out"))??;
    if crate::official_client_compat::timing::deadline_expired(deadline) {
        bail!("played timed out");
    }
    if res.player != user.id {
        bail!("{}", tl!("invalid-record"));
    }
    debug!(
        room = room.id.to_string(),
        user = user.id,
        "user played: {res:?}"
    );
    // P0-C: refuse a late commit — the client has already timed out.
    if crate::official_client_compat::timing::deadline_expired(deadline) {
        bail!("played timed out");
    }
    // Route state mutation through RoomActor mailbox.
    user.server
        .room_commands
        .submit_result(
            &user.server, &room.id.to_string(), user.id,
            res.score, res.accuracy, res.perfect, res.good,
            res.bad, res.miss, res.max_combo, res.full_combo,
            res.std, res.std_score,
            Some(deadline),
            origin.to_room_origin(),
        )
        .await
        .map_err(|e| anyhow!("{}", tr(e)))?;
    Ok(())
}

pub async fn abort(user: Arc<User>, deadline: Instant, origin: &CommandOrigin) -> Result<()> {
    let room = current_room(&user).await?;
    user.server
        .room_commands
        .abort_round(
            &user.server,
            &room.id.to_string(),
            user.id,
            Some(deadline),
            origin.to_room_origin(),
        )
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

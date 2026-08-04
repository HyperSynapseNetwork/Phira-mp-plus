//! Room management methods on PlusServerState.

use phira_mp_common::{Message, RoomEvent, RoomId, ServerCommand};
use serde_json::Value;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use tracing::{trace, warn};

use super::config::normalize_phira_api_endpoint;
use super::state::PlusServerState;

impl PlusServerState {
    // ── Monitor helpers ──────────────────────────────────────────────

    /// 获取房间 monitor 会话
    pub async fn get_room_monitor(&self) -> Option<Arc<crate::session::Session>> {
        self.room_monitor
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade)
    }
    /// 设置房间 monitor 会话
    pub async fn set_room_monitor(&self, session: Weak<crate::session::Session>) {
        *self.room_monitor.write().await = Some(session);
    }
    /// 获取游戏 monitor 会话
    pub async fn get_game_monitor(&self, player_id: i32) -> Option<Arc<crate::session::Session>> {
        self.game_monitors
            .read()
            .await
            .get(&player_id)
            .and_then(Weak::upgrade)
    }
    /// 设置游戏 monitor 会话
    pub async fn set_game_monitor(&self, player_id: i32, session: Weak<crate::session::Session>) {
        self.game_monitors.write().await.insert(player_id, session);
    }

    // ── Room creation ────────────────────────────────────────────────

    /// 创建无人持久空房间。该房间没有初始房主，房主保持系统（host=-1）；
    /// 首个加入的普通玩家**不会**静默成为房主（房主只能由房主创建者或
    /// 管理员 `room host` 设置）。
    ///
    /// TODO(Phase2-WorkD): This method bypasses RoomCommandGateway — it creates
    /// the Room struct directly and inserts it into state.rooms. No mailbox
    /// command exists for room creation. A future RoomActorCommand::CreateRoom
    /// variant could unify this path with the actor lifecycle.
    ///
    /// persistent_empty and phira_api_endpoint are now routed through the
    /// gateway after the room actor is registered.
    pub async fn create_empty_room(
        self: &Arc<Self>,
        room_id: &str,
        endpoint: Option<String>,
        persistent_empty: bool,
    ) -> Result<Value, String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let endpoint = endpoint
            .map(|value| normalize_phira_api_endpoint(&value))
            .transpose()?;
        let max_users = self.config.max_users_per_room.unwrap_or(100);
        let room = Arc::new(crate::room::Room::new_empty(
            rid.clone(),
            Some(Arc::clone(&self.plugin_manager)),
            Arc::downgrade(self),
            max_users,
            Some(Arc::clone(&self.round_store)),
        ));
        // PMP25 P4: 先插入 registry，再通过 RoomActorInit 一次性初始化属性。
        // 初始化失败时回滚删除 room。
        {
            let mut rooms = self.rooms.write().await;
            if rooms.contains_key(&rid) {
                return Err("room already exists".to_string());
            }
            if let Some(limit) = self.config.max_rooms {
                if rooms.len() >= limit {
                    return Err(format!("server room limit reached (max {limit})"));
                }
            }
            rooms.insert(rid.clone(), Arc::clone(&room));
        }
        let init_result = self
            .room_commands
            .init_empty_room(self, &rid.to_string(), endpoint.clone(), persistent_empty)
            .await;
        if let Err(e) = init_result {
            // Rollback: remove room from registry since init failed
            self.rooms.write().await.remove(&rid);
            return Err(format!("room init failed: {e}"));
        }
        let data = crate::session_room::build_room_data(&room).await;
        self.publish_room_event(RoomEvent::CreateRoom {
            room: rid.clone(),
            data,
        })
        .await;
        self.dispatch_plugin_event(crate::plugin::PluginEvent::RoomCreate {
            user_id: 0,
            room_id: rid.to_string(),
        })
        .await;
        // Read properties for response from snapshot.
        let control = room.control_snapshot();
        Ok(serde_json::json!({
            "ok": true,
            "room_id": rid.to_string(),
            "uuid": room.uuid.to_string(),
            "persistent_empty": control.persistent_empty,
            "phira_api_endpoint": self.config.phira_api_endpoint,
            "phira_api_endpoint_override": control.phira_api_endpoint,
        }))
    }

    // ── Room persistence / metadata ──────────────────────────────────

    /// Set the persistent_empty flag for a room.
    /// Routes through the RoomCommandGateway to persist in actor state.
    pub async fn set_room_persistent_empty(
        &self,
        room_id: &str,
        persistent: bool,
    ) -> Result<Value, String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        // Apply the actor command FIRST, then dispatch plugin event only on success.
        let result = self
            .room_commands
            .set_persistent_empty(self, &rid.to_string(), persistent)
            .await?;
        self.dispatch_plugin_event(crate::plugin::PluginEvent::RoomModify {
            user_id: 0,
            room_id: rid.to_string(),
            data: serde_json::json!({"action":"persistent_empty","value": persistent}).to_string(),
        })
        .await;
        Ok(result)
    }

    /// 如果房间没有真实房主或系统 `?` 房主，让指定普通玩家成为房主。
    ///
    /// After actor cutover, host is always managed via actor snapshot and
    /// mailbox commands. Host is set at actor init (player-created rooms from
    /// `creator_id`) or via SetHost (admin). The AddUser handler never assigns
    /// host to a joiner — empty/system-hosted rooms keep `host_id = None`
    /// (host -1). This function serves as an additional safety net for paths
    /// (like force_move) that bypass AddUser.
    ///
    /// P0-I: `announce` controls whether a first-host assignment broadcasts
    /// `ChangeHost(true)` inline (via the actor SetHost handler). The JOIN path
    /// passes `announce=false`: the actor AddUser already promoted the joiner,
    /// so this returns whether the user is the host WITHOUT calling set_host —
    /// the caller (join_room) defers the `ChangeHost(true)` packet to the
    /// post-response compat queue so it can never arrive before JoinRoom(Ok)
    /// or reach a new session after a reconnect. Non-join paths (leave_room
    /// host reassign, force_move) pass `announce=true` and keep the immediate
    /// set_host broadcast.
    pub async fn assign_room_host_if_missing(
        &self,
        room: &Arc<crate::room::Room>,
        user: &Arc<crate::session::User>,
        monitor: bool,
        announce: bool,
    ) -> bool {
        if monitor {
            return false;
        }
        // Check if room already has a host. When it does, report whether THIS
        // user is the current host (used by the join path to decide whether to
        // schedule the deferred ChangeHost(true)).
        let control = room.control_snapshot();
        tracing::debug!(
            room = %room.id,
            user = user.id,
            host_id = ?control.host_id,
            system_host = control.system_host,
            "assign_room_host_if_missing check"
        );
        if control.host_id.is_some() {
            return control.host_id == Some(user.id);
        }
        if control.system_host {
            return false;
        }
        if !announce {
            // P0-I: the actor AddUser promotes the first non-monitor joiner
            // without broadcasting. Never call set_host here — the inline
            // ChangeHost(true) would arrive before JoinRoom(Ok). Report the
            // promoted state so the caller schedules the compensation instead.
            return control.host_id == Some(user.id);
        }
        tracing::info!(
            user = user.id, room = %room.id,
            "assigning host to first joiner"
        );
        self.room_commands
            .set_host(self, &room.id.to_string(), Some(user.id))
            .await
            .is_ok()
    }

    // ── Display metadata refresh ─────────────────────────────────────

    /// 刷新房间内展示用用户名与谱面名。只影响服务端 TUI/Web/欢迎语/历史展示；不改客户端本机 Phira API。
    ///
    /// After Phase 2 Work C, display name and chart mutations route through
    /// the RoomCommandGateway.
    pub async fn refresh_room_display_metadata(&self, room: &Arc<crate::room::Room>) {
        // Use server's default endpoint (room override no longer directly readable).
        let endpoint = self.config.phira_api_endpoint.clone();
        Self::refresh_room_display_metadata_with_endpoint(
            room,
            self,
            endpoint,
            Arc::clone(&self.phira_client),
        )
        .await;
    }

    async fn refresh_room_display_metadata_with_endpoint(
        room: &Arc<crate::room::Room>,
        state: &PlusServerState,
        endpoint: String,
        phira_client: Arc<crate::phira_client::PhiraRetryClient>,
    ) {
        let people = room
            .users()
            .await
            .into_iter()
            .chain(room.monitors().await.into_iter())
            .collect::<Vec<_>>();
        for user in people {
            let mut display = user.name.clone();
            if let Some(token) = user.auth_token().await {
                if let Some((remote_id, remote_name)) = phira_client
                    .fetch_user_by_token(&endpoint, None, &token)
                    .await
                {
                    if remote_id == user.id || user.id < 0 {
                        display = remote_name;
                    }
                }
            }
            // Route display name through the actor mailbox.
            let _ = state
                .room_commands
                .set_display_name(state, &room.id.to_string(), user.id, &display)
                .await;
        }
        // Route chart through the actor mailbox.
        let chart_id = {
            // Read chart id from snapshot.
            if let Some(snap) = state.room_snapshot(&room.id.to_string()) {
                snap.chart
            } else {
                None
            }
        };
        if let Some(chart_id) = chart_id {
            let name = match phira_client.fetch_chart_by_id(&endpoint, chart_id).await {
                Some(chart) => chart.name,
                None => {
                    tracing::warn!("refresh_metadata: failed to fetch chart {chart_id}");
                    format!("#{chart_id}")
                }
            };
            let _ = state
                .room_commands
                .set_chart(state, &room.id.to_string(), chart_id, &name, 0, None, None)
                .await;
            room.publish_update(phira_mp_common::PartialRoomData {
                chart: Some(chart_id),
                ..Default::default()
            })
            .await;
        }
    }

    /// 后台刷新房间展示元数据。
    ///
    /// 这个流程会访问 Phira `/me` 和 `/chart/<id>`，自定义 endpoint 慢、不可达或 502 时可能
    /// 等到 reqwest 超时。加入房间、强制迁移、设置 endpoint 等协议关键路径不能等待它，
    /// 否则客户端会先看到 timeout，随后重连才发现服务端其实已经把用户放进房间。
    ///
    /// The room override is no longer directly readable from Room — use the
    /// server's default endpoint.
    ///
    /// NOTE: Accepts `&Arc<Self>` so the background task can clone the Arc.
    /// Called as `server.refresh_room_display_metadata_background(&room)` where
    /// `server: Arc<PlusServerState>`.
    pub fn refresh_room_display_metadata_background(
        self: &Arc<Self>,
        room: &Arc<crate::room::Room>,
    ) {
        let permit = match Arc::clone(&self.room_metadata_refresh_gate).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                trace!(
                    room = room.id.to_string(),
                    "skipping room metadata refresh because refresh concurrency is saturated"
                );
                return;
            }
        };
        let room = Arc::clone(room);
        let state = Arc::clone(self);
        let endpoint = self.config.phira_api_endpoint.clone();
        let phira_client = Arc::clone(&self.phira_client);
        crate::supervisor_actor::spawn_named("room-metadata-refresh", async move {
            let _permit = permit;
            PlusServerState::refresh_room_display_metadata_with_endpoint(
                &room,
                &state,
                endpoint,
                phira_client,
            )
            .await;
        });
    }

    /// Refresh room display metadata by room ID (background spawn).
    async fn refresh_room_display_metadata_background_by_id(
        self: &Arc<Self>,
        room_id: &str,
    ) {
        let rooms = self.rooms.read().await;
        let rid: RoomId = match room_id.to_string().try_into() {
            Ok(id) => id,
            Err(_) => return,
        };
        if let Some(room) = rooms.get(&rid).map(Arc::clone) {
            drop(rooms);
            self.refresh_room_display_metadata_background(&room);
        }
    }

    // ── Room user history ────────────────────────────────────────────

    pub(crate) async fn record_user_room_history(
        &self,
        user_id: i32,
        room_id: String,
        room_uuid: String,
        joined_at: i64,
    ) {
        {
            let mut history = self.user_room_history.write().await;
            let entries = history.entry(user_id).or_default();
            entries.push((room_id.clone(), room_uuid.clone(), joined_at));
            if entries.len() > super::state::USER_ROOM_HISTORY_LIMIT {
                let remove = entries.len() - super::state::USER_ROOM_HISTORY_LIMIT;
                entries.drain(0..remove);
            }
        }
        // Primary: route through PersistenceWorker
        let worker_event = crate::persistence::message::PersistenceEvent::UserRoomHistory {
            user_id,
            room_id: room_id.clone(),
            room_uuid: room_uuid.clone(),
            joined_at,
        };
        if self.persistence_worker.enqueue(worker_event).await.is_err() {
            warn!("record_user_room_history: worker enqueue failed, data kept in memory only");
        }
    }

    // ── Remote players (PDFP Lite federation) ─────────────────────────

    /// 注册一个"远程玩家"（无本地 session 的虚拟 User）并加入房间。
    ///
    /// PDFP Lite 联邦前置：远端服务器的玩家在本服务器以 `auth_token=None`
    /// 的虚拟 User 表示，房间成员仍由 room_actor 权威管理（`add_user`）。
    /// `remote` 标志使其可区分：`try_send` 对其静默丢弃消息，断线/离线路径
    /// （`dangle`/UserDisconnect）由本地 session 触发、永远不会到达远程玩家。
    pub async fn add_remote_player(
        self: &Arc<Self>,
        room_id: &str,
        player_id: i32,
        player_name: &str,
    ) -> Result<Value, String> {
        // 0 是系统身份，负 id 保留给 monitor 会话——远程玩家必须是正 id。
        if player_id <= 0 {
            return Err(format!("invalid player_id: {player_id}"));
        }
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let room = {
            let rooms = self.rooms.read().await;
            rooms
                .get(&rid)
                .map(Arc::clone)
                .ok_or_else(|| format!("room not found: {room_id}"))?
        };

        // 注册门内创建/复用虚拟 User。同一 id 已存在且带本地 session 时拒绝——
        // 一个 id 不能同时代表本地连接与远程玩家，否则成员解析/广播会互相覆盖。
        let user = {
            let _gate = self.user_registration_gate.lock().await;
            let mut users = self.users.write().await;
            match users.get(&player_id).map(Arc::clone) {
                Some(existing) => {
                    if !existing.remote.load(Ordering::SeqCst) {
                        return Err(format!(
                            "player {player_id} already exists with a local session"
                        ));
                    }
                    existing
                }
                None => {
                    let user = Arc::new(crate::session::User::new(
                        player_id,
                        player_name.to_string(),
                        crate::l10n::Language::default(),
                        Arc::clone(self),
                        None,
                    ));
                    user.remote.store(true, Ordering::SeqCst);
                    users.insert(player_id, Arc::clone(&user));
                    user
                }
            }
        };

        // 已在房间成员中则拒绝（幂等防重入；远程玩家在连接注册表与 actor
        // members 中都有条目，这里以连接注册表为准做去重检查）。user.room
        // 单值不变量：远程玩家同时只能在一个房间，跨房间重复加入会悬挂旧
        // 房间的成员条目。
        if room.users().await.iter().any(|u| u.id == player_id) {
            return Err(format!("player {player_id} already in room {room_id}"));
        }
        if user.room.read().await.is_some() {
            return Err(format!(
                "player {player_id} is already in another room; remove it first"
            ));
        }

        // 房间满预检（与 join 路径一致），再经 room_actor 提交成员。
        let max_users = room.control_snapshot().max_users;
        if room.users().await.len() >= max_users {
            return Err("room is full".to_string());
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        self.room_commands
            .add_user(self, room_id, player_id, player_name, false, deadline, None)
            .await?;

        // 同步连接注册表（broadcast 用）与 user.room 指向。注册表拒绝时
        // 回滚 actor 成员，避免 Ghost member（同 join 路径的补偿逻辑）。
        if !room.add_user(Arc::downgrade(&user), false).await {
            let _ = self
                .room_commands
                .remove_user(self, room_id, player_id, None, None)
                .await;
            return Err("room is full".to_string());
        }
        *user.room.write().await = Some(Arc::clone(&room));

        // 与普通 join 路径一致：向房间内广播 OnJoinRoom + JoinRoom，使本地
        // 客户端的玩家名单即时反映远程玩家。远程玩家自身无 session，broadcast
        // 对其 try_send 静默丢弃。
        room.broadcast(ServerCommand::OnJoinRoom(user.to_info()))
            .await;
        room.broadcast(ServerCommand::Message(Message::JoinRoom {
            user: player_id,
            name: user.name.clone(),
        }))
        .await;

        Ok(serde_json::json!({
            "ok": true,
            "room_id": room_id,
            "user_id": player_id,
            "user_name": user.name.clone(),
            "remote": true,
        }))
    }

    /// 移除远程玩家：先经 room_actor 权威退出（on_user_leave 清 room 指向并
    /// 移除房间弱引用、广播 LeaveRoom、必要时转移 host），再清理全局 users 注册表。
    ///
    /// 只接受 `remote` 标志的虚拟 User——本地 session 用户应走 `room.kick`，
    /// 避免插件借此踢掉真实玩家。
    pub async fn remove_remote_player(
        self: &Arc<Self>,
        room_id: &str,
        player_id: i32,
    ) -> Result<Value, String> {
        if player_id <= 0 {
            return Err(format!("invalid player_id: {player_id}"));
        }
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        {
            let rooms = self.rooms.read().await;
            rooms
                .get(&rid)
                .ok_or_else(|| format!("room not found: {room_id}"))?;
        }
        let user = {
            let users = self.users.read().await;
            users.get(&player_id).map(Arc::clone)
        }
        .ok_or_else(|| format!("player {player_id} not found"))?;
        if !user.remote.load(Ordering::SeqCst) {
            return Err(format!("player {player_id} has a local session; use room.kick"));
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        self.room_commands
            .remove_user(self, room_id, player_id, Some(deadline), None)
            .await?;
        // on_user_leave 已把 user 从房间弱引用移除并清 user.room；这里清理全局
        // 注册表（与 disconnect.rs 一致：ptr 校验避免误删被替换的用户）。
        let mut users = self.users.write().await;
        if users.get(&player_id).is_some_and(|u| Arc::ptr_eq(u, &user)) {
            users.remove(&player_id);
        }
        Ok(serde_json::json!({
            "ok": true,
            "room_id": room_id,
            "user_id": player_id,
        }))
    }

    // ── Force move user ──────────────────────────────────────────────

    /// 管理员强制把用户迁移到指定房间，绕过房间人数、锁定、进行中等普通加入限制。
    ///
    /// Uses a compiler approach via RoomCommandGateway:
    ///   1. RemoveUser from old room (actor state + Room connection registry)
    ///   2. AddUser to new room (actor state)
    ///   3. Update connection registry (force_add_user)
    ///   4. If AddUser fails after RemoveUser, rollback: re-AddUser to old room
    pub async fn force_move_user_to_room(
        self: &Arc<Self>,
        room_id: &str,
        target_id: i32,
        monitor: bool,
    ) -> Result<Value, String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let target_room = {
            let rooms = self.rooms.read().await;
            rooms.get(&rid).map(Arc::clone).ok_or("room not found")?
        };
        let user = {
            let users = self.users.read().await;
            users
                .get(&target_id)
                .map(Arc::clone)
                .ok_or("user not found")?
        };

        let old_room = user.room.read().await.as_ref().map(Arc::clone);
        let old_room_id = old_room.as_ref().map(|room| room.id.to_string());
        let was_monitor = user.monitor.load(Ordering::SeqCst);
        let same_room = old_room
            .as_ref()
            .is_some_and(|room| room.id.to_string() == rid.to_string());

        // Phase 1: RemoveUser from old room via actor (actor state + Room connection registry).
        let old_room_dropped = if let Some(ref old_room_val) = old_room {
            if same_room {
                false
            } else {
                let old_id_text = old_room_val.id.to_string();
                let remove_result = self
                    .room_commands
                    .remove_user(self, &old_id_text, target_id, None, None)
                    .await;
                match remove_result {
                    Ok(val) => val
                        .get("room_dropped")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    Err(e) => {
                        // 审计 P3: Actor RemoveUser 失败时不再 fallback 到 direct cleanup，
                        // 而是终止整个 move 操作。使用者应当重试或关闭 Session。
                        return Err(format!(
                            "force_move: RemoveUser failed for user {target_id} in {old_id_text}: {e}"
                        ));
                    }
                }
            }
        } else {
            false
        };

        // Phase 2: AddUser to new room via actor (actor state).
        user.monitor.store(monitor, Ordering::SeqCst);
        // Admin force-move has no client-facing deadline; use the same budget as
        // the room mailbox COMMAND_TIMEOUT so the deadline check never fires
        // before the mailbox would time out on its own.
        let admin_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let add_result = if same_room {
            // Same room: skip AddUser (would duplicate in members list).
            Ok(serde_json::json!({"monitor": monitor}))
        } else {
            self.room_commands
                .add_user(
                    self,
                    &rid.to_string(),
                    target_id,
                    &user.name,
                    monitor,
                    admin_deadline,
                    None,
                )
                .await
        };

        // 审计 P1 (PMP28): AddUser 失败时通过 actor 回滚 RemoveUser; 如果旧房间已被删除则置
        // user.room 为 None 避免悬挂引用。
        if let Err(err) = add_result {
            user.monitor.store(was_monitor, Ordering::SeqCst);
            if let Some(ref old_room_val) = old_room {
                if !same_room && !old_room_dropped {
                    let re_add = self
                        .room_commands
                        .add_user(
                            self,
                            &old_room_val.id.to_string(),
                            target_id,
                            &user.name,
                            was_monitor,
                            admin_deadline,
                            None,
                        )
                        .await;
                    if re_add.is_err() {
                        warn!(
                            target_id,
                            old_room = %old_room_val.id,
                            "force_move rollback: actor AddUser failed; user may be unattached"
                        );
                    }
                }
                if old_room_dropped {
                    *user.room.write().await = None;
                } else {
                    *user.room.write().await = Some(Arc::clone(old_room_val));
                }
            }
            return Err(err);
        }

        // Phase 3: Update connection registry.
        target_room
            .force_add_user(Arc::downgrade(&user), monitor)
            .await;
        *user.room.write().await = Some(Arc::clone(&target_room));

        // Phase 4: Set monitor live flag.
        if monitor {
            let _ = self
                .room_commands
                .set_live(self, &rid.to_string(), true)
                .await;
        }

        // Phase 5: Assign host if missing.
        self.assign_room_host_if_missing(&target_room, &user, monitor, true)
            .await;
        self.refresh_room_display_metadata_background(&target_room);

        // Phase 6: Broadcast join.
        let join = ServerCommand::OnJoinRoom(user.to_info());
        let message = ServerCommand::Message(phira_mp_common::Message::JoinRoom {
            user: user.id,
            name: user.name.clone(),
        });
        if monitor {
            target_room.broadcast_players(join).await;
            target_room.broadcast_players(message).await;
        } else {
            target_room.broadcast(join).await;
            target_room.broadcast(message).await;
            if !same_room || was_monitor {
                self.publish_room_event(RoomEvent::JoinRoom {
                    room: rid.clone(),
                    user: target_id,
                })
                .await;
            }
        }

        // Phase 7: Send JoinRoom response to user.
        let mut users = target_room.users().await;
        users.extend(target_room.monitors().await);
        let room_state = crate::session_room::build_client_room_state(&target_room, &user).await;
        let is_host = room_state.is_host;
        // JoinRoom(Ok) 是「加入新房间」的关键响应：必须经 Critical 路径
        // `send_and_flush` 证明真正 flush 到 socket 才返回（P0-E/P0-F），与正常
        // join_room 的 `origin.send_and_flush(JoinRoom(Ok))` 一致。不能用
        // `try_send`（best-effort `OutboundItem::Packet`）：出站队列拥塞时会静默
        // 丢弃并断开 Session，被转移用户收不到通知、客户端卡住直到重连。响应
        // 非房间状态事件，room_seq 传 None（cutover 不适用，绝不剔除）。
        let join_ok = ServerCommand::JoinRoom(Ok(phira_mp_common::JoinRoomResponse {
            state: room_state.state,
            users: users.into_iter().map(|user| user.to_info()).collect(),
            live: target_room.is_live(),
        }));
        match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            user.send_and_flush(join_ok),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                warn!(target_id, room = %rid, "force_move: JoinRoom(Ok) send failed: {err}");
            }
            Err(_) => {
                warn!(target_id, room = %rid, "force_move: JoinRoom(Ok) flush timed out");
            }
        }
        // ChangeHost 是状态告知（非响应），随 JoinRoom(Ok) flush 后经 FIFO 到达；
        // 作为告知传 None（cutover 不剔除）。
        user.try_send(ServerCommand::ChangeHost(is_host), None).await;

        // Phase 8: Record history.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.record_user_room_history(
            target_id,
            rid.to_string(),
            target_room.uuid.to_string(),
            now,
        )
        .await;

        // Phase 9: Plugin events.
        self.dispatch_plugin_event(crate::plugin::PluginEvent::RoomJoin {
            user_id: target_id,
            room_id: rid.to_string(),
            is_monitor: monitor,
        })
        .await;
        self.dispatch_plugin_event(crate::plugin::PluginEvent::RoomModify {
            user_id: target_id,
            room_id: rid.to_string(),
            data: serde_json::json!({"action":"force-move","from": old_room_id.clone(),"monitor": monitor}).to_string(),
        })
        .await;

        // Phase 10: System message.
        {
            let uname = user.name.clone();
            target_room.send_system_msg(
                &|lang| {
                    let mut a = fluent::FluentArgs::new();
                    a.set("name", &uname);
                    crate::l10n::translate_system(lang, "user-moved-to-room", &a)
                },
            ).await;
        }

        Ok(serde_json::json!({
            "ok": true,
            "room_id": rid.to_string(),
            "target_id": target_id,
            "monitor": monitor,
            "from": old_room_id,
        }))
    }

    // ── Room hidden flag ─────────────────────────────────────────────

    pub async fn set_room_hidden(&self, room_id: &str, hidden: bool) -> Result<Value, String> {
        self.room_commands
            .set_hidden(self, room_id, hidden)
            .await
    }

    // ── Phira API endpoint ───────────────────────────────────────────

    pub async fn get_room_phira_api_endpoint(&self, room_id: &str) -> Result<Value, String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        // Read from control snapshot (populated from actor state).
        let room = {
            let rooms = self.rooms.read().await;
            rooms.get(&rid).map(Arc::clone).ok_or("room not found")?
        };
        let control = room.control_snapshot();
        let override_endpoint = control.phira_api_endpoint;
        let using_room_override = override_endpoint.is_some();
        let effective_endpoint = override_endpoint
            .clone()
            .unwrap_or_else(|| self.config.phira_api_endpoint.clone());
        Ok(serde_json::json!({
            "ok": true,
            "room_id": rid.to_string(),
            "phira_api_endpoint": effective_endpoint,
            "phira_api_endpoint_override": override_endpoint,
            "using_room_override": using_room_override,
        }))
    }

    pub async fn set_room_phira_api_endpoint(
        self: &Arc<Self>,
        room_id: &str,
        endpoint: Option<String>,
    ) -> Result<Value, String> {
        let normalized = endpoint
            .map(|value| normalize_phira_api_endpoint(&value))
            .transpose()?;
        // Route through gateway.
        self.room_commands
            .set_phira_api_endpoint(self, room_id, normalized.clone())
            .await?;
        self.refresh_room_display_metadata_background_by_id(room_id)
            .await;
        let _control = {
            let rooms = self.rooms.read().await;
            let rid: RoomId = room_id
                .to_string()
                .try_into()
                .map_err(|_| "invalid room_id".to_string())?;
            rooms.get(&rid).map(|r| r.control_snapshot())
        };
        let using_room_override = normalized.is_some();
        let effective_endpoint = normalized
            .clone()
            .unwrap_or_else(|| self.config.phira_api_endpoint.clone());
        Ok(serde_json::json!({
            "ok": true,
            "room_id": room_id.to_string(),
            "phira_api_endpoint": effective_endpoint,
            "phira_api_endpoint_override": normalized,
            "using_room_override": using_room_override,
        }))
    }
}

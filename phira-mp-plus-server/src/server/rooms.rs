//! Room management methods on PlusServerState.
//!
//! Extracted from orig.rs.

use phira_mp_common::{RoomEvent, RoomId, ServerCommand};
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

    /// 创建无人持久空房间。该房间没有初始房主，首个加入的普通玩家会静默成为房主。
    ///
    /// TODO(Phase2-WorkD): This method bypasses RoomCommandGateway — it creates
    /// the Room struct directly and inserts it into state.rooms. No mailbox
    /// command exists for room creation. A future RoomActorCommand::CreateRoom
    /// variant could unify this path with the actor lifecycle.
    ///
    /// TODO(Phase2-WorkC): Room no longer holds state (set_persistent_empty,
    /// set_phira_api_endpoint_override removed). These must be routed through
    /// the gateway after the room actor is registered.
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
        let endpoint = match endpoint {
            Some(value) => Some(normalize_phira_api_endpoint(&value)?),
            None => None,
        };
        let max_users = self.config.max_users_per_room.unwrap_or(100);
        let room = Arc::new(crate::room::Room::new_empty(
            rid.clone(),
            Some(Arc::clone(&self.plugin_manager)),
            Arc::downgrade(self),
            max_users,
            Some(Arc::clone(&self.round_store)),
        ));
        // TODO(Phase2-WorkC): Route set_persistent_empty through gateway once
        // a RoomActorCommand::SetPersistentEmpty variant exists.
        // For now, the value is not persisted to actor state.
        if let Some(endpoint) = endpoint.clone() {
            // Route endpoint override through gateway.
            let _ = self
                .room_commands
                .set_phira_api_endpoint(self, &rid.to_string(), Some(endpoint))
                .await;
        }
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
        // Re-use session_room's build function for consistency.
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

    /// TODO(Phase2-WorkD): Direct room mutation bypassing gateway. No
    /// RoomActorCommand::SetPersistentEmpty variant exists yet. Consider adding
    /// one so this operation goes through the per-room mailbox.
    ///
    /// TODO(Phase2-WorkC): Room no longer has set_persistent_empty. The
    /// persistent_empty flag should be stored in actor_state. This function
    /// is kept as a stub that records the plugin event but does not persist
    /// the flag until a gateway command exists.
    pub async fn set_room_persistent_empty(
        &self,
        room_id: &str,
        persistent: bool,
    ) -> Result<Value, String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        // Room no longer has set_persistent_empty. The value would need to be
        // stored in actor_state via a future gateway command.
        self.dispatch_plugin_event(crate::plugin::PluginEvent::RoomModify {
            user_id: 0,
            room_id: rid.to_string(),
            data: serde_json::json!({"action":"persistent_empty","value": persistent}).to_string(),
        })
        .await;
        Ok(
            serde_json::json!({"ok": true, "room_id": rid.to_string(), "persistent_empty": persistent}),
        )
    }

    /// 如果房间没有真实房主或系统 `?` 房主，让指定普通玩家成为房主。
    ///
    /// After Phase 2 Work C, routes through RoomCommandGateway::set_host().
    /// The `announce` parameter is preserved for call sites that need the
    /// protocol-ordering guarantee (JoinRoom(Ok) before ChangeHost).
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
        // Check if room has a host via control_snapshot (from actor cache).
        let control = room.control_snapshot();
        if control.host_id.is_some() || control.system_host {
            return false;
        }
        // Use the gateway to set host. The gateway always announces, but
        // callers with announce=false (first-joiner flow) rely on the caller
        // to send JoinRoom(Ok) first, then ChangeHost. The gateway-set host
        // will be visible in the snapshot before the response is sent.
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
            if let Some(chart) = phira_client.fetch_chart_by_id(&endpoint, chart_id).await {
                let name = chart.name.clone();
                let _ = state
                    .room_commands
                    .set_chart(state, &room.id.to_string(), chart_id, &name)
                    .await;
                room.publish_update(phira_mp_common::PartialRoomData {
                    chart: Some(chart_id),
                    ..Default::default()
                })
                .await;
            }
        }
    }

    /// 后台刷新房间展示元数据。
    ///
    /// 这个流程会访问 Phira `/me` 和 `/chart/<id>`，自定义 endpoint 慢、不可达或 502 时可能
    /// 等到 reqwest 超时。加入房间、强制迁移、设置 endpoint 等协议关键路径不能等待它，
    /// 否则客户端会先看到 timeout，随后重连才发现服务端其实已经把用户放进房间。
    ///
    /// After Phase 2 Work C, the room override is no longer directly readable
    /// from Room. We use the server's default endpoint.
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

    // ── Force move user ──────────────────────────────────────────────

    /// 管理员强制把用户迁移到指定房间，绕过房间人数、锁定、进行中等普通加入限制。
    // TODO(Phase2-WorkD): Direct room/user mutation bypassing gateway. This
    // multi-step operation (leave old room → join new room) should be routed
    // through RoomCommandGateway mailbox commands (RemoveUser + AddUser) for
    // serialized actor state transitions.
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

        if let Some(room) = old_room.as_ref().filter(|_| !same_room) {
            let old_id = room.id.clone();
            let old_id_text = old_id.to_string();
            if room.on_user_leave(&user).await {
                self.rooms.write().await.remove(&old_id);
            }
            if !was_monitor {
                self.publish_room_event(RoomEvent::LeaveRoom {
                    room: old_id,
                    user: target_id,
                })
                .await;
            }
            self.dispatch_plugin_event(crate::plugin::PluginEvent::RoomLeave {
                user_id: target_id,
                room_id: old_id_text,
            })
            .await;
        }

        user.monitor.store(monitor, Ordering::SeqCst);
        target_room
            .force_add_user(Arc::downgrade(&user), monitor)
            .await;
        *user.room.write().await = Some(Arc::clone(&target_room));
        if monitor {
            target_room.live.store(true, Ordering::SeqCst);
        }
        self.assign_room_host_if_missing(&target_room, &user, monitor, false)
            .await;
        self.refresh_room_display_metadata_background(&target_room);

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

        let mut users = target_room.users().await;
        users.extend(target_room.monitors().await);
        let room_state = crate::session_room::build_client_room_state(&target_room, &user).await;
        let is_host = room_state.is_host;
        user.try_send(ServerCommand::JoinRoom(Ok(
            phira_mp_common::JoinRoomResponse {
                state: room_state.state,
                users: users.into_iter().map(|user| user.to_info()).collect(),
                live: target_room.is_live(),
            },
        )))
        .await;
        user.try_send(ServerCommand::ChangeHost(is_host)).await;

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

        target_room
            .send(phira_mp_common::Message::Chat {
                user: 0,
                content: format!("用户 {} 已被管理员强制转移到本房间", user.name),
            })
            .await;

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
        let normalized = match endpoint {
            Some(value) => Some(normalize_phira_api_endpoint(&value)?),
            None => None,
        };
        // Route through gateway.
        self.room_commands
            .set_phira_api_endpoint(self, room_id, normalized.clone())
            .await?;
        self.refresh_room_display_metadata_background_by_id(room_id)
            .await;
        let control = {
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

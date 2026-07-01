//! Existing room operation adapters behind the Runtime v2 gateway.

use super::{command::RoomActorCommand, RoomCommandGateway};
use crate::{plugin::PluginEvent, server::PlusServerState};
use phira_mp_common::{Message, PartialRoomData, RoomEvent, RoomId};
use serde_json::Value;
use std::{sync::{atomic::Ordering, Arc}, time::Instant};

impl RoomCommandGateway {
    pub(super) async fn find_room(
        &self,
        state: &PlusServerState,
        room_id: &str,
    ) -> Result<(RoomId, Arc<crate::room::Room>), String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let room = {
            let rooms = state.rooms.read().await;
            rooms.get(&rid).map(Arc::clone)
        }
        .ok_or_else(|| "room not found".to_string())?;
        Ok((rid, room))
    }

    /// Kick a user/monitor from a room.
    pub async fn kick_user(
        &self,
        state: &PlusServerState,
        room_id: &str,
        target_id: i32,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let result = self
            .room_mailbox_or_inline(
                room_id,
                |reply| RoomActorCommand::KickUser {
                    room_id: room_id.to_string(),
                    target_id,
                    reply,
                },
                || self.kick_user_inline(state, room_id, target_id),
            )
            .await;
        self.finish_command(state, "kick", room_id, started, result).into_legacy()
    }

    pub(super) async fn kick_user_inline(
        &self,
        state: &PlusServerState,
        room_id: &str,
        target_id: i32,
    ) -> Result<Value, String> {
        let (rid, room) = self.find_room(state, room_id).await?;
        let users = room.users().await;
        let monitors = room.monitors().await;
        let user = users
            .into_iter()
            .chain(monitors.into_iter())
            .find(|u| u.id == target_id)
            .ok_or_else(|| "user not in room".to_string())?;

        room.send(Message::Chat {
            user: 0,
            content: format!("用户 {} 已被管理员踢出房间", user.name),
        })
        .await;
        let was_monitor = user.monitor.load(Ordering::SeqCst);
        let should_drop = room.on_user_leave(&user).await;
        if should_drop {
            state.rooms.write().await.remove(&rid);
        }
        if !was_monitor {
            state
                .publish_room_event(RoomEvent::LeaveRoom {
                    room: rid.clone(),
                    user: target_id,
                })
                .await;
        }
        state
            .plugin_manager
            .trigger(&PluginEvent::RoomModify {
                user_id: target_id,
                room_id: room_id.to_string(),
                data: serde_json::json!({"action":"kicked"}).to_string(),
            })
            .await;
        Ok(serde_json::json!({
            "ok": true,
            "room_id": room_id,
            "user_id": target_id,
            "user_name": user.name.clone(),
            "room_dropped": should_drop,
        }))
    }

    /// Close and remove a room.
    pub async fn close_room(&self, state: &PlusServerState, room_id: &str) -> Result<Value, String> {
        let started = Instant::now();
        let result = self
            .room_mailbox_or_inline(
                room_id,
                |reply| RoomActorCommand::CloseRoom {
                    room_id: room_id.to_string(),
                    reply,
                },
                || self.close_room_inline(state, room_id),
            )
            .await;
        self.finish_command(state, "close", room_id, started, result).into_legacy()
    }

    pub(super) async fn close_room_inline(&self, state: &PlusServerState, room_id: &str) -> Result<Value, String> {
        let (rid, room) = self.find_room(state, room_id).await?;
        let room_id_str = room.id.to_string();
        room.send(Message::Chat {
            user: 0,
            content: "房间已被管理员关闭".to_string(),
        })
        .await;
        for user in room.users().await {
            *user.room.write().await = None;
            state
                .publish_room_event(RoomEvent::LeaveRoom {
                    room: rid.clone(),
                    user: user.id,
                })
                .await;
        }
        for monitor in room.monitors().await {
            *monitor.room.write().await = None;
        }
        state.rooms.write().await.remove(&rid);
        state
            .plugin_manager
            .trigger(&PluginEvent::RoomModify {
                user_id: 0,
                room_id: room_id_str.clone(),
                data: serde_json::json!({"action":"closed"}).to_string(),
            })
            .await;
        Ok(serde_json::json!({"ok": true, "room_id": room_id_str}))
    }

    /// Start a room through the existing admin-start path.
    ///
    /// Runtime v2 Step 17 routes this through the per-room mailbox.  The mailbox
    /// serializes this higher-risk state-machine transition with other admin room
    /// writes, while the existing `Room::begin_admin_start` implementation still
    /// owns the protocol behavior.
    pub async fn start_room(&self, state: &PlusServerState, room_id: &str) -> Result<Value, String> {
        let started = Instant::now();
        let result = self
            .room_mailbox_or_inline_control(
                room_id,
                |reply| RoomActorCommand::StartRoom {
                    room_id: room_id.to_string(),
                    reply,
                },
                || self.start_room_inline(state, room_id),
            )
            .await;
        self.finish_command(state, "start", room_id, started, result).into_legacy()
    }

    pub(super) async fn start_room_inline(&self, state: &PlusServerState, room_id: &str) -> Result<Value, String> {
        let (_rid, room) = self.find_room(state, room_id).await?;
        room.begin_admin_start().await.map_err(|e| e.to_string())?;
        if let Some(pm) = &room.plugin_manager {
            pm.trigger(&PluginEvent::GameStart {
                user_id: 0,
                room_id: room_id.to_string(),
            })
            .await;
        }
        Ok(serde_json::json!({
            "ok": true,
            "room_id": room_id,
            "action": "start",
            "route": "per_room_mailbox",
        }))
    }

    /// Cancel a pending admin-start wait state.
    ///
    /// The old inline version sent `CancelGame` while holding the room state write
    /// lock.  Step 17 keeps the external behavior but narrows the critical section:
    /// it flips `WaitForReady -> SelectChart` first, drops the lock, and only then
    /// sends client/control messages and publishes state changes.
    pub async fn cancel_start(&self, state: &PlusServerState, room_id: &str) -> Result<Value, String> {
        let started = Instant::now();
        let result = self
            .room_mailbox_or_inline_control(
                room_id,
                |reply| RoomActorCommand::CancelStart {
                    room_id: room_id.to_string(),
                    reply,
                },
                || self.cancel_start_inline(state, room_id),
            )
            .await;
        self.finish_command(state, "cancel", room_id, started, result).into_legacy()
    }

    pub(super) async fn cancel_start_inline(&self, state: &PlusServerState, room_id: &str) -> Result<Value, String> {
        let (_rid, room) = self.find_room(state, room_id).await?;
        let canceled = {
            let mut room_state = room.state.write().await;
            if matches!(&*room_state, crate::room::InternalRoomState::WaitForReady { .. }) {
                *room_state = crate::room::InternalRoomState::SelectChart;
                true
            } else {
                false
            }
        };
        if canceled {
            room.send(Message::CancelGame { user: 0 }).await;
            room.finish_admin_start().await;
            room.on_state_change().await;
        }
        Ok(serde_json::json!({
            "ok": true,
            "room_id": room_id,
            "canceled": canceled,
            "route": "per_room_mailbox",
        }))
    }

    /// Set the room host. `None` means the system `?` host.
    pub async fn set_host(
        &self,
        state: &PlusServerState,
        room_id: &str,
        target_id: Option<i32>,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let result = self
            .room_mailbox_or_inline(
                room_id,
                |reply| RoomActorCommand::SetHost {
                    room_id: room_id.to_string(),
                    target_id,
                    reply,
                },
                || self.set_host_inline(state, room_id, target_id),
            )
            .await;
        self.finish_command(state, "set_host", room_id, started, result).into_legacy()
    }

    pub(super) async fn set_host_inline(
        &self,
        state: &PlusServerState,
        room_id: &str,
        target_id: Option<i32>,
    ) -> Result<Value, String> {
        let (_rid, room) = self.find_room(state, room_id).await?;
        match target_id {
            Some(user_id) => {
                let mut user_name = user_id.to_string();
                for user in room.users().await {
                    if user.id == user_id {
                        user_name = room.display_name(&user).await;
                        break;
                    }
                }
                room.send(Message::Chat {
                    user: 0,
                    content: format!("房主已转移给 {}", user_name),
                })
                .await;
                room.set_host(Some(user_id), true).await.map_err(|e| e.to_string())?;
                Ok(serde_json::json!({
                    "ok": true,
                    "room_id": room_id,
                    "host": user_id,
                    "host_name": user_name,
                    "host_is_system": false,
                }))
            }
            None => {
                room.send(Message::Chat {
                    user: 0,
                    content: "房主已设为系统 ?".to_string(),
                })
                .await;
                room.set_host(None, true).await.map_err(|e| e.to_string())?;
                Ok(serde_json::json!({
                    "ok": true,
                    "room_id": room_id,
                    "host": Value::Null,
                    "host_name": "?",
                    "host_is_system": true,
                }))
            }
        }
    }

    pub async fn set_lock(
        &self,
        state: &PlusServerState,
        room_id: &str,
        locked: bool,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let result = self
            .room_mailbox_or_inline(
                room_id,
                |reply| RoomActorCommand::SetLock {
                    room_id: room_id.to_string(),
                    locked,
                    reply,
                },
                || self.set_lock_inline(state, room_id, locked),
            )
            .await;
        self.finish_command(state, "set_lock", room_id, started, result).into_legacy()
    }

    pub(super) async fn set_lock_inline(
        &self,
        state: &PlusServerState,
        room_id: &str,
        locked: bool,
    ) -> Result<Value, String> {
        let (_rid, room) = self.find_room(state, room_id).await?;
        room.locked.store(locked, Ordering::SeqCst);
        room.send(Message::LockRoom { lock: locked }).await;
        room.publish_update(PartialRoomData {
            lock: Some(locked),
            ..Default::default()
        })
        .await;
        state
            .plugin_manager
            .trigger(&PluginEvent::RoomModify {
                user_id: 0,
                room_id: room_id.to_string(),
                data: serde_json::json!({"action":"lock","value":locked}).to_string(),
            })
            .await;
        Ok(serde_json::json!({"ok": true, "room_id": room_id, "locked": locked}))
    }

    pub async fn set_cycle(
        &self,
        state: &PlusServerState,
        room_id: &str,
        cycle: bool,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let result = self
            .room_mailbox_or_inline(
                room_id,
                |reply| RoomActorCommand::SetCycle {
                    room_id: room_id.to_string(),
                    cycle,
                    reply,
                },
                || self.set_cycle_inline(state, room_id, cycle),
            )
            .await;
        self.finish_command(state, "set_cycle", room_id, started, result).into_legacy()
    }

    pub(super) async fn set_cycle_inline(
        &self,
        state: &PlusServerState,
        room_id: &str,
        cycle: bool,
    ) -> Result<Value, String> {
        let (_rid, room) = self.find_room(state, room_id).await?;
        room.cycle.store(cycle, Ordering::SeqCst);
        room.send(Message::CycleRoom { cycle }).await;
        room.publish_update(PartialRoomData {
            cycle: Some(cycle),
            ..Default::default()
        })
        .await;
        state
            .plugin_manager
            .trigger(&PluginEvent::RoomModify {
                user_id: 0,
                room_id: room_id.to_string(),
                data: serde_json::json!({"action":"cycle","value":cycle}).to_string(),
            })
            .await;
        Ok(serde_json::json!({"ok": true, "room_id": room_id, "cycle": cycle}))
    }
}

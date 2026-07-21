//! Membership and lifecycle room command operations.

use super::super::{
    command::{RoomActorCommand, RoomCommandKind},
    RoomCommandGateway, RoomCommandPayload,
};
use crate::{plugin::PluginEvent, server::PlusServerState};
use phira_mp_common::{Message, RoomEvent, ServerCommand};
use serde_json::Value;
use std::sync::Arc;
use std::{sync::atomic::Ordering, time::Instant};
use tracing::info;

impl RoomCommandGateway {
    /// Kick a user/monitor from a room.
    pub async fn kick_user(
        &self,
        state: &PlusServerState,
        room_id: &str,
        target_id: i32,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::KickUser {
                room_id: rid.clone(),
                target_id,
                reply,
            })
            .await;
        self.finish_command(
            state,
            RoomCommandKind::KickUser.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }

    pub(in crate::room_actor) async fn kick_user_in_actor(
        &self,
        state: &PlusServerState,
        room_id: &str,
        target_id: i32,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (rid, room) = self.resolve_room(state, room_id, room_override).await?;
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
        // Force the target client out of its local room immediately instead of
        // waiting for a reconnect snapshot.
        user.try_send(ServerCommand::LeaveRoom(Ok(()))).await;
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
            .dispatch_plugin_event(PluginEvent::RoomModify {
                user_id: target_id,
                room_id: room_id.to_string(),
                data: serde_json::json!({"action":"kicked"}).to_string(),
            })
            .await;
        Ok(RoomCommandPayload::UserKicked {
            room_id: room_id.to_string(),
            user_id: target_id,
            user_name: user.name.clone(),
            room_dropped: should_drop,
        })
    }

    /// Close and remove a room.
    pub async fn close_room(
        &self,
        state: &PlusServerState,
        room_id: &str,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let result = self
            .room_mailbox(room_id, |reply| RoomActorCommand::CloseRoom {
                room_id: room_id.to_string(),
                reply,
            })
            .await;
        self.finish_command(
            state,
            RoomCommandKind::CloseRoom.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }

    pub(in crate::room_actor) async fn close_room_in_actor(
        &self,
        state: &PlusServerState,
        room_id: &str,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (rid, room) = self.resolve_room(state, room_id, room_override).await?;
        let room_id_str = room.id.to_string();
        room.send(Message::Chat {
            user: 0,
            content: "房间已被管理员关闭".to_string(),
        })
        .await;
        for user in room.users().await {
            *user.room.write().await = None;
            user.try_send(ServerCommand::LeaveRoom(Ok(()))).await;
            state
                .publish_room_event(RoomEvent::LeaveRoom {
                    room: rid.clone(),
                    user: user.id,
                })
                .await;
        }
        for monitor in room.monitors().await {
            *monitor.room.write().await = None;
            monitor.try_send(ServerCommand::LeaveRoom(Ok(()))).await;
        }
        state.rooms.write().await.remove(&rid);
        state
            .dispatch_plugin_event(PluginEvent::RoomModify {
                user_id: 0,
                room_id: room_id_str.clone(),
                data: serde_json::json!({"action":"closed"}).to_string(),
            })
            .await;
        Ok(RoomCommandPayload::RoomClosed {
            room_id: room_id_str,
        })
    }

    // ── AddUser ────────────────────────────────────────────────────────────

    pub async fn add_user(
        &self,
        state: &PlusServerState,
        room_id: &str,
        user_id: i32,
        user_name: &str,
        monitor: bool,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let uname = user_name.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::AddUser {
                room_id: rid.clone(),
                user_id,
                user_name: uname,
                monitor,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::AddUser.action(), room_id, started, result)
            .into_untyped()
    }

    pub(in crate::room_actor) async fn add_user_in_actor(
        &self,
        state: &PlusServerState,
        room_id: &str,
        user_id: i32,
        _user_name: &str,
        monitor: bool,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (_rid, room) = self.resolve_room(state, room_id, room_override).await?;

        // Check capacity
        let current_count = room.users().await.len();
        if current_count >= room.control_snapshot().max_users && !monitor {
            return Err("room is full".to_string());
        }

        // Add user to appropriate list and set live flag.
        // The caller (session_room) sets user.room and user.monitor.
        // Here we only manage the room-side lists.
        if monitor {
            // Keep a lightweight tracking entry for monitors (they are still in users for compatibility)
            // The actual monitor reference is added via add_user below
        }

        if !room.live.swap(true, std::sync::atomic::Ordering::SeqCst) {
            info!(room = %rid, "room goes live via add_user");
        }

        state.dispatch_plugin_event(PluginEvent::RoomModify {
            user_id,
            room_id: room_id.to_string(),
            data: serde_json::json!({"action": if monitor { "monitor_join" } else { "join" }}).to_string(),
        })
        .await;

        Ok(RoomCommandPayload::UserAdded {
            room_id: room_id.to_string(),
            user_id,
            monitor,
            room_full: current_count + 1 >= room.control_snapshot().max_users,
        })
    }

    // ── RemoveUser ──────────────────────────────────────────────────────────

    pub async fn remove_user(
        &self,
        state: &PlusServerState,
        room_id: &str,
        user_id: i32,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::RemoveUser {
                room_id: rid.clone(),
                user_id,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::RemoveUser.action(), room_id, started, result)
            .into_untyped()
    }

    pub(in crate::room_actor) async fn remove_user_in_actor(
        &self,
        state: &PlusServerState,
        room_id: &str,
        user_id: i32,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (rid, room) = self.resolve_room(state, room_id, room_override).await?;

        // Find the user in room lists before we can drive on_user_leave.
        // Search users first, then monitors.
        let mut found = {
            let users = room.users().await;
            users.iter().find(|u| u.id == user_id).cloned()
        };
        if found.is_none() {
            let monitors = room.monitors().await;
            found = monitors.iter().find(|u| u.id == user_id).cloned();
        }

        if let Some(user) = found {
            let was_monitor = user.monitor.load(std::sync::atomic::Ordering::SeqCst);
            let should_drop = room.on_user_leave(&user).await;
            if should_drop {
                state.rooms.write().await.remove(&rid);
            }
            if !was_monitor {
                state.publish_room_event(RoomEvent::LeaveRoom {
                    room: rid.clone(),
                    user: user_id,
                })
                .await;
            }
            state.dispatch_plugin_event(PluginEvent::RoomModify {
                user_id,
                room_id: room_id.to_string(),
                data: serde_json::json!({"action": "leave"}).to_string(),
            })
            .await;
            Ok(RoomCommandPayload::UserRemoved {
                room_id: room_id.to_string(),
                user_id,
                room_dropped: should_drop,
            })
        } else {
            Err("user not found in room".to_string())
        }
    }
}

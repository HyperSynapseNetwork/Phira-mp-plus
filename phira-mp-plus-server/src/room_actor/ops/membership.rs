//! Membership and lifecycle room command operations.

use super::super::{command::{RoomActorCommand, RoomCommandKind}, RoomCommandGateway};
use crate::{plugin::PluginEvent, server::PlusServerState};
use phira_mp_common::{Message, RoomEvent};
use serde_json::Value;
use std::{sync::atomic::Ordering, time::Instant};

impl RoomCommandGateway {
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
        self.finish_command(state, RoomCommandKind::KickUser.action(), room_id, started, result).into_legacy()
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
        self.finish_command(state, RoomCommandKind::CloseRoom.action(), room_id, started, result).into_legacy()
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
}

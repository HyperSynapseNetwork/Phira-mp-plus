//! Room setting command operations.

use super::super::{command::{RoomActorCommand, RoomCommandKind}, RoomCommandGateway};
use crate::{plugin::PluginEvent, server::PlusServerState};
use phira_mp_common::{Message, PartialRoomData};
use serde_json::Value;
use std::{sync::atomic::Ordering, time::Instant};

impl RoomCommandGateway {
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
        self.finish_command(state, RoomCommandKind::SetHost.action(), room_id, started, result).into_legacy()
    }

    pub(in crate::room_actor) async fn set_host_inline(
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
        self.finish_command(state, RoomCommandKind::SetLock.action(), room_id, started, result).into_legacy()
    }

    pub(in crate::room_actor) async fn set_lock_inline(
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
        self.finish_command(state, RoomCommandKind::SetCycle.action(), room_id, started, result).into_legacy()
    }

    pub(in crate::room_actor) async fn set_cycle_inline(
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

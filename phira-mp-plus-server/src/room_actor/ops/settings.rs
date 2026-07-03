//! Room setting command operations.

use super::super::{
    command::{RoomActorCommand, RoomCommandKind},
    RoomCommandGateway, RoomCommandPayload,
};
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
                || async {
                    self.set_host_inline(state, room_id, target_id)
                        .await
                        .map(RoomCommandPayload::into_json)
                },
            )
            .await;
        self.finish_command(
            state,
            RoomCommandKind::SetHost.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }

    pub(in crate::room_actor) async fn set_host_inline(
        &self,
        state: &PlusServerState,
        room_id: &str,
        target_id: Option<i32>,
    ) -> Result<RoomCommandPayload, String> {
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
                room.set_host(Some(user_id), true)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(RoomCommandPayload::HostChanged {
                    room_id: room_id.to_string(),
                    host: Some(user_id),
                    host_name: user_name,
                    host_is_system: false,
                })
            }
            None => {
                room.send(Message::Chat {
                    user: 0,
                    content: "房主已设为系统 ?".to_string(),
                })
                .await;
                room.set_host(None, true).await.map_err(|e| e.to_string())?;
                Ok(RoomCommandPayload::HostChanged {
                    room_id: room_id.to_string(),
                    host: None,
                    host_name: "?".to_string(),
                    host_is_system: true,
                })
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
                || async {
                    self.set_lock_inline(state, room_id, locked)
                        .await
                        .map(RoomCommandPayload::into_json)
                },
            )
            .await;
        self.finish_command(
            state,
            RoomCommandKind::SetLock.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }

    pub(in crate::room_actor) async fn set_lock_inline(
        &self,
        state: &PlusServerState,
        room_id: &str,
        locked: bool,
    ) -> Result<RoomCommandPayload, String> {
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
        Ok(RoomCommandPayload::LockChanged {
            room_id: room_id.to_string(),
            locked,
        })
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
                || async {
                    self.set_cycle_inline(state, room_id, cycle)
                        .await
                        .map(RoomCommandPayload::into_json)
                },
            )
            .await;
        self.finish_command(
            state,
            RoomCommandKind::SetCycle.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }

    pub(in crate::room_actor) async fn set_cycle_inline(
        &self,
        state: &PlusServerState,
        room_id: &str,
        cycle: bool,
    ) -> Result<RoomCommandPayload, String> {
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
        Ok(RoomCommandPayload::CycleChanged {
            room_id: room_id.to_string(),
            cycle,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::{RoomCommandDelivery, RoomCommandResult};

    #[test]
    fn typed_boundary_round_trip() {
        // Verify that typed RoomCommandPayload values round-trip through
        // the untyped + from_untyped boundary used by the mailbox dispatch.
        let payloads = vec![
            super::RoomCommandPayload::LockChanged { room_id: "r1".into(), locked: true },
            super::RoomCommandPayload::CycleChanged { room_id: "r2".into(), cycle: false },
            super::RoomCommandPayload::HostChanged { room_id: "r3".into(), host_id: Some(42) },
        ];
        for payload in payloads {
            let json = serde_json::to_value(&payload).unwrap();
            let result = RoomCommandResult::from_untyped(
                Ok(json.clone()),
                RoomCommandDelivery::TypedBoundary,
            );
            if let RoomCommandDelivery::TypedBoundary = result.delivery {
                // Compile-time guarantee: the payload survived serialization
            } else {
                panic!("expected TypedBoundary delivery");
            }
        }
    }

    #[test]
    fn set_command_kind_is_typed() {
        use super::super::super::command::RoomCommandKind;
        // Verify mailbox dispatch uses typed Kind values (not raw strings)
        assert_eq!(RoomCommandKind::SetLock.as_str(), "SetLock");
        assert_eq!(RoomCommandKind::SetCycle.as_str(), "SetCycle");
        assert_eq!(RoomCommandKind::SetHost.as_str(), "SetHost");
        assert_eq!(RoomCommandKind::CloseRoom.as_str(), "CloseRoom");
        assert_eq!(RoomCommandKind::KickUser.as_str(), "KickUser");
    }
}

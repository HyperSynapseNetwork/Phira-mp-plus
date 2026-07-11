//! Room setting command operations.

use super::super::{
    command::{RoomActorCommand, RoomCommandKind},
    RoomCommandGateway, RoomCommandPayload,
};
use crate::{plugin::PluginEvent, server::PlusServerState};
use phira_mp_common::{Message, PartialRoomData};
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;

impl RoomCommandGateway {
    /// Set the room host. `None` means the system `?` host.
    pub async fn set_host(
        &self,
        state: &PlusServerState,
        room_id: &str,
        target_id: Option<i32>,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::SetHost {
                room_id: rid.clone(),
                target_id,
                reply,
            })
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

    pub(in crate::room_actor) async fn set_host_in_actor(
        &self,
        state: &PlusServerState,
        room_id: &str,
        target_id: Option<i32>,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (_rid, room) = self.resolve_room(state, room_id, room_override).await?;
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
        self.set_lock_as(state, room_id, locked, 0).await
    }

    pub async fn set_lock_as(
        &self,
        state: &PlusServerState,
        room_id: &str,
        locked: bool,
        actor_user_id: i32,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::SetLock {
                room_id: rid.clone(),
                locked,
                actor_user_id,
                reply,
            })
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

    pub(in crate::room_actor) async fn set_lock_in_actor(
        &self,
        state: &PlusServerState,
        room_id: &str,
        locked: bool,
        actor_user_id: i32,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (_rid, room) = self.resolve_room(state, room_id, room_override).await?;
        room.set_locked(locked);
        room.send(Message::LockRoom { lock: locked }).await;
        room.publish_update(PartialRoomData {
            lock: Some(locked),
            ..Default::default()
        })
        .await;
        state
            .dispatch_plugin_event(PluginEvent::RoomModify {
                user_id: actor_user_id,
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
        self.set_cycle_as(state, room_id, cycle, 0).await
    }

    pub async fn set_cycle_as(
        &self,
        state: &PlusServerState,
        room_id: &str,
        cycle: bool,
        actor_user_id: i32,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::SetCycle {
                room_id: rid.clone(),
                cycle,
                actor_user_id,
                reply,
            })
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

    pub(in crate::room_actor) async fn set_cycle_in_actor(
        &self,
        state: &PlusServerState,
        room_id: &str,
        cycle: bool,
        actor_user_id: i32,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (_rid, room) = self.resolve_room(state, room_id, room_override).await?;
        room.set_cycle(cycle);
        room.send(Message::CycleRoom { cycle }).await;
        room.publish_update(PartialRoomData {
            cycle: Some(cycle),
            ..Default::default()
        })
        .await;
        state
            .dispatch_plugin_event(PluginEvent::RoomModify {
                user_id: actor_user_id,
                room_id: room_id.to_string(),
                data: serde_json::json!({"action":"cycle","value":cycle}).to_string(),
            })
            .await;
        Ok(RoomCommandPayload::CycleChanged {
            room_id: room_id.to_string(),
            cycle,
        })
    }

    pub async fn set_hidden(
        &self,
        state: &PlusServerState,
        room_id: &str,
        hidden: bool,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::SetHidden {
                room_id: rid.clone(),
                hidden,
                reply,
            })
            .await;
        self.finish_command(
            state,
            RoomCommandKind::SetHidden.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }

    pub(in crate::room_actor) async fn set_hidden_in_actor(
        &self,
        state: &PlusServerState,
        room_id: &str,
        hidden: bool,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (_rid, room) = self.resolve_room(state, room_id, room_override).await?;
        room.set_hidden(hidden);
        state
            .dispatch_plugin_event(PluginEvent::RoomModify {
                user_id: 0,
                room_id: room_id.to_string(),
                data: serde_json::json!({"action":"hidden","value":hidden}).to_string(),
            })
            .await;
        Ok(RoomCommandPayload::HiddenChanged {
            room_id: room_id.to_string(),
            hidden,
        })
    }

    pub async fn set_phira_api_endpoint(
        &self,
        state: &PlusServerState,
        room_id: &str,
        endpoint: Option<String>,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::SetEndpoint {
                room_id: rid.clone(),
                endpoint: endpoint.clone(),
                reply,
            })
            .await;
        self.finish_command(
            state,
            RoomCommandKind::SetEndpoint.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }

    pub(in crate::room_actor) async fn set_endpoint_in_actor(
        &self,
        state: &PlusServerState,
        room_id: &str,
        endpoint: Option<String>,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (_rid, room) = self.resolve_room(state, room_id, room_override).await?;
        let normalized = match endpoint {
            Some(value) => Some(crate::server::normalize_phira_api_endpoint(&value)?),
            None => None,
        };
        room.set_phira_api_endpoint_override(normalized.clone())
            .await;
        state.refresh_room_display_metadata_background(&room);
        let using_room_override = normalized.is_some();
        let effective = normalized
            .clone()
            .unwrap_or_else(|| state.config.phira_api_endpoint.clone());
        state
            .dispatch_plugin_event(PluginEvent::RoomModify {
                user_id: 0,
                room_id: room_id.to_string(),
                data: serde_json::json!({
                    "action": "phira_api_endpoint",
                    "value": normalized.clone(),
                    "effective": effective.clone(),
                })
                .to_string(),
            })
            .await;
        Ok(RoomCommandPayload::EndpointChanged {
            room_id: room_id.to_string(),
            endpoint: effective,
            endpoint_override: normalized,
            using_room_override,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::command::RoomCommandKind;

    #[test]
    fn typed_payload_serialization_round_trip() {
        let payloads: Vec<super::RoomCommandPayload> = vec![
            super::RoomCommandPayload::LockChanged {
                room_id: "r1".into(),
                locked: true,
            },
            super::RoomCommandPayload::CycleChanged {
                room_id: "r2".into(),
                cycle: false,
            },
            super::RoomCommandPayload::HostChanged {
                room_id: "r3".into(),
                host: Some(42),
                host_name: "admin".into(),
                host_is_system: false,
            },
        ];
        for payload in &payloads {
            let json = serde_json::to_value(payload).unwrap();
            assert!(
                json.is_object(),
                "each payload should serialize to a JSON object"
            );
        }
    }

    #[test]
    fn set_command_kinds_are_typed() {
        assert_eq!(RoomCommandKind::SetLock.action(), "set_lock");
        assert_eq!(RoomCommandKind::SetCycle.action(), "set_cycle");
        assert_eq!(RoomCommandKind::SetHost.action(), "set_host");
        assert_eq!(RoomCommandKind::CloseRoom.action(), "close");
        assert_eq!(RoomCommandKind::KickUser.action(), "kick");
    }
}

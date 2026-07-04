//! Game-control room command operations.


use std::sync::Arc;
use super::super::{
    command::{RoomActorCommand, RoomCommandKind},
    RoomCommandGateway, RoomCommandPayload,
};
use crate::{plugin::PluginEvent, server::PlusServerState};
use phira_mp_common::Message;
use serde_json::Value;
use std::time::Instant;

impl RoomCommandGateway {
    /// Start a room through the existing admin-start path.
    ///
    /// Runtime v2 Step 17 routes this through the per-room mailbox.  The mailbox
    /// serializes this higher-risk state-machine transition with other admin room
    /// writes, while the existing `Room::begin_admin_start` implementation still
    /// owns the protocol behavior.
    pub async fn start_room(
        &self,
        state: &PlusServerState,
        room_id: &str,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let result = self
            .room_mailbox_only(
                room_id,
                |reply| RoomActorCommand::StartRoom {
                    room_id: room_id.to_string(),
                    reply,
                },
            )
            .await;
        self.finish_command(
            state,
            RoomCommandKind::StartRoom.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }

    pub(in crate::room_actor) async fn start_room_inline(
        &self,
        state: &PlusServerState,
        room_id: &str,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (_rid, room) = self.resolve_room(state, room_id, room_override).await?;
        room.begin_admin_start().await.map_err(|e| e.to_string())?;
        if let Some(pm) = &room.plugin_manager {
            pm.trigger(&PluginEvent::GameStart {
                user_id: 0,
                room_id: room_id.to_string(),
            })
            .await;
        }
        Ok(RoomCommandPayload::RoomStarted {
            room_id: room_id.to_string(),
        })
    }

    /// Cancel a pending admin-start wait state.
    ///
    /// The old inline version sent `CancelGame` while holding the room state write
    /// lock.  Step 17 keeps the external behavior but narrows the critical section:
    /// it flips `WaitForReady -> SelectChart` first, drops the lock, and only then
    /// sends client/control messages and publishes state changes.
    pub async fn cancel_start(
        &self,
        state: &PlusServerState,
        room_id: &str,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let result = self
            .room_mailbox_only(
                room_id,
                |reply| RoomActorCommand::CancelStart {
                    room_id: room_id.to_string(),
                    reply,
                },
            )
            .await;
        self.finish_command(
            state,
            RoomCommandKind::CancelStart.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }

    pub(in crate::room_actor) async fn cancel_start_inline(
        &self,
        state: &PlusServerState,
        room_id: &str,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (_rid, room) = self.resolve_room(state, room_id, room_override).await?;
        let canceled = {
            let mut room_state = room.state.write().await;
            if matches!(
                &*room_state,
                crate::room::InternalRoomState::WaitForReady { .. }
            ) {
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
        Ok(RoomCommandPayload::CancelResult {
            room_id: room_id.to_string(),
            canceled,
        })
    }
}

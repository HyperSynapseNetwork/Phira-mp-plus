use super::super::{
    command::{RoomActorCommand, RoomCommandKind},
    RoomCommandGateway,
};
use crate::server::PlusServerState;
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
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::StartRoom {
                room_id: rid.clone(),
                reply,
            })
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
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::CancelStart {
                room_id: rid.clone(),
                reply,
            })
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


    // ── HostStart ─────────────────────────────────────────────────────────

    /// Host-initiated game start. Routes through the per-room mailbox.
    pub async fn host_start(
        &self,
        state: &PlusServerState,
        room_id: &str,
        user_id: i32,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::HostStart {
                room_id: rid.clone(),
                user_id,
                reply,
            })
            .await;
        self.finish_command(
            state,
            RoomCommandKind::HostStart.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }

}

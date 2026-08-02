use super::super::{
    command::{RoomActorCommand, RoomCommandKind, RoomOrigin},
    RoomCommandGateway,
};
use crate::server::PlusServerState;
use serde_json::Value;
use std::time::Instant;

impl RoomCommandGateway {
    /// Start a room through the existing admin-start path.
    ///
    /// Runtime Step 17 routes this through the per-room mailbox.  The mailbox
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
            .room_mailbox(&rid, None, |reply| RoomActorCommand::StartRoom {
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


    /// 让房间进入准备阶段（CLI `room ready`，无用户 ID）。
    ///
    /// 与 [`Self::start_room`]（admin 强开）不同：本方法以
    /// `admin_started=false` 进入 `WaitForReady`，不跳过玩家准备检查——游戏在
    /// 所有玩家 ready（或 `ready_countdown_secs` 倒计时超时）后才开始。
    pub async fn enter_ready_phase(
        &self,
        state: &PlusServerState,
        room_id: &str,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, None, |reply| RoomActorCommand::EnterReadyPhase {
                room_id: rid.clone(),
                reply,
            })
            .await;
        self.finish_command(
            state,
            RoomCommandKind::EnterReadyPhase.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }

    /// Cancel a pending admin-start wait state.
    ///
    /// Flip `WaitForReady -> SelectChart` first and drop the room state lock
    /// before sending client/control messages — keep the critical section narrow.
    pub async fn cancel_start(
        &self,
        state: &PlusServerState,
        room_id: &str,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, None, |reply| RoomActorCommand::CancelStart {
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
        deadline: Instant,
        origin: RoomOrigin,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, Some(deadline), |reply| RoomActorCommand::HostStart {
                room_id: rid.clone(),
                user_id,
                deadline,
                origin,
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

//! Room actor command envelope types.

use super::RoomCommandResult;
use tokio::sync::oneshot;

pub(super) enum RoomActorCommand {
    SetLock {
        room_id: String,
        locked: bool,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    SetCycle {
        room_id: String,
        cycle: bool,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    SetHost {
        room_id: String,
        target_id: Option<i32>,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    CloseRoom {
        room_id: String,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    KickUser {
        room_id: String,
        target_id: i32,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    StartRoom {
        room_id: String,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    CancelStart {
        room_id: String,
        reply: oneshot::Sender<RoomCommandResult>,
    },
}

impl RoomActorCommand {
    pub(super) fn reply_with(self, result: RoomCommandResult) {
        match self {
            Self::SetLock { reply, .. }
            | Self::SetCycle { reply, .. }
            | Self::SetHost { reply, .. }
            | Self::CloseRoom { reply, .. }
            | Self::KickUser { reply, .. }
            | Self::StartRoom { reply, .. }
            | Self::CancelStart { reply, .. } => {
                let _ = reply.send(result);
            }
        }
    }
}

//! Room actor command envelope types.

use super::RoomCommandResult;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RoomCommandKind {
    SetLock,
    SetCycle,
    SetHost,
    CloseRoom,
    KickUser,
    StartRoom,
    CancelStart,
}

impl RoomCommandKind {
    pub(super) fn action(self) -> &'static str {
        match self {
            Self::SetLock => "set_lock",
            Self::SetCycle => "set_cycle",
            Self::SetHost => "set_host",
            Self::CloseRoom => "close",
            Self::KickUser => "kick",
            Self::StartRoom => "start",
            Self::CancelStart => "cancel",
        }
    }

    pub(super) fn stops_room_mailbox_after_execution(self) -> bool {
        matches!(self, Self::CloseRoom)
    }
}

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
    pub(super) fn kind(&self) -> RoomCommandKind {
        match self {
            Self::SetLock { .. } => RoomCommandKind::SetLock,
            Self::SetCycle { .. } => RoomCommandKind::SetCycle,
            Self::SetHost { .. } => RoomCommandKind::SetHost,
            Self::CloseRoom { .. } => RoomCommandKind::CloseRoom,
            Self::KickUser { .. } => RoomCommandKind::KickUser,
            Self::StartRoom { .. } => RoomCommandKind::StartRoom,
            Self::CancelStart { .. } => RoomCommandKind::CancelStart,
        }
    }

    /// Extract the room_id from any command variant.
    pub(super) fn room_id(&self) -> &str {
        match self {
            Self::SetLock { room_id, .. }
            | Self::SetCycle { room_id, .. }
            | Self::SetHost { room_id, .. }
            | Self::CloseRoom { room_id, .. }
            | Self::KickUser { room_id, .. }
            | Self::StartRoom { room_id, .. }
            | Self::CancelStart { room_id, .. } => room_id,
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_action_names_are_stable_contract() {
        assert_eq!(RoomCommandKind::SetLock.action(), "set_lock");
        assert_eq!(RoomCommandKind::SetCycle.action(), "set_cycle");
        assert_eq!(RoomCommandKind::SetHost.action(), "set_host");
        assert_eq!(RoomCommandKind::CloseRoom.action(), "close");
        assert_eq!(RoomCommandKind::KickUser.action(), "kick");
        assert_eq!(RoomCommandKind::StartRoom.action(), "start");
        assert_eq!(RoomCommandKind::CancelStart.action(), "cancel");
    }

    #[test]
    fn only_close_stops_room_mailbox_by_default() {
        assert!(RoomCommandKind::CloseRoom.stops_room_mailbox_after_execution());
        assert!(!RoomCommandKind::KickUser.stops_room_mailbox_after_execution());
        assert!(!RoomCommandKind::StartRoom.stops_room_mailbox_after_execution());
        assert!(!RoomCommandKind::CancelStart.stops_room_mailbox_after_execution());
    }
}

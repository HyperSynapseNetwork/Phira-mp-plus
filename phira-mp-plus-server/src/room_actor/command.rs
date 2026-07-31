//! Room actor command envelope types.

use crate::plugin::{JudgeEventItem, TouchEventPoint};
use super::RoomCommandResult;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RoomCommandKind {
    SetLock,
    SetCycle,
    SetHost,
    SetHidden,
    SetEndpoint,
    SetPersistentEmpty,
    CloseRoom,
    KickUser,
    StartRoom,
    CancelStart,
    HostStart,
    SetChart,
    SetReady,
    CancelReady,
    SubmitResult,
    AbortRound,
    AddUser,
    RemoveUser,
    SetLive,
    AddTouches,
    AddJudges,
    SetDisplayName,
    TelemetryTouches,
    TelemetryJudges,
}

impl RoomCommandKind {
    pub(super) fn action(self) -> &'static str {
        match self {
            Self::SetLock => "set_lock",
            Self::SetCycle => "set_cycle",
            Self::SetHost => "set_host",
            Self::SetHidden => "set_hidden",
            Self::SetEndpoint => "set_phira_api_endpoint",
            Self::CloseRoom => "close",
            Self::KickUser => "kick",
            Self::StartRoom => "start",
            Self::CancelStart => "cancel",
            Self::HostStart => "host_start",
            Self::SetChart => "set_chart",
            Self::SetReady => "set_ready",
            Self::CancelReady => "cancel_ready",
            Self::SubmitResult => "submit_result",
            Self::AbortRound => "abort_round",
            Self::AddUser => "add_user",
            Self::RemoveUser => "remove_user",
            Self::SetLive => "set_live",
            Self::AddTouches => "add_touches",
            Self::AddJudges => "add_judges",
            Self::SetDisplayName => "set_display_name",
            Self::SetPersistentEmpty => "set_persistent_empty",
            Self::TelemetryTouches => "telemetry_touches",
            Self::TelemetryJudges => "telemetry_judges",
        }
    }

    pub(super) fn stops_room_mailbox_after_execution(self) -> bool {
        matches!(self, Self::CloseRoom)
    }
}

pub(crate) enum RoomActorCommand {
    SetLock {
        room_id: String,
        locked: bool,
        actor_user_id: i32,
        /// Absolute actor deadline (P0-C/P0-G). The handler refuses the state
        /// transition when the deadline has already passed.
        deadline: std::time::Instant,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    SetCycle {
        room_id: String,
        cycle: bool,
        actor_user_id: i32,
        /// Absolute actor deadline (P0-C/P0-G).
        deadline: std::time::Instant,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    SetHost {
        room_id: String,
        target_id: Option<i32>,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    SetHidden {
        room_id: String,
        hidden: bool,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    SetEndpoint {
        room_id: String,
        endpoint: Option<String>,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    #[allow(dead_code)]
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
    HostStart {
        room_id: String,
        user_id: i32,
        /// Absolute actor deadline (P0-C/P0-G). The handler refuses the state
        /// transition when the deadline has already passed.
        deadline: std::time::Instant,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    SetChart {
        room_id: String,
        chart_id: i32,
        chart_name: String,
        actor_user_id: i32,
        /// Absolute actor deadline (P0-C/P0-G). The handler refuses to mutate
        /// the selected chart when the deadline has already passed.
        deadline: std::time::Instant,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    SetReady {
        room_id: String,
        user_id: i32,
        /// Absolute actor deadline (P0-C/P0-G). The handler refuses to insert
        /// into `started` when the deadline has already passed.
        deadline: std::time::Instant,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    CancelReady {
        room_id: String,
        user_id: i32,
        /// Absolute actor deadline (P0-C/P0-G).
        deadline: std::time::Instant,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    SubmitResult {
        room_id: String,
        user_id: i32,
        score: i32,
        accuracy: f32,
        perfect: i32,
        good: i32,
        bad: i32,
        miss: i32,
        max_combo: i32,
        full_combo: bool,
        std: f32,
        std_score: f32,
        /// Absolute actor deadline (P0-C/P0-G). The handler refuses to insert
        /// into `results` when the deadline has already passed.
        deadline: std::time::Instant,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    AbortRound {
        room_id: String,
        user_id: i32,
        /// Absolute actor deadline (P0-C/P0-G). The handler refuses to insert
        /// into `aborted` when the deadline has already passed.
        deadline: std::time::Instant,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    /// Fire-and-forget telemetry variant — no oneshot reply, casts through
    /// a dedicated telemetry channel to avoid control mailbox contention.
    #[allow(dead_code)]
    TelemetryTouches {
        room_id: String,
        user_id: i32,
        touches: Vec<TouchEventPoint>,
    },
    /// Fire-and-forget telemetry variant — no oneshot reply.
    #[allow(dead_code)]
    TelemetryJudges {
        room_id: String,
        user_id: i32,
        judges: Vec<JudgeEventItem>,
    },
    #[allow(dead_code)]
    AddUser {
        room_id: String,
        user_id: i32,
        user_name: String,
        monitor: bool,
        /// Absolute actor deadline (P0-C/P0-G). The handler refuses to add the
        /// user when the deadline has already passed.
        deadline: std::time::Instant,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    RemoveUser {
        room_id: String,
        user_id: i32,
        /// Absolute actor deadline (P0-C/P0-G). The handler refuses to remove
        /// the user when the deadline has already passed.
        deadline: std::time::Instant,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    SetLive {
        room_id: String,
        live: bool,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    #[allow(dead_code)]
    AddTouches {
        room_id: String,
        user_id: i32,
        touches: Vec<TouchEventPoint>,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    #[allow(dead_code)]
    AddJudges {
        room_id: String,
        user_id: i32,
        judges: Vec<JudgeEventItem>,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    SetDisplayName {
        room_id: String,
        user_id: i32,
        name: String,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    SetPersistentEmpty {
        room_id: String,
        persistent_empty: bool,
        reply: oneshot::Sender<RoomCommandResult>,
    },
}

impl RoomActorCommand {
    pub(super) fn kind(&self) -> RoomCommandKind {
        match self {
            Self::SetLock { .. } => RoomCommandKind::SetLock,
            Self::SetCycle { .. } => RoomCommandKind::SetCycle,
            Self::SetHost { .. } => RoomCommandKind::SetHost,
            Self::SetHidden { .. } => RoomCommandKind::SetHidden,
            Self::SetEndpoint { .. } => RoomCommandKind::SetEndpoint,
            Self::CloseRoom { .. } => RoomCommandKind::CloseRoom,
            Self::KickUser { .. } => RoomCommandKind::KickUser,
            Self::StartRoom { .. } => RoomCommandKind::StartRoom,
            Self::CancelStart { .. } => RoomCommandKind::CancelStart,
            Self::HostStart { .. } => RoomCommandKind::HostStart,
            Self::SetChart { .. } => RoomCommandKind::SetChart,
            Self::SetReady { .. } => RoomCommandKind::SetReady,
            Self::CancelReady { .. } => RoomCommandKind::CancelReady,
            Self::SubmitResult { .. } => RoomCommandKind::SubmitResult,
            Self::AbortRound { .. } => RoomCommandKind::AbortRound,
            Self::AddUser { .. } => RoomCommandKind::AddUser,
            Self::RemoveUser { .. } => RoomCommandKind::RemoveUser,
            Self::SetLive { .. } => RoomCommandKind::SetLive,
            Self::AddTouches { .. } => RoomCommandKind::AddTouches,
            Self::AddJudges { .. } => RoomCommandKind::AddJudges,
            Self::SetDisplayName { .. } => RoomCommandKind::SetDisplayName,
            Self::SetPersistentEmpty { .. } => RoomCommandKind::SetPersistentEmpty,
            Self::TelemetryTouches { .. } => RoomCommandKind::TelemetryTouches,
            Self::TelemetryJudges { .. } => RoomCommandKind::TelemetryJudges,
        }
    }

    pub(super) fn reply_with(self, result: RoomCommandResult) {
        match self {
            Self::SetLock { reply, .. }
            | Self::SetCycle { reply, .. }
            | Self::SetHost { reply, .. }
            | Self::SetHidden { reply, .. }
            | Self::SetEndpoint { reply, .. }
            | Self::CloseRoom { reply, .. }
            | Self::KickUser { reply, .. }
            | Self::StartRoom { reply, .. }
            | Self::CancelStart { reply, .. }
            | Self::HostStart { reply, .. }
            | Self::SetChart { reply, .. }
            | Self::SetReady { reply, .. }
            | Self::CancelReady { reply, .. }
            | Self::SubmitResult { reply, .. }
            | Self::AbortRound { reply, .. }
            | Self::AddUser { reply, .. }
            | Self::RemoveUser { reply, .. }
            | Self::SetLive { reply, .. }
            | Self::AddTouches { reply, .. }
            | Self::AddJudges { reply, .. }
            | Self::SetDisplayName { reply, .. }
            | Self::SetPersistentEmpty { reply, .. } => {
                let _ = reply.send(result);
            }
            // Telemetry variants are fire-and-forget — no reply channel.
            Self::TelemetryTouches { .. } | Self::TelemetryJudges { .. } => {}
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
        assert_eq!(RoomCommandKind::SetHidden.action(), "set_hidden");
        assert_eq!(
            RoomCommandKind::SetEndpoint.action(),
            "set_phira_api_endpoint"
        );
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

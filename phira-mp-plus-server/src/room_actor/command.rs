//! Room actor command envelope types.

use crate::plugin::{JudgeEventItem, TouchEventPoint};
use super::RoomCommandResult;
use tokio::sync::oneshot;

/// Origin identity of the Session that issued a room command (PMP44 P0-C).
/// Session-originated commands carry `Some((session_id, generation))`; the
/// per-room actor re-validates this against the user's current binding at its
/// commit point, so a reconnect during mailbox wait makes the command stale
/// before any authoritative state is mutated. Non-session callers
/// (CLI/admin/recovery/plugin) pass `None`.
///
/// `pub(crate)`: `session.rs` (outside the `room_actor` module tree) projects a
/// `CommandOrigin` into this token via `CommandOrigin::to_room_origin`.
pub(crate) type RoomOrigin = Option<(uuid::Uuid, u64)>;

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
    /// PMP45 P0-K: 设置房间 degraded 标志（Join 补偿失败后阻塞后续 Join，
    /// 直到操作员 / 未来 reconcile 清空）。
    SetDegraded,
    AddTouches,
    AddJudges,
    SetDisplayName,
    TelemetryTouches,
    TelemetryJudges,
    /// PMP45 P0-F: 原子认证快照。认证路径经 room mailbox 获取一致快照。
    BindAndSnapshot,
    /// PMP45 P0-O: 内部响应后检查（`RemoveUser` 触发，fire-and-forget）。
    CheckAllReady,
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
            Self::SetDegraded => "set_degraded",
            Self::AddTouches => "add_touches",
            Self::AddJudges => "add_judges",
            Self::SetDisplayName => "set_display_name",
            Self::SetPersistentEmpty => "set_persistent_empty",
            Self::TelemetryTouches => "telemetry_touches",
            Self::TelemetryJudges => "telemetry_judges",
            Self::BindAndSnapshot => "bind_and_snapshot",
            Self::CheckAllReady => "check_all_ready",
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
        /// PMP44 P0-C: origin Session (id + generation) that issued this
        /// command; re-validated at the commit point.
        origin: RoomOrigin,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    SetCycle {
        room_id: String,
        cycle: bool,
        actor_user_id: i32,
        /// Absolute actor deadline (P0-C/P0-G).
        deadline: std::time::Instant,
        /// PMP44 P0-C: origin Session (id + generation).
        origin: RoomOrigin,
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
        /// PMP44 P0-C: origin Session (id + generation).
        origin: RoomOrigin,
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
        /// PMP44 P0-C: origin Session (id + generation).
        origin: RoomOrigin,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    SetReady {
        room_id: String,
        user_id: i32,
        /// Absolute actor deadline (P0-C/P0-G). The handler refuses to insert
        /// into `started` when the deadline has already passed.
        deadline: std::time::Instant,
        /// PMP44 P0-C: origin Session (id + generation).
        origin: RoomOrigin,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    CancelReady {
        room_id: String,
        user_id: i32,
        /// Absolute actor deadline (P0-C/P0-G).
        deadline: std::time::Instant,
        /// PMP44 P0-C: origin Session (id + generation).
        origin: RoomOrigin,
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
        /// PMP44 P0-C: origin Session (id + generation).
        origin: RoomOrigin,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    AbortRound {
        room_id: String,
        user_id: i32,
        /// Absolute actor deadline (P0-C/P0-G). The handler refuses to insert
        /// into `aborted` when the deadline has already passed.
        deadline: std::time::Instant,
        /// PMP44 P0-C: origin Session (id + generation).
        origin: RoomOrigin,
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
        /// PMP44 P0-C: origin Session (id + generation).
        origin: RoomOrigin,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    RemoveUser {
        room_id: String,
        user_id: i32,
        /// Absolute actor deadline (P0-C/P0-G). The handler refuses to remove
        /// the user when the deadline has already passed.
        deadline: std::time::Instant,
        /// PMP44 P0-C: origin Session (id + generation).
        origin: RoomOrigin,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    SetLive {
        room_id: String,
        live: bool,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    /// PMP45 P0-K: 设置房间 degraded 标志。Join 补偿失败后置 true 阻塞后续
    /// Join；操作员 / 未来 reconcile 可置 false 恢复。
    SetDegraded {
        room_id: String,
        degraded: bool,
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
    /// PMP45 P0-F: 原子认证快照。Room Actor 在自身的排序点一次性捕获
    /// `ClientRoomState`（state / members / display_names 全部来自
    /// `actor_state`），并返回网关 command_seq 作为 cutover token。认证路径
    /// 用它作为快照切换对齐点（P0-G 之后 cutover 只剔除 `SnapshotCovered`
    /// 事件）。
    BindAndSnapshot {
        room_id: String,
        user_id: i32,
        reply: oneshot::Sender<RoomCommandResult>,
    },
    /// PMP45 P0-O: 内部响应后命令——`RemoveUser` 在回复之后经 mailbox 重入，
    /// 在 Actor 排序点执行 `check_all_ready`（插件回调 + DB 轮次检查不阻塞
    /// 原回复，audit §26）。fire-and-forget：发起方丢弃 reply 接收端，无客户端
    /// 等待该回复。
    CheckAllReady {
        room_id: String,
        /// 响应后检查的绝对 deadline。非会话内部路径，使用 30s 兜底。
        deadline: std::time::Instant,
        /// fire-and-forget：发起方丢弃 reply 接收端，无客户端等待。用
        /// `Sender<()>` 而非 `Sender<RoomCommandResult>`——避免 `RoomActorCommand`
        /// 通过 reply 类型依赖 `RoomCommandResult`（即 `execute_with_actor` 的
        /// 返回类型），消除 async opaque future 的 E0391 类型循环。
        reply: oneshot::Sender<()>,
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
            Self::SetDegraded { .. } => RoomCommandKind::SetDegraded,
            Self::AddTouches { .. } => RoomCommandKind::AddTouches,
            Self::AddJudges { .. } => RoomCommandKind::AddJudges,
            Self::SetDisplayName { .. } => RoomCommandKind::SetDisplayName,
            Self::SetPersistentEmpty { .. } => RoomCommandKind::SetPersistentEmpty,
            Self::TelemetryTouches { .. } => RoomCommandKind::TelemetryTouches,
            Self::TelemetryJudges { .. } => RoomCommandKind::TelemetryJudges,
            Self::BindAndSnapshot { .. } => RoomCommandKind::BindAndSnapshot,
            Self::CheckAllReady { .. } => RoomCommandKind::CheckAllReady,
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
            | Self::SetDegraded { reply, .. }
            | Self::AddTouches { reply, .. }
            | Self::AddJudges { reply, .. }
            | Self::SetDisplayName { reply, .. }
            | Self::SetPersistentEmpty { reply, .. }
            | Self::BindAndSnapshot { reply, .. } => {
                let _ = reply.send(result);
            }
            // CheckAllReady 是 fire-and-forget（Sender<()>）——确认已执行即可。
            Self::CheckAllReady { reply, .. } => {
                let _ = reply.send(());
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
        assert_eq!(RoomCommandKind::BindAndSnapshot.action(), "bind_and_snapshot");
    }

    #[test]
    fn only_close_stops_room_mailbox_by_default() {
        assert!(RoomCommandKind::CloseRoom.stops_room_mailbox_after_execution());
        assert!(!RoomCommandKind::KickUser.stops_room_mailbox_after_execution());
        assert!(!RoomCommandKind::StartRoom.stops_room_mailbox_after_execution());
        assert!(!RoomCommandKind::CancelStart.stops_room_mailbox_after_execution());
    }
}

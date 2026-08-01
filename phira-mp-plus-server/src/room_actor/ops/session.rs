use super::super::{
    command::{RoomActorCommand, RoomCommandKind, RoomOrigin},
    BindAndSnapshotData, RoomCommandGateway,
};
use crate::server::PlusServerState;
use serde_json::Value;
use std::time::Instant;

impl RoomCommandGateway {
    // ── BindAndSnapshot (PMP45 P0-F) ────────────────────────────────────────

    /// PMP45 P0-F: 原子认证快照。把 `BindAndSnapshot` 路由进房间 mailbox，让
    /// Room Actor 在自身的排序点一次性捕获 `ClientRoomState`（state / members
    /// / display_names 全部来自 actor_state），并返回快照数据与权威
    /// `snapshot_seq`（room_event_seq，PMP46 Blocker 2）。
    ///
    /// `deadline` 为绝对 actor 截止（认证绝对预算）；`None` 时回退到 mailbox
    /// 内部 30s 超时。失败（mailbox 不可用/超时/拒绝）返回 `Err`，认证路径
    /// fail-closed（audit §7.5）——绝不回退到非原子的
    /// `session_room::build_client_room_state` 进入 Active。
    pub async fn bind_and_snapshot(
        &self,
        state: &PlusServerState,
        room_id: &str,
        user_id: i32,
        deadline: Option<Instant>,
    ) -> Result<BindAndSnapshotData, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, deadline, |reply| RoomActorCommand::BindAndSnapshot {
                room_id: rid.clone(),
                user_id,
                reply,
            })
            .await;
        let result = self
            .finish_command(
                state,
                RoomCommandKind::BindAndSnapshot.action(),
                room_id,
                started,
                result,
            );
        let payload = result
            .into_payload()
            .ok_or_else(|| "bind_and_snapshot failed".to_string())?;
        serde_json::from_value(payload["snapshot"].clone())
            .map_err(|e| format!("bind_and_snapshot payload decode failed: {e}"))
    }


    // ── SetChart ──────────────────────────────────────────────────────────

    /// Set the selected chart (pre-fetched from Phira API by caller).
    ///
    /// `deadline` is the absolute actor deadline for session-originated
    /// commands; non-session callers (CLI/admin/force-move) pass `None` and the
    /// gateway falls back to the internal 30s room-mailbox timeout.
    /// `origin` is the issuing Session (id + generation) for session-originated
    /// commands; non-session callers pass `None` (PMP44 P0-C).
    pub async fn set_chart(
        &self,
        state: &PlusServerState,
        room_id: &str,
        chart_id: i32,
        chart_name: &str,
        actor_user_id: i32,
        deadline: Option<Instant>,
        origin: RoomOrigin,
    ) -> Result<Value, String> {
        let deadline = deadline.unwrap_or_else(|| Instant::now() + std::time::Duration::from_secs(30));
        let started = Instant::now();
        let rid = room_id.to_string();
        let cname = chart_name.to_string();
        let result = self
            .room_mailbox(&rid, Some(deadline), |reply| RoomActorCommand::SetChart {
                room_id: rid.clone(),
                chart_id,
                chart_name: cname,
                actor_user_id,
                deadline,
                origin,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::SetChart.action(), room_id, started, result)
            .into_untyped()
    }


    // ── SetReady ──────────────────────────────────────────────────────────

    pub async fn set_ready(
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
            .room_mailbox(&rid, Some(deadline), |reply| RoomActorCommand::SetReady {
                room_id: rid.clone(),
                user_id,
                deadline,
                origin,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::SetReady.action(), room_id, started, result)
            .into_untyped()
    }


    // ── CancelReady ───────────────────────────────────────────────────────

    pub async fn cancel_ready(
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
            .room_mailbox(&rid, Some(deadline), |reply| RoomActorCommand::CancelReady {
                room_id: rid.clone(),
                user_id,
                deadline,
                origin,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::CancelReady.action(), room_id, started, result)
            .into_untyped()
    }


    // ── SubmitResult ──────────────────────────────────────────────────────

    pub async fn submit_result(
        &self,
        state: &PlusServerState,
        room_id: &str,
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
        deadline: Option<Instant>,
        origin: RoomOrigin,
    ) -> Result<Value, String> {
        let deadline = deadline.unwrap_or_else(|| Instant::now() + std::time::Duration::from_secs(30));
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, Some(deadline), |reply| RoomActorCommand::SubmitResult {
                room_id: rid.clone(),
                user_id,
                score,
                accuracy,
                perfect,
                good,
                bad,
                miss,
                max_combo,
                full_combo,
                std,
                std_score,
                deadline,
                origin,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::SubmitResult.action(), room_id, started, result)
            .into_untyped()
    }


    // ── AbortRound ────────────────────────────────────────────────────────

    pub async fn abort_round(
        &self,
        state: &PlusServerState,
        room_id: &str,
        user_id: i32,
        deadline: Option<Instant>,
        origin: RoomOrigin,
    ) -> Result<Value, String> {
        let deadline = deadline.unwrap_or_else(|| Instant::now() + std::time::Duration::from_secs(30));
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, Some(deadline), |reply| RoomActorCommand::AbortRound {
                room_id: rid.clone(),
                user_id,
                deadline,
                origin,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::AbortRound.action(), room_id, started, result)
            .into_untyped()
    }

}

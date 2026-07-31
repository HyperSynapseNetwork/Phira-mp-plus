use super::super::{
    command::{RoomActorCommand, RoomCommandKind},
    RoomCommandGateway,
};
use crate::server::PlusServerState;
use serde_json::Value;
use std::time::Instant;

impl RoomCommandGateway {
    // ── SetChart ──────────────────────────────────────────────────────────

    /// Set the selected chart (pre-fetched from Phira API by caller).
    ///
    /// `deadline` is the absolute actor deadline for session-originated
    /// commands; non-session callers (CLI/admin/force-move) pass `None` and the
    /// gateway falls back to the internal 30s room-mailbox timeout.
    pub async fn set_chart(
        &self,
        state: &PlusServerState,
        room_id: &str,
        chart_id: i32,
        chart_name: &str,
        actor_user_id: i32,
        deadline: Option<Instant>,
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
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, Some(deadline), |reply| RoomActorCommand::SetReady {
                room_id: rid.clone(),
                user_id,
                deadline,
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
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, Some(deadline), |reply| RoomActorCommand::CancelReady {
                room_id: rid.clone(),
                user_id,
                deadline,
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
    ) -> Result<Value, String> {
        let deadline = deadline.unwrap_or_else(|| Instant::now() + std::time::Duration::from_secs(30));
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, Some(deadline), |reply| RoomActorCommand::AbortRound {
                room_id: rid.clone(),
                user_id,
                deadline,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::AbortRound.action(), room_id, started, result)
            .into_untyped()
    }

}

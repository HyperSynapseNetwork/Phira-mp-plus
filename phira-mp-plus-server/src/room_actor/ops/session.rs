use super::super::{
    command::{RoomActorCommand, RoomCommandKind},
    RoomCommandGateway,
};
use crate::server::PlusServerState;
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;

impl RoomCommandGateway {
    // ── SetChart ──────────────────────────────────────────────────────────

    /// Set the selected chart (pre-fetched from Phira API by caller).
    pub async fn set_chart(
        &self,
        state: &PlusServerState,
        room_id: &str,
        chart_id: i32,
        chart_name: &str,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let cname = chart_name.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::SetChart {
                room_id: rid.clone(),
                chart_id,
                chart_name: cname,
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
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::SetReady {
                room_id: rid.clone(),
                user_id,
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
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::CancelReady {
                room_id: rid.clone(),
                user_id,
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
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::SubmitResult {
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
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::AbortRound {
                room_id: rid.clone(),
                user_id,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::AbortRound.action(), room_id, started, result)
            .into_untyped()
    }

}

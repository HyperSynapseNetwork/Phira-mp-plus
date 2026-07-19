//! Session-driven room operations behind the Runtime v2 gateway.
//!
//! Migration Step: route join/leave/ready/round state mutations through
//! the per-room mailbox so all room state transitions are serialized.

use super::super::{
    command::{RoomActorCommand, RoomCommandKind},
    RoomCommandGateway, RoomCommandPayload,
};
use crate::server::PlusServerState;
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;

impl RoomCommandGateway {
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
        self.finish_command(
            state,
            RoomCommandKind::SelectChart.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }

    pub(in crate::room_actor) async fn set_chart_in_actor(
        &self,
        _state: &PlusServerState,
        room_id: &str,
        chart_id: i32,
        chart_name: &str,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (_rid, room) = self.resolve_room(_state, room_id, room_override).await?;
        let room_state = room.state.read().await;
        if !matches!(*room_state, crate::room::InternalRoomState::SelectChart) {
            return Err("cannot set chart outside SelectChart state".to_string());
        }
        drop(room_state);

        *room.chart.write().await = Some(crate::server::Chart {
            id: chart_id,
            name: chart_name.to_string(),
        });

        room.send(crate::phira_mp_common::Message::SelectChart {
            user: 0,
            name: chart_name.to_string(),
            id: chart_id,
        })
        .await;
        room.on_state_change().await;
        room.publish_update(crate::phira_mp_common::PartialRoomData {
            chart: Some(chart_id),
            ..Default::default()
        })
        .await;

        Ok(RoomCommandPayload::ChartSelected {
            room_id: room_id.to_string(),
            chart_id,
        })
    }

    /// Mark a user as ready.
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
        self.finish_command(
            state,
            RoomCommandKind::SetReady.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }

    pub(in crate::room_actor) async fn set_ready_in_actor(
        &self,
        _state: &PlusServerState,
        room_id: &str,
        user_id: i32,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (_rid, room) = self.resolve_room(_state, room_id, room_override).await?;
        {
            let mut guard = room.state.write().await;
            if let crate::room::InternalRoomState::WaitForReady {
                ref mut started, ..
            } = *guard
            {
                started.insert(user_id);
            }
        }
        room.send(crate::phira_mp_common::Message::UserReady { user: user_id })
            .await;
        Ok(RoomCommandPayload::UserReady {
            room_id: room_id.to_string(),
            user_id,
        })
    }

    /// Cancel a user's ready.
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
        self.finish_command(
            state,
            RoomCommandKind::CancelReady.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }

    pub(in crate::room_actor) async fn cancel_ready_in_actor(
        &self,
        _state: &PlusServerState,
        room_id: &str,
        user_id: i32,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (_rid, room) = self.resolve_room(_state, room_id, room_override).await?;
        {
            let mut guard = room.state.write().await;
            if let crate::room::InternalRoomState::WaitForReady {
                ref mut started, ..
            } = *guard
            {
                started.remove(&user_id);
            }
        }
        room.send(crate::phira_mp_common::Message::CancelReady { user: user_id })
            .await;
        Ok(RoomCommandPayload::UserNotReady {
            room_id: room_id.to_string(),
            user_id,
        })
    }

    /// Record a user's round result.
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
        self.finish_command(
            state,
            RoomCommandKind::SubmitResult.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }

    pub(in crate::room_actor) async fn submit_result_in_actor(
        &self,
        _state: &PlusServerState,
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
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (_rid, room) = self.resolve_room(_state, room_id, room_override).await?;
        {
            let mut guard = room.state.write().await;
            if let crate::room::InternalRoomState::Playing {
                ref mut results, ..
            } = *guard
            {
                results.insert(
                    user_id,
                    crate::server::Record {
                        id: 0,
                        player: user_id,
                        score,
                        perfect,
                        good,
                        bad,
                        miss,
                        max_combo,
                        accuracy,
                        full_combo,
                        std: 0.0,
                        std_score: 0.0,
                    },
                );
            }
        }
        Ok(RoomCommandPayload::RoundResultSubmitted {
            room_id: room_id.to_string(),
            user_id,
            score,
        })
    }

    /// Mark a user as aborted in current round.
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
        self.finish_command(
            state,
            RoomCommandKind::AbortRound.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }

    pub(in crate::room_actor) async fn abort_round_in_actor(
        &self,
        _state: &PlusServerState,
        room_id: &str,
        user_id: i32,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (_rid, room) = self.resolve_room(_state, room_id, room_override).await?;
        {
            let mut guard = room.state.write().await;
            if let crate::room::InternalRoomState::Playing {
                ref mut aborted, ..
            } = *guard
            {
                aborted.insert(user_id);
            }
        }
        Ok(RoomCommandPayload::RoundAborted {
            room_id: room_id.to_string(),
            user_id,
        })
    }
}

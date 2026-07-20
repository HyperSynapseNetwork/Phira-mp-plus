//! Session-driven room operations behind the Runtime v2 gateway.
//!
//! Migration Step: route all gameplay state mutations through the per-room
//! mailbox. Each _in_actor method implements the state transition + side
//! effects (broadcasts, plugin events, round store) in one critical section.

use super::super::{
    command::{RoomActorCommand, RoomCommandKind},
    RoomCommandGateway, RoomCommandPayload,
};
use crate::plugin::PluginEvent;
use crate::server::PlusServerState;
use phira_mp_common::Message;
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

    pub(in crate::room_actor) async fn set_chart_in_actor(
        &self,
        state: &PlusServerState,
        room_id: &str,
        chart_id: i32,
        chart_name: &str,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (rid, room) = self.resolve_room(state, room_id, room_override).await?;
        // Validate state
        let room_state = room.state.read().await;
        if !matches!(*room_state, crate::room::InternalRoomState::SelectChart) {
            return Err("cannot set chart outside SelectChart state".to_string());
        }
        drop(room_state);

        *room.chart.write().await = Some(crate::server::Chart {
            id: chart_id,
            name: chart_name.to_string(),
        });

        room.send(Message::SelectChart {
            user: 0,
            name: chart_name.to_string(),
            id: chart_id,
        })
        .await;
        room.on_state_change().await;
        room.publish_update(phira_mp_common::PartialRoomData {
            chart: Some(chart_id),
            ..Default::default()
        })
        .await;

        Ok(RoomCommandPayload::ChartSelected { room_id: room_id.to_string(), chart_id })
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

    pub(in crate::room_actor) async fn set_ready_in_actor(
        &self,
        state: &PlusServerState,
        room_id: &str,
        user_id: i32,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (rid, room) = self.resolve_room(state, room_id, room_override).await?;
        {
            let mut guard = room.state.write().await;
            if let crate::room::InternalRoomState::WaitForReady { ref mut started, .. } = *guard {
                if !started.insert(user_id) {
                    return Err("already ready".to_string());
                }
            } else {
                return Err("not in WaitForReady state".to_string());
            }
        }
        room.send(Message::Ready { user: user_id }).await;
        state.publish_runtime_event(crate::event_bus::MpEvent::PlayerReadyChanged {
            room_id: rid.clone(),
            user_id,
            ready: true,
        });
        room.check_all_ready().await;
        Ok(RoomCommandPayload::UserReady { room_id: room_id.to_string(), user_id })
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

    pub(in crate::room_actor) async fn cancel_ready_in_actor(
        &self,
        state: &PlusServerState,
        room_id: &str,
        user_id: i32,
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (rid, room) = self.resolve_room(state, room_id, room_override).await?;
        let was_host = {
            // Peek at host outside lock — check_host checks control lock
            let control = room.control_snapshot();
            control.host_id == Some(user_id)
        };
        {
            let mut guard = room.state.write().await;
            if let crate::room::InternalRoomState::WaitForReady { ref mut started, .. } = *guard {
                if !started.remove(&user_id) {
                    return Err("not ready".to_string());
                }
                if was_host {
                    // Host cancel → revert to SelectChart
                    room.send(Message::CancelGame { user: user_id }).await;
                    *guard = crate::room::InternalRoomState::SelectChart;
                    drop(guard);
                    room.finish_admin_start().await;
                    room.on_state_change().await;
                } else {
                    room.send(Message::CancelReady { user: user_id }).await;
                }
            } else {
                return Err("not in WaitForReady state".to_string());
            }
        }
        state.publish_runtime_event(crate::event_bus::MpEvent::PlayerReadyChanged {
            room_id: rid.clone(),
            user_id,
            ready: false,
        });
        Ok(RoomCommandPayload::UserNotReady { room_id: room_id.to_string(), user_id })
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

    pub(in crate::room_actor) async fn submit_result_in_actor(
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
        room_override: Option<Arc<crate::room::Room>>,
    ) -> Result<RoomCommandPayload, String> {
        let (rid, room) = self.resolve_room(state, room_id, room_override).await?;
        let record = crate::server::Record {
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
        };
        {
            let mut guard = room.state.write().await;
            if let crate::room::InternalRoomState::Playing { results, aborted } = &mut *guard {
                if aborted.contains(&user_id) {
                    return Err("user aborted".to_string());
                }
                if results.insert(user_id, record).is_some() {
                    return Err("already uploaded".to_string());
                }
            } else {
                return Err("not in Playing state".to_string());
            }
        }
        room.send(Message::Played {
            user: user_id,
            score,
            accuracy,
            full_combo,
        })
        .await;
        room.check_all_ready().await;
        state.dispatch_plugin_event(PluginEvent::GameEnd {
            user_id,
            user_name: String::new(),
            room_id: room_id.to_string(),
            score,
            accuracy,
            perfect,
            good,
            bad,
            miss,
            max_combo,
            full_combo,
        })
        .await;
        Ok(RoomCommandPayload::RoundResultSubmitted {
            room_id: room_id.to_string(),
            user_id,
            score,
        })
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
            if let crate::room::InternalRoomState::Playing { results, aborted } = &mut *guard {
                if results.contains_key(&user_id) {
                    return Err("already uploaded".to_string());
                }
                if !aborted.insert(user_id) {
                    return Err("already aborted".to_string());
                }
            } else {
                return Err("not in Playing state".to_string());
            }
        }
        room.send(Message::Abort { user: user_id }).await;
        room.check_all_ready().await;
        Ok(RoomCommandPayload::RoundAborted { room_id: room_id.to_string(), user_id })
    }
}

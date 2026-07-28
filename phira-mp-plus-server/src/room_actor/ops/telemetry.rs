//! Telemetry command adapters behind the Runtime gateway.
//!
//! AddTouches and AddJudges use fire-and-forget `TelemetryTouches` /
//! `TelemetryJudges` commands that skip the oneshot reply, avoiding
//! control mailbox contention (审计 P0).  SetDisplayName remains a
//! request/reply control command.

use super::super::{
    command::{RoomActorCommand, RoomCommandKind},
    RoomCommandGateway,
};
use crate::plugin::{JudgeEventItem, TouchEventPoint};
use crate::server::PlusServerState;
use serde_json::Value;
use std::time::Instant;

impl RoomCommandGateway {
    /// Cache a batch of touch data for a player — fire-and-forget.
    ///
    /// Uses `try_send` to the per-room telemetry channel without waiting
    /// for a oneshot reply, so control commands (join, leave, start, …)
    /// are never blocked by high-frequency telemetry.
    pub async fn add_touches(
        &self,
        room_id: &str,
        user_id: i32,
        touches: &[TouchEventPoint],
    ) -> Result<Value, String> {
        let rid = room_id.to_string();
        let data = touches.to_vec();
        // 审计 P0: cast fire-and-forget telemetry via try_send.
        if let Some(tx) = self.telemetry_sender(room_id).await {
            match tx.try_send(RoomActorCommand::TelemetryTouches {
                room_id: rid.clone(),
                user_id,
                touches: data,
            }) {
                Ok(()) => return Ok(serde_json::json!({"ok": true})),
                Err(_) => {
                    // Telemetry channel saturated — return error, caller will log.
                    return Err("telemetry channel full".to_string());
                }
            }
        }
        // Fall back to control mailbox path if telemetry sender unavailable.
        self
            .room_mailbox(&rid, |reply| RoomActorCommand::AddTouches {
                room_id: rid.clone(),
                user_id,
                touches: data,
                reply,
            })
            .await
            .into_untyped()
    }

    /// Cache a batch of judge data for a player — fire-and-forget.
    pub async fn add_judges(
        &self,
        room_id: &str,
        user_id: i32,
        judges: &[JudgeEventItem],
    ) -> Result<Value, String> {
        let rid = room_id.to_string();
        let data = judges.to_vec();
        if let Some(tx) = self.telemetry_sender(room_id).await {
            match tx.try_send(RoomActorCommand::TelemetryJudges {
                room_id: rid.clone(),
                user_id,
                judges: data,
            }) {
                Ok(()) => return Ok(serde_json::json!({"ok": true})),
                Err(_) => {
                    return Err("telemetry channel full".to_string());
                }
            }
        }
        self
            .room_mailbox(&rid, |reply| RoomActorCommand::AddJudges {
                room_id: rid.clone(),
                user_id,
                judges: data,
                reply,
            })
            .await
            .into_untyped()
    }

    /// Set a player's display name.
    ///
    /// Writes via the per-room mailbox to actor_state.display_names,
    /// then mirrors to Room for display in chat/results.
    pub async fn set_display_name(
        &self,
        state: &PlusServerState,
        room_id: &str,
        user_id: i32,
        name: &str,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let uname = name.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::SetDisplayName {
                room_id: rid.clone(),
                user_id,
                name: uname,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::SetDisplayName.action(), room_id, started, result)
            .into_untyped()
    }
}

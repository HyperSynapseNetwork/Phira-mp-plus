//! Telemetry command adapters behind the Runtime v2 gateway.
//!
//! AddTouches, AddJudges, and SetDisplayName route through the per-room
//! mailbox so that `player_data` and `display_names` writes are serialized
//! by the actor and mirrored in Room.

use super::super::{
    command::{RoomActorCommand, RoomCommandKind},
    RoomCommandGateway,
};
use crate::plugin::{JudgeEventItem, TouchEventPoint};
use crate::server::PlusServerState;
use serde_json::Value;
use std::time::Instant;

impl RoomCommandGateway {
    /// Cache a batch of touch data for a player.
    ///
    /// Writes via the per-room mailbox to actor_state.player_data,
    /// then mirrors to Room for plugin WASM reads.
    pub async fn add_touches(
        &self,
        state: &PlusServerState,
        room_id: &str,
        user_id: i32,
        touches: &[TouchEventPoint],
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let data = touches.to_vec();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::AddTouches {
                room_id: rid.clone(),
                user_id,
                touches: data,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::AddTouches.action(), room_id, started, result)
            .into_untyped()
    }

    /// Cache a batch of judge data for a player.
    ///
    /// Writes via the per-room mailbox to actor_state.player_data,
    /// then mirrors to Room for plugin WASM reads.
    pub async fn add_judges(
        &self,
        state: &PlusServerState,
        room_id: &str,
        user_id: i32,
        judges: &[JudgeEventItem],
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let data = judges.to_vec();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::AddJudges {
                room_id: rid.clone(),
                user_id,
                judges: data,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::AddJudges.action(), room_id, started, result)
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

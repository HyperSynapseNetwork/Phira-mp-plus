//! Membership and lifecycle room command operations.

use super::super::{
    command::{RoomActorCommand, RoomCommandKind},
    RoomCommandGateway, RoomCommandPayload,
};
use crate::{plugin::PluginEvent, server::PlusServerState};
use phira_mp_common::{Message, RoomEvent, ServerCommand};
use serde_json::Value;
use std::sync::Arc;
use std::{sync::atomic::Ordering, time::Instant};
use tracing::info;

impl RoomCommandGateway {
    /// Kick a user/monitor from a room.
    pub async fn kick_user(
        &self,
        state: &PlusServerState,
        room_id: &str,
        target_id: i32,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::KickUser {
                room_id: rid.clone(),
                target_id,
                reply,
            })
            .await;
        self.finish_command(
            state,
            RoomCommandKind::KickUser.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }


    /// Close and remove a room.
    pub async fn close_room(
        &self,
        state: &PlusServerState,
        room_id: &str,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let result = self
            .room_mailbox(room_id, |reply| RoomActorCommand::CloseRoom {
                room_id: room_id.to_string(),
                reply,
            })
            .await;
        self.finish_command(
            state,
            RoomCommandKind::CloseRoom.action(),
            room_id,
            started,
            result,
        )
        .into_untyped()
    }


    // ── AddUser ────────────────────────────────────────────────────────────

    pub async fn add_user(
        &self,
        state: &PlusServerState,
        room_id: &str,
        user_id: i32,
        user_name: &str,
        monitor: bool,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let uname = user_name.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::AddUser {
                room_id: rid.clone(),
                user_id,
                user_name: uname,
                monitor,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::AddUser.action(), room_id, started, result)
            .into_untyped()
    }


    // ── RemoveUser ──────────────────────────────────────────────────────────

    pub async fn remove_user(
        &self,
        state: &PlusServerState,
        room_id: &str,
        user_id: i32,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, |reply| RoomActorCommand::RemoveUser {
                room_id: rid.clone(),
                user_id,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::RemoveUser.action(), room_id, started, result)
            .into_untyped()
    }

}

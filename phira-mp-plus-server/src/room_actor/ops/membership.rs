use super::super::{
    command::{RoomActorCommand, RoomCommandKind, RoomOrigin},
    RoomCommandGateway,
};
use crate::server::PlusServerState;
use serde_json::Value;
use std::time::Instant;

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
            .room_mailbox(&rid, None, |reply| RoomActorCommand::KickUser {
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
            .room_mailbox(room_id, None, |reply| RoomActorCommand::CloseRoom {
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
        deadline: Instant,
        origin: RoomOrigin,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let uname = user_name.to_string();
        let result = self
            .room_mailbox(&rid, Some(deadline), |reply| RoomActorCommand::AddUser {
                room_id: rid.clone(),
                user_id,
                user_name: uname,
                monitor,
                deadline,
                origin,
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
        deadline: Option<Instant>,
        origin: RoomOrigin,
    ) -> Result<Value, String> {
        let deadline = deadline.unwrap_or_else(|| Instant::now() + std::time::Duration::from_secs(30));
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, Some(deadline), |reply| RoomActorCommand::RemoveUser {
                room_id: rid.clone(),
                user_id,
                deadline,
                origin,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::RemoveUser.action(), room_id, started, result)
            .into_untyped()
    }


    // ── SetLive ─────────────────────────────────────────────────────────────

    /// Set the live flag for the room.
    pub async fn set_live(
        &self,
        state: &PlusServerState,
        room_id: &str,
        live: bool,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let rid = room_id.to_string();
        let result = self
            .room_mailbox(&rid, None, |reply| RoomActorCommand::SetLive {
                room_id: rid.clone(),
                live,
                reply,
            })
            .await;
        self.finish_command(state, RoomCommandKind::SetLive.action(), room_id, started, result)
            .into_untyped()
    }

}

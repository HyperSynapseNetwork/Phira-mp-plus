//! Existing room operation adapters behind the Runtime v2 gateway.

mod control;
mod membership;
mod session;
mod settings;

use super::RoomCommandGateway;
use crate::server::PlusServerState;
use phira_mp_common::RoomId;
use std::sync::Arc;

impl RoomCommandGateway {
    /// Resolve a room, preferring a pre-resolved reference from the context.
    #[allow(dead_code)]
    pub(super) async fn resolve_room(
        &self,
        state: &PlusServerState,
        room_id: &str,
        preferred: Option<Arc<crate::room::Room>>,
    ) -> Result<(RoomId, Arc<crate::room::Room>), String> {
        if let Some(room) = preferred {
            let rid: RoomId = room_id
                .to_string()
                .try_into()
                .map_err(|_| "invalid room_id".to_string())?;
            let current = {
                let rooms = state.rooms.read().await;
                rooms.get(&rid).map(Arc::clone)
            }
            .ok_or_else(|| "room not found".to_string())?;
            if !Arc::ptr_eq(&current, &room) {
                return Err("room actor reference is stale".to_string());
            }
            return Ok((rid, room));
        }
        self.find_room(state, room_id).await
    }

    #[allow(dead_code)]
    pub(super) async fn find_room(
        &self,
        state: &PlusServerState,
        room_id: &str,
    ) -> Result<(RoomId, Arc<crate::room::Room>), String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let room = {
            let rooms = state.rooms.read().await;
            rooms.get(&rid).map(Arc::clone)
        }
        .ok_or_else(|| "room not found".to_string())?;
        Ok((rid, room))
    }
}

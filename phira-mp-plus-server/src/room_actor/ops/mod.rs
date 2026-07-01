//! Existing room operation adapters behind the Runtime v2 gateway.

mod control;
mod membership;
mod settings;

use super::RoomCommandGateway;
use crate::server::PlusServerState;
use phira_mp_common::RoomId;
use std::sync::Arc;

impl RoomCommandGateway {
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

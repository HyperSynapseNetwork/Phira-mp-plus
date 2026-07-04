//! Execution context for Runtime v2 room command handlers.
//!
//! The context carries the gateway, server state, and an optional room reference.
//! When commands are executed through the per-room mailbox, the room reference
//! is provided so handlers don't need to call `find_room()` again.
//! This is the first step toward true room-owned actor state.

use super::RoomCommandGateway;
use crate::server::PlusServerState;
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct RoomCommandContext<'a> {
    pub(super) gateway: &'a RoomCommandGateway,
    pub(super) state: &'a PlusServerState,
    /// Room reference set by per-room mailbox worker.
    /// When `Some`, handlers use this instead of `find_room()`.
    pub(super) room: Option<Arc<crate::room::Room>>,
}

impl<'a> RoomCommandContext<'a> {
    pub(super) fn new(gateway: &'a RoomCommandGateway, state: &'a PlusServerState) -> Self {
        Self { gateway, state, room: None }
    }

    /// Create a context with a room reference (used by per-room mailbox).
    pub(super) fn with_room(
        gateway: &'a RoomCommandGateway,
        state: &'a PlusServerState,
        room: Arc<crate::room::Room>,
    ) -> Self {
        Self { gateway, state, room: Some(room) }
    }
}

//! Execution context for Runtime v2 room command handlers.
//!
//! This is intentionally small for now.  The handler still delegates to the
//! existing `Room` state machine through `RoomCommandGateway` inline adapters,
//! but all mailbox execution now crosses an explicit context boundary.  Later
//! steps can replace the adapter calls with true room-owned actor state without
//! changing mailbox routing again.

use super::RoomCommandGateway;
use crate::server::PlusServerState;

#[derive(Clone, Copy)]
pub(super) struct RoomCommandContext<'a> {
    pub(super) gateway: &'a RoomCommandGateway,
    pub(super) state: &'a PlusServerState,
}

impl<'a> RoomCommandContext<'a> {
    pub(super) fn new(gateway: &'a RoomCommandGateway, state: &'a PlusServerState) -> Self {
        Self { gateway, state }
    }
}

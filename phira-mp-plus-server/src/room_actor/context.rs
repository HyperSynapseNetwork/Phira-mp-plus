//! Execution context for Runtime v2 room command handlers.
//!
//! The context carries the gateway, server state, an optional room reference,
//! and an optional actor reference.  When commands are executed through the
//! per-room mailbox with an actor, the actor's owned state is the authority
//! for room data.  The shared `Room` reference is still carried for side
//! effects (broadcasts, plugin events) during migration.

use super::actor::RoomActor;
use super::RoomCommandGateway;
use crate::server::PlusServerState;
use std::sync::Arc;

pub(super) struct RoomCommandContext<'a> {
    pub(super) gateway: &'a RoomCommandGateway,
    pub(super) state: &'a PlusServerState,
    /// Room reference set by per-room mailbox worker.
    /// When `Some`, handlers use this instead of `find_room()`.
    pub(super) room: Option<Arc<crate::room::Room>>,
    /// Actor reference for state modifications (migration target).
    pub(super) actor: Option<&'a mut RoomActor>,
}

impl<'a> RoomCommandContext<'a> {
    /// Create a context with a room reference (used by per-room mailbox).
    pub(super) fn with_room(
        gateway: &'a RoomCommandGateway,
        state: &'a PlusServerState,
        room: Arc<crate::room::Room>,
    ) -> Self {
        Self {
            gateway,
            state,
            room: Some(room),
            actor: None,
        }
    }

    /// Create a context with both room and actor references.
    pub(super) fn with_actor(
        gateway: &'a RoomCommandGateway,
        state: &'a PlusServerState,
        room: Arc<crate::room::Room>,
        actor: &'a mut RoomActor,
    ) -> Self {
        Self {
            gateway,
            state,
            room: Some(room),
            actor: Some(actor),
        }
    }

    /// Get a mutable reference to the actor's state, if available.
    pub(super) fn actor_state(&mut self) -> Option<&mut crate::room_actor::actor::RoomActorState> {
        self.actor
            .as_mut()
            .and_then(|a| a.actor_state.as_mut())
    }

    /// Get the actor state (panics if not available).
    pub(super) fn expect_actor_state(&mut self) -> &mut crate::room_actor::actor::RoomActorState {
        self.actor_state()
            .expect("execute_with_actor requires actor_state to be initialized")
    }
}

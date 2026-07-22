//! Execution context for Runtime v2 room command handlers.
//!
//! Carries server state, room reference for broadcasts, and actor
//! reference for state mutations.

use super::actor::RoomActor;
use crate::server::PlusServerState;
use std::sync::Arc;

pub(super) struct RoomCommandContext<'a> {
    pub(super) state: &'a PlusServerState,
    /// Room reference for broadcasts and plugin events.
    pub(super) room: Option<Arc<crate::room::Room>>,
    /// Actor reference for state modifications.
    pub(super) actor: Option<&'a mut RoomActor>,
}

impl<'a> RoomCommandContext<'a> {
    /// Create a context with room and actor references.
    pub(super) fn with_actor(
        state: &'a PlusServerState,
        room: Arc<crate::room::Room>,
        actor: &'a mut RoomActor,
    ) -> Self {
        Self {
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

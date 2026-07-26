//! Execution context for Runtime room command handlers.
//!
//! Carries a `RoomLifecycle` reference (for broadcasts, events, user access)
//! and a mutable `RoomActorState` reference (for state mutations).

use super::actor::RoomActorState;
use super::lifecycle::RoomLifecycle;

pub(super) struct RoomCommandContext<'a> {
    /// Lifecycle abstraction for room/server operations.
    pub(super) lc: &'a dyn RoomLifecycle,
    /// Mutable actor state reference for state mutations.
    as_: &'a mut RoomActorState,
}

impl<'a> RoomCommandContext<'a> {
    /// Create a new context with lifecycle and actor state references.
    pub(super) fn new(lc: &'a dyn RoomLifecycle, as_: &'a mut RoomActorState) -> Self {
        Self { lc, as_ }
    }

    /// Get a mutable reference to the actor's state (always present).
    pub(super) fn expect_actor_state(&mut self) -> &mut RoomActorState {
        self.as_
    }
}

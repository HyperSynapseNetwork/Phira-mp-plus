//! Compatibility facade for Runtime v2 persistence worker.
//!
//! The implementation now lives under `crate::persistence::*` so persistence
//! infrastructure can evolve without growing this legacy root module again.

pub use crate::persistence::{
    spawn_event_bus_mirror,
    PersistenceEvent,
    PersistenceStats,
    PersistenceTraceEntry,
    PersistenceWorker,
};

//! Re-export module for Runtime v2 persistence worker.
//!
//! The implementation lives under `crate::persistence::*` so persistence
//! infrastructure can evolve without growing a single root module.

pub use crate::persistence::{
    PersistenceEvent, PersistenceStats, PersistenceTraceEntry,
    PersistenceWorker,
};

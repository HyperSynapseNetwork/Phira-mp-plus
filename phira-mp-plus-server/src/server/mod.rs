//! Phira-mp+ server — orchestration layer and public re-exports.
//!
//! This module has been decomposed from the original 4351-line `server.rs`
//! into focused sub-modules.  The legacy `orig` module still holds code that
//! hasn't been moved yet (room management, benchmark, state query dispatch,
//! PlusServer::new).
//!
//! **Current layout:**
//!
//! | Module        | Responsibility                              |
//! |---------------|---------------------------------------------|
//! | `benchmark`   | BenchRequest, HybridBenchmarkConfig, token helpers |
//! | `config`      | PlusConfig, LiveConfig, RuntimeV2Config, … |
//! | `events`      | Event subscribers (runtime/plugin observer) |
//! | `snapshot`    | RoomSnapshot, UserSnapshot, build_snapshot  |
//! | `state`       | PlusServerState struct definition           |
//! | `orig`        | Legacy code (being gradually stripped)      |
//!
//! **Compatibility:** All `crate::server::*` items are re-exported via
//! `pub use config::*; pub use orig::*;` so existing callers keep working
//! unchanged during the migration.

pub mod benchmark;
pub mod config;
pub mod events;
pub mod query;
pub mod snapshot;
pub mod state;

// Legacy — content that hasn't been moved yet
mod orig;

// ── Re-exports for backward compatibility ──
pub use benchmark::*;
pub use config::*;
pub use orig::*;
pub use query::*;

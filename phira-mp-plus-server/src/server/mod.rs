//! Phira-mp+ server — orchestration layer and public re-exports.
//!
//! This module is being incrementally decomposed from the original 4351-line
//! `server.rs` into focused sub-modules.
//!
//! **Phase 2 layout:**
//!
//! | Module        | Responsibility                              |
//! |---------------|---------------------------------------------|
//! | `orig`        | Legacy code (being gradually stripped)      |
//! | `config`      | PlusConfig, PlusConfigCli, LiveConfig, …    |
//! | `snapshot`    | RoomSnapshot, UserSnapshot, build_snapshot  |
//! | `state`       | PlusServerState field definition            |
//!
//! **Compatibility:** All `crate::server::*` items are re-exported here so
//! existing callers keep working unchanged during the migration.

pub mod config;
pub mod snapshot;
pub mod state;

// Legacy — content that hasn't been moved yet
mod orig;

// ── Re-exports for backward compatibility ──
pub use config::*;
pub use orig::*;

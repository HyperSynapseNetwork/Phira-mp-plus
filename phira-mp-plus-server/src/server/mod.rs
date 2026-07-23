//! Phira-mp+ server — orchestration layer and public re-exports.
//!
//! This module has been decomposed from the original 4351-line `server.rs`
//! into focused sub-modules.
//!
//! **Current layout:**
//!
//! | Module        | Responsibility                              |
//! |---------------|---------------------------------------------|
//! | `accept`      | PlusServer::accept() — TCP listener accept  |
//! | `benchmark`   | BenchRequest, HybridBenchmarkConfig, token helpers, benchmark execution |
//! | `config`      | PlusConfig, LiveConfig, RuntimeConfig, … |
//! | `disconnect`  | disconnect_banned_user, run_admin_kick_user |
//! | `events`      | Event subscribers, publishers, monitor routing |
//! | `init`        | PlusServer::new() — full server initialization |
//! | `query`       | State query dispatch (sync engine for CLI/WIT/Web) |
//! | `rooms`       | Room management methods on PlusServerState  |
//! | `snapshot`    | RoomSnapshot, UserSnapshot, build_snapshot, room_snapshot |
//! | `state`       | PlusServerState, PlusServer, ServerStats struct definitions |
//!
//! **Compatibility:** All `crate::server::*` items are re-exported so
//! existing callers keep working unchanged during the migration.

pub mod accept;
pub mod benchmark;
pub mod config;
pub mod disconnect;
pub mod events;
pub mod init;
pub mod query;
pub mod rooms;
pub mod snapshot;
pub mod state;

// ── Re-exports for backward compatibility ──
pub use benchmark::*;
pub use config::*;
pub use events::*;
pub use query::*;
pub use state::*;
pub(crate) use disconnect::*;

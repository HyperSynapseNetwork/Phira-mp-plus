//! OpenUDS API module — Unix Domain Socket interface for external tools.
//!
//! Exposes ALL CLI capabilities (room management, player management, server
//! control, broadcasts, plugin control, runtime diagnostics) plus event
//! subscriptions and high-frequency data streams to external tools such as
//! PPB, bots, and admin tools.
//!
//! # Architecture
//!
//! ```text
//!                    PMP Existing Modules
//!   room_commands / ban_manager / plugin_manager / CLI / ...
//!            │                        │
//!       dispatch                    events
//!            ▼                        ▼
//!     ┌─────────────────────────────────────┐
//!     │          OpenUDS API Module          │
//!     │  dispatch.rs   events.rs  streams.rs │
//!     │             session.rs              │
//!     │        protocol.rs  auth.rs         │
//!     │             server.rs               │
//!     └─────────────────┬───────────────────┘
//!                       │ UDS
//!     ┌─────────────────▼───────────────────┐
//!     │          External Tools              │
//!     └─────────────────────────────────────┘
//! ```
//!
//! # Transport
//!
//! - Unix Domain Socket (`tokio::net::UnixListener` / `UnixStream`)
//! - Frame format: length prefix (u32 LE) + JSON (UTF-8 payload)
//! - Max payload: 16 MiB
//!
//! # Authentication
//!
//! Two modes:
//! 1. **Token mode** (auth_token set in config): Client sends
//!    `{"type":"authenticate","token":"xxx"}` → validated → `authenticated`
//! 2. **CLI approve mode** (auth_token empty): Client sends
//!    `{"type":"authenticate","client_name":"my-tool"}` → gets `pending_id`
//!    → admin runs `approve openuds <pending_id>` in CLI → `authenticated`

pub mod auth;
pub mod dispatch;
pub mod events;
pub mod protocol;
pub mod server;
pub mod session;
pub mod streams;

use crate::openuds::auth::AuthState;
use crate::openuds::streams::StreamManager;
use std::sync::Arc;
use std::sync::OnceLock;

/// Global reference to the AuthState, set when the OpenUDS server starts.
/// Used by the CLI `approve` command to approve pending connections.
static AUTH_STATE: OnceLock<Arc<AuthState>> = OnceLock::new();

/// Set the global auth state reference (called from server::start).
pub fn set_auth_state(state: Arc<AuthState>) {
    let _ = AUTH_STATE.set(state);
}

/// Get the global auth state reference (used by CLI approve command).
pub fn get_auth_state() -> Option<&'static Arc<AuthState>> {
    AUTH_STATE.get()
}

/// Global StreamManager, set when the OpenUDS server starts. Lets the
/// production touch/judge telemetry path deliver high-frequency frames to
/// OpenUDS sessions subscribed to "touches"/"judges" streams.
static STREAM_MANAGER: OnceLock<Arc<StreamManager>> = OnceLock::new();

/// Set the global stream manager (called from server::start).
pub fn set_stream_manager(mgr: Arc<StreamManager>) {
    let _ = STREAM_MANAGER.set(mgr);
}

/// Get the global stream manager reference (None until OpenUDS starts).
pub fn get_stream_manager() -> Option<&'static Arc<StreamManager>> {
    STREAM_MANAGER.get()
}

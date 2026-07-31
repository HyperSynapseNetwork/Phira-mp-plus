//! Server instance identity — a unique identifier generated at each startup.
//!
//! Used to distinguish sessions from the current server instance vs stale
//! sessions from previous (crashed) instances.  Set once during `PlusServer::new()`
//! before any connections are accepted.

use std::sync::OnceLock;

static SERVER_INSTANCE_ID: OnceLock<String> = OnceLock::new();

/// Generate and store the server instance ID on startup.
/// Panics if called more than once.
pub fn init() -> &'static str {
    let id = uuid::Uuid::new_v4().to_string();
    SERVER_INSTANCE_ID
        .set(id)
        .expect("server instance ID already initialized");
    SERVER_INSTANCE_ID.get().expect("just set")
}

/// Return the current server instance ID.
/// Returns "unknown" if `init()` has not been called yet (should not happen
/// in normal operation, but provides a safe fallback).
pub fn current() -> &'static str {
    SERVER_INSTANCE_ID.get().map(|s| s.as_str()).unwrap_or("unknown")
}

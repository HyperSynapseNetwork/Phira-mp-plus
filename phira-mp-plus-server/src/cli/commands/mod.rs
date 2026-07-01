//! CLI command-family dispatchers.
//!
//! These modules intentionally contain routing and argument validation only.
//! The actual command implementations still live on `CliHandler` for now, so
//! this split is a low-risk structural step toward smaller command modules.

pub(super) mod admin;
pub(super) mod broadcast;
pub(super) mod plugin;
pub(super) mod room;

//! Resource quota limits for plugin TCP operations.
//!
//! These constants enforce per-plugin resource limits (PMP25 P5).

/// Maximum concurrent outbound connections per plugin.
pub(crate) const MAX_CONNECTIONS_PER_PLUGIN: u32 = 32;

/// Maximum concurrent listeners per plugin.
pub(crate) const MAX_LISTENERS_PER_PLUGIN: u32 = 8;

/// Maximum buffered read bytes per connection.
pub(crate) const MAX_READ_BUF_PER_CONNECTION: usize = 1_048_576; // 1 MB

/// Maximum total buffered read bytes per plugin (across all connections).
pub(crate) const MAX_READ_BUF_PER_PLUGIN: usize = 4_194_304; // 4 MB

/// Maximum pending events per plugin before rate limiting drops new events.
pub(crate) const MAX_PENDING_EVENTS_PER_PLUGIN: usize = 64;

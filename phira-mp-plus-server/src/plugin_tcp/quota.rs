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

/// Maximum total payload bytes buffered in a plugin's event channel (across
/// both queues).  Bounds memory even when event count is within limits but
/// individual receive payloads are large (PMP38 P0-F).
pub(crate) const MAX_PENDING_EVENT_BYTES_PER_PLUGIN: usize = 4 * 1024 * 1024; // 4 MiB

/// Maximum raw payload bytes for a SINGLE receive event.  A larger chunk is
/// dropped immediately (per-event bound, P1).  Receive events that are merged
/// into an existing queued receive must also stay within this bound (P0-G).
pub(crate) const MAX_EVENT_RAW_BYTES: usize = 1024 * 1024; // 1 MiB

/// Maximum total raw payload bytes buffered for a SINGLE connection's pending
/// events.  Bounds per-connection memory even when the plugin-wide budget is
/// not exhausted (P0-G).
pub(crate) const MAX_PENDING_EVENT_BYTES_PER_CONNECTION: usize = 2 * 1024 * 1024; // 2 MiB

/// Reserved slice of the plugin event budget exclusively for lifecycle events
/// (accept/connect/error/disconnect).  A receive flood cannot consume this
/// reserve, so lifecycle events always have room (P0-F).
pub(crate) const MAX_LIFECYCLE_RESERVED_BYTES: usize = 256 * 1024; // 256 KiB

/// Per-connection sustained read rate (bytes/sec) for plugin TCP receive.
/// A token bucket of this size allows full-burst reads; sustained throughput
/// is throttled to this rate (P1: per-connection rate).
pub(crate) const MAX_RATE_BYTES_PER_SEC: usize = 5 * 1024 * 1024; // 5 MiB/s

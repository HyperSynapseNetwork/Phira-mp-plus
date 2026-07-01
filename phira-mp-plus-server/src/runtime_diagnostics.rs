//! Runtime v2 low-overhead diagnostics constants.
//!
//! These values are implementation windows for diagnostics caches, not product
//! resource throttles. They bound in-memory observation structures so status,
//! Web readonly diagnostics and TUI panels can stay cheap by default without
//! changing gameplay, persistence semantics, room/session behavior or benchmark
//! execution.

use std::time::Duration;

/// EventBus channel size for Runtime v2 observation events.
pub const EVENT_BUS_CHANNEL_CAPACITY: usize = 1024;

/// Recent EventBus trace window kept in memory for readonly diagnostics.
pub const EVENT_TRACE_WINDOW: usize = 256;

/// Recent benchmark reports kept in memory as digests/full latest entries.
pub const BENCHMARK_REPORT_HISTORY: usize = 64;

/// Default recent-list size for CLI/Web readonly diagnostics.
pub const BENCHMARK_REPORT_RECENT_DEFAULT: usize = 12;

/// Synchronous state-query timeout used by plugin/Web readonly bridge calls.
pub const RUNTIME_STATE_QUERY_TIMEOUT: Duration = Duration::from_millis(2000);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_windows_are_small_but_not_tiny() {
        assert!(EVENT_BUS_CHANNEL_CAPACITY >= 128);
        assert!(EVENT_TRACE_WINDOW >= 32);
        assert!(BENCHMARK_REPORT_HISTORY >= 8);
        assert!(BENCHMARK_REPORT_RECENT_DEFAULT <= BENCHMARK_REPORT_HISTORY);
        assert!(RUNTIME_STATE_QUERY_TIMEOUT.as_millis() >= 500);
    }
}

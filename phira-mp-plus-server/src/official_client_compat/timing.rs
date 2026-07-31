//! Response timing compatibility with the official Phira client (P0-B/P0-C).
//!
//! The official client performs `send` before it installs the response
//! callback, so an unnaturally fast PMP response can race the callback
//! installation. We compensate by enforcing a configurable minimum response
//! latency measured from command receipt. The same module carries the absolute
//! per-command actor deadline that must stay well below the client's ~7s
//! timeout.
//!
//! Invariant: **never sleep while holding a lock**. All wait helpers here are
//! pure `Instant` math plus an async `sleep`; callers must invoke them outside
//! any `Mutex`/`RwLock` guard.

use crate::server::config::PlusConfig;
use std::time::{Duration, Instant};

/// Official-client response timing knobs derived from server config.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CompatTiming {
    /// When true, responses are delayed so the total server-side latency is at
    /// least `minimum_response_latency`, simulating the official server's
    /// natural scheduling and preserving the client's callback install order.
    pub official_phira_client: bool,
    /// Floor on server-side response latency (0 when compat is disabled).
    pub minimum_response_latency: Duration,
}

impl CompatTiming {
    pub(crate) fn from_config(config: &PlusConfig) -> Self {
        Self {
            official_phira_client: config.compatibility.official_phira_client,
            minimum_response_latency: Duration::from_millis(
                config.compatibility.minimum_response_latency_ms,
            ),
        }
    }

    /// Remaining time until the minimum response latency window (measured from
    /// `received_at`) has elapsed. Never blocks — returns `Duration::ZERO`
    /// when the window already passed or the compat layer is disabled.
    pub(crate) fn remaining_minimum_latency(&self, received_at: Instant) -> Duration {
        if !self.official_phira_client {
            return Duration::ZERO;
        }
        self.minimum_response_latency
            .saturating_sub(received_at.elapsed())
    }

    /// Sleep (never under a lock) until the minimum response latency window has
    /// elapsed since `received_at`. No-op when the window already passed or the
    /// compat layer is disabled.
    pub(crate) async fn wait_until_minimum(&self, received_at: Instant) {
        let remaining = self.remaining_minimum_latency(received_at);
        if !remaining.is_zero() {
            tokio::time::sleep(remaining).await;
        }
    }
}

/// Whether an absolute actor deadline has already passed.
///
/// Callers MUST check this before mutating any authoritative state. A command
/// that arrives at the actor after its deadline must be answered with the
/// matching error response and must NOT commit state.
pub(crate) fn deadline_expired(deadline: Instant) -> bool {
    Instant::now() > deadline
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disabled_compat_never_waits() {
        let timing = CompatTiming {
            official_phira_client: false,
            minimum_response_latency: Duration::from_millis(10_000),
        };
        let received = Instant::now();
        assert_eq!(
            timing.remaining_minimum_latency(received),
            Duration::ZERO
        );
        timing.wait_until_minimum(received).await;
    }

    #[test]
    fn remaining_is_zero_after_window_elapsed() {
        let timing = CompatTiming {
            official_phira_client: true,
            minimum_response_latency: Duration::from_millis(10),
        };
        let received = Instant::now() - Duration::from_millis(50);
        assert_eq!(
            timing.remaining_minimum_latency(received),
            Duration::ZERO
        );
    }

    #[test]
    fn remaining_is_bounded_by_minimum() {
        let timing = CompatTiming {
            official_phira_client: true,
            minimum_response_latency: Duration::from_millis(10),
        };
        let received = Instant::now();
        let remaining = timing.remaining_minimum_latency(received);
        assert!(!remaining.is_zero());
        assert!(remaining <= Duration::from_millis(10));
    }

    #[tokio::test]
    async fn wait_until_minimum_enforces_the_floor() {
        let timing = CompatTiming {
            official_phira_client: true,
            minimum_response_latency: Duration::from_millis(20),
        };
        let received = Instant::now();
        timing.wait_until_minimum(received).await;
        // After waiting, the elapsed time must be at least the minimum latency.
        assert!(received.elapsed() >= Duration::from_millis(20));
    }

    #[test]
    fn deadline_expired_detects_past_deadline() {
        assert!(deadline_expired(Instant::now() - Duration::from_millis(1)));
        assert!(!deadline_expired(Instant::now() + Duration::from_secs(1)));
    }
}

//! Protocol observability counters (PMP42 P1).
//!
//! Tracks the official-client response pipeline end to end:
//!
//! ```text
//! request_received → … → response_queued → (critical) → response_flushed
//! ```
//!
//! Two counters must stay at zero in production:
//! - `silent_response_paths`: a request-type command produced no response and
//!   was not a `NoResponseExpected` (Touches/Judges) path.
//! - `late_commit`: a command reached the actor after its absolute deadline and
//!   was refused execution.
//!
//! The latency histogram records the server-side response latency from command
//! receipt to the point the response is queued, bucketed in milliseconds.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Millisecond bucket boundaries (upper bound exclusive). Bucket `i` covers
/// `[boundaries[i-1], boundaries[i])`; the final bucket covers
/// `[last_boundary, +∞)`.
const LATENCY_BOUNDARIES_MS: [u64; 8] = [1, 5, 10, 50, 100, 500, 1_000, 5_000];

/// Simple fixed-bucket latency histogram (all fields const-constructible).
#[derive(Debug)]
pub(crate) struct LatencyHistogram {
    counts: [AtomicU64; LATENCY_BOUNDARIES_MS.len() + 1],
}

impl LatencyHistogram {
    pub(crate) const fn new() -> Self {
        Self {
            counts: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
        }
    }

    pub(crate) fn record(&self, elapsed: Duration) {
        let ms = elapsed.as_millis() as u64;
        let idx = LATENCY_BOUNDARIES_MS
            .iter()
            .position(|boundary| ms < *boundary)
            .unwrap_or(self.counts.len() - 1);
        self.counts[idx].fetch_add(1, Ordering::Relaxed);
    }

    /// Total number of recorded samples (test aid).
    #[cfg(test)]
    pub(crate) fn total(&self) -> u64 {
        self.counts.iter().map(|c| c.load(Ordering::Relaxed)).sum()
    }
}

/// Global protocol trace. Process-wide counters are sufficient because the
/// server runs one process per deployment; values are `Relaxed` and only used
/// for observability and CI assertions.
#[derive(Debug)]
pub(crate) struct ProtocolTrace {
    /// Every request-type command received by the dispatch path.
    pub request_received: AtomicU64,
    /// Responses accepted into the outbound send queue.
    pub response_queued: AtomicU64,
    /// Responses that were flushed to the socket via `send_and_flush`.
    pub response_flushed: AtomicU64,
    /// Dispatch `None` results that were NOT a NoResponseExpected path.
    /// MUST be 0 under normal operation.
    pub silent_response_paths: AtomicU64,
    /// Commands that arrived at the actor after their absolute deadline and
    /// were refused execution. MUST be 0 under normal operation.
    pub late_commit: AtomicU64,
    /// Server-side response latency histogram (ms buckets).
    pub latency_histogram: LatencyHistogram,
}

pub(crate) static PROTOCOL_TRACE: ProtocolTrace = ProtocolTrace {
    request_received: AtomicU64::new(0),
    response_queued: AtomicU64::new(0),
    response_flushed: AtomicU64::new(0),
    silent_response_paths: AtomicU64::new(0),
    late_commit: AtomicU64::new(0),
    latency_histogram: LatencyHistogram::new(),
};

impl ProtocolTrace {
    pub(crate) fn get() -> &'static Self {
        &PROTOCOL_TRACE
    }

    pub(crate) fn record_response_latency(&self, received_at: Instant) {
        self.latency_histogram.record(received_at.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_buckets_by_ms() {
        let hist = LatencyHistogram::new();
        hist.record(Duration::from_micros(500)); // < 1ms
        hist.record(Duration::from_millis(3)); // 1..5
        hist.record(Duration::from_millis(60)); // 50..100
        hist.record(Duration::from_secs(10)); // overflow
        assert_eq!(hist.total(), 4);
        assert_eq!(hist.counts[0].load(Ordering::Relaxed), 1);
        assert_eq!(hist.counts[1].load(Ordering::Relaxed), 1);
        assert_eq!(hist.counts[4].load(Ordering::Relaxed), 1);
        assert_eq!(
            hist.counts[LATENCY_BOUNDARIES_MS.len()].load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn trace_counters_are_accessible() {
        let trace = ProtocolTrace::get();
        trace
            .request_received
            .fetch_add(1, Ordering::Relaxed);
        assert!(trace.request_received.load(Ordering::Relaxed) >= 1);
    }
}

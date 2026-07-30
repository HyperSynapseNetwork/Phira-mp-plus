//! High-frequency (HF) telemetry writer — bypasses WAL for Touch/Judge data.
//!
//! Touch/Judge data is high-volume and does not need the crash-recovery
//! guarantees that the WAL provides for critical events. This writer:
//!
//! 1. Receives HF items via a bounded channel (backpressure on overflow)
//! 2. Batches by configurable size and flush interval
//! 3. Writes directly to PostgreSQL (bypassing WAL entirely)
//! 4. Retries on failure up to max_retries, then drops batch
//! 5. Tracks atomic counters for observability
//!
//! # Shutdown lifecycle
//!
//! When [`HighFrequencyWriter::shutdown`] is called, a control message is sent
//! to flush all pending items and stop the background task. The same occurs
//! when the last sender is dropped (channel closed).
//!
//! # Submodules
//!
//! - `writer`   — [`HighFrequencyWriter`] struct, background loop, flush/overflow logic
//! - `postgres` — PostgreSQL COPY helpers and data preparation

use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub mod postgres;
pub mod writer;

// ── Re-exports ──────────────────────────────────────────────────────────────

pub use writer::HighFrequencyWriter;

// ── Defaults ─────────────────────────────────────────────────────────────────

const DEFAULT_CHANNEL_CAPACITY: usize = 4096;
const DEFAULT_MAX_BATCH_SIZE: usize = 256;
const DEFAULT_FLUSH_INTERVAL_MS: u64 = 500;
const DEFAULT_MAX_RETRIES: u32 = 3;
pub(crate) const HF_SCHEMA_VERSION: i32 = 3;

// ── HighFrequencyKind ────────────────────────────────────────────────────────

/// Classification of a high-frequency telemetry item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighFrequencyKind {
    Touch,
    Judge,
}

impl HighFrequencyKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Touch => "touch",
            Self::Judge => "judge",
        }
    }
}

// ── HighFrequencyConfig ──────────────────────────────────────────────────────

/// Configuration for the [`HighFrequencyWriter`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HighFrequencyConfig {
    pub channel_capacity: usize,
    pub max_batch_size: usize,
    pub flush_interval_ms: u64,
    pub max_retries: u32,
    // ── Overflow queue (审计 P2) ──────────────────────────────────
    /// Capacity of the overflow queue (0 = disabled).
    pub overflow_capacity: usize,
    /// Max age of an overflow item in milliseconds before it is dropped.
    pub overflow_max_age_ms: u64,
    // ── Retry (审计 P1) ──────────────────────────────────────────
    /// Maximum total time in milliseconds to spend retrying a failed batch
    /// before giving up (in addition to the first attempt). 0 = no limit.
    pub retry_max_age_ms: u64,
}

impl Default for HighFrequencyConfig {
    fn default() -> Self {
        Self {
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
            max_batch_size: DEFAULT_MAX_BATCH_SIZE,
            flush_interval_ms: DEFAULT_FLUSH_INTERVAL_MS,
            max_retries: DEFAULT_MAX_RETRIES,
            overflow_capacity: 1024,
            overflow_max_age_ms: 5000,
            retry_max_age_ms: 30_000,
        }
    }
}

// ── HighFrequencyItem ────────────────────────────────────────────────────────

/// A single high-frequency telemetry item.
///
/// The `payload` must contain `event_id` (for idempotent INSERT), `room_id`,
/// `count` and `data` (the actual touch/judge points).
#[derive(Debug, Clone)]
pub struct HighFrequencyItem {
    pub kind: HighFrequencyKind,
    pub round_id: String,
    pub user_id: i32,
    pub payload: Value,
    pub created_at_ms: i64,
    /// Monotonic admission sequence assigned by [`enqueue`](HighFrequencyWriter::enqueue).
    pub admission_seq: u64,
}

impl HighFrequencyItem {
    /// Extract the idempotency key from the payload.
    pub fn event_id(&self) -> String {
        self.payload
            .get("event_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    /// Extract the optional room_id from the payload.
    pub fn room_id(&self) -> Option<String> {
        self.payload
            .get("room_id")
            .and_then(Value::as_str)
            .map(|s| s.to_string())
    }

    /// Number of telemetry points inside this item.
    pub fn item_count(&self) -> usize {
        self.payload
            .get("count")
            .and_then(Value::as_u64)
            .map(|c| c as usize)
            .unwrap_or_else(|| {
                self.payload
                    .get("data")
                    .and_then(Value::as_array)
                    .map(|a| a.len())
                    .unwrap_or(1)
            })
    }
}

// ── HighFrequencyStats ───────────────────────────────────────────────────────

/// Atomic counters for the [`HighFrequencyWriter`].
///
/// All counters use relaxed ordering — they are diagnostic hints, not
/// synchronisation primitives.
#[derive(Debug)]
pub struct HighFrequencyStats {
    /// Total items received via [`enqueue`](HighFrequencyWriter::enqueue).
    pub received: AtomicU64,
    /// Total items committed to the database.
    pub committed: AtomicU64,
    /// Total items dropped after exhausting retries.
    pub dropped: AtomicU64,
    /// Total retry attempts made across all batches.
    pub retrying: AtomicU64,
    /// Timestamp (unix millis) of the oldest unflushed batch item, or 0 if none pending.
    pub oldest_batch_at: AtomicU64,
    // ── Point-level metrics (审计 P2) ──────────────────────────────
    /// Total touch/point count received across all items.
    pub received_points: AtomicU64,
    /// Total touch/point count committed to the database.
    pub committed_points: AtomicU64,
    /// Total touch/point count dropped.
    pub dropped_points: AtomicU64,
    /// Number of times the main channel was full (overflow triggered).
    pub queue_full_count: AtomicU64,
    /// Millisecond timestamp when the database last returned an error, or 0.
    pub last_database_error_at: AtomicU64,
    // ── Sequence counters (审计 P3) ────────────────────────────────
    /// Monotonic admission sequence number (incremented per accepted item).
    pub admission_sequence: AtomicU64,
    /// Highest admission sequence assigned so far (0 = none assigned yet).
    /// Updated right after `admission_sequence` is fetched-and-added so that
    /// it always reflects the *last* assigned sequence (as opposed to
    /// `admission_sequence` which is the *next* sequence to assign).
    /// Used by the Flush handler to determine the target watermark.
    pub last_accepted_sequence: AtomicU64,
    /// Highest admission_sequence whose batch has been durably committed.
    pub committed_sequence: AtomicU64,
    /// Highest admission_sequence where ALL sequences <= this value have been
    /// durably committed.  Stays behind `committed_sequence` when gaps exist
    /// (e.g. a batch was dropped, creating a missing sequence).
    pub continuous_committed_watermark: AtomicU64,
}

impl HighFrequencyStats {
    /// Take a consistent point-in-time snapshot of the counters.
    pub fn snapshot(&self) -> HighFrequencyStatsSnapshot {
        HighFrequencyStatsSnapshot {
            received: self.received.load(Ordering::Relaxed),
            committed: self.committed.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            retrying: self.retrying.load(Ordering::Relaxed),
            oldest_batch_at: self.oldest_batch_at.load(Ordering::Relaxed),
            received_points: self.received_points.load(Ordering::Relaxed),
            committed_points: self.committed_points.load(Ordering::Relaxed),
            dropped_points: self.dropped_points.load(Ordering::Relaxed),
            queue_full_count: self.queue_full_count.load(Ordering::Relaxed),
            last_database_error_at: self.last_database_error_at.load(Ordering::Relaxed),
            admission_sequence: self.admission_sequence.load(Ordering::Relaxed),
            last_accepted_sequence: self.last_accepted_sequence.load(Ordering::Relaxed),
            committed_sequence: self.committed_sequence.load(Ordering::Relaxed),
            continuous_committed_watermark: self.continuous_committed_watermark.load(Ordering::Relaxed),
        }
    }

    /// Reset all counters (used after snapshot extraction for cumulative deltas).
    pub fn reset(&self) {
        self.received.store(0, Ordering::Relaxed);
        self.committed.store(0, Ordering::Relaxed);
        self.dropped.store(0, Ordering::Relaxed);
        self.retrying.store(0, Ordering::Relaxed);
        self.oldest_batch_at.store(0, Ordering::Relaxed);
        self.received_points.store(0, Ordering::Relaxed);
        self.committed_points.store(0, Ordering::Relaxed);
        self.dropped_points.store(0, Ordering::Relaxed);
        self.queue_full_count.store(0, Ordering::Relaxed);
        self.last_database_error_at.store(0, Ordering::Relaxed);
        self.admission_sequence.store(1, Ordering::Relaxed);
        self.last_accepted_sequence.store(0, Ordering::Relaxed);
        self.committed_sequence.store(0, Ordering::Relaxed);
        self.continuous_committed_watermark.store(0, Ordering::Relaxed);
    }
}

/// Point-in-time copy of [`HighFrequencyStats`]. Exportable over JSON.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HighFrequencyStatsSnapshot {
    pub received: u64,
    pub committed: u64,
    pub dropped: u64,
    pub retrying: u64,
    pub oldest_batch_at: u64,
    // ── Point-level metrics (审计 P2) ──────────────────────────────
    pub received_points: u64,
    pub committed_points: u64,
    pub dropped_points: u64,
    pub queue_full_count: u64,
    pub last_database_error_at: u64,
    // ── Sequence counters (审计 P3) ────────────────────────────────
    pub admission_sequence: u64,
    pub last_accepted_sequence: u64,
    pub committed_sequence: u64,
    pub continuous_committed_watermark: u64,
}

// ── EnqueueOutcome ───────────────────────────────────────────────────────────

/// Result of enqueuing a high-frequency telemetry item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// Item was accepted into the main bounded channel.
    MainQueue,
    /// Item was accepted into the overflow queue (main queue was full).
    OverflowQueue,
    /// Item was dropped because both main and overflow queues were full.
    Dropped,
}

// ── Utilities ────────────────────────────────────────────────────────────────

/// Unix-millis timestamp helper.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_item(kind: HighFrequencyKind, user_id: i32) -> HighFrequencyItem {
        let event_id = uuid::Uuid::new_v4().to_string();
        HighFrequencyItem {
            kind,
            round_id: "round-1".to_string(),
            user_id,
            payload: json!({
                "event_id": event_id,
                "room_id": "room-1",
                "round_id": "round-1",
                "user_id": user_id,
                "count": 3,
                "data": [
                    {"time": 1.0, "x": 0.1, "y": 0.2},
                    {"time": 1.5, "x": 0.3, "y": 0.4},
                    {"time": 2.0, "x": 0.5, "y": 0.6},
                ],
            }),
            created_at_ms: now_ms(),
            admission_seq: 0,
        }
    }

    #[test]
    fn item_accessors_extract_from_payload() {
        let item = make_item(HighFrequencyKind::Touch, 42);
        assert_eq!(item.event_id().len(), 36);
        assert_eq!(item.room_id(), Some("room-1".to_string()));
        assert_eq!(item.item_count(), 3);
    }

    #[test]
    fn kind_as_str_returns_lowercase() {
        assert_eq!(HighFrequencyKind::Touch.as_str(), "touch");
        assert_eq!(HighFrequencyKind::Judge.as_str(), "judge");
    }

    #[test]
    fn stats_snapshot_is_consistent() {
        let stats = HighFrequencyStats {
            received: AtomicU64::new(10),
            committed: AtomicU64::new(8),
            dropped: AtomicU64::new(2),
            retrying: AtomicU64::new(1),
            oldest_batch_at: AtomicU64::new(12345),
            received_points: AtomicU64::new(30),
            committed_points: AtomicU64::new(24),
            dropped_points: AtomicU64::new(6),
            queue_full_count: AtomicU64::new(3),
            last_database_error_at: AtomicU64::new(67890),
            admission_sequence: AtomicU64::new(100),
            last_accepted_sequence: AtomicU64::new(99),
            committed_sequence: AtomicU64::new(95),
            continuous_committed_watermark: AtomicU64::new(80),
        };
        let snap = stats.snapshot();
        assert_eq!(snap.received, 10);
        assert_eq!(snap.committed, 8);
        assert_eq!(snap.dropped, 2);
        assert_eq!(snap.retrying, 1);
        assert_eq!(snap.oldest_batch_at, 12345);
        assert_eq!(snap.received_points, 30);
        assert_eq!(snap.committed_points, 24);
        assert_eq!(snap.dropped_points, 6);
        assert_eq!(snap.queue_full_count, 3);
        assert_eq!(snap.last_database_error_at, 67890);
        assert_eq!(snap.admission_sequence, 100);
        assert_eq!(snap.last_accepted_sequence, 99);
        assert_eq!(snap.committed_sequence, 95);
        assert_eq!(snap.continuous_committed_watermark, 80);
    }
}

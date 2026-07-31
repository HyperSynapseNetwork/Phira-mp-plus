//! Deferred control messages (Flush/Shutdown) for the persistence worker.
//!
//! Extracted from `worker.rs` so the worker loop stays focused on event
//! dispatch while control-message lifecycle (the deferred fence state machine)
//! lives here.

use std::time::Instant;
use tokio::sync::oneshot;

/// A deferred Flush or Shutdown control message waiting for all events with
/// `wal_sequence <= target` to reach a durable terminal state before replying.
///
/// Ownership must be preserved across re-checks (do NOT `take()` it until
/// ready or expired) — otherwise the oneshot reply sender is dropped and the
/// caller receives "acknowledgement was dropped" instead of a result.
#[derive(Debug)]
pub(crate) enum PendingControl {
    FlushReply {
        target: u64,
        reply: oneshot::Sender<Result<(), String>>,
        deadline: Instant,
    },
    Shutdown {
        target: u64,
        reply: oneshot::Sender<Result<(), String>>,
        deadline: Instant,
    },
}

impl PendingControl {
    /// The WAL sequence fence target.
    pub(crate) fn target(&self) -> u64 {
        match self {
            Self::FlushReply { target, .. } | Self::Shutdown { target, .. } => *target,
        }
    }

    /// The absolute deadline captured from the caller.
    pub(crate) fn deadline(&self) -> Instant {
        match self {
            Self::FlushReply { deadline, .. } | Self::Shutdown { deadline, .. } => *deadline,
        }
    }

    /// Whether this is a Shutdown (as opposed to Flush) control.
    pub(crate) fn is_shutdown(&self) -> bool {
        matches!(self, Self::Shutdown { .. })
    }

    /// Consume the control and return its reply sender plus whether it was a
    /// Shutdown (caller should break the worker loop).
    pub(crate) fn finish(self) -> (oneshot::Sender<Result<(), String>>, bool) {
        match self {
            Self::FlushReply { reply, .. } => (reply, false),
            Self::Shutdown { reply, .. } => (reply, true),
        }
    }
}

//! Telemetry cutover mode and decision logic.
//!
//! Controls whether production Touch/Judge telemetry goes through
//! the direct path only, or direct + worker mirror.

//! Runtime v2 high-frequency telemetry infrastructure.
//!
//! Production Touch/Judge telemetry is staged through the actor-based
//! [`TelemetryBatcher`] which batches items, flushes on interval or
//! batch-size threshold, and writes via the synchronous DB ack path.
//!
//! The cutover switch (`TelemetryCutoverMode`) controls whether items go
//! through the batcher mirror, or direct-path only, so the server can
//! observe Runtime v2 batch performance without risking data loss: the
//! direct path remains the authoritative source of truth.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, trace, warn};

const MAX_TELEMETRY_TRACE: usize = 64;
pub const TELEMETRY_SCHEMA_VERSION: i32 = 2;
static TELEMETRY_BATCH_SEQ: AtomicU64 = AtomicU64::new(1);

// ── Cutover mode & decision ──────────────────────────────────────────

/// Cutover mode controlling how production Touch/Judge telemetry is persisted.
///
/// Two modes: [`DirectOnly`] (safe default) and [`WorkerPreferred`] (direct
/// source + worker mirror). DualWrite and FallbackOnly have been removed —
/// they were development-stage comparison modes that added hot-path complexity.
/// WorkerPreferred gives runtime v2 async batch visibility while keeping the
/// direct RoundStore/db.rs path as the authoritative production fact source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryCutoverMode {
    /// Only the direct RoundStore/db.rs path writes production Touch/Judge data.
    DirectOnly,
    /// Direct RoundStore/db.rs (authoritative) + best-effort Runtime v2 worker
    /// enqueue as async mirror / batch observation.
    WorkerPreferred,
}

/// Structured decision derived from a [`TelemetryCutoverMode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelemetryCutoverDecision {
    pub mode: TelemetryCutoverMode,
    pub enqueue_worker: bool,
    pub write_direct_before_worker_result: bool,
}

impl TelemetryCutoverDecision {
    pub fn from_mode(mode: TelemetryCutoverMode) -> Self {
        match mode {
            TelemetryCutoverMode::DirectOnly => Self {
                mode,
                enqueue_worker: false,
                write_direct_before_worker_result: true,
            },
            TelemetryCutoverMode::WorkerPreferred => Self {
                mode,
                enqueue_worker: true,
                write_direct_before_worker_result: true,
            },
        }
    }

    pub fn should_write_direct_after_worker_enqueue(self, _worker_enqueue_ok: bool) -> bool {
        self.write_direct_before_worker_result
    }
}

impl Default for TelemetryCutoverMode {
    fn default() -> Self {
        // Safety: DirectOnly ensures Touches/Judges never silently drop.
        // WorkerPreferred must be explicitly opted into by the operator.
        Self::DirectOnly
    }
}

impl TelemetryCutoverMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DirectOnly => "direct_only",
            Self::WorkerPreferred => "worker_preferred",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "direct" | "direct_only" | "direct-only" => Some(Self::DirectOnly),
            "worker_preferred" | "worker-preferred" => Some(Self::WorkerPreferred),
            _ => None,
        }
    }

    pub fn should_write_direct(self) -> bool {
        self.cutover_decision().write_direct_before_worker_result
    }

    pub fn should_enqueue_worker(self) -> bool {
        self.cutover_decision().enqueue_worker
    }

    pub fn cutover_decision(self) -> TelemetryCutoverDecision {
        TelemetryCutoverDecision::from_mode(self)
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::DirectOnly => "direct RoundStore/db.rs only; Runtime v2 batcher is bypassed",
            Self::WorkerPreferred => {
                "Runtime v2 telemetry batch-write mirror + direct write for safety"
            }
        }
    }

    pub fn variants() -> &'static [TelemetryCutoverMode] {
        &[Self::DirectOnly, Self::WorkerPreferred]
    }
}

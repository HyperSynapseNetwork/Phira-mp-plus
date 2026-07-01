//! Backward-compatible facade for Runtime v2 telemetry infrastructure.
//!
//! New code should prefer `crate::telemetry::*`. This facade intentionally stays
//! tiny so existing server/config/session call sites can move gradually without
//! keeping the old monolithic batcher file alive.

pub use crate::telemetry::{
    TelemetryBatcher,
    TelemetryBatcherPolicy,
    TelemetryBatcherStats,
    TelemetryCutoverDecision,
    TelemetryCutoverMode,
    TelemetryItem,
    TelemetryKind,
    TelemetryTraceEntry,
    TELEMETRY_SCHEMA_VERSION,
};

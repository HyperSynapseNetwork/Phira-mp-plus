//! Runtime v2 telemetry infrastructure.
//!
//! Split into: cutover, batcher, tests.

mod batcher;
mod cutover;
mod tests;

pub use batcher::{
    flush_pending, next_batch_uuid, raw_item_count, run_batcher, write_runtime_telemetry_batch,
    TelemetryBatcher, TelemetryBatcherPolicy, TelemetryBatcherStats, TelemetryItem, TelemetryKind,
    TelemetryTraceEntry, MAX_TELEMETRY_TRACE, TELEMETRY_BATCH_SEQ, TELEMETRY_SCHEMA_VERSION,
};
pub use cutover::{
    TelemetryCutoverDecision, TelemetryCutoverMode,
};

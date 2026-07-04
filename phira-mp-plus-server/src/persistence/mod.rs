//! Runtime v2 persistence infrastructure.
//!
//! This module contains typed persistence envelopes, queue diagnostics, EventBus
//! mirror adapters, worker runtime and schema contracts. Keeping these pieces
//! split prevents the persistence subsystem from becoming another large bucket.

pub mod admin;
pub mod benchmark;
pub mod diagnostics;
pub mod events;
pub mod message;
pub mod mirror;
pub mod pipeline;
pub mod queries;
pub mod rounds;
pub mod schema;
pub mod simulation;
pub mod stats;
pub mod telemetry;
pub mod users;
pub mod worker;

pub use benchmark::{
    BenchmarkReportHistoryQuery, BenchmarkReportHistoryRow, BenchmarkReportPersistenceRecord,
};
pub use diagnostics::{PersistencePipeline, PersistenceQueueHealth};
pub use message::PersistenceEvent;
pub use mirror::spawn_event_bus_mirror;
pub use stats::{
    PersistenceLatencyStats, PersistenceStats, PersistenceTraceEntry, TelemetryCutoverObservation,
    TelemetryCutoverStats,
};
pub use worker::PersistenceWorker;

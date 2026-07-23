//! Persistence infrastructure.
//!
//! This module contains typed persistence envelopes, queue diagnostics, worker
//! runtime and schema contracts.

pub mod admin;
pub mod benchmark;
pub mod diagnostics;
pub mod events;
pub mod message;
pub mod queries;
pub mod rounds;
pub mod schema;
pub mod simulation;
pub mod stats;
pub mod telemetry;
pub mod users;
pub mod wal;
pub mod worker;

pub use benchmark::{
    BenchmarkReportHistoryQuery, BenchmarkReportHistoryRow, BenchmarkReportPersistenceRecord,
};
pub use diagnostics::{PersistencePipeline, PersistenceQueueHealth};
pub use message::PersistenceEvent;
pub use stats::{
    PersistenceLatencyStats, PersistenceStats, PersistenceTraceEntry,
};
pub use worker::PersistenceWorker;

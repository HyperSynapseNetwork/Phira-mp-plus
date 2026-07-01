//! Runtime v2 persistence infrastructure.
//!
//! This module contains typed persistence envelopes and schema contracts shared
//! by `persistence_worker.rs` and `db.rs`.  The current worker remains the public
//! runtime facade for now; these smaller modules prevent the next persistence
//! phase from turning one file into another large bucket.

pub mod benchmark;
pub mod diagnostics;
pub mod schema;

pub use benchmark::{BenchmarkReportHistoryQuery, BenchmarkReportHistoryRow, BenchmarkReportPersistenceRecord};
pub use diagnostics::{PersistencePipeline, PersistenceQueueHealth};

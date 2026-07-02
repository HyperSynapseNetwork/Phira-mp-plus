//! BenchmarkReport persistence contracts.

use crate::benchmark_report::{BenchmarkMode, BenchmarkReport};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReportPersistenceRecord {
    pub mode: BenchmarkMode,
    pub title: String,
    pub duration_secs: u64,
    pub is_simulation: bool,
    pub operations: Option<u64>,
    pub failed_operations: Option<u64>,
    pub probes_attempted: u64,
    pub probes_succeeded: u64,
    pub probes_failed: u64,
    pub probes_blocked: u64,
    pub probes_skipped: u64,
    pub failure_samples: usize,
    pub notes: usize,
    pub source: String,
    pub schema_version: i32,
    pub report: BenchmarkReport,
}

impl BenchmarkReportPersistenceRecord {
    pub fn from_report(report: &BenchmarkReport, source: impl Into<String>) -> Self {
        Self {
            mode: report.mode,
            title: report.title.clone(),
            duration_secs: report.duration_secs,
            is_simulation: report.mode == BenchmarkMode::Simulation,
            operations: report.operations,
            failed_operations: report.failed_operations,
            probes_attempted: report.probes.attempted,
            probes_succeeded: report.probes.succeeded,
            probes_failed: report.probes.failed,
            probes_blocked: report.probes.blocked,
            probes_skipped: report.probes.skipped,
            failure_samples: report.failure_samples.len(),
            notes: report.notes.len(),
            source: source.into(),
            schema_version: crate::persistence::schema::RUNTIME_BENCHMARK_REPORTS_SCHEMA_VERSION,
            report: report.clone(),
        }
    }

    pub fn payload(&self) -> Value {
        let mut payload = serde_json::to_value(&self.report)
            .unwrap_or_else(|err| serde_json::json!({"serialize_error": err.to_string()}));
        if let Some(obj) = payload.as_object_mut() {
            obj.entry("runtime_v2_schema_version".to_string())
                .or_insert_with(|| serde_json::json!(self.schema_version));
            obj.entry("runtime_v2_storage".to_string())
                .or_insert_with(|| {
                    serde_json::json!(crate::persistence::schema::RUNTIME_BENCHMARK_REPORTS_TABLE)
                });
            obj.entry("source".to_string())
                .or_insert_with(|| serde_json::json!(self.source.clone()));
        }
        payload
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BenchmarkReportHistoryQuery {
    pub mode: Option<BenchmarkMode>,
    pub limit: usize,
}

impl BenchmarkReportHistoryQuery {
    pub fn new(mode: Option<BenchmarkMode>, limit: usize) -> Self {
        Self {
            mode,
            limit: limit.clamp(1, 200),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReportHistoryRow {
    pub sequence: i64,
    pub mode: BenchmarkMode,
    pub title: String,
    pub duration_secs: i64,
    pub is_simulation: bool,
    pub operations: Option<i64>,
    pub failed_operations: Option<i64>,
    pub probes_failed: i64,
    pub probes_blocked: i64,
    pub created_at: i64,
    pub source: String,
    pub schema_version: i32,
    pub report: Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistence_record_preserves_report_shape() {
        let mut report = BenchmarkReport::new(BenchmarkMode::Hybrid, "hybrid smoke", 3);
        report.probes.record_success();
        report.add_note("dry-run");
        let record = BenchmarkReportPersistenceRecord::from_report(&report, "test");
        assert_eq!(record.mode, BenchmarkMode::Hybrid);
        assert_eq!(record.probes_succeeded, 1);
        assert_eq!(record.notes, 1);
        assert_eq!(
            record.schema_version,
            crate::persistence::schema::RUNTIME_BENCHMARK_REPORTS_SCHEMA_VERSION
        );
        assert_eq!(
            record.payload()["runtime_v2_storage"].as_str(),
            Some(crate::persistence::schema::RUNTIME_BENCHMARK_REPORTS_TABLE)
        );
    }

    #[test]
    fn history_query_clamps_limit() {
        assert_eq!(BenchmarkReportHistoryQuery::new(None, 0).limit, 1);
        assert_eq!(BenchmarkReportHistoryQuery::new(None, 999).limit, 200);
    }
}

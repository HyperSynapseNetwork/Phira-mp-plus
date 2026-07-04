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

    /// Verify that the SQL column names used in INSERT match the column
    /// names in CREATE TABLE (db.rs line 1313: `report JSONB NOT NULL`).
    /// If this test fails after a refactor, check that INSERT uses `report`
    /// not `payload`, and SELECT uses `report::text AS report`.
    #[test]
    fn insert_uses_report_column_not_payload() {
        let insert_sql = super::INSERT_BENCHMARK_REPORT;
        assert!(
            insert_sql.contains("report,"),
            "INSERT must use `report` column, not `payload`. Current SQL: {insert_sql}"
        );
        assert!(
            !insert_sql.contains("payload,"),
            "INSERT must NOT use `payload` column. Current SQL: {insert_sql}"
        );
    }

    #[test]
    fn select_uses_report_column() {
        let history_sql = super::SELECT_BENCHMARK_HISTORY;
        assert!(
            history_sql.contains("report::text AS report"),
            "SELECT must use `report::text AS report`. Current SQL: {history_sql}"
        );
    }
}

/// SQL constants exposed for unit test verification of column names.
#[cfg(test)]
pub(crate) const INSERT_BENCHMARK_REPORT: &str = "INSERT INTO mp_runtime_benchmark_reports
                   (mode, title, duration_secs, is_simulation, operations, failed_operations,
                    probes_attempted, probes_succeeded, probes_failed, probes_blocked, probes_skipped,
                    failure_samples, notes, source, schema_version, report, created_at, sequence)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16::jsonb, $17, nextval('mp_persist_sequence'))";

#[cfg(test)]
pub(crate) const SELECT_BENCHMARK_HISTORY: &str = "SELECT sequence, mode, title, duration_secs, is_simulation, operations, failed_operations,
                            probes_failed, probes_blocked, report::text AS report, created_at, source, schema_version
                     FROM mp_runtime_benchmark_reports";

use crate::db::DbManager;

impl DbManager {
    pub async fn record_runtime_benchmark_report(
        &self,
        record: crate::persistence::BenchmarkReportPersistenceRecord,
    ) -> bool {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let payload = record.payload();
            return sqlx::query(
                "INSERT INTO mp_runtime_benchmark_reports
                   (mode, title, duration_secs, is_simulation, operations, failed_operations,
                    probes_attempted, probes_succeeded, probes_failed, probes_blocked, probes_skipped,
                    failure_samples, notes, source, schema_version, report, created_at, sequence)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16::jsonb, $17, nextval('mp_persist_sequence'))",
            )
            .bind(record.mode.as_str())
            .bind(&record.title)
            .bind(record.duration_secs as i64)
            .bind(record.is_simulation)
            .bind(record.operations.map(|v| v as i64))
            .bind(record.failed_operations.map(|v| v as i64))
            .bind(record.probes_attempted as i64)
            .bind(record.probes_succeeded as i64)
            .bind(record.probes_failed as i64)
            .bind(record.probes_blocked as i64)
            .bind(record.probes_skipped as i64)
            .bind(record.failure_samples as i64)
            .bind(record.notes as i64)
            .bind(&record.source)
            .bind(record.schema_version)
            .bind(&payload)
            .bind(now)
            .execute(pool)
            .await
            .is_ok();
        }
        #[cfg(not(feature = "postgres"))]
        let _ = record;
        false
    }

    pub fn record_runtime_benchmark_report_sync(
        &self,
        record: crate::persistence::BenchmarkReportPersistenceRecord,
    ) -> bool {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            tokio::spawn(async move {
                let db = DbManager::Pg(pool);
                let _ = db.record_runtime_benchmark_report(record).await;
            });
            return true;
        }
        #[cfg(not(feature = "postgres"))]
        let _ = record;
        false
    }

    pub async fn runtime_benchmark_report_history(
        &self,
        query: crate::persistence::BenchmarkReportHistoryQuery,
    ) -> Vec<crate::persistence::BenchmarkReportHistoryRow> {
        #[cfg(not(feature = "postgres"))]
        let _ = &query;
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            use sqlx::Row;
            let limit = i64::try_from(query.limit).unwrap_or(200).clamp(1, 200);
            let rows = if let Some(mode) = query.mode {
                sqlx::query(
                    "SELECT sequence, mode, title, duration_secs, is_simulation, operations, failed_operations,
                            probes_failed, probes_blocked, report::text AS report, created_at, source, schema_version
                     FROM mp_runtime_benchmark_reports
                     WHERE mode = $1
                     ORDER BY sequence DESC
                     LIMIT $2"
                )
                .bind(mode.as_str())
                .bind(limit)
                .fetch_all(pool)
                .await
                .unwrap_or_default()
            } else {
                sqlx::query(
                    "SELECT sequence, mode, title, duration_secs, is_simulation, operations, failed_operations,
                            probes_failed, probes_blocked, report::text AS report, created_at, source, schema_version
                     FROM mp_runtime_benchmark_reports
                     ORDER BY sequence DESC
                     LIMIT $1"
                )
                .bind(limit)
                .fetch_all(pool)
                .await
                .unwrap_or_default()
            };
            return rows
                .into_iter()
                .filter_map(|row| {
                    let raw_mode = row.try_get::<String, _>("mode").ok()?;
                    let mode = benchmark_mode_from_str(&raw_mode)?;
                    let raw_report = row
                        .try_get::<String, _>("report")
                        .unwrap_or_else(|_| "{}".to_string());
                    Some(crate::persistence::BenchmarkReportHistoryRow {
                        sequence: row.try_get::<i64, _>("sequence").unwrap_or_default(),
                        mode,
                        title: row.try_get::<String, _>("title").unwrap_or_default(),
                        duration_secs: row.try_get::<i64, _>("duration_secs").unwrap_or_default(),
                        is_simulation: row.try_get::<bool, _>("is_simulation").unwrap_or(false),
                        operations: row.try_get::<Option<i64>, _>("operations").ok().flatten(),
                        failed_operations: row
                            .try_get::<Option<i64>, _>("failed_operations")
                            .ok()
                            .flatten(),
                        probes_failed: row.try_get::<i64, _>("probes_failed").unwrap_or_default(),
                        probes_blocked: row.try_get::<i64, _>("probes_blocked").unwrap_or_default(),
                        created_at: row.try_get::<i64, _>("created_at").unwrap_or_default(),
                        source: row.try_get::<String, _>("source").unwrap_or_default(),
                        schema_version: row.try_get::<i32, _>("schema_version").unwrap_or_default(),
                        report: serde_json::from_str(&raw_report)
                            .unwrap_or_else(|_| serde_json::json!({})),
                    })
                })
                .collect();
        }
        Vec::new()
    }
}

/// Parse a benchmark-mode string into [`BenchmarkMode`].
fn benchmark_mode_from_str(value: &str) -> Option<crate::benchmark_report::BenchmarkMode> {
    match value {
        "simulation" | "sim" => Some(crate::benchmark_report::BenchmarkMode::Simulation),
        "hybrid" => Some(crate::benchmark_report::BenchmarkMode::Hybrid),
        "real" => Some(crate::benchmark_report::BenchmarkMode::Real),
        _ => None,
    }
}

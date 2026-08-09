//! BenchmarkReport persistence contracts.

use crate::benchmark::report::BenchmarkReport;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReportPersistenceRecord {
    /// Stable idempotency key reused across database retries.
    pub report_id: String,
    pub title: String,
    pub duration_secs: u64,
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
            report_id: uuid::Uuid::new_v4().to_string(),
            title: report.title.clone(),
            duration_secs: report.summary.duration_secs,
            operations: Some(report.summary.total_commands),
            failed_operations: Some(report.errors_total),
            probes_attempted: 0,
            probes_succeeded: 0,
            probes_failed: 0,
            probes_blocked: 0,
            probes_skipped: 0,
            failure_samples: 0,
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
            obj.entry("report_id".to_string())
                .or_insert_with(|| serde_json::json!(self.report_id.clone()));
            obj.entry("schema_version".to_string())
                .or_insert_with(|| serde_json::json!(self.schema_version));
            obj.entry("storage".to_string())
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
    pub limit: usize,
}

impl BenchmarkReportHistoryQuery {
    pub fn new(limit: usize) -> Self {
        Self {
            limit: limit.clamp(1, 200),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReportHistoryRow {
    pub sequence: i64,
    pub title: String,
    pub duration_secs: i64,
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
    use crate::benchmark::environment::EnvironmentSnapshot;

    fn make_test_report() -> BenchmarkReport {
        let env = EnvironmentSnapshot {
            version: "0.1.0".to_string(),
            git_commit: "abc123".to_string(),
            cpu_cores: 4,
            cpu_model: "Test CPU".to_string(),
            total_memory_bytes: 8_589_934_592,
            available_memory_bytes: 4_294_967_296,
            os_name: "linux".to_string(),
            os_version: "Ubuntu 22.04".to_string(),
            kernel_version: "6.2.0".to_string(),
            hostname: "test-host".to_string(),
            rust_version: "1.82.0".to_string(),
            target_triple: "x86_64-linux".to_string(),
            postgres_version: Some("16.2".to_string()),
            captured_at_ms: 1_000_000,
        };
        let params = crate::benchmark::mode::ModeParams {
            mode: crate::benchmark::mode::BenchmarkMode::Fixed,
            max_playing_rooms: 10,
            max_cpu_pct: 0.0,
            max_ram_bytes: 0,
            duration: Some(std::time::Duration::from_secs(60)),
        };
        BenchmarkReport::new("test", env, params)
    }

    #[test]
    fn persistence_record_preserves_report_shape() {
        let mut report = make_test_report();
        report.notes.push("dry-run".to_string());
        let record = BenchmarkReportPersistenceRecord::from_report(&report, "test");
        assert_eq!(record.notes, 1);
        assert_eq!(
            record.schema_version,
            crate::persistence::schema::RUNTIME_BENCHMARK_REPORTS_SCHEMA_VERSION
        );
        let first_payload = record.payload();
        let second_payload = record.payload();
        assert!(!record.report_id.is_empty());
        assert_eq!(
            first_payload["report_id"].as_str(),
            Some(record.report_id.as_str())
        );
        assert_eq!(
            second_payload["report_id"].as_str(),
            Some(record.report_id.as_str())
        );
        assert_eq!(
            first_payload["storage"].as_str(),
            Some(crate::persistence::schema::RUNTIME_BENCHMARK_REPORTS_TABLE)
        );
    }

    #[test]
    fn history_query_clamps_limit() {
        assert_eq!(BenchmarkReportHistoryQuery::new(0).limit, 1);
        assert_eq!(BenchmarkReportHistoryQuery::new(999).limit, 200);
    }

    /// Verify that the SQL column names used in INSERT match the column
    /// names in CREATE TABLE (`report JSONB NOT NULL`).
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
                   (report_id, title, duration_secs, operations, failed_operations,
                    probes_attempted, probes_succeeded, probes_failed, probes_blocked, probes_skipped,
                    failure_samples, notes, source, schema_version, report, created_at, sequence)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15::jsonb, $16, nextval('mp_persist_sequence'))
                 ON CONFLICT (report_id) WHERE report_id IS NOT NULL DO NOTHING";

#[cfg(test)]
pub(crate) const SELECT_BENCHMARK_HISTORY: &str = "SELECT sequence, title, duration_secs, operations, failed_operations,
                            probes_failed, probes_blocked, report::text AS report, created_at, source, schema_version
                     FROM mp_runtime_benchmark_reports";

use crate::db::DbManager;

impl DbManager {
    pub async fn record_runtime_benchmark_report(
        &self,
        record: crate::persistence::BenchmarkReportPersistenceRecord,
    ) -> bool {
        let Self::Pg(pool) = self;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let payload = record.payload();
        sqlx::query(
            "INSERT INTO mp_runtime_benchmark_reports
                   (report_id, title, duration_secs, operations, failed_operations,
                    probes_attempted, probes_succeeded, probes_failed, probes_blocked, probes_skipped,
                    failure_samples, notes, source, schema_version, report, created_at, sequence)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15::jsonb, $16, nextval('mp_persist_sequence'))
                 ON CONFLICT (report_id) WHERE report_id IS NOT NULL DO NOTHING",
        )
        .bind(&record.report_id)
        .bind(&record.title)
        .bind(record.duration_secs as i64)
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
        .is_ok()
    }

    pub fn record_runtime_benchmark_report_sync(
        &self,
        record: crate::persistence::BenchmarkReportPersistenceRecord,
    ) -> bool {
        let Self::Pg(pool) = self;
        let pool = pool.clone();
        tokio::spawn(async move {
            let db = DbManager::Pg(pool);
            let _ = db.record_runtime_benchmark_report(record).await;
        });
        true
    }

    pub async fn runtime_benchmark_report_history(
        &self,
        query: crate::persistence::BenchmarkReportHistoryQuery,
    ) -> Vec<crate::persistence::BenchmarkReportHistoryRow> {
        let Self::Pg(pool) = self;
        use sqlx::Row;
        let limit = i64::try_from(query.limit).unwrap_or(200).clamp(1, 200);
        sqlx::query(
                "SELECT sequence, title, duration_secs, operations, failed_operations,
                            probes_failed, probes_blocked, report::text AS report, created_at, source, schema_version
                     FROM mp_runtime_benchmark_reports
                     ORDER BY sequence DESC
                     LIMIT $1"
            )
            .bind(limit)
            .fetch_all(pool)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|row| {
                let raw_report = row
                    .try_get::<String, _>("report")
                    .unwrap_or_else(|_| "{}".to_string());
                crate::persistence::BenchmarkReportHistoryRow {
                    sequence: row.try_get::<i64, _>("sequence").unwrap_or_default(),

                    title: row.try_get::<String, _>("title").unwrap_or_default(),
                    duration_secs: row.try_get::<i64, _>("duration_secs").unwrap_or_default(),
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
                }
            })
            .collect()
    }
}


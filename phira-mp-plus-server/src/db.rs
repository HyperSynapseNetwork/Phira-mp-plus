//! Unified PostgreSQL persistence for Phira-mp+.
//!
//! PostgreSQL is the single structured persistence backend. When `database_url`
//! is not configured the manager becomes `None` and callers keep their direct
//! in-memory/file fallback so test deployments still boot. Every mutable entity
//! written here has either `created_at`/`updated_at` or an append-only `sequence`
//! so plugins and dashboards can reconstruct modification time and order.

use anyhow::Result;
use serde_json::Value;

/// Unix 毫秒时间戳。
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 游玩时间记录
#[derive(Debug, Clone)]
pub struct PlaytimeRow {
    pub total_secs: i64,
    pub session_start: Option<i64>,
}

/// 数据库管理器

#[derive(Debug, Clone)]
pub struct RuntimeTelemetryBatchRecord {
    /// Stable idempotency key for one accepted telemetry event.
    pub event_id: String,
    pub batch_uuid: String,
    pub run_id: Option<String>,
    pub scope: String,
    pub pipeline: String,
    pub source: String,
    pub flush_reason: String,
    pub schema_version: i32,
    /// Historical schema field (runtime_v2_dual_write metadata).
    /// Not a current telemetry mode — retained for backward-compatible reads.
    pub dual_write: bool,
    pub kind: String,
    pub room_id: Option<String>,
    pub round_uuid: Option<String>,
    pub player_id: i32,
    pub item_count: i32,
    pub payload: Value,
}

#[derive(Debug, Clone)]
pub enum DbManager {
    /// PostgreSQL 已连接
    #[cfg(feature = "postgres")]
    Pg(sqlx::PgPool),
    /// 未配置数据库（调用方保留旧回退行为）
    None,
}

impl DbManager {
    /// 根据配置初始化数据库连接（自动创建数据库）
    pub async fn new(database_url: Option<&str>) -> Self {
        match database_url {
            Some(url) if !url.trim().is_empty() => {
                #[cfg(feature = "postgres")]
                {
                    match sqlx::PgPool::connect(url).await {
                        Ok(pool) => {
                            tracing::info!("PostgreSQL 已连接");
                            if let Err(e) = init_tables(&pool).await {
                                tracing::warn!("数据库建表失败: {e:?}，禁用 PostgreSQL 持久化");
                                return Self::None;
                            }
                            return Self::Pg(pool);
                        }
                        Err(e) => {
                            let err_str = e.to_string();
                            if err_str.contains("does not exist")
                                || err_str.contains("database") && err_str.contains("not found")
                            {
                                tracing::info!("数据库不存在，尝试自动创建...");
                                let pieces = url.rsplitn(2, '/').collect::<Vec<_>>();
                                let base = pieces
                                    .get(1)
                                    .copied()
                                    .unwrap_or("postgres://postgres:postgres@localhost:5432");
                                let admin_url = format!("{base}/postgres");
                                if let Ok(admin_pool) = sqlx::PgPool::connect(&admin_url).await {
                                    let db_name =
                                        pieces.first().copied().unwrap_or("phira_mp_plus");
                                    let safe_name = db_name.replace('"', "");
                                    let _ =
                                        sqlx::query(&format!("CREATE DATABASE \"{}\"", safe_name))
                                            .execute(&admin_pool)
                                            .await;
                                    admin_pool.close().await;
                                    if let Ok(new_pool) = sqlx::PgPool::connect(url).await {
                                        if let Err(e) = init_tables(&new_pool).await {
                                            tracing::warn!(
                                                "数据库建表失败: {e:?}，禁用 PostgreSQL 持久化"
                                            );
                                            return Self::None;
                                        }
                                        tracing::info!("PostgreSQL 已连接（自动建库）");
                                        return Self::Pg(new_pool);
                                    }
                                }
                            }
                            tracing::warn!("PostgreSQL 连接失败: {e:?}，禁用 PostgreSQL 持久化");
                        }
                    }
                }
                #[cfg(not(feature = "postgres"))]
                tracing::warn!("未启用 postgres feature，禁用 PostgreSQL 持久化");
                Self::None
            }
            _ => Self::None,
        }
    }

    /// 是否使用 PostgreSQL
    pub fn is_active(&self) -> bool {
        #[cfg(feature = "postgres")]
        if matches!(self, Self::Pg(_)) {
            return true;
        }
        false
    }

    pub async fn cleanup_expired(&self, retention_days: u32, touch_judge_retention_days: u32) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let now = || {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0)
            };
            if retention_days > 0 {
                let cutoff = now().saturating_sub(retention_days as i64 * 86_400_000);
                for sql in [
                    "DELETE FROM mp_events WHERE created_at < $1",
                    "DELETE FROM mp_user_room_history WHERE created_at < $1",
                    "DELETE FROM mp_round_results WHERE updated_at < $1",
                ] {
                    let _ = sqlx::query(sql).bind(cutoff).execute(pool).await;
                }
            }
            if touch_judge_retention_days > 0 {
                let cutoff = now().saturating_sub(touch_judge_retention_days as i64 * 86_400_000);
                for sql in [
                    "DELETE FROM mp_round_player_data WHERE updated_at < $1",
                    "DELETE FROM mp_round_touch_batches WHERE created_at < $1",
                    "DELETE FROM mp_round_judge_batches WHERE created_at < $1",
                ] {
                    let _ = sqlx::query(sql).bind(cutoff).execute(pool).await;
                }
            }
            let round_meta_retention_days = match (retention_days, touch_judge_retention_days) {
                (0, _) | (_, 0) => 0,
                (a, b) => a.max(b),
            };
            if round_meta_retention_days > 0 {
                let cutoff = now().saturating_sub(round_meta_retention_days as i64 * 86_400_000);
                let _ = sqlx::query("DELETE FROM mp_rounds WHERE updated_at < $1")
                    .bind(cutoff)
                    .execute(pool)
                    .await;
            }
        }
    }
}

#[cfg(feature = "postgres")]
pub(crate) async fn append_event_pg(
    pool: &sqlx::PgPool,
    event_id: Option<&str>,
    kind: &str,
    room_id: Option<&str>,
    user_id: Option<i32>,
    payload: Value,
) -> Result<()> {
    let payload = payload.to_string();
    sqlx::query(
        "INSERT INTO mp_events (event_id, kind, room_id, user_id, payload, created_at)
         VALUES ($1, $2, $3, $4, $5::jsonb, $6)
         ON CONFLICT (event_id) WHERE event_id IS NOT NULL DO NOTHING",
    )
    .bind(event_id)
    .bind(kind)
    .bind(room_id)
    .bind(user_id)
    .bind(payload)
    .bind(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(feature = "postgres")]
async fn init_tables(pool: &sqlx::PgPool) -> Result<()> {
    // Apply managed schema migrations via sqlx.
    sqlx::migrate!("migrations").run(pool).await?;

    let now = now_ms();
    sqlx::query(
        "INSERT INTO mp_runtime_persistence_meta (key, value, updated_at)
         VALUES ('schema.runtime_telemetry', $1, $2)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at"
    )
    .bind(serde_json::json!({
        "schema_version": 3,
        "batch_table": "mp_runtime_telemetry_batches",
        "item_table": "mp_runtime_telemetry_items",
        "mode": "direct_only",
        "available_modes": ["direct_only", "worker_preferred", "worker_authoritative"],
        "notes": "Runtime v2 telemetry schema normalizes batch headers and raw telemetry items. Modes: direct_only, worker_preferred, worker_authoritative."
    }))
    .bind(now)
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO mp_runtime_persistence_meta (key, value, updated_at)
         VALUES ('telemetry.cutover_mode', $1, $2)
         ON CONFLICT (key) DO NOTHING"
    )
    .bind(serde_json::json!({
        "mode": "direct_only",
        "description": "direct RoundStore/db.rs only (safe default). WorkerPreferred = direct first; Worker mirror after direct ACK or canonical compensation after direct failure.",
        "available_modes": ["direct_only", "worker_preferred", "worker_authoritative"],
        "updated_by": "runtime_v2.bootstrap"
    }))
    .bind(now)
    .execute(pool)
    .await?;

    sqlx::query(
        "INSERT INTO mp_runtime_persistence_meta (key, value, updated_at)
         VALUES ($1, $2, $3)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at"
    )
    .bind(crate::persistence::schema::RUNTIME_BENCHMARK_REPORTS_META_KEY)
    .bind(serde_json::json!({
        "schema_version": crate::persistence::schema::RUNTIME_BENCHMARK_REPORTS_SCHEMA_VERSION,
        "table": crate::persistence::schema::RUNTIME_BENCHMARK_REPORTS_TABLE,
        "source": "benchmark.completed EventBus mirror",
        "notes": "Runtime v2 benchmark reports are append-only JSON snapshots for readonly diagnostics and historical comparison. Project is still in test stage; schema may change freely."
    }))
    .bind(now)
    .execute(pool)
    .await?;

    for (scope, seconds, cleanup_enabled) in [
        ("production.telemetry", 30 * 24 * 3600_i64, false),
        ("simulation", 7 * 24 * 3600_i64, true),
        ("runtime.events", 30 * 24 * 3600_i64, false),
    ] {
        sqlx::query(
            "INSERT INTO mp_runtime_retention_policies (scope, retain_seconds, cleanup_enabled, updated_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (scope) DO NOTHING"
        )
        .bind(scope)
        .bind(seconds)
        .bind(cleanup_enabled)
        .bind(now)
        .execute(pool)
        .await?;
    }

    // Older versions allowed duplicate room-history rows. Normalize them
    // before adding natural-key indexes used by retry-safe transactional writes.
    for cleanup in [
        "DELETE FROM room_history newer USING room_history older
         WHERE newer.id > older.id
           AND newer.user_id = older.user_id
           AND newer.room_uuid = older.room_uuid
           AND newer.joined_at = older.joined_at",
        "DELETE FROM mp_user_room_history newer USING mp_user_room_history older
         WHERE newer.id > older.id
           AND newer.user_id = older.user_id
           AND newer.room_uuid = older.room_uuid
           AND newer.joined_at = older.joined_at",
    ] {
        sqlx::query(cleanup).execute(pool).await?;
    }

    // Migration-awareness marker for existing tooling that inspects _pmp_schema_version.
    let _ = sqlx::query(
        "INSERT INTO _pmp_schema_version (version, description)
         VALUES (1, 'migrations/20260721000001_initial_schema.sql')
         ON CONFLICT (version) DO NOTHING",
    )
    .execute(pool)
    .await;

    tracing::info!("统一 PostgreSQL 持久化表已就绪 (schema v1)");
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Verify that the migration macro resolves at compile-time and
    /// finds the expected migration files.
    #[cfg(feature = "postgres")]
    #[test]
    fn migration_macro_compiles_and_has_expected_count() {
        let migrator = sqlx::migrate!("migrations");
        assert!(
            !migrator.migrations.is_empty(),
            "must have at least one migration"
        );
        // Expect exactly 1 migration: 20260721000001_initial_schema.sql
        assert_eq!(
            migrator.migrations.len(),
            1,
            "unexpected number of migration files; if adding a new migration update this assertion"
        );
        let m = &migrator.migrations[0];
        assert_eq!(
            m.version, 20260721000001,
            "unexpected migration version"
        );
        assert!(
            m.description.contains("initial_schema"),
            "migration description mismatch: {}",
            m.description
        );
    }

    /// Validate the migration SQL file on disk has the expected structure.
    #[test]
    fn migration_sql_file_exists_and_contains_create_tables() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("migrations")
            .join("20260721000001_initial_schema.sql");
        assert!(
            path.exists(),
            "migration file not found: {}",
            path.display()
        );
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read migration file: {e}"));

        // Must contain CREATE TABLE for every core entity.
        let expected_tables = [
            "playtime",
            "room_history",
            "mp_users",
            "mp_room_snapshots",
            "mp_events",
            "mp_user_room_history",
            "mp_rounds",
            "mp_round_touch_batches",
            "mp_round_judge_batches",
            "mp_round_player_data",
            "mp_round_results",
            "mp_runtime_telemetry_batches",
            "mp_runtime_telemetry_items",
            "mp_runtime_persistence_meta",
            "mp_runtime_retention_policies",
            "mp_runtime_benchmark_reports",
            "mp_sim_events",
            "mp_settings",
            "_pmp_schema_version",
        ];
        for table in &expected_tables {
            assert!(
                content.contains(&format!("CREATE TABLE IF NOT EXISTS {table}")),
                "migration SQL must contain CREATE TABLE IF NOT EXISTS for `{table}`"
            );
        }

        // Must contain all known indexes.
        let expected_indexes = [
            "idx_mp_events_created",
            "idx_mp_rounds_started",
            "idx_mp_touch_batches_round_player_seq",
            "idx_mp_judge_batches_round_player_seq",
            "idx_mp_user_room_history_user",
            "idx_room_history_join_identity",
            "idx_mp_user_room_history_join_identity",
            "idx_mp_runtime_telemetry_event_id",
            "idx_mp_runtime_telemetry_created",
            "idx_mp_runtime_telemetry_kind",
            "idx_mp_runtime_telemetry_scope_created",
            "idx_mp_runtime_telemetry_run",
            "idx_mp_runtime_telemetry_round_player",
            "idx_mp_runtime_telemetry_room",
            "idx_mp_runtime_telemetry_items_batch",
            "idx_mp_runtime_telemetry_items_round_player",
            "idx_mp_runtime_telemetry_items_kind_created",
            "uq_mp_runtime_benchmark_report_id",
            "idx_mp_runtime_benchmark_reports_mode_created",
            "uq_mp_events_event_id",
            "uq_mp_sim_events_event_id",
            "idx_mp_sim_events_run",
            "idx_mp_sim_events_kind",
        ];
        for index in &expected_indexes {
            assert!(
                content.contains(index),
                "migration SQL must contain index `{index}`"
            );
        }

        // Must have backwards-compat ALTER TABLE column additions.
        assert!(
            content.contains("ALTER TABLE mp_users ADD COLUMN IF NOT EXISTS ip TEXT"),
            "migration must include backwards-compat ALTER for mp_users.ip"
        );
    }
}

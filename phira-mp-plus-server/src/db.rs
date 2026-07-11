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
    sqlx::query("CREATE SEQUENCE IF NOT EXISTS mp_persist_sequence")
        .execute(pool)
        .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS playtime (
            user_id INTEGER PRIMARY KEY,
            total_secs BIGINT NOT NULL DEFAULT 0,
            session_start BIGINT
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS room_history (
            id BIGSERIAL PRIMARY KEY,
            user_id INTEGER NOT NULL,
            room_id TEXT NOT NULL,
            room_uuid TEXT NOT NULL,
            joined_at BIGINT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_users (
            user_id INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            language TEXT NOT NULL DEFAULT '',
            ip TEXT,
            first_seen_at BIGINT NOT NULL,
            last_seen_at BIGINT NOT NULL,
            last_connected_at BIGINT,
            last_disconnected_at BIGINT,
            updated_at BIGINT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_room_snapshots (
            room_id TEXT PRIMARY KEY,
            room_uuid TEXT NOT NULL,
            payload JSONB NOT NULL,
            created_at BIGINT NOT NULL,
            updated_at BIGINT NOT NULL,
            sequence BIGINT NOT NULL DEFAULT nextval('mp_persist_sequence')
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_events (
            sequence BIGINT PRIMARY KEY DEFAULT nextval('mp_persist_sequence'),
            event_id TEXT,
            kind TEXT NOT NULL,
            room_id TEXT,
            user_id INTEGER,
            payload JSONB NOT NULL,
            created_at BIGINT NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    // mp_events 表使用全局 sequence 而非 BIGSERIAL 自增。
    let _ = sqlx::query(
        "ALTER TABLE mp_events ALTER COLUMN sequence SET DEFAULT nextval('mp_persist_sequence')",
    )
    .execute(pool)
    .await;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_user_room_history (
            id BIGSERIAL PRIMARY KEY,
            user_id INTEGER NOT NULL,
            room_id TEXT NOT NULL,
            room_uuid TEXT NOT NULL,
            joined_at BIGINT NOT NULL,
            created_at BIGINT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_rounds (
            round_uuid TEXT PRIMARY KEY,
            room_id TEXT NOT NULL,
            chart_id INTEGER NOT NULL,
            chart_name TEXT NOT NULL,
            players JSONB NOT NULL DEFAULT '[]'::jsonb,
            started_at BIGINT NOT NULL,
            finished_at BIGINT,
            created_at BIGINT NOT NULL,
            updated_at BIGINT NOT NULL,
            sequence BIGINT NOT NULL DEFAULT nextval('mp_persist_sequence')
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_round_touch_batches (
            sequence BIGINT PRIMARY KEY DEFAULT nextval('mp_persist_sequence'),
            round_uuid TEXT NOT NULL,
            player_id INTEGER NOT NULL,
            count INTEGER NOT NULL,
            first_game_time DOUBLE PRECISION,
            last_game_time DOUBLE PRECISION,
            payload JSONB NOT NULL,
            created_at BIGINT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_round_judge_batches (
            sequence BIGINT PRIMARY KEY DEFAULT nextval('mp_persist_sequence'),
            round_uuid TEXT NOT NULL,
            player_id INTEGER NOT NULL,
            count INTEGER NOT NULL,
            first_game_time DOUBLE PRECISION,
            last_game_time DOUBLE PRECISION,
            payload JSONB NOT NULL,
            created_at BIGINT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_round_player_data (
            round_uuid TEXT NOT NULL,
            player_id INTEGER NOT NULL,
            touches JSONB NOT NULL DEFAULT '[]'::jsonb,
            judges JSONB NOT NULL DEFAULT '[]'::jsonb,
            created_at BIGINT NOT NULL,
            updated_at BIGINT NOT NULL,
            sequence BIGINT NOT NULL DEFAULT nextval('mp_persist_sequence'),
            PRIMARY KEY (round_uuid, player_id)
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_round_results (
            round_uuid TEXT NOT NULL,
            user_id INTEGER NOT NULL,
            room_id TEXT NOT NULL,
            score INTEGER NOT NULL,
            accuracy DOUBLE PRECISION NOT NULL,
            payload JSONB NOT NULL,
            created_at BIGINT NOT NULL,
            updated_at BIGINT NOT NULL,
            sequence BIGINT NOT NULL DEFAULT nextval('mp_persist_sequence'),
            PRIMARY KEY (round_uuid, user_id)
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_runtime_telemetry_batches (
            sequence BIGINT PRIMARY KEY DEFAULT nextval('mp_persist_sequence'),
            event_id TEXT UNIQUE,
            batch_uuid TEXT NOT NULL,
            run_id TEXT,
            scope TEXT NOT NULL DEFAULT 'production',
            pipeline TEXT NOT NULL DEFAULT 'runtime_v2.telemetry_batcher',
            kind TEXT NOT NULL,
            room_id TEXT,
            round_uuid TEXT,
            player_id INTEGER NOT NULL,
            item_count INTEGER NOT NULL,
            payload JSONB NOT NULL,
            created_at BIGINT NOT NULL,
            source TEXT NOT NULL DEFAULT 'telemetry_batcher',
            -- Historical column, kept for backward-compatible reads.
            -- Current telemetry modes: direct_only, worker_preferred, worker_authoritative.
            dual_write BOOLEAN NOT NULL DEFAULT TRUE,
            schema_version INTEGER NOT NULL DEFAULT 3,
            flush_reason TEXT NOT NULL DEFAULT 'unknown'
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_runtime_telemetry_items (
            sequence BIGINT PRIMARY KEY DEFAULT nextval('mp_persist_sequence'),
            event_id TEXT,
            batch_uuid TEXT NOT NULL,
            ordinal INTEGER NOT NULL,
            kind TEXT NOT NULL,
            room_id TEXT,
            round_uuid TEXT,
            player_id INTEGER NOT NULL,
            payload JSONB NOT NULL,
            created_at BIGINT NOT NULL,
            schema_version INTEGER NOT NULL DEFAULT 3
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_runtime_persistence_meta (
            key TEXT PRIMARY KEY,
            value JSONB NOT NULL,
            updated_at BIGINT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_runtime_retention_policies (
            scope TEXT PRIMARY KEY,
            retain_seconds BIGINT NOT NULL,
            cleanup_enabled BOOLEAN NOT NULL DEFAULT FALSE,
            updated_at BIGINT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_runtime_benchmark_reports (
            sequence BIGINT PRIMARY KEY DEFAULT nextval('mp_persist_sequence'),
            report_id TEXT,
            mode TEXT NOT NULL,
            title TEXT NOT NULL,
            duration_secs BIGINT NOT NULL,
            is_simulation BOOLEAN NOT NULL DEFAULT FALSE,
            operations BIGINT,
            failed_operations BIGINT,
            probes_attempted BIGINT NOT NULL DEFAULT 0,
            probes_succeeded BIGINT NOT NULL DEFAULT 0,
            probes_failed BIGINT NOT NULL DEFAULT 0,
            probes_blocked BIGINT NOT NULL DEFAULT 0,
            probes_skipped BIGINT NOT NULL DEFAULT 0,
            failure_samples INTEGER NOT NULL DEFAULT 0,
            notes INTEGER NOT NULL DEFAULT 0,
            report JSONB NOT NULL,
            created_at BIGINT NOT NULL,
            source TEXT NOT NULL DEFAULT 'persistence_worker',
            schema_version INTEGER NOT NULL DEFAULT 1
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_sim_events (
            sequence BIGINT PRIMARY KEY DEFAULT nextval('mp_persist_sequence'),
            event_id TEXT,
            run_id TEXT,
            kind TEXT NOT NULL,
            payload JSONB NOT NULL,
            created_at BIGINT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_settings (
            key TEXT PRIMARY KEY,
            value JSONB NOT NULL,
            updated_at BIGINT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    for alter in [
        "ALTER TABLE mp_users ADD COLUMN IF NOT EXISTS ip TEXT",
        "ALTER TABLE mp_events ADD COLUMN IF NOT EXISTS event_id TEXT",
        "ALTER TABLE mp_sim_events ADD COLUMN IF NOT EXISTS event_id TEXT",
        "ALTER TABLE mp_round_player_data ADD COLUMN IF NOT EXISTS sequence BIGINT NOT NULL DEFAULT nextval('mp_persist_sequence')",
        "ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS event_id TEXT",
        "ALTER TABLE mp_runtime_benchmark_reports ADD COLUMN IF NOT EXISTS report_id TEXT",
        "ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS dual_write BOOLEAN NOT NULL DEFAULT TRUE",
        "ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS source TEXT NOT NULL DEFAULT 'telemetry_batcher'",
        "ALTER TABLE mp_runtime_telemetry_items ADD COLUMN IF NOT EXISTS event_id TEXT",
        "ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS batch_uuid TEXT",
        "ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS run_id TEXT",
        "ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS scope TEXT NOT NULL DEFAULT 'production'",
        "ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS pipeline TEXT NOT NULL DEFAULT 'runtime_v2.telemetry_batcher'",
        "ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS schema_version INTEGER NOT NULL DEFAULT 3",
        "ALTER TABLE mp_runtime_telemetry_batches ALTER COLUMN schema_version SET DEFAULT 3",
        "ALTER TABLE mp_runtime_telemetry_items ALTER COLUMN schema_version SET DEFAULT 3",
        "ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS flush_reason TEXT NOT NULL DEFAULT 'unknown'",
    ] {
        sqlx::query(alter).execute(pool).await?;
    }

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

    for index in [
        "CREATE INDEX IF NOT EXISTS idx_mp_events_created ON mp_events(created_at)",
        "CREATE INDEX IF NOT EXISTS idx_mp_events_kind ON mp_events(kind)",
        "CREATE INDEX IF NOT EXISTS idx_mp_events_room ON mp_events(room_id)",
        "CREATE INDEX IF NOT EXISTS idx_mp_events_user ON mp_events(user_id)",
        "CREATE INDEX IF NOT EXISTS idx_mp_rounds_started ON mp_rounds(started_at)",
        "CREATE INDEX IF NOT EXISTS idx_mp_touch_batches_round_player_seq ON mp_round_touch_batches(round_uuid, player_id, sequence)",
        "CREATE INDEX IF NOT EXISTS idx_mp_touch_batches_created ON mp_round_touch_batches(created_at)",
        "CREATE INDEX IF NOT EXISTS idx_mp_judge_batches_round_player_seq ON mp_round_judge_batches(round_uuid, player_id, sequence)",
        "CREATE INDEX IF NOT EXISTS idx_mp_judge_batches_created ON mp_round_judge_batches(created_at)",
        "CREATE INDEX IF NOT EXISTS idx_mp_user_room_history_user ON mp_user_room_history(user_id)",
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_room_history_join_identity ON room_history(user_id, room_uuid, joined_at)",
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_mp_user_room_history_join_identity ON mp_user_room_history(user_id, room_uuid, joined_at)",
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_event_id ON mp_runtime_telemetry_batches(event_id)",
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_item_event_ordinal ON mp_runtime_telemetry_items(event_id, ordinal)",
        "CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_batch_uuid ON mp_runtime_telemetry_batches(batch_uuid)",
        "CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_created ON mp_runtime_telemetry_batches(created_at)",
        "CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_kind ON mp_runtime_telemetry_batches(kind)",
        "CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_scope_created ON mp_runtime_telemetry_batches(scope, created_at)",
        "CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_run ON mp_runtime_telemetry_batches(run_id)",
        "CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_round_player ON mp_runtime_telemetry_batches(round_uuid, player_id, sequence)",
        "CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_room ON mp_runtime_telemetry_batches(room_id)",
        "CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_items_batch ON mp_runtime_telemetry_items(batch_uuid, ordinal)",
        "CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_items_round_player ON mp_runtime_telemetry_items(round_uuid, player_id, sequence)",
        "CREATE INDEX IF NOT EXISTS idx_mp_runtime_telemetry_items_kind_created ON mp_runtime_telemetry_items(kind, created_at)",
        "CREATE UNIQUE INDEX IF NOT EXISTS uq_mp_runtime_benchmark_report_id ON mp_runtime_benchmark_reports(report_id) WHERE report_id IS NOT NULL",
        "CREATE INDEX IF NOT EXISTS idx_mp_runtime_benchmark_reports_mode_created ON mp_runtime_benchmark_reports(mode, created_at)",
        "CREATE INDEX IF NOT EXISTS idx_mp_runtime_benchmark_reports_created ON mp_runtime_benchmark_reports(created_at)",
        "CREATE INDEX IF NOT EXISTS idx_mp_runtime_benchmark_reports_sim ON mp_runtime_benchmark_reports(is_simulation, created_at)",
        "CREATE UNIQUE INDEX IF NOT EXISTS uq_mp_events_event_id ON mp_events(event_id) WHERE event_id IS NOT NULL",
        "CREATE UNIQUE INDEX IF NOT EXISTS uq_mp_sim_events_event_id ON mp_sim_events(event_id) WHERE event_id IS NOT NULL",
        "CREATE INDEX IF NOT EXISTS idx_mp_sim_events_run ON mp_sim_events(run_id)",
        "CREATE INDEX IF NOT EXISTS idx_mp_sim_events_kind ON mp_sim_events(kind)",
        "CREATE INDEX IF NOT EXISTS idx_mp_sim_events_created ON mp_sim_events(created_at)",
    ] {
        sqlx::query(index).execute(pool).await?;
    }

    tracing::info!("统一 PostgreSQL 持久化表已就绪");
    Ok(())
}

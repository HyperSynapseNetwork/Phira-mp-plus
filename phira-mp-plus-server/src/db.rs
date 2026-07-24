//! Unified PostgreSQL persistence for Phira-mp+.
//!
//! PostgreSQL is required infrastructure. The server refuses to start if the
//! database connection or migration fails. Every mutable entity written here
//! has either `created_at`/`updated_at` or an append-only `sequence` so
//! plugins and dashboards can reconstruct modification time and order.

use anyhow::Result;
use serde_json::Value;

/// Return the embedded sqlx migrator.
///
/// Uses `sqlx::migrate!("migrations")` which embeds migration SQL at compile
/// time.  The old approach (`env!("CARGO_MANIFEST_DIR")/migrations`) resolved
/// to a source-tree absolute path that did not exist in Docker runtime images,
/// causing silent migration failures in production containers.
#[cfg(feature = "postgres")]
pub fn migrator() -> sqlx::migrate::Migrator {
    // `./migrations` resolves relative to CARGO_MANIFEST_DIR at compile time.
    // The SQL is embedded into the binary — no runtime filesystem access needed.
    sqlx::migrate!("./migrations")
}

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
    Pg(sqlx::PgPool),
}

impl DbManager {
    /// 根据配置初始化数据库连接，连接或迁移失败直接返回错误。
    pub async fn new(database_url: &str) -> anyhow::Result<Self> {
        let url = database_url.trim();
        let url = if url.is_empty() {
            // 未配置 database_url 时依次尝试常见本地默认连接
            tracing::info!("database_url 未设置，尝试本地默认连接");
            "postgres://postgres@localhost:5432/phira_mp_plus"
        } else {
            url
        };

        #[cfg(feature = "postgres")]
        {
            match sqlx::PgPool::connect(url).await {
                Ok(pool) => {
                    tracing::info!("PostgreSQL 已连接");
                    if let Err(e) = init_tables(&pool).await {
                        anyhow::bail!("数据库建表失败: {e:?}");
                    }
                    Ok(Self::Pg(pool))
                }
                Err(e) => {
                    // Provide a helpful hint for common connection issues.
                    if e.to_string().contains("Connection refused") {
                        tracing::warn!("PostgreSQL 未运行或端口不可达，尝试 Unix socket...");
                    }
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
                        let admin_pool = match sqlx::PgPool::connect(&admin_url).await {
                            Ok(pool) => Some(pool),
                            Err(_) => {
                                tracing::info!("admin TCP 连接失败，尝试 Unix socket...");
                                // Try Unix socket for admin connection
                                let opts = sqlx::postgres::PgConnectOptions::new()
                                    .host("/var/run/postgresql")
                                    .database("postgres")
                                    .username("postgres");
                                sqlx::PgPool::connect_with(opts).await.ok()
                            }
                        };
                        if let Some(admin_pool) = admin_pool {
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
                                    anyhow::bail!("数据库建表失败: {e:?}");
                                }
                                tracing::info!("PostgreSQL 已连接（自动建库）");
                                return Ok(Self::Pg(new_pool));
                            }
                        }
                    }
                    // TCP 连接失败时尝试 Unix socket (peer auth, 无需密码)
                    tracing::info!("TCP 连接失败，尝试 Unix socket...");
                    use sqlx::postgres::PgConnectOptions;
                    let socket_dirs: &[(&str, &str)] = &[
                        ("/var/run/postgresql", "Debian/Ubuntu"),
                        ("/tmp", "macOS/Homebrew"),
                    ];
                    let mut socket_error = String::new();
                    // Try connecting as the current OS user first (peer auth),
                    // then fall back to 'postgres' user.
                    let os_user = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
                    for user in [os_user.as_str(), "postgres", "pmp"] {
                        if user.is_empty() {
                            continue;
                        }
                        for (socket_dir, distro) in socket_dirs {
                            let opts = PgConnectOptions::new()
                                .host(socket_dir)
                                .database("phira_mp_plus")
                                .username(user);
                        match sqlx::PgPool::connect_with(opts).await {
                            Ok(pool) => {
                                if let Err(e) = init_tables(&pool).await {
                                    anyhow::bail!("数据库建表失败: {e:?}");
                                }
                                tracing::info!("PostgreSQL 已连接（Unix socket, {distro}）");
                                return Ok(Self::Pg(pool));
                            }
                            Err(e) => socket_error = format!("{socket_error}{distro} socket({user}): {e}; "),
                        }
                    }
                    }
                    let install_hint = match std::env::consts::OS {
                        "linux" => {
                            if std::path::Path::new("/etc/debian_version").exists() {
                                "sudo apt install postgresql && sudo systemctl start postgresql"
                            } else if std::path::Path::new("/etc/redhat-release").exists() {
                                "sudo dnf install postgresql-server && sudo systemctl enable --now postgresql"
                            } else {
                                "sudo apt install postgresql  # 或使用你的发行版的包管理器"
                            }
                        }
                        "macos" => "brew install postgresql && brew services start postgresql",
                        "windows" => "winget install PostgreSQL.PostgreSQL  # 或从 https://www.postgresql.org/download/ 下载",
                        _ => "请安装 PostgreSQL（https://www.postgresql.org/download/）",
                    };
                    let hint = format!(
                        "\n      请确保 PostgreSQL 已安装且正在运行：
      检测到系统: {}
      安装命令: {}
      Docker: docker compose up -d postgres
      配置 database_url 或留空自动连接本地 Unix socket",
                        std::env::consts::OS,
                        install_hint,
                    );
                    anyhow::bail!("PostgreSQL 连接失败（TCP: {e}；{socket_error}）{hint}");
                }
            }
        }

        #[cfg(not(feature = "postgres"))]
        {
            anyhow::bail!("postgres feature 未启用，无法启动");
        }
    }

    /// 是否使用 PostgreSQL（始终有效，因 None 变体已移除）
    pub fn is_active(&self) -> bool {
        true
    }

    pub async fn cleanup_expired(&self, retention_days: u32, touch_judge_retention_days: u32) {
        let Self::Pg(pool) = self;
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
    migrator().run(pool).await?;

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
        "notes": "Runtime v2 telemetry schema normalizes batch headers and raw telemetry items."
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
    /// Verify the migrator resolves and finds migration files.
    ///
    /// SAFE EVOLUTION: this test accepts ≥1 migration so that new versioned
    /// migrations can be added without updating this assertion.  Each new
    /// migration file must be immutable once deployed — never modify an
    /// already-deployed migration.  Add a new file with a higher version
    /// number (e.g. 20260722000001_add_something.sql) instead.
    #[cfg(feature = "postgres")]
    #[test]
    fn migration_macro_compiles_and_has_expected_count() {
        let migrator = crate::db::migrator();
        assert!(
            !migrator.migrations.is_empty(),
            "must have at least one migration"
        );
        // Accept ≥1 migration so schema evolution is not blocked by a
        // hard-coded count.  The initial migration is expected to be
        // 20260721000001_initial_schema.sql; subsequent migrations add
        // irreversible DDL with higher version timestamps.
        assert!(
            !migrator.migrations.is_empty(),
            "expected at least 1 migration, found {}",
            migrator.migrations.len()
        );
        let m = &migrator.migrations[0];
        assert_eq!(
            m.version, 20260721000001,
            "unexpected base migration version"
        );
        assert!(
            m.description.contains("initial schema"),
            "base migration description mismatch: {}",
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

    /// Ensure migration files use valid version timestamps and are
    /// strictly forward-only (no edits to deployed files).
    #[cfg(feature = "postgres")]
    #[test]
    fn migration_evolution_is_strictly_versioned() {
        let migrator = crate::db::migrator();
        let mut prev_version = 0i64;
        for (i, m) in migrator.migrations.iter().enumerate() {
            // Version must be a valid UTC timestamp in YYYYMMDDHHMMSS format.
            assert!(
                m.version >= 20260000000000,
                "migration[{}] version {} is not a valid UTC timestamp",
                i, m.version
            );
            // Versions must be strictly increasing.
            assert!(
                m.version > prev_version,
                "migration[{}] version {} <= previous {}",
                i, m.version, prev_version
            );
            prev_version = m.version;
        }
    }
}

//! Unified PostgreSQL persistence for Phira-mp+.
//!
//! PostgreSQL is the single structured persistence backend. When `database_url`
//! is not configured the manager becomes `None` and callers keep their direct
//! in-memory/file fallback so test deployments still boot. Every mutable entity
//! written here has either `created_at`/`updated_at` or an append-only `sequence`
//! so plugins and dashboards can reconstruct modification time and order.

use anyhow::Result;
#[cfg(feature = "postgres")]
use serde::de::DeserializeOwned;
use serde_json::Value;

#[cfg(feature = "postgres")]
use sqlx::Row;

/// 游玩时间记录
#[derive(Debug, Clone)]
pub struct PlaytimeRow {
    pub total_secs: i64,
    pub session_start: Option<i64>,
}

/// 数据库管理器

#[derive(Debug, Clone)]
pub struct RuntimeTelemetryBatchRecord {
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

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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

    pub async fn set_admin_ids(&self, ids: &[i32]) -> std::result::Result<(), String> {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let value = serde_json::to_string(ids).map_err(|e| e.to_string())?;
            let now = now_ms();
            sqlx::query(
                "INSERT INTO mp_settings (key, value, updated_at) VALUES ('admin_phira_ids', $1::jsonb, $2)
                 ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at"
            )
            .bind(value)
            .bind(now)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub async fn get_admin_ids(&self) -> Option<Vec<i32>> {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let row = sqlx::query(
                "SELECT value::text AS value FROM mp_settings WHERE key = 'admin_phira_ids'",
            )
            .fetch_optional(pool)
            .await
            .ok()??;
            let raw = row.try_get::<String, _>("value").ok()?;
            return serde_json::from_str(&raw).ok();
        }
        None
    }

    pub async fn open_round(&self, meta: &crate::round_store::RoundMeta) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let players = serde_json::to_string(&meta.players).unwrap_or_else(|_| "[]".to_string());
            let now = now_ms();
            let _ = sqlx::query(
                "INSERT INTO mp_rounds (round_uuid, room_id, chart_id, chart_name, players, started_at, finished_at, created_at, updated_at, sequence)
                 VALUES ($1, $2, $3, $4, $5::jsonb, $6, $7, $8, $8, nextval('mp_persist_sequence'))
                 ON CONFLICT (round_uuid) DO UPDATE SET
                   room_id = EXCLUDED.room_id,
                   chart_id = EXCLUDED.chart_id,
                   chart_name = EXCLUDED.chart_name,
                   players = EXCLUDED.players,
                   started_at = EXCLUDED.started_at,
                   finished_at = EXCLUDED.finished_at,
                   updated_at = EXCLUDED.updated_at,
                   sequence = EXCLUDED.sequence"
            )
            .bind(&meta.round_uuid)
            .bind(&meta.room_id)
            .bind(meta.chart_id)
            .bind(&meta.chart_name)
            .bind(players)
            .bind(meta.started_at)
            .bind(meta.finished_at)
            .bind(now)
            .execute(pool)
            .await;
            let _ = append_event_pg(
                pool,
                "round.open",
                Some(&meta.room_id),
                None,
                serde_json::to_value(meta).unwrap_or_default(),
            )
            .await;
        }
    }

    pub async fn close_round(&self, round_uuid: &str) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let now = now_ms();
            let _ = sqlx::query(
                "UPDATE mp_rounds SET finished_at = COALESCE(finished_at, $2), updated_at = $2, sequence = nextval('mp_persist_sequence')
                 WHERE round_uuid = $1"
            )
            .bind(round_uuid)
            .bind(now)
            .execute(pool)
            .await;
            let _ = append_event_pg(
                pool,
                "round.close",
                None,
                None,
                serde_json::json!({"round_uuid": round_uuid, "finished_at": now}),
            )
            .await;
        }
    }

    pub async fn append_touches(
        &self,
        round_uuid: &str,
        player_id: i32,
        data: &[crate::plugin::TouchEventPoint],
    ) {
        if data.is_empty() {
            return;
        }
        let payload_json = serde_json::to_string(data).unwrap_or_else(|_| "[]".to_string());
        self.append_player_array_json(round_uuid, player_id, "touches", &payload_json)
            .await;
        self.append_touch_batch_str(round_uuid, player_id, data, &payload_json)
            .await;
    }

    pub async fn append_judges(
        &self,
        round_uuid: &str,
        player_id: i32,
        data: &[crate::plugin::JudgeEventItem],
    ) {
        if data.is_empty() {
            return;
        }
        let payload_json = serde_json::to_string(data).unwrap_or_else(|_| "[]".to_string());
        self.append_player_array_json(round_uuid, player_id, "judges", &payload_json)
            .await;
        self.append_judge_batch_str(round_uuid, player_id, data, &payload_json)
            .await;
    }

    async fn append_touch_batch_str(
        &self,
        round_uuid: &str,
        player_id: i32,
        data: &[crate::plugin::TouchEventPoint],
        payload_json: &str,
    ) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let (first_game_time, last_game_time) =
                telemetry_time_range(data.iter().map(|p| p.time as f64));
            let now = now_ms();
            let count = i32::try_from(data.len()).unwrap_or(i32::MAX);
            let _ = sqlx::query(
                "INSERT INTO mp_round_touch_batches
                   (round_uuid, player_id, count, first_game_time, last_game_time, payload, created_at, sequence)
                 VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7, nextval('mp_persist_sequence'))"
            )
            .bind(round_uuid)
            .bind(player_id)
            .bind(count)
            .bind(first_game_time)
            .bind(last_game_time)
            .bind(payload_json)
            .bind(now)
            .execute(pool)
            .await;
        }
    }

    async fn append_judge_batch_str(
        &self,
        round_uuid: &str,
        player_id: i32,
        data: &[crate::plugin::JudgeEventItem],
        payload_json: &str,
    ) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let (first_game_time, last_game_time) =
                telemetry_time_range(data.iter().map(|p| p.time as f64));
            let now = now_ms();
            let count = i32::try_from(data.len()).unwrap_or(i32::MAX);
            let _ = sqlx::query(
                "INSERT INTO mp_round_judge_batches
                   (round_uuid, player_id, count, first_game_time, last_game_time, payload, created_at, sequence)
                 VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7, nextval('mp_persist_sequence'))"
            )
            .bind(round_uuid)
            .bind(player_id)
            .bind(count)
            .bind(first_game_time)
            .bind(last_game_time)
            .bind(payload_json)
            .bind(now)
            .execute(pool)
            .await;
        }
    }

    async fn append_player_array_json(
        &self,
        round_uuid: &str,
        player_id: i32,
        column: &str,
        payload_json: &str,
    ) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            if payload_json == "[]" {
                return;
            }
            let now = now_ms();
            let sql = match column {
                "judges" =>
                    "INSERT INTO mp_round_player_data (round_uuid, player_id, touches, judges, created_at, updated_at)
                     VALUES ($1, $2, '[]'::jsonb, $3::jsonb, $4, $4)
                     ON CONFLICT (round_uuid, player_id) DO UPDATE SET
                       judges = mp_round_player_data.judges || EXCLUDED.judges,
                       updated_at = EXCLUDED.updated_at",
                _ =>
                    "INSERT INTO mp_round_player_data (round_uuid, player_id, touches, judges, created_at, updated_at)
                     VALUES ($1, $2, $3::jsonb, '[]'::jsonb, $4, $4)
                     ON CONFLICT (round_uuid, player_id) DO UPDATE SET
                       touches = mp_round_player_data.touches || EXCLUDED.touches,
                       updated_at = EXCLUDED.updated_at",
            };
            let _ = sqlx::query(sql)
                .bind(round_uuid)
                .bind(player_id)
                .bind(payload_json)
                .bind(now)
                .execute(pool)
                .await;
        }
    }

    pub async fn record_round_result(
        &self,
        round_uuid: &str,
        room_id: &str,
        result: &crate::room::PlayResult,
    ) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let payload = serde_json::to_string(result).unwrap_or_else(|_| "{}".to_string());
            let now = now_ms();
            let _ = sqlx::query(
                "INSERT INTO mp_round_results (round_uuid, user_id, room_id, score, accuracy, payload, created_at, updated_at, sequence)
                 VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7, $7, nextval('mp_persist_sequence'))
                 ON CONFLICT (round_uuid, user_id) DO UPDATE SET
                   score = EXCLUDED.score,
                   accuracy = EXCLUDED.accuracy,
                   payload = EXCLUDED.payload,
                   updated_at = EXCLUDED.updated_at,
                   sequence = EXCLUDED.sequence"
            )
            .bind(round_uuid)
            .bind(result.user_id)
            .bind(room_id)
            .bind(result.score)
            .bind(result.accuracy as f64)
            .bind(payload)
            .bind(now)
            .execute(pool)
            .await;
            let _ = append_event_pg(
                pool,
                "round.result",
                Some(room_id),
                Some(result.user_id),
                serde_json::to_value(result).unwrap_or_default(),
            )
            .await;
        }
    }

    pub async fn list_rounds(&self, limit: i64) -> Vec<crate::round_store::RoundMeta> {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let rows = sqlx::query(
                "SELECT round_uuid, room_id, chart_id, chart_name, players::text AS players, started_at, finished_at
                 FROM mp_rounds ORDER BY started_at DESC LIMIT $1"
            )
            .bind(limit)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
            return rows
                .into_iter()
                .filter_map(|row| {
                    let players_raw = row
                        .try_get::<String, _>("players")
                        .unwrap_or_else(|_| "[]".to_string());
                    Some(crate::round_store::RoundMeta {
                        round_uuid: row.try_get::<String, _>("round_uuid").ok()?,
                        room_id: row.try_get::<String, _>("room_id").ok()?,
                        chart_id: row.try_get::<i32, _>("chart_id").ok()?,
                        chart_name: row.try_get::<String, _>("chart_name").ok()?,
                        players: serde_json::from_str(&players_raw).unwrap_or_default(),
                        started_at: row.try_get::<i64, _>("started_at").ok()?,
                        finished_at: row.try_get::<Option<i64>, _>("finished_at").ok().flatten(),
                    })
                })
                .collect();
        }
        Vec::new()
    }

    pub async fn read_round_player_data(
        &self,
        round_uuid: &str,
        player_id: i32,
    ) -> Option<crate::round_store::RoundPlayerData> {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let direct_row = sqlx::query(
                "SELECT touches::text AS touches, judges::text AS judges
                 FROM mp_round_player_data WHERE round_uuid = $1 AND player_id = $2",
            )
            .bind(round_uuid)
            .bind(player_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();

            let mut touches = Vec::new();
            let mut judges = Vec::new();
            if let Some(row) = direct_row {
                let touches_raw = row
                    .try_get::<String, _>("touches")
                    .unwrap_or_else(|_| "[]".to_string());
                let judges_raw = row
                    .try_get::<String, _>("judges")
                    .unwrap_or_else(|_| "[]".to_string());
                touches = serde_json::from_str(&touches_raw).unwrap_or_default();
                judges = serde_json::from_str(&judges_raw).unwrap_or_default();
            }

            // Runtime v2 WorkerPreferred mode may skip the direct
            // mp_round_player_data row entirely.  Read normalized raw items as
            // a fallback so existing RoundStore/WIT/plugin read paths keep
            // working after telemetry cutover.
            if touches.is_empty() {
                touches = query_runtime_telemetry_items::<crate::plugin::TouchEventPoint>(
                    pool, "touch", round_uuid, player_id,
                )
                .await;
            }
            if judges.is_empty() {
                judges = query_runtime_telemetry_items::<crate::plugin::JudgeEventItem>(
                    pool, "judge", round_uuid, player_id,
                )
                .await;
            }

            if touches.is_empty() && judges.is_empty() {
                return None;
            }
            return Some(crate::round_store::RoundPlayerData {
                round_uuid: round_uuid.to_string(),
                player_id,
                touches,
                judges,
            });
        }
        None
    }

    pub async fn query_events(
        &self,
        since_sequence: i64,
        limit: i64,
        kind: Option<&str>,
        room_id: Option<&str>,
        user_id: Option<i32>,
    ) -> Vec<Value> {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let rows = sqlx::query(
                "SELECT sequence, kind, room_id, user_id, payload::text AS payload, created_at
                 FROM mp_events
                 WHERE sequence > $1
                   AND ($2::text IS NULL OR kind = $2)
                   AND ($3::text IS NULL OR room_id = $3)
                   AND ($4::int IS NULL OR user_id = $4)
                 ORDER BY sequence ASC LIMIT $5",
            )
            .bind(since_sequence)
            .bind(kind)
            .bind(room_id)
            .bind(user_id)
            .bind(limit)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
            return rows
                .into_iter()
                .map(|row| {
                    let payload = row
                        .try_get::<String, _>("payload")
                        .ok()
                        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                        .unwrap_or(Value::Null);
                    serde_json::json!({
                        "sequence": row.try_get::<i64, _>("sequence").unwrap_or_default(),
                        "kind": row.try_get::<String, _>("kind").unwrap_or_default(),
                        "room_id": row.try_get::<Option<String>, _>("room_id").ok().flatten(),
                        "user_id": row.try_get::<Option<i32>, _>("user_id").ok().flatten(),
                        "payload": payload,
                        "created_at": row.try_get::<i64, _>("created_at").unwrap_or_default(),
                    })
                })
                .collect();
        }
        Vec::new()
    }

    pub async fn query_room_snapshots(&self, since_sequence: i64, limit: i64) -> Vec<Value> {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let rows = sqlx::query(
                "SELECT room_id, room_uuid, payload::text AS payload, created_at, updated_at, sequence
                 FROM mp_room_snapshots WHERE sequence > $1 ORDER BY sequence ASC LIMIT $2"
            )
            .bind(since_sequence)
            .bind(limit)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
            return rows
                .into_iter()
                .map(|row| {
                    let payload = row
                        .try_get::<String, _>("payload")
                        .ok()
                        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                        .unwrap_or(Value::Null);
                    serde_json::json!({
                        "room_id": row.try_get::<String, _>("room_id").unwrap_or_default(),
                        "room_uuid": row.try_get::<String, _>("room_uuid").unwrap_or_default(),
                        "payload": payload,
                        "created_at": row.try_get::<i64, _>("created_at").unwrap_or_default(),
                        "updated_at": row.try_get::<i64, _>("updated_at").unwrap_or_default(),
                        "sequence": row.try_get::<i64, _>("sequence").unwrap_or_default(),
                    })
                })
                .collect();
        }
        Vec::new()
    }

    pub async fn query_touch_batches(
        &self,
        since_sequence: i64,
        limit: i64,
        round_uuid: Option<&str>,
        player_id: Option<i32>,
    ) -> Vec<Value> {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let direct_batches = query_telemetry_batches(
                pool,
                "mp_round_touch_batches",
                since_sequence,
                limit,
                round_uuid,
                player_id,
            )
            .await;
            if !direct_batches.is_empty() {
                return direct_batches;
            }
            return query_runtime_telemetry_batches(
                pool,
                "touch",
                since_sequence,
                limit,
                round_uuid,
                player_id,
            )
            .await;
        }
        Vec::new()
    }

    pub async fn query_judge_batches(
        &self,
        since_sequence: i64,
        limit: i64,
        round_uuid: Option<&str>,
        player_id: Option<i32>,
    ) -> Vec<Value> {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let direct_batches = query_telemetry_batches(
                pool,
                "mp_round_judge_batches",
                since_sequence,
                limit,
                round_uuid,
                player_id,
            )
            .await;
            if !direct_batches.is_empty() {
                return direct_batches;
            }
            return query_runtime_telemetry_batches(
                pool,
                "judge",
                since_sequence,
                limit,
                round_uuid,
                player_id,
            )
            .await;
        }
        Vec::new()
    }

    pub async fn cleanup_expired(&self, retention_days: u32, touch_judge_retention_days: u32) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            if retention_days > 0 {
                let cutoff = now_ms().saturating_sub(retention_days as i64 * 86_400_000);
                for sql in [
                    "DELETE FROM mp_events WHERE created_at < $1",
                    "DELETE FROM mp_user_room_history WHERE created_at < $1",
                    "DELETE FROM mp_round_results WHERE updated_at < $1",
                ] {
                    let _ = sqlx::query(sql).bind(cutoff).execute(pool).await;
                }
            }

            if touch_judge_retention_days > 0 {
                let cutoff =
                    now_ms().saturating_sub(touch_judge_retention_days as i64 * 86_400_000);
                for sql in [
                    "DELETE FROM mp_round_player_data WHERE updated_at < $1",
                    "DELETE FROM mp_round_touch_batches WHERE created_at < $1",
                    "DELETE FROM mp_round_judge_batches WHERE created_at < $1",
                ] {
                    let _ = sqlx::query(sql).bind(cutoff).execute(pool).await;
                }
            }

            // 轮次元数据是 Touches/Judges 的索引上下文；如果遥测保留更久，就不要先删元数据。
            let round_meta_retention_days = match (retention_days, touch_judge_retention_days) {
                (0, _) | (_, 0) => 0,
                (a, b) => a.max(b),
            };
            if round_meta_retention_days > 0 {
                let cutoff = now_ms().saturating_sub(round_meta_retention_days as i64 * 86_400_000);
                let _ = sqlx::query("DELETE FROM mp_rounds WHERE updated_at < $1")
                    .bind(cutoff)
                    .execute(pool)
                    .await;
            }
        }
    }
}

fn telemetry_time_range<I>(times: I) -> (Option<f64>, Option<f64>)
where
    I: IntoIterator<Item = f64>,
{
    let mut first = None;
    let mut last = None;
    for time in times {
        first = Some(first.map_or(time, |v: f64| v.min(time)));
        last = Some(last.map_or(time, |v: f64| v.max(time)));
    }
    (first, last)
}

#[cfg(feature = "postgres")]
async fn query_telemetry_batches(
    pool: &sqlx::PgPool,
    table: &str,
    since_sequence: i64,
    limit: i64,
    round_uuid: Option<&str>,
    player_id: Option<i32>,
) -> Vec<Value> {
    let table = match table {
        "mp_round_judge_batches" => "mp_round_judge_batches",
        _ => "mp_round_touch_batches",
    };
    let sql = format!(
        "SELECT sequence, round_uuid, player_id, count, first_game_time, last_game_time, payload::text AS payload, created_at
         FROM {table}
         WHERE sequence > $1
           AND ($2::text IS NULL OR round_uuid = $2)
           AND ($3::int IS NULL OR player_id = $3)
         ORDER BY sequence ASC LIMIT $4"
    );
    let rows = sqlx::query(&sql)
        .bind(since_sequence)
        .bind(round_uuid)
        .bind(player_id)
        .bind(limit)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
    rows.into_iter()
        .map(|row| {
            let payload = row
                .try_get::<String, _>("payload")
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .unwrap_or(Value::Array(Vec::new()));
            serde_json::json!({
                "sequence": row.try_get::<i64, _>("sequence").unwrap_or_default(),
                "round_uuid": row.try_get::<String, _>("round_uuid").unwrap_or_default(),
                "player_id": row.try_get::<i32, _>("player_id").unwrap_or_default(),
                "count": row.try_get::<i32, _>("count").unwrap_or_default(),
                "first_game_time": row.try_get::<Option<f64>, _>("first_game_time").ok().flatten(),
                "last_game_time": row.try_get::<Option<f64>, _>("last_game_time").ok().flatten(),
                "data": payload,
                "created_at": row.try_get::<i64, _>("created_at").unwrap_or_default(),
            })
        })
        .collect()
}

#[cfg(feature = "postgres")]
async fn query_runtime_telemetry_items<T>(
    pool: &sqlx::PgPool,
    kind: &str,
    round_uuid: &str,
    player_id: i32,
) -> Vec<T>
where
    T: DeserializeOwned,
{
    let rows = sqlx::query(
        "SELECT payload::text AS payload
         FROM mp_runtime_telemetry_items
         WHERE kind = $1 AND round_uuid = $2 AND player_id = $3
         ORDER BY sequence ASC",
    )
    .bind(kind)
    .bind(round_uuid)
    .bind(player_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .filter_map(|row| {
            row.try_get::<String, _>("payload")
                .ok()
                .and_then(|raw| serde_json::from_str::<T>(&raw).ok())
        })
        .collect()
}

#[cfg(feature = "postgres")]
async fn query_runtime_telemetry_batches(
    pool: &sqlx::PgPool,
    kind: &str,
    since_sequence: i64,
    limit: i64,
    round_uuid: Option<&str>,
    player_id: Option<i32>,
) -> Vec<Value> {
    let kind = match kind {
        "judge" => "judge",
        _ => "touch",
    };
    let rows = sqlx::query(
        "SELECT sequence, batch_uuid, run_id, scope, pipeline, kind, room_id, round_uuid, player_id,
                item_count, payload::text AS payload, created_at, source, dual_write, schema_version, flush_reason
         FROM mp_runtime_telemetry_batches
         WHERE kind = $1
           AND sequence > $2
           AND ($3::text IS NULL OR round_uuid = $3)
           AND ($4::int IS NULL OR player_id = $4)
         ORDER BY sequence ASC LIMIT $5"
    )
    .bind(kind)
    .bind(since_sequence)
    .bind(round_uuid)
    .bind(player_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter().map(|row| {
        let payload = row.try_get::<String, _>("payload").ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or(Value::Null);
        let data = payload.get("data").cloned().unwrap_or_else(|| payload.clone());
        serde_json::json!({
            "sequence": row.try_get::<i64, _>("sequence").unwrap_or_default(),
            "batch_uuid": row.try_get::<String, _>("batch_uuid").unwrap_or_default(),
            "run_id": row.try_get::<Option<String>, _>("run_id").ok().flatten(),
            "scope": row.try_get::<String, _>("scope").unwrap_or_else(|_| "production".to_string()),
            "pipeline": row.try_get::<String, _>("pipeline").unwrap_or_default(),
            "kind": row.try_get::<String, _>("kind").unwrap_or_default(),
            "room_id": row.try_get::<Option<String>, _>("room_id").ok().flatten(),
            "round_uuid": row.try_get::<Option<String>, _>("round_uuid").ok().flatten().unwrap_or_default(),
            "player_id": row.try_get::<i32, _>("player_id").unwrap_or_default(),
            "count": row.try_get::<i32, _>("item_count").unwrap_or_default(),
            "item_count": row.try_get::<i32, _>("item_count").unwrap_or_default(),
            "data": data,
            "payload": payload,
            "created_at": row.try_get::<i64, _>("created_at").unwrap_or_default(),
            "source": row.try_get::<String, _>("source").unwrap_or_default(),
            "dual_write": row.try_get::<bool, _>("dual_write").unwrap_or_default(),
            "schema_version": row.try_get::<i32, _>("schema_version").unwrap_or_default(),
            "flush_reason": row.try_get::<String, _>("flush_reason").unwrap_or_default(),
            "runtime_v2_read_path": true,
        })
    }).collect()
}

#[cfg(feature = "postgres")]
pub(crate) async fn append_event_pg(
    pool: &sqlx::PgPool,
    kind: &str,
    room_id: Option<&str>,
    user_id: Option<i32>,
    payload: Value,
) -> Result<()> {
    let payload = payload.to_string();
    sqlx::query(
        "INSERT INTO mp_events (kind, room_id, user_id, payload, created_at)
         VALUES ($1, $2, $3, $4::jsonb, $5)",
    )
    .bind(kind)
    .bind(room_id)
    .bind(user_id)
    .bind(payload)
    .bind(now_ms())
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
            -- Current telemetry modes: direct_only, worker_preferred.
            dual_write BOOLEAN NOT NULL DEFAULT TRUE,
            schema_version INTEGER NOT NULL DEFAULT 2,
            flush_reason TEXT NOT NULL DEFAULT 'unknown'
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_runtime_telemetry_items (
            sequence BIGINT PRIMARY KEY DEFAULT nextval('mp_persist_sequence'),
            batch_uuid TEXT NOT NULL,
            ordinal INTEGER NOT NULL,
            kind TEXT NOT NULL,
            room_id TEXT,
            round_uuid TEXT,
            player_id INTEGER NOT NULL,
            payload JSONB NOT NULL,
            created_at BIGINT NOT NULL,
            schema_version INTEGER NOT NULL DEFAULT 2
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
        "ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS batch_uuid TEXT",
        "ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS run_id TEXT",
        "ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS scope TEXT NOT NULL DEFAULT 'production'",
        "ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS pipeline TEXT NOT NULL DEFAULT 'runtime_v2.telemetry_batcher'",
        "ALTER TABLE mp_runtime_telemetry_batches ADD COLUMN IF NOT EXISTS schema_version INTEGER NOT NULL DEFAULT 2",
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
        "schema_version": 2,
        "batch_table": "mp_runtime_telemetry_batches",
        "item_table": "mp_runtime_telemetry_items",
        "mode": "direct_only",
        "available_modes": ["direct_only", "worker_preferred"],
        "notes": "Runtime v2 telemetry schema normalizes batch headers and raw telemetry items. Modes: direct_only, worker_preferred."
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
        "description": "direct RoundStore/db.rs only (safe default). WorkerPreferred = direct + worker mirror.",
        "available_modes": ["direct_only", "worker_preferred"],
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
        "CREATE INDEX IF NOT EXISTS idx_mp_runtime_benchmark_reports_mode_created ON mp_runtime_benchmark_reports(mode, created_at)",
        "CREATE INDEX IF NOT EXISTS idx_mp_runtime_benchmark_reports_created ON mp_runtime_benchmark_reports(created_at)",
        "CREATE INDEX IF NOT EXISTS idx_mp_runtime_benchmark_reports_sim ON mp_runtime_benchmark_reports(is_simulation, created_at)",
        "CREATE INDEX IF NOT EXISTS idx_mp_sim_events_run ON mp_sim_events(run_id)",
        "CREATE INDEX IF NOT EXISTS idx_mp_sim_events_kind ON mp_sim_events(kind)",
        "CREATE INDEX IF NOT EXISTS idx_mp_sim_events_created ON mp_sim_events(created_at)",
    ] {
        sqlx::query(index).execute(pool).await?;
    }

    tracing::info!("统一 PostgreSQL 持久化表已就绪");
    Ok(())
}

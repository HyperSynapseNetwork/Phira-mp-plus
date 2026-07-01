//! Unified PostgreSQL persistence for Phira-mp+.
//!
//! PostgreSQL is the single structured persistence backend. When `database_url`
//! is not configured the manager becomes `None` and callers keep their legacy
//! in-memory/file fallback so old deployments still boot. Every mutable entity
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
    pub dual_write: bool,
    pub kind: String,
    pub room_id: Option<String>,
    pub round_uuid: Option<String>,
    pub player_id: i32,
    pub item_count: i32,
    pub payload: Value,
}

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


fn with_telemetry_storage_meta(mut payload: Value, record: &RuntimeTelemetryBatchRecord, written_at: i64) -> Value {
    if let Some(obj) = payload.as_object_mut() {
        obj.entry("runtime_v2_schema_version".to_string())
            .or_insert_with(|| serde_json::json!(record.schema_version));
        obj.entry("runtime_v2_storage".to_string())
            .or_insert_with(|| serde_json::json!("mp_runtime_telemetry_batches"));
        obj.entry("runtime_v2_item_table".to_string())
            .or_insert_with(|| serde_json::json!("mp_runtime_telemetry_items"));
        obj.entry("batch_uuid".to_string())
            .or_insert_with(|| serde_json::json!(record.batch_uuid.clone()));
        obj.entry("scope".to_string())
            .or_insert_with(|| serde_json::json!(record.scope.clone()));
        obj.entry("pipeline".to_string())
            .or_insert_with(|| serde_json::json!(record.pipeline.clone()));
        obj.entry("source".to_string())
            .or_insert_with(|| serde_json::json!(record.source.clone()));
        obj.entry("flush_reason".to_string())
            .or_insert_with(|| serde_json::json!(record.flush_reason.clone()));
        obj.entry("dual_write".to_string())
            .or_insert_with(|| serde_json::json!(record.dual_write));
        obj.entry("written_at".to_string())
            .or_insert_with(|| serde_json::json!(written_at));
        if let Some(run_id) = &record.run_id {
            obj.entry("run_id".to_string())
                .or_insert_with(|| serde_json::json!(run_id));
        }
    }
    payload
}

fn telemetry_item_rows(payload: &Value) -> Vec<Value> {
    payload
        .get("data")
        .and_then(Value::as_array)
        .map(|items| items.clone())
        .unwrap_or_else(|| vec![payload.clone()])
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
                                let base = pieces.get(1).copied().unwrap_or("postgres://postgres:postgres@localhost:5432");
                                let admin_url = format!("{base}/postgres");
                                if let Ok(admin_pool) = sqlx::PgPool::connect(&admin_url).await {
                                    let db_name = pieces.first().copied().unwrap_or("phira_mp_plus");
                                    let safe_name = db_name.replace('"', "");
                                    let _ = sqlx::query(&format!("CREATE DATABASE \"{}\"", safe_name))
                                        .execute(&admin_pool)
                                        .await;
                                    admin_pool.close().await;
                                    if let Ok(new_pool) = sqlx::PgPool::connect(url).await {
                                        if let Err(e) = init_tables(&new_pool).await {
                                            tracing::warn!("数据库建表失败: {e:?}，禁用 PostgreSQL 持久化");
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

    /// 标记用户上线（设置 session_start）
    pub fn set_online_sync(&self, user_id: i32) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            tokio::spawn(async move {
                let now = now_secs();
                let _ = sqlx::query(
                    "INSERT INTO playtime (user_id, total_secs, session_start) VALUES ($1, 0, $2)
                     ON CONFLICT (user_id) DO UPDATE SET session_start = $2"
                )
                .bind(user_id)
                .bind(now)
                .execute(&pool)
                .await;
                let _ = append_event_pg(&pool, "user.online", None, Some(user_id), serde_json::json!({"user_id": user_id})).await;
            });
        }
    }

    /// 标记用户离线（累加本次会话时间）
    pub fn set_offline_sync(&self, user_id: i32) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            tokio::spawn(async move {
                let now = now_secs();
                let _ = sqlx::query(
                    "UPDATE playtime SET total_secs = total_secs + $1 - session_start, session_start = NULL
                     WHERE user_id = $2 AND session_start IS NOT NULL"
                )
                .bind(now)
                .bind(user_id)
                .execute(&pool)
                .await;
                let _ = append_event_pg(&pool, "user.offline", None, Some(user_id), serde_json::json!({"user_id": user_id})).await;
            });
        }
    }



    pub fn record_runtime_persistence_meta_sync(&self, key: &str, value: Value) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            let key = key.to_string();
            tokio::spawn(async move {
                let now = now_ms();
                let _ = sqlx::query(
                    "INSERT INTO mp_runtime_persistence_meta (key, value, updated_at)
                     VALUES ($1, $2, $3)
                     ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at"
                )
                .bind(&key)
                .bind(value)
                .bind(now)
                .execute(&pool)
                .await;
            });
        }
    }

    pub fn record_runtime_telemetry_batches_sync(&self, records: Vec<RuntimeTelemetryBatchRecord>) -> bool {
        if records.is_empty() {
            return true;
        }
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            tokio::spawn(async move {
                let now = now_ms();
                for record in records {
                    let batch_uuid = record.batch_uuid.clone();
                    let payload = with_telemetry_storage_meta(record.payload.clone(), &record, now);
                    let _ = sqlx::query(
                        "INSERT INTO mp_runtime_telemetry_batches
                           (batch_uuid, run_id, scope, pipeline, kind, room_id, round_uuid, player_id, item_count,
                            payload, created_at, source, dual_write, schema_version, flush_reason)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)"
                    )
                    .bind(&record.batch_uuid)
                    .bind(record.run_id.as_deref())
                    .bind(&record.scope)
                    .bind(&record.pipeline)
                    .bind(&record.kind)
                    .bind(record.room_id.as_deref())
                    .bind(record.round_uuid.as_deref())
                    .bind(record.player_id)
                    .bind(record.item_count)
                    .bind(&payload)
                    .bind(now)
                    .bind(&record.source)
                    .bind(record.dual_write)
                    .bind(record.schema_version)
                    .bind(&record.flush_reason)
                    .execute(&pool)
                    .await;

                    for (ordinal, raw_item) in telemetry_item_rows(&payload).into_iter().enumerate() {
                        let _ = sqlx::query(
                            "INSERT INTO mp_runtime_telemetry_items
                               (batch_uuid, ordinal, kind, room_id, round_uuid, player_id, payload, created_at, schema_version)
                             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
                        )
                        .bind(&batch_uuid)
                        .bind(i32::try_from(ordinal).unwrap_or(i32::MAX))
                        .bind(&record.kind)
                        .bind(record.room_id.as_deref())
                        .bind(record.round_uuid.as_deref())
                        .bind(record.player_id)
                        .bind(raw_item)
                        .bind(now)
                        .bind(record.schema_version)
                        .execute(&pool)
                        .await;
                    }
                }
            });
            return true;
        }
        false
    }

    pub fn record_user_seen_sync(&self, user_id: i32, name: &str, language: &str, ip: Option<String>) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            let name = name.to_string();
            let language = language.to_string();
            tokio::spawn(async move {
                let now = now_ms();
                let _ = sqlx::query(
                    "INSERT INTO mp_users (user_id, name, language, first_seen_at, last_seen_at, last_connected_at, updated_at)
                     VALUES ($1, $2, $3, $4, $4, $4, $4)
                     ON CONFLICT (user_id) DO UPDATE SET
                       name = EXCLUDED.name,
                       language = EXCLUDED.language,
                       last_seen_at = EXCLUDED.last_seen_at,
                       last_connected_at = EXCLUDED.last_connected_at,
                       updated_at = EXCLUDED.updated_at"
                )
                .bind(user_id)
                .bind(&name)
                .bind(&language)
                .bind(now)
                .execute(&pool)
                .await;
                let _ = append_event_pg(&pool, "user.connect", None, Some(user_id), serde_json::json!({
                    "user_id": user_id,
                    "name": name,
                    "language": language,
                    "ip": ip,
                })).await;
            });
        }
    }

    pub fn record_user_disconnect_sync(&self, user_id: i32, name: &str) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            let name = name.to_string();
            tokio::spawn(async move {
                let now = now_ms();
                let _ = sqlx::query(
                    "UPDATE mp_users SET last_disconnected_at = $2, updated_at = $2 WHERE user_id = $1"
                )
                .bind(user_id)
                .bind(now)
                .execute(&pool)
                .await;
                let _ = append_event_pg(&pool, "user.disconnect", None, Some(user_id), serde_json::json!({
                    "user_id": user_id,
                    "name": name,
                })).await;
            });
        }
    }

    pub fn record_sim_event_sync(&self, run_id: Option<String>, kind: &str, payload: Value) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            let kind = kind.to_string();
            tokio::spawn(async move {
                let now = now_ms();
                let _ = sqlx::query(
                    "INSERT INTO mp_sim_events (run_id, kind, payload, created_at)
                     VALUES ($1, $2, $3, $4)"
                )
                .bind(run_id.as_deref())
                .bind(&kind)
                .bind(payload)
                .bind(now)
                .execute(&pool)
                .await;
            });
        }
    }

    pub fn record_room_snapshot_sync(&self, room_id: String, room_uuid: String, payload: Value) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            tokio::spawn(async move {
                let _ = record_room_snapshot_pg(&pool, &room_id, &room_uuid, payload).await;
            });
        }
    }

    pub fn record_room_event_sync(&self, kind: &str, room_id: Option<String>, user_id: Option<i32>, payload: Value) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            let kind = kind.to_string();
            tokio::spawn(async move {
                let _ = append_event_pg(&pool, &kind, room_id.as_deref(), user_id, payload).await;
            });
        }
    }

    pub fn record_user_room_history_sync(&self, user_id: i32, room_id: String, room_uuid: String, joined_at: i64) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            tokio::spawn(async move {
                let _ = sqlx::query(
                    "INSERT INTO room_history (user_id, room_id, room_uuid, joined_at) VALUES ($1, $2, $3, $4)"
                )
                .bind(user_id)
                .bind(&room_id)
                .bind(&room_uuid)
                .bind(joined_at)
                .execute(&pool)
                .await;
                let _ = sqlx::query(
                    "INSERT INTO mp_user_room_history (user_id, room_id, room_uuid, joined_at, created_at)
                     VALUES ($1, $2, $3, $4, $4)"
                )
                .bind(user_id)
                .bind(&room_id)
                .bind(&room_uuid)
                .bind(joined_at)
                .execute(&pool)
                .await;
                let _ = append_event_pg(&pool, "room.join", Some(&room_id), Some(user_id), serde_json::json!({
                    "user_id": user_id,
                    "room_id": room_id,
                    "room_uuid": room_uuid,
                    "joined_at": joined_at,
                })).await;
            });
        }
    }

    pub async fn get_playtime(&self, user_id: i32) -> Option<PlaytimeRow> {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let row = sqlx::query("SELECT total_secs, session_start FROM playtime WHERE user_id = $1")
                .bind(user_id)
                .fetch_optional(pool)
                .await
                .ok()??;
            return Some(PlaytimeRow {
                total_secs: row.try_get::<i64, _>("total_secs").unwrap_or(0),
                session_start: row.try_get::<Option<i64>, _>("session_start").ok().flatten(),
            });
        }
        None
    }

    pub async fn top_playtime(&self, limit: i64) -> Vec<Value> {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let now = now_secs();
            let rows = sqlx::query(
                "SELECT p.user_id, COALESCE(u.name, p.user_id::text) AS name,
                        p.total_secs + CASE WHEN p.session_start IS NULL THEN 0 ELSE $2 - p.session_start END AS secs
                 FROM playtime p LEFT JOIN mp_users u ON u.user_id = p.user_id
                 ORDER BY secs DESC LIMIT $1"
            )
            .bind(limit)
            .bind(now)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
            return rows.into_iter().map(|row| serde_json::json!({
                "user_id": row.try_get::<i32, _>("user_id").unwrap_or_default(),
                "name": row.try_get::<String, _>("name").unwrap_or_default(),
                "total_secs": row.try_get::<i64, _>("secs").unwrap_or_default(),
            })).collect();
        }
        Vec::new()
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
            let row = sqlx::query("SELECT value::text AS value FROM mp_settings WHERE key = 'admin_phira_ids'")
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
            let _ = append_event_pg(pool, "round.open", Some(&meta.room_id), None, serde_json::to_value(meta).unwrap_or_default()).await;
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
            let _ = append_event_pg(pool, "round.close", None, None, serde_json::json!({"round_uuid": round_uuid, "finished_at": now})).await;
        }
    }

    pub async fn append_touches(&self, round_uuid: &str, player_id: i32, data: &[crate::plugin::TouchEventPoint]) {
        if data.is_empty() { return; }
        let value = serde_json::to_value(data).unwrap_or(Value::Array(Vec::new()));
        self.append_player_array(round_uuid, player_id, "touches", value.clone()).await;
        self.append_touch_batch(round_uuid, player_id, data, value).await;
    }

    pub async fn append_judges(&self, round_uuid: &str, player_id: i32, data: &[crate::plugin::JudgeEventItem]) {
        if data.is_empty() { return; }
        let value = serde_json::to_value(data).unwrap_or(Value::Array(Vec::new()));
        self.append_player_array(round_uuid, player_id, "judges", value.clone()).await;
        self.append_judge_batch(round_uuid, player_id, data, value).await;
    }

    async fn append_touch_batch(&self, round_uuid: &str, player_id: i32, data: &[crate::plugin::TouchEventPoint], payload: Value) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let (first_game_time, last_game_time) = telemetry_time_range(data.iter().map(|p| p.time as f64));
            let now = now_ms();
            let count = i32::try_from(data.len()).unwrap_or(i32::MAX);
            let payload = payload.to_string();
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
            .bind(payload)
            .bind(now)
            .execute(pool)
            .await;
        }
    }

    async fn append_judge_batch(&self, round_uuid: &str, player_id: i32, data: &[crate::plugin::JudgeEventItem], payload: Value) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let (first_game_time, last_game_time) = telemetry_time_range(data.iter().map(|p| p.time as f64));
            let now = now_ms();
            let count = i32::try_from(data.len()).unwrap_or(i32::MAX);
            let payload = payload.to_string();
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
            .bind(payload)
            .bind(now)
            .execute(pool)
            .await;
        }
    }

    async fn append_player_array(&self, round_uuid: &str, player_id: i32, column: &str, data: Value) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            if !data.as_array().is_some_and(|arr| !arr.is_empty()) {
                return;
            }
            let now = now_ms();
            let data = data.to_string();
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
                .bind(data)
                .bind(now)
                .execute(pool)
                .await;
        }
    }

    pub async fn record_round_result(&self, round_uuid: &str, room_id: &str, result: &crate::room::PlayResult) {
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
            let _ = append_event_pg(pool, "round.result", Some(room_id), Some(result.user_id), serde_json::to_value(result).unwrap_or_default()).await;
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
            return rows.into_iter().filter_map(|row| {
                let players_raw = row.try_get::<String, _>("players").unwrap_or_else(|_| "[]".to_string());
                Some(crate::round_store::RoundMeta {
                    round_uuid: row.try_get::<String, _>("round_uuid").ok()?,
                    room_id: row.try_get::<String, _>("room_id").ok()?,
                    chart_id: row.try_get::<i32, _>("chart_id").ok()?,
                    chart_name: row.try_get::<String, _>("chart_name").ok()?,
                    players: serde_json::from_str(&players_raw).unwrap_or_default(),
                    started_at: row.try_get::<i64, _>("started_at").ok()?,
                    finished_at: row.try_get::<Option<i64>, _>("finished_at").ok().flatten(),
                })
            }).collect();
        }
        Vec::new()
    }

    pub async fn read_round_player_data(&self, round_uuid: &str, player_id: i32) -> Option<crate::round_store::RoundPlayerData> {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let legacy_row = sqlx::query(
                "SELECT touches::text AS touches, judges::text AS judges
                 FROM mp_round_player_data WHERE round_uuid = $1 AND player_id = $2"
            )
            .bind(round_uuid)
            .bind(player_id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();

            let mut touches = Vec::new();
            let mut judges = Vec::new();
            if let Some(row) = legacy_row {
                let touches_raw = row.try_get::<String, _>("touches").unwrap_or_else(|_| "[]".to_string());
                let judges_raw = row.try_get::<String, _>("judges").unwrap_or_else(|_| "[]".to_string());
                touches = serde_json::from_str(&touches_raw).unwrap_or_default();
                judges = serde_json::from_str(&judges_raw).unwrap_or_default();
            }

            // Runtime v2 worker_only/fallback_only modes may skip the legacy
            // mp_round_player_data row entirely.  Read normalized raw items as
            // a fallback so existing RoundStore/WIT/plugin read paths keep
            // working after telemetry cutover.
            if touches.is_empty() {
                touches = query_runtime_telemetry_items::<crate::plugin::TouchEventPoint>(
                    pool,
                    "touch",
                    round_uuid,
                    player_id,
                )
                .await;
            }
            if judges.is_empty() {
                judges = query_runtime_telemetry_items::<crate::plugin::JudgeEventItem>(
                    pool,
                    "judge",
                    round_uuid,
                    player_id,
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

    pub async fn query_events(&self, since_sequence: i64, limit: i64, kind: Option<&str>, room_id: Option<&str>, user_id: Option<i32>) -> Vec<Value> {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let rows = sqlx::query(
                "SELECT sequence, kind, room_id, user_id, payload::text AS payload, created_at
                 FROM mp_events
                 WHERE sequence > $1
                   AND ($2::text IS NULL OR kind = $2)
                   AND ($3::text IS NULL OR room_id = $3)
                   AND ($4::int IS NULL OR user_id = $4)
                 ORDER BY sequence ASC LIMIT $5"
            )
            .bind(since_sequence)
            .bind(kind)
            .bind(room_id)
            .bind(user_id)
            .bind(limit)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
            return rows.into_iter().map(|row| {
                let payload = row.try_get::<String, _>("payload").ok()
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
            }).collect();
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
            return rows.into_iter().map(|row| {
                let payload = row.try_get::<String, _>("payload").ok()
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
            }).collect();
        }
        Vec::new()
    }

    pub async fn query_touch_batches(&self, since_sequence: i64, limit: i64, round_uuid: Option<&str>, player_id: Option<i32>) -> Vec<Value> {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let legacy = query_telemetry_batches(pool, "mp_round_touch_batches", since_sequence, limit, round_uuid, player_id).await;
            if !legacy.is_empty() {
                return legacy;
            }
            return query_runtime_telemetry_batches(pool, "touch", since_sequence, limit, round_uuid, player_id).await;
        }
        Vec::new()
    }

    pub async fn query_judge_batches(&self, since_sequence: i64, limit: i64, round_uuid: Option<&str>, player_id: Option<i32>) -> Vec<Value> {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let legacy = query_telemetry_batches(pool, "mp_round_judge_batches", since_sequence, limit, round_uuid, player_id).await;
            if !legacy.is_empty() {
                return legacy;
            }
            return query_runtime_telemetry_batches(pool, "judge", since_sequence, limit, round_uuid, player_id).await;
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
                let cutoff = now_ms().saturating_sub(touch_judge_retention_days as i64 * 86_400_000);
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
    rows.into_iter().map(|row| {
        let payload = row.try_get::<String, _>("payload").ok()
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
    }).collect()
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
         ORDER BY sequence ASC"
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
async fn append_event_pg(pool: &sqlx::PgPool, kind: &str, room_id: Option<&str>, user_id: Option<i32>, payload: Value) -> Result<()> {
    let payload = payload.to_string();
    sqlx::query(
        "INSERT INTO mp_events (kind, room_id, user_id, payload, created_at)
         VALUES ($1, $2, $3, $4::jsonb, $5)"
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
async fn record_room_snapshot_pg(pool: &sqlx::PgPool, room_id: &str, room_uuid: &str, payload: Value) -> Result<()> {
    let payload = payload.to_string();
    let now = now_ms();
    sqlx::query(
        "INSERT INTO mp_room_snapshots (room_id, room_uuid, payload, created_at, updated_at, sequence)
         VALUES ($1, $2, $3::jsonb, $4, $4, nextval('mp_persist_sequence'))
         ON CONFLICT (room_id) DO UPDATE SET
           room_uuid = EXCLUDED.room_uuid,
           payload = EXCLUDED.payload,
           updated_at = EXCLUDED.updated_at,
           sequence = EXCLUDED.sequence"
    )
    .bind(room_id)
    .bind(room_uuid)
    .bind(payload)
    .bind(now)
    .execute(pool)
    .await?;
    append_event_pg(pool, "room.snapshot", Some(room_id), None, serde_json::json!({
        "room_id": room_id,
        "room_uuid": room_uuid,
    })).await?;
    Ok(())
}

/// 创建数据库表
#[cfg(feature = "postgres")]
async fn init_tables(pool: &sqlx::PgPool) -> Result<()> {
    sqlx::query("CREATE SEQUENCE IF NOT EXISTS mp_persist_sequence").execute(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS playtime (
            user_id INTEGER PRIMARY KEY,
            total_secs BIGINT NOT NULL DEFAULT 0,
            session_start BIGINT
        )"
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
        )"
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
        )"
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
        )"
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
        )"
    )
    .execute(pool)
    .await?;
    // 兼容早期补丁中以 BIGSERIAL 创建的 mp_events：改回全局 sequence，统一跨表顺序。
    let _ = sqlx::query("ALTER TABLE mp_events ALTER COLUMN sequence SET DEFAULT nextval('mp_persist_sequence')")
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
        )"
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
        )"
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
        )"
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
        )"
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
        )"
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
        )"
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
            dual_write BOOLEAN NOT NULL DEFAULT TRUE,
            schema_version INTEGER NOT NULL DEFAULT 2,
            flush_reason TEXT NOT NULL DEFAULT 'unknown'
        )"
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
        )"
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_runtime_persistence_meta (
            key TEXT PRIMARY KEY,
            value JSONB NOT NULL,
            updated_at BIGINT NOT NULL
        )"
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_runtime_retention_policies (
            scope TEXT PRIMARY KEY,
            retain_seconds BIGINT NOT NULL,
            cleanup_enabled BOOLEAN NOT NULL DEFAULT FALSE,
            updated_at BIGINT NOT NULL
        )"
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
        )"
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS mp_settings (
            key TEXT PRIMARY KEY,
            value JSONB NOT NULL,
            updated_at BIGINT NOT NULL
        )"
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
        "mode": "dual_write",
        "available_modes": ["legacy_only", "dual_write", "worker_only", "fallback_only"],
        "notes": "Runtime v2 telemetry schema normalizes batch headers and raw telemetry items. Project is still in test stage; schema may change freely."
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
        "mode": "dual_write",
        "description": "legacy direct write plus Runtime v2 telemetry batch-write",
        "available_modes": ["legacy_only", "dual_write", "worker_only", "fallback_only"],
        "updated_by": "runtime_v2.bootstrap"
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
        "CREATE INDEX IF NOT EXISTS idx_mp_sim_events_run ON mp_sim_events(run_id)",
        "CREATE INDEX IF NOT EXISTS idx_mp_sim_events_kind ON mp_sim_events(kind)",
        "CREATE INDEX IF NOT EXISTS idx_mp_sim_events_created ON mp_sim_events(created_at)",
    ] {
        sqlx::query(index).execute(pool).await?;
    }

    tracing::info!("统一 PostgreSQL 持久化表已就绪");
    Ok(())
}

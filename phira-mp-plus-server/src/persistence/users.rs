//! User persistence — online/offline tracking, playtime, user records.
//!
//! Extracted from db.rs to keep user-related SQL separate from
//! general-purpose database helpers.

use crate::db::DbManager;
use serde_json::Value;
use sqlx::Row;

impl DbManager {
    /// Mark a user as online and wait for PostgreSQL acknowledgement.
    pub async fn set_online(&self, user_id: i32) -> bool {
        let Self::Pg(pool) = self;
        let now = now_ms_inline();
        sqlx::query(
            "INSERT INTO playtime (user_id, total_secs, session_start) VALUES ($1, 0, $2)
                 ON CONFLICT (user_id) DO UPDATE SET
                   total_secs = playtime.total_secs + CASE
                     WHEN playtime.session_start IS NULL THEN 0
                     ELSE GREATEST(0, ($2 - playtime.session_start) / 1000)
                   END,
                   session_start = $2",
        )
        .bind(user_id)
        .bind(now)
        .execute(pool)
        .await
        .is_ok()
    }

    /// Mark a user as offline and wait for PostgreSQL acknowledgement.
    pub async fn set_offline(&self, user_id: i32) -> bool {
        let Self::Pg(pool) = self;
        let now = now_ms_inline();
        sqlx::query(
            "UPDATE playtime
                 SET total_secs = total_secs + GREATEST(0, ($2 - session_start) / 1000),
                     session_start = NULL
                 WHERE user_id = $1 AND session_start IS NOT NULL",
        )
        .bind(user_id)
        .bind(now)
        .execute(pool)
        .await
        .is_ok()
    }

    /// Upsert a user record and wait for acknowledgement.
    /// Increments login_count on conflict to track total visit count.
    pub async fn record_user_seen(
        &self,
        user_id: i32,
        name: &str,
        language: &str,
        ip: Option<String>,
    ) -> Result<(), sqlx::Error> {
        let Self::Pg(pool) = self;
        let now = now_ms_inline();
        sqlx::query(
            "INSERT INTO mp_users (
                   user_id, name, language, ip, first_seen_at, last_seen_at,
                   last_connected_at, updated_at, login_count
                 )
                 VALUES ($1, $2, $3, $4, $5, $5, $5, $5, 1)
                 ON CONFLICT (user_id) DO UPDATE SET
                   name = EXCLUDED.name,
                   language = EXCLUDED.language,
                   ip = COALESCE(EXCLUDED.ip, mp_users.ip),
                   last_seen_at = EXCLUDED.last_seen_at,
                   last_connected_at = EXCLUDED.last_connected_at,
                   updated_at = EXCLUDED.updated_at,
                   login_count = mp_users.login_count + 1",
        )
        .bind(user_id)
        .bind(name)
        .bind(language)
        .bind(ip)
        .bind(now)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Record a user disconnect and wait for acknowledgement.
    pub async fn record_user_disconnect(&self, user_id: i32, name: &str) -> bool {
        let Self::Pg(pool) = self;
        let now = now_ms_inline();
        sqlx::query(
            "UPDATE mp_users
                 SET name = $2,
                     last_seen_at = $3,
                     last_disconnected_at = $3,
                     updated_at = $3
                 WHERE user_id = $1",
        )
        .bind(user_id)
        .bind(name)
        .bind(now)
        .execute(pool)
        .await
        .is_ok()
    }

    /// Mark a user as online.
    pub fn set_online_sync(&self, user_id: i32) {
        let Self::Pg(pool) = self;
        let pool = pool.clone();
        let now = now_ms_inline();
        tokio::spawn(async move {
            let _ = sqlx::query(
                "INSERT INTO playtime (user_id, total_secs, session_start) VALUES ($1, 0, $2)
                     ON CONFLICT (user_id) DO UPDATE SET
                       total_secs = playtime.total_secs + CASE
                         WHEN playtime.session_start IS NULL THEN 0
                         ELSE GREATEST(0, ($2 - playtime.session_start) / 1000)
                       END,
                       session_start = $2",
            )
            .bind(user_id)
            .bind(now)
            .execute(&pool)
            .await;
        });
    }

    /// Mark a user as offline and update total playtime.
    pub fn set_offline_sync(&self, user_id: i32) {
        let Self::Pg(pool) = self;
        let pool = pool.clone();
        let now = now_ms_inline();
        tokio::spawn(async move {
            let _ = sqlx::query(
                "UPDATE playtime
                     SET total_secs = total_secs + GREATEST(0, ($2 - session_start) / 1000),
                         session_start = NULL
                     WHERE user_id = $1 AND session_start IS NOT NULL",
            )
            .bind(user_id)
            .bind(now)
            .execute(&pool)
            .await;
        });
    }

    /// Record that a user was seen (upsert into mp_users).
    pub fn record_user_seen_sync(
        &self,
        user_id: i32,
        name: &str,
        language: &str,
        ip: Option<String>,
    ) {
        let Self::Pg(pool) = self;
        let pool = pool.clone();
        let name = name.to_string();
        let language = language.to_string();
        let now = now_ms_inline();
        tokio::spawn(async move {
            let _ = sqlx::query(
                "INSERT INTO mp_users (
                       user_id, name, language, ip, first_seen_at, last_seen_at,
                       last_connected_at, updated_at
                     )
                     VALUES ($1, $2, $3, $4, $5, $5, $5, $5)
                     ON CONFLICT (user_id) DO UPDATE SET
                       name = EXCLUDED.name,
                       language = EXCLUDED.language,
                       ip = COALESCE(EXCLUDED.ip, mp_users.ip),
                       last_seen_at = EXCLUDED.last_seen_at,
                       last_connected_at = EXCLUDED.last_connected_at,
                       updated_at = EXCLUDED.updated_at",
            )
            .bind(user_id)
            .bind(&name)
            .bind(&language)
            .bind(&ip)
            .bind(now)
            .execute(&pool)
            .await;
        });
    }

    /// Record a user's IP address in user_ip_history (upsert).
    /// Called on each connection so we track all IPs a user has used.
    pub fn record_user_ip(&self, user_id: i32, ip: &str) {
        let Self::Pg(pool) = self;
        let pool = pool.clone();
        let ip = ip.to_string();
        let now = now_ms_inline();
        tokio::spawn(async move {
            let _ = sqlx::query(
                "INSERT INTO user_ip_history (user_id, ip, first_seen_at, last_seen_at, use_count)
                 VALUES ($1, $2, $3, $3, 1)
                 ON CONFLICT (user_id, ip) DO UPDATE SET
                   last_seen_at = EXCLUDED.last_seen_at,
                   use_count = user_ip_history.use_count + 1",
            )
            .bind(user_id)
            .bind(&ip)
            .bind(now)
            .execute(&pool)
            .await;
        });
    }

    /// Record user disconnect with optional name and time.
    pub fn record_user_disconnect_sync(&self, user_id: i32, name: &str) {
        let Self::Pg(pool) = self;
        let pool = pool.clone();
        let name = name.to_string();
        let now = now_ms_inline();
        tokio::spawn(async move {
            let _ = sqlx::query(
                "UPDATE mp_users
                     SET name = $2,
                         last_seen_at = $3,
                         last_disconnected_at = $3,
                         updated_at = $3
                     WHERE user_id = $1",
            )
            .bind(user_id)
            .bind(&name)
            .bind(now)
            .execute(&pool)
            .await;
        });
    }

    /// Get total playtime for a user.
    pub async fn get_playtime(&self, user_id: i32) -> Option<crate::db::PlaytimeRow> {
        let Self::Pg(pool) = self;
        let row =
            sqlx::query("SELECT total_secs, session_start FROM playtime WHERE user_id = $1")
                .bind(user_id)
                .fetch_optional(pool)
                .await
                .ok()??;
        Some(crate::db::PlaytimeRow {
            total_secs: row.try_get::<i64, _>("total_secs").unwrap_or(0),
            session_start: row
                .try_get::<Option<i64>, _>("session_start")
                .ok()
                .flatten(),
        })
    }

    /// Get top playtime users.
    pub async fn top_playtime(&self, limit: i64) -> Vec<Value> {
        let Self::Pg(pool) = self;
        let now = now_ms_inline();
        let rows = sqlx::query(
            "SELECT p.user_id, COALESCE(u.name, p.user_id::text) AS name,
                        p.total_secs + CASE
                          WHEN p.session_start IS NULL THEN 0
                          ELSE GREATEST(0, ($2 - p.session_start) / 1000)
                        END AS secs
                 FROM playtime p LEFT JOIN mp_users u ON u.user_id = p.user_id
                 ORDER BY secs DESC LIMIT $1",
        )
        .bind(limit)
        .bind(now)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        rows.into_iter()
            .map(|row| {
                serde_json::json!({
                    "user_id": row.try_get::<i32, _>("user_id").unwrap_or_default(),
                    "total_playtime": row.try_get::<i64, _>("secs").unwrap_or_default(),
                })
            })
            .collect()
    }

    /// Clean up stale playtime sessions that were orphaned by a server crash.
    ///
    /// A session is considered stale if `session_start` is older than 24 hours.
    /// The elapsed time is accrued to `total_secs` and `session_start` is set to
    /// NULL so the row is ready for the next normal online/offline cycle.
    pub async fn cleanup_stale_playtime_sessions(&self) -> std::result::Result<u64, String> {
        let Self::Pg(pool) = self;
        let now = now_ms_inline();
        let result = sqlx::query(
            "UPDATE playtime
             SET total_secs = total_secs + GREATEST(0, ($1 - session_start) / 1000),
                 session_start = NULL
             WHERE session_start IS NOT NULL
               AND session_start < $1 - 86400000",
        )
        .bind(now)
        .execute(pool)
        .await
        .map_err(|e| format!("cleanup stale playtime sessions: {e}"))?;
        Ok(result.rows_affected())
    }
}

impl DbManager {
    /// Record a runtime persistence metadata key/value pair.
    pub fn record_runtime_persistence_meta_sync(&self, key: &str, value: Value) {
        let Self::Pg(pool) = self;
        let pool = pool.clone();
        let key = key.to_string();
        let now = now_ms_inline();
        tokio::spawn(async move {
            let _ = sqlx::query(
                "INSERT INTO mp_runtime_persistence_meta (key, value, updated_at)
                     VALUES ($1, $2::jsonb, $3)
                     ON CONFLICT (key) DO UPDATE
                     SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at",
            )
            .bind(&key)
            .bind(&value)
            .bind(now)
            .execute(&pool)
            .await;
        });
    }
}

/// Inline now_ms helper (replaces db::now_ms for standalone module).
fn now_ms_inline() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

//! User persistence — online/offline tracking, playtime, user records.
//!
//! Extracted from db.rs to keep user-related SQL separate from
//! general-purpose database helpers.

use crate::db::DbManager;
use serde_json::Value;
use sqlx::Row;

impl DbManager {
    /// Mark a user as offline and wait for PostgreSQL acknowledgement.
    /// Only closes the playtime session whose `server_instance_id` AND
    /// `session_id` match the event's — prevents an old offline event from
    /// closing a newer session after a same-instance reconnect (session
    /// generation protection).
    pub async fn set_offline(
        &self,
        user_id: i32,
        event_instance_id: &str,
        event_session_id: &str,
    ) -> bool {
        let Self::Pg(pool) = self;
        let now = now_ms_inline();
        sqlx::query(
            "UPDATE playtime
                 SET total_secs = total_secs + GREATEST(0, ($2 - session_start) / 1000),
                     session_start = NULL,
                     server_instance_id = NULL,
                     session_id = NULL
                 WHERE user_id = $1
                   AND session_start IS NOT NULL
                   AND (server_instance_id IS NOT DISTINCT FROM $3)
                   AND (session_id IS NOT DISTINCT FROM $4 OR $4 = '')",
        )
        .bind(user_id)
        .bind(now)
        .bind(event_instance_id)
        .bind(event_session_id)
        .execute(pool)
        .await
        .is_ok()
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

    /// Reconcile `mp_users.login_count` against the actual per-user visit count
    /// in `mp_user_visits` (P1: total visits reconciliation).
    ///
    /// Returns the number of user rows corrected.  Runs periodically so that
    /// transient auth failures or retry paths cannot permanently desync the
    /// aggregate counter from the idempotent visit ledger.
    pub async fn reconcile_login_counts(&self) -> std::result::Result<u64, String> {
        let Self::Pg(pool) = self;
        let result = sqlx::query(
            "UPDATE mp_users u
             SET login_count = v.visits, updated_at = EXTRACT(EPOCH FROM now())::bigint * 1000
             FROM (SELECT user_id, COUNT(*) AS visits FROM mp_user_visits GROUP BY user_id) v
             WHERE u.user_id = v.user_id AND u.login_count != v.visits",
        )
        .execute(pool)
        .await
        .map_err(|e| format!("reconcile login counts: {e}"))?;
        Ok(result.rows_affected())
    }

    /// Atomic commit for a full UserAuthenticated event in a single PG transaction.
    ///
    /// Combines visit recording (idempotent via `mp_user_visits`), user upsert
    /// with conditional `login_count` increment, IP history, and playtime
    /// online-set into one atomic operation.  If the `event_id` has already been
    /// processed, `login_count` is *not* incremented — making the handler fully
    /// idempotent against retry/replay.
    ///
    /// The `server_instance_id` comes from the event itself (captured at event
    /// creation time, not read from the global at process time) so that WAL/DLQ
    /// replay on a new instance preserves the original session ownership.
    pub async fn commit_user_authenticated(
        &self,
        event_id: &str,
        session_id: &str,
        user_id: i32,
        user_name: &str,
        language: &str,
        ip: &str,
        connected_at: i64,
        server_instance_id: &str,
    ) -> bool {
        let Self::Pg(pool) = self;
        let now = now_ms_inline();

        let mut tx = match pool.begin().await {
            Ok(tx) => tx,
            Err(_) => return false,
        };

        // 1. Insert visit record (idempotent guard).
        // `ON CONFLICT DO NOTHING` (no index specified) catches conflicts on
        // ANY unique constraint — both event_id and session_id.  Previously
        // `ON CONFLICT (event_id)` only handled the event_id index, so a retry
        // that reused an existing session_id with a new event_id raised a hard
        // error, failing the whole transaction and sending the event to the
        // dead-letter queue in a loop (P1).
        let is_new_visit = match sqlx::query(
            "INSERT INTO mp_user_visits (event_id, session_id, user_id, connected_at, created_at)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT DO NOTHING",
        )
        .bind(event_id)
        .bind(session_id)
        .bind(user_id)
        .bind(connected_at)
        .bind(now)
        .execute(&mut *tx)
        .await
        {
            Ok(r) if r.rows_affected() > 0 => true,
            Ok(_) => {
                // Not inserted — an event_id or session_id conflict.  Look up
                // the existing visit and verify it belongs to the same user
                // (idempotent replay) rather than a different user reusing
                // this session (data integrity error).
                let existing: Option<i32> = sqlx::query_scalar(
                    "SELECT user_id FROM mp_user_visits
                      WHERE event_id = $1 OR session_id = $2
                      LIMIT 1",
                )
                .bind(event_id)
                .bind(session_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| ())
                .ok()
                .flatten();
                match existing {
                    Some(uid) if uid == user_id => {
                        // Same user replayed this auth — treat as idempotent
                        // success, do NOT increment login_count again.
                        false
                    }
                    Some(_) => {
                        // session_id reused by a DIFFERENT user — integrity
                        // violation.  Do not silently proceed.
                        tracing::warn!(
                            session_id,
                            "mp_user_visits session_id already used by another user; \
                             refusing to commit conflicting auth"
                        );
                        let _ = tx.rollback().await;
                        return false;
                    }
                    None => {
                        // Conflict reported but no row found (race) — safest to
                        // treat as idempotent to avoid a login_count bump.
                        tracing::warn!(
                            session_id,
                            "mp_user_visits conflict but no existing row found; treating as idempotent"
                        );
                        false
                    }
                }
            }
            Err(_) => {
                let _ = tx.rollback().await;
                return false;
            }
        };

        // 2. Upsert user — only increment login_count when this is a new visit.
        let login_inc: i64 = if is_new_visit { 1 } else { 0 };
        if sqlx::query(
            "INSERT INTO mp_users (
                   user_id, name, language, ip, first_seen_at, last_seen_at,
                   last_connected_at, updated_at, login_count
               )
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 1)
               ON CONFLICT (user_id) DO UPDATE SET
                 name = EXCLUDED.name,
                 language = EXCLUDED.language,
                 ip = COALESCE(EXCLUDED.ip, mp_users.ip),
                 last_seen_at = EXCLUDED.last_seen_at,
                 last_connected_at = EXCLUDED.last_connected_at,
                 updated_at = EXCLUDED.updated_at,
                 login_count = mp_users.login_count + $9",
        )
        .bind(user_id)
        .bind(user_name)
        .bind(language)
        .bind(ip)
        .bind(now)
        .bind(now)
        .bind(connected_at)
        .bind(now)
        .bind(login_inc)
        .execute(&mut *tx)
        .await
        .is_err()
        {
            let _ = tx.rollback().await;
            return false;
        }

        // 3. Record IP in user_ip_history (best-effort, does not fail the tx).
        if !ip.is_empty() {
            let _ = sqlx::query(
                "INSERT INTO user_ip_history (user_id, ip, first_seen_at, last_seen_at, use_count)
                 VALUES ($1, $2, $3, $3, 1)
                 ON CONFLICT (user_id, ip) DO UPDATE SET
                   last_seen_at = EXCLUDED.last_seen_at,
                   use_count = user_ip_history.use_count + 1",
            )
            .bind(user_id)
            .bind(ip)
            .bind(now)
            .execute(&mut *tx)
            .await;
        }

        // 4. Set online (playtime) — use connected_at for session_start so the
        //    elapsed-time calculation is relative to the actual connection time.
        //    Use the event's server_instance_id (captured when the event was
        //    created) so WAL/DLQ replay on a new instance preserves the
        //    original session ownership.  Previously the global
        //    server_instance::current() was read here, which caused historical
        //    auth events replayed on a new instance to be marked as belonging
        //    to the current instance — producing phantom online sessions.
        if sqlx::query(
            "INSERT INTO playtime (user_id, total_secs, session_start, server_instance_id, session_id)
             VALUES ($1, 0, $2, $3, $4)
             ON CONFLICT (user_id) DO UPDATE SET
               total_secs = playtime.total_secs + CASE
                 WHEN playtime.session_start IS NULL THEN 0
                 WHEN playtime.server_instance_id IS DISTINCT FROM $3 THEN 0
                 ELSE GREATEST(0, ($2 - playtime.session_start) / 1000)
               END,
               session_start = $2,
               server_instance_id = $3,
               session_id = $4",
        )
        .bind(user_id)
        .bind(connected_at)
        .bind(server_instance_id)
        .bind(session_id)
        .execute(&mut *tx)
        .await
        .is_err()
        {
            let _ = tx.rollback().await;
            return false;
        }

        tx.commit().await.is_ok()
    }
    /// Clean up stale playtime sessions that were orphaned by a server crash.
    ///
    /// On startup any `session_start` that is still set belongs to a previous
    /// server instance (planned shutdown or crash).  Only sessions whose
    /// `server_instance_id` differs from the current instance are closed —
    /// sessions re-established by WAL replay (which carry the current instance
    /// ID) are left active.
    ///
    /// The elapsed time is accrued to `total_secs` (capped at
    /// `max_recovery_secs` per session) and `session_start` and
    /// `server_instance_id` are set to NULL so the row is ready for the next
    /// normal online/offline cycle.
    ///
    /// The cap (default 3600s = 1 hour) prevents a long outage from being
    /// counted as playtime upon recovery.
    pub async fn close_all_stale_sessions(
        &self,
        max_recovery_secs: i64,
    ) -> std::result::Result<u64, String> {
        let Self::Pg(pool) = self;
        let now = now_ms_inline();
        let instance_id = crate::server_instance::current();
        // Accrue playtime only up to the old instance's last known alive time
        // (heartbeat), not the recovery's startup time.  Server downtime
        // between the old instance's death and this recovery is therefore NOT
        // counted as player playtime (P0-H).  Sessions without an instance
        // record (pre-migration data) fall back to the startup time, capped at
        // max_recovery_secs as before.
        let result = sqlx::query(
            "UPDATE playtime p
             SET total_secs = p.total_secs + LEAST(
                   GREATEST(0, (COALESCE(
                     (SELECT si.last_alive_at FROM mp_server_instances si
                       WHERE si.instance_id = p.server_instance_id), $1)
                     - p.session_start) / 1000),
                   $2),
                 session_start = NULL,
                 server_instance_id = NULL
             WHERE p.session_start IS NOT NULL
               AND (p.server_instance_id IS DISTINCT FROM $3)",
        )
        .bind(now)
        .bind(max_recovery_secs)
        .bind(instance_id)
        .execute(pool)
        .await
        .map_err(|e| format!("close all stale playtime sessions: {e}"))?;
        Ok(result.rows_affected())
    }

    /// Register the current server instance in `mp_server_instances`.
    pub async fn register_server_instance(
        &self,
        instance_id: &str,
        now: i64,
    ) -> std::result::Result<(), String> {
        let Self::Pg(pool) = self;
        sqlx::query(
            "INSERT INTO mp_server_instances (instance_id, created_at, last_alive_at)
             VALUES ($1, $2, $2)
             ON CONFLICT (instance_id) DO UPDATE SET last_alive_at = EXCLUDED.last_alive_at",
        )
        .bind(instance_id)
        .bind(now)
        .execute(pool)
        .await
        .map_err(|e| format!("register server instance: {e}"))?;
        Ok(())
    }

    /// Update the current instance's heartbeat (last_alive_at).
    pub async fn heartbeat_server_instance(
        &self,
        instance_id: &str,
        now: i64,
    ) -> std::result::Result<(), String> {
        let Self::Pg(pool) = self;
        sqlx::query(
            "UPDATE mp_server_instances SET last_alive_at = $2 WHERE instance_id = $1",
        )
        .bind(instance_id)
        .bind(now)
        .execute(pool)
        .await
        .map_err(|e| format!("heartbeat server instance: {e}"))?;
        Ok(())
    }

    /// Fetch the last known alive time for a given server instance.
    pub async fn server_instance_last_alive(
        &self,
        instance_id: &str,
    ) -> std::result::Result<Option<i64>, String> {
        let Self::Pg(pool) = self;
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT last_alive_at FROM mp_server_instances WHERE instance_id = $1",
        )
        .bind(instance_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("fetch server instance last alive: {e}"))?;
        Ok(row.map(|(v,)| v))
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

//! Room event and snapshot persistence.
//!
//! Canonical room history and snapshot writes use PostgreSQL transactions so
//! the materialized table and its audit event are committed together.

use crate::db::DbManager;
use serde_json::Value;

#[cfg(feature = "postgres")]
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

impl DbManager {
    /// Record a room join in both compatibility and Runtime v2 history tables,
    /// together with the audit event, in one transaction.
    ///
    /// The `(user_id, room_uuid, joined_at)` natural key makes retry after an
    /// uncertain acknowledgement idempotent for the two history tables.
    pub async fn record_user_room_history(
        &self,
        user_id: i32,
        room_id: &str,
        room_uuid: &str,
        joined_at: i64,
    ) -> bool {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let Ok(mut transaction) = pool.begin().await else {
                return false;
            };
            let compatibility_insert = sqlx::query(
                "INSERT INTO room_history (user_id, room_id, room_uuid, joined_at)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (user_id, room_uuid, joined_at) DO NOTHING",
            )
            .bind(user_id)
            .bind(room_id)
            .bind(room_uuid)
            .bind(joined_at)
            .execute(&mut *transaction)
            .await;
            let compatibility_insert = match compatibility_insert {
                Ok(result) => result,
                Err(_) => return false,
            };
            let runtime_insert = sqlx::query(
                "INSERT INTO mp_user_room_history
                   (user_id, room_id, room_uuid, joined_at, created_at)
                 VALUES ($1, $2, $3, $4, $4)
                 ON CONFLICT (user_id, room_uuid, joined_at) DO NOTHING",
            )
            .bind(user_id)
            .bind(room_id)
            .bind(room_uuid)
            .bind(joined_at)
            .execute(&mut *transaction)
            .await;
            let runtime_insert = match runtime_insert {
                Ok(result) => result,
                Err(_) => return false,
            };
            if compatibility_insert.rows_affected() == 0 && runtime_insert.rows_affected() == 0 {
                return transaction.commit().await.is_ok();
            }
            let payload = serde_json::json!({
                "user_id": user_id,
                "room_id": room_id,
                "room_uuid": room_uuid,
                "joined_at": joined_at,
            });
            if sqlx::query(
                "INSERT INTO mp_events (kind, room_id, user_id, payload, created_at)
                 VALUES ('room.join', $1, $2, $3, $4)",
            )
            .bind(room_id)
            .bind(user_id)
            .bind(payload)
            .bind(joined_at)
            .execute(&mut *transaction)
            .await
            .is_err()
            {
                return false;
            }
            return transaction.commit().await.is_ok();
        }
        false
    }

    /// Upsert the canonical room snapshot and append its audit event in one
    /// transaction.
    pub async fn record_room_snapshot(
        &self,
        room_id: &str,
        room_uuid: &str,
        payload: Value,
    ) -> bool {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            return record_room_snapshot_pg(pool, room_id, room_uuid, payload)
                .await
                .is_ok();
        }
        #[cfg(not(feature = "postgres"))]
        let _ = (room_id, room_uuid, payload);
        false
    }

    /// Record a room snapshot via spawn-and-forget compatibility adapter.
    /// New production paths should enqueue a typed `PersistenceEvent` instead.
    pub fn record_room_snapshot_sync(&self, room_id: String, room_uuid: String, payload: Value) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            tokio::spawn(async move {
                let _ = record_room_snapshot_pg(&pool, &room_id, &room_uuid, payload).await;
            });
        }
    }

    /// Record a room event and return PostgreSQL acknowledgement.
    pub async fn record_room_event(
        &self,
        kind: &str,
        room_id: Option<String>,
        user_id: Option<i32>,
        payload: Value,
    ) -> bool {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let event_id = payload
                .get("event_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string);
            return crate::db::append_event_pg(
                pool,
                event_id.as_deref(),
                kind,
                room_id.as_deref(),
                user_id,
                payload,
            )
            .await
            .is_ok();
        }
        #[cfg(not(feature = "postgres"))]
        let _ = (kind, room_id, user_id, payload);
        false
    }

    /// Synchronous compatibility adapter for a room event.
    pub fn record_room_event_sync(
        &self,
        kind: &str,
        room_id: Option<String>,
        user_id: Option<i32>,
        payload: Value,
    ) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            let kind = kind.to_string();
            tokio::spawn(async move {
                let db = DbManager::Pg(pool);
                let _ = db.record_room_event(&kind, room_id, user_id, payload).await;
            });
        }
        #[cfg(not(feature = "postgres"))]
        let _ = (kind, room_id, user_id, payload);
    }

    /// Compatibility adapter for room join history. It delegates to the same
    /// transactional implementation used by `PersistenceWorker`.
    pub fn record_user_room_history_sync(
        &self,
        user_id: i32,
        room_id: String,
        room_uuid: String,
        joined_at: i64,
    ) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let db = DbManager::Pg(pool.clone());
            tokio::spawn(async move {
                let _ = db
                    .record_user_room_history(user_id, &room_id, &room_uuid, joined_at)
                    .await;
            });
        }
    }
}

#[cfg(feature = "postgres")]
async fn record_room_snapshot_pg(
    pool: &sqlx::PgPool,
    room_id: &str,
    room_uuid: &str,
    payload: Value,
) -> Result<(), anyhow::Error> {
    let now = now_ms();
    let mut transaction = pool.begin().await?;
    let snapshot_write = sqlx::query(
        "INSERT INTO mp_room_snapshots
           (room_id, room_uuid, payload, created_at, updated_at, sequence)
         VALUES ($1, $2, $3, $4, $4, nextval('mp_persist_sequence'))
         ON CONFLICT (room_id) DO UPDATE SET
           room_uuid = EXCLUDED.room_uuid,
           payload = EXCLUDED.payload,
           updated_at = EXCLUDED.updated_at,
           sequence = EXCLUDED.sequence
         WHERE mp_room_snapshots.room_uuid IS DISTINCT FROM EXCLUDED.room_uuid
            OR mp_room_snapshots.payload IS DISTINCT FROM EXCLUDED.payload",
    )
    .bind(room_id)
    .bind(room_uuid)
    .bind(&payload)
    .bind(now)
    .execute(&mut *transaction)
    .await?;

    if snapshot_write.rows_affected() == 0 {
        transaction.commit().await?;
        return Ok(());
    }

    sqlx::query(
        "INSERT INTO mp_events (kind, room_id, user_id, payload, created_at)
         VALUES ('room.snapshot', $1, NULL, $2, $3)",
    )
    .bind(room_id)
    .bind(serde_json::json!({
        "room_id": room_id,
        "room_uuid": room_uuid,
    }))
    .bind(now)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await?;
    Ok(())
}

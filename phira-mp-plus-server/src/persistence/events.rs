//! Room event and snapshot persistence.
//!
//! Extracted from db.rs to keep room-level event INSERT logic separate from
//! general-purpose database helpers.

use crate::db::DbManager;
use serde_json::Value;

impl DbManager {
    /// Record user room join history and wait for all three writes.
    pub async fn record_user_room_history(
        &self,
        user_id: i32,
        room_id: &str,
        room_uuid: &str,
        joined_at: i64,
    ) -> bool {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            if sqlx::query(
                "INSERT INTO room_history (user_id, room_id, room_uuid, joined_at) VALUES ($1, $2, $3, $4)",
            )
            .bind(user_id)
            .bind(room_id)
            .bind(room_uuid)
            .bind(joined_at)
            .execute(pool)
            .await
            .is_err()
            {
                return false;
            }
            if sqlx::query(
                "INSERT INTO mp_user_room_history (user_id, room_id, room_uuid, joined_at, created_at)
                 VALUES ($1, $2, $3, $4, $4)",
            )
            .bind(user_id)
            .bind(room_id)
            .bind(room_uuid)
            .bind(joined_at)
            .execute(pool)
            .await
            .is_err()
            {
                return false;
            }
            return crate::db::append_event_pg(
                pool,
                "room.join",
                Some(room_id),
                Some(user_id),
                serde_json::json!({
                    "user_id": user_id,
                    "room_id": room_id,
                    "room_uuid": room_uuid,
                    "joined_at": joined_at,
                }),
            )
            .await
            .is_ok();
        }
        false
    }

    /// Record a room snapshot via spawn-and-forget.
    pub fn record_room_snapshot_sync(&self, room_id: String, room_uuid: String, payload: Value) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            tokio::spawn(async move {
                let _ = record_room_snapshot_pg(&pool, &room_id, &room_uuid, payload).await;
            });
        }
    }

    /// Record a room event (e.g. room.join, room.leave, room.snapshot) and return
    /// success status.
    pub async fn record_room_event(
        &self,
        kind: &str,
        room_id: Option<String>,
        user_id: Option<i32>,
        payload: Value,
    ) -> bool {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            return crate::db::append_event_pg(pool, kind, room_id.as_deref(), user_id, payload)
                .await
                .is_ok();
        }
        #[cfg(not(feature = "postgres"))]
        let _ = (kind, room_id, user_id, payload);
        false
    }

    /// Synchronous (spawn-and-forget) variant of record_room_event.
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

    /// Record user room join history (room_history + mp_user_room_history + mp_events).
    pub fn record_user_room_history_sync(
        &self,
        user_id: i32,
        room_id: String,
        room_uuid: String,
        joined_at: i64,
    ) {
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
                let _ = crate::db::append_event_pg(
                    &pool,
                    "room.join",
                    Some(&room_id),
                    Some(user_id),
                    serde_json::json!({
                        "user_id": user_id,
                        "room_id": room_id,
                        "room_uuid": room_uuid,
                        "joined_at": joined_at,
                    }),
                )
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
    let payload = payload.to_string();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
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
    crate::db::append_event_pg(
        pool,
        "room.snapshot",
        Some(room_id),
        None,
        serde_json::json!({
            "room_id": room_id,
            "room_uuid": room_uuid,
        }),
    )
    .await?;
    Ok(())
}

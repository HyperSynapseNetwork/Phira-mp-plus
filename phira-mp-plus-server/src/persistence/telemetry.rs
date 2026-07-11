//! Runtime v2 telemetry persistence — production Touch/Judge batch writes.
//!
//! Extracted from db.rs to keep the batch INSERT logic separate from
//! general-purpose database helpers.  TelemetryCutoverMode is in
//! crate::telemetry; this module owns the SQL writing side.

use crate::db::DbManager;
use serde_json::Value;

/// Number of telemetry points inside a payload's "data" array, or 1 if no array.
pub fn telemetry_point_count(payload: &Value) -> usize {
    payload
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(1)
}

impl DbManager {
    /// Write runtime telemetry batches to PostgreSQL.
    ///
    /// Each record becomes one row in mp_runtime_telemetry_batches, and each
    /// item inside the payload's "data" array becomes one row in
    /// mp_runtime_telemetry_items.
    pub async fn record_runtime_telemetry_batches(
        &self,
        records: Vec<crate::db::RuntimeTelemetryBatchRecord>,
    ) -> bool {
        if records.is_empty() {
            return true;
        }
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let Ok(mut transaction) = pool.begin().await else {
                return false;
            };
            for record in records {
                let items: Vec<Value> = record
                    .payload
                    .get("data")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_else(|| vec![record.payload.clone()]);
                if sqlx::query(
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
                .bind(&record.payload)
                .bind(now)
                .bind(&record.source)
                .bind(record.dual_write)
                .bind(record.schema_version)
                .bind(&record.flush_reason)
                .execute(&mut *transaction)
                .await
                .is_err()
                {
                    return false;
                }

                for (ordinal, raw_item) in items.into_iter().enumerate() {
                    if sqlx::query(
                        "INSERT INTO mp_runtime_telemetry_items
                           (batch_uuid, ordinal, kind, room_id, round_uuid, player_id, payload, created_at, schema_version)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
                    )
                    .bind(&record.batch_uuid)
                    .bind(i32::try_from(ordinal).unwrap_or(i32::MAX))
                    .bind(&record.kind)
                    .bind(record.room_id.as_deref())
                    .bind(record.round_uuid.as_deref())
                    .bind(record.player_id)
                    .bind(raw_item)
                    .bind(now)
                    .bind(record.schema_version)
                    .execute(&mut *transaction)
                    .await
                    .is_err()
                    {
                        return false;
                    }
                }
            }
            return transaction.commit().await.is_ok();
        }
        #[cfg(not(feature = "postgres"))]
        let _ = records;
        false
    }

    /// Synchronous (spawn-and-forget) variant of record_runtime_telemetry_batches.
    pub fn record_runtime_telemetry_batches_sync(
        &self,
        records: Vec<crate::db::RuntimeTelemetryBatchRecord>,
    ) -> bool {
        if records.is_empty() {
            return true;
        }
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let pool = pool.clone();
            tokio::spawn(async move {
                let db = DbManager::Pg(pool);
                let _ = db.record_runtime_telemetry_batches(records).await;
            });
            return true;
        }
        #[cfg(not(feature = "postgres"))]
        let _ = records;
        false
    }
}

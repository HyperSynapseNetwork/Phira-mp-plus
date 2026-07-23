//! Runtime v2 telemetry persistence — production Touch/Judge batch writes.
//!
//! Extracted from db.rs to keep the batch INSERT logic separate from
//! general-purpose database helpers.

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
        let Self::Pg(pool) = self;
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
            let header_insert = sqlx::query(
                "INSERT INTO mp_runtime_telemetry_batches
                       (event_id, batch_uuid, run_id, scope, pipeline, kind, room_id, round_uuid, player_id, item_count,
                        payload, created_at, source, schema_version, flush_reason)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
                     ON CONFLICT (event_id) DO NOTHING"
            )
            .bind(&record.event_id)
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
            .bind(record.schema_version)
            .bind(&record.flush_reason)
            .execute(&mut *transaction)
            .await;
            let header_insert = match header_insert {
                Ok(result) => result,
                Err(_) => return false,
            };
            if header_insert.rows_affected() == 0 {
                // The event was committed by an earlier attempt whose ACK
                // was lost. Do not append canonical data a second time.
                continue;
            }

            // The Runtime v2 worker must also update the canonical round
            // tables used by existing read APIs.
            if record.scope == "production" {
                let Some(round_uuid) = record.round_uuid.as_deref() else {
                    return false;
                };
                let (field, batch_table) = match record.kind.as_str() {
                    "touch" => ("touches", "mp_round_touch_batches"),
                    "judge" => ("judges", "mp_round_judge_batches"),
                    _ => return false,
                };
                let payload_json =
                    serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string());
                let mut first_game_time: Option<f64> = None;
                let mut last_game_time: Option<f64> = None;
                for value in &items {
                    if let Some(time) = value.get("time").and_then(Value::as_f64) {
                        first_game_time =
                            Some(first_game_time.map_or(time, |current| current.min(time)));
                        last_game_time =
                            Some(last_game_time.map_or(time, |current| current.max(time)));
                    }
                }
                let canonical_sql = format!(
                    "INSERT INTO mp_round_player_data
                           (round_uuid, player_id, {field}, created_at, updated_at, sequence)
                         VALUES ($1, $2, $3::jsonb, $4, $4, nextval('mp_persist_sequence'))
                         ON CONFLICT (round_uuid, player_id) DO UPDATE SET
                           {field} = mp_round_player_data.{field} || $3::jsonb,
                           updated_at = $4, sequence = nextval('mp_persist_sequence')"
                );
                if sqlx::query(&canonical_sql)
                    .bind(round_uuid)
                    .bind(record.player_id)
                    .bind(&payload_json)
                    .bind(now)
                    .execute(&mut *transaction)
                    .await
                    .is_err()
                {
                    return false;
                }

                let batch_sql = format!(
                    "INSERT INTO {batch_table}
                           (round_uuid, player_id, count, first_game_time, last_game_time, payload, created_at, sequence)
                         VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7, nextval('mp_persist_sequence'))"
                );
                if sqlx::query(&batch_sql)
                    .bind(round_uuid)
                    .bind(record.player_id)
                    .bind(i32::try_from(items.len()).unwrap_or(i32::MAX))
                    .bind(first_game_time)
                    .bind(last_game_time)
                    .bind(&payload_json)
                    .bind(now)
                    .execute(&mut *transaction)
                    .await
                    .is_err()
                {
                    return false;
                }
            }

            for (ordinal, raw_item) in items.into_iter().enumerate() {
                if sqlx::query(
                    "INSERT INTO mp_runtime_telemetry_items
                           (event_id, batch_uuid, ordinal, kind, room_id, round_uuid, player_id, payload, created_at, schema_version)
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
                )
                .bind(&record.event_id)
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
        transaction.commit().await.is_ok()
    }

    /// Synchronous (spawn-and-forget) variant of record_runtime_telemetry_batches.
    pub fn record_runtime_telemetry_batches_sync(
        &self,
        records: Vec<crate::db::RuntimeTelemetryBatchRecord>,
    ) -> bool {
        if records.is_empty() {
            return true;
        }
        let Self::Pg(pool) = self;
        let pool = pool.clone();
        tokio::spawn(async move {
            let db = DbManager::Pg(pool);
            let _ = db.record_runtime_telemetry_batches(records).await;
        });
        true
    }
}

//! PostgreSQL write helpers for the high-frequency telemetry writer.
//!
//! Provides CSV-formatting utilities and the COPY-based write path that
//! bypasses the WAL for Touch/Judge batch and item tables.
//!
//! # Usage
//!
//! These functions are called by [`super::writer::flush_batch`] and are not
//! intended for direct external use.

use crate::db::{DbManager, RuntimeTelemetryBatchRecord};
use serde_json::Value;

use super::{now_ms, HF_SCHEMA_VERSION, HighFrequencyItem};

// ── CSV helpers ──────────────────────────────────────────────────────────────

/// CSV-quote a non-null string for PostgreSQL COPY CSV.
/// Empty string becomes `""` (non-null empty).  Quoting is applied when the
/// value contains commas, double-quotes, or newlines.
fn csv_quote(s: &str) -> String {
    if s.is_empty() {
        return r#""""#.into();
    }
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!(r#""{}""#, s.replace('"', r#""""#))
    } else {
        s.to_string()
    }
}

/// CSV representation of an optional string: `None` becomes NULL (empty
/// unquoted field in COPY CSV).
fn csv_opt_str(v: &Option<String>) -> String {
    match v {
        Some(s) => csv_quote(s),
        None => String::new(),
    }
}

/// CSV representation of a JSON value: serialised to a JSON string, then
/// CSV-quoted.
fn csv_json(v: &serde_json::Value) -> String {
    let s = serde_json::to_string(v).unwrap_or_else(|_| "null".into());
    format!(r#""{}""#, s.replace('"', r#""""#))
}

// ── COPY write ───────────────────────────────────────────────────────────────

/// Attempt to write telemetry records using PostgreSQL COPY for maximum
/// throughput.  If COPY is unavailable or fails, delegates to the
/// INSERT-based fallback path.
pub(crate) async fn try_copy_write(
    db: &DbManager,
    records: &[RuntimeTelemetryBatchRecord],
) -> Result<(), String> {
    let DbManager::Pg(pool) = db;
    try_copy_write_inner(pool, records).await
}

#[cfg(feature = "postgres")]
async fn try_copy_write_inner(
    pool: &sqlx::PgPool,
    records: &[RuntimeTelemetryBatchRecord],
) -> Result<(), String> {
    use std::fmt::Write as _;
    let mut transaction = pool
        .begin()
        .await
        .map_err(|e| format!("begin transaction: {e}"))?;
    let now = now_ms();

    // ── Build CSV data ──────────────────────────────────────────────────
    let mut batch_csv = String::with_capacity(records.len() * 256);
    let mut items_csv = String::with_capacity(records.len() * 512);

    for record in records {
        // Batch row (omitting auto-generated `sequence` column)
        let _ = writeln!(
            batch_csv,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            csv_quote(&record.event_id),
            csv_quote(&record.batch_uuid),
            csv_opt_str(&record.run_id),
            csv_quote(&record.scope),
            csv_quote(&record.pipeline),
            csv_quote(&record.kind),
            csv_opt_str(&record.room_id),
            csv_opt_str(&record.round_uuid),
            record.player_id,
            record.item_count,
            csv_json(&record.payload),
            now,
            csv_quote(&record.source),
            record.schema_version,
            csv_quote(&record.flush_reason),
        );

        // Item rows (from payload.data array, or payload itself as one item)
        let item_values: Vec<&Value> = record
            .payload
            .get("data")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_else(|| vec![&record.payload]);

        for (ordinal, raw_item) in item_values.iter().enumerate() {
            let _ = writeln!(
                items_csv,
                "{},{},{},{},{},{},{},{},{},{}",
                csv_quote(&record.event_id),
                csv_quote(&record.batch_uuid),
                ordinal,
                csv_quote(&record.kind),
                csv_opt_str(&record.room_id),
                csv_opt_str(&record.round_uuid),
                record.player_id,
                csv_json(raw_item),
                now,
                record.schema_version,
            );
        }
    }

    // ── COPY mp_runtime_telemetry_batches ───────────────────────────────
    {
        let mut copy = transaction
            .copy_in_raw(
                "COPY mp_runtime_telemetry_batches \
                 (event_id, batch_uuid, run_id, scope, pipeline, kind, \
                  room_id, round_uuid, player_id, item_count, payload, \
                  created_at, source, schema_version, flush_reason) \
                 FROM STDIN WITH (FORMAT CSV)",
            )
            .await
            .map_err(|e| format!("copy start batches: {e}"))?;

        copy.send(batch_csv.as_bytes())
            .await
            .map_err(|e| format!("copy send batches: {e}"))?;
        copy.finish()
            .await
            .map_err(|e| format!("copy finish batches: {e}"))?;
    }

    // ── COPY mp_runtime_telemetry_items ─────────────────────────────────
    {
        let mut copy = transaction
            .copy_in_raw(
                "COPY mp_runtime_telemetry_items \
                 (event_id, batch_uuid, ordinal, kind, room_id, round_uuid, \
                  player_id, payload, created_at, schema_version) \
                 FROM STDIN WITH (FORMAT CSV)",
            )
            .await
            .map_err(|e| format!("copy start items: {e}"))?;

        copy.send(items_csv.as_bytes())
            .await
            .map_err(|e| format!("copy send items: {e}"))?;
        copy.finish()
            .await
            .map_err(|e| format!("copy finish items: {e}"))?;
    }

    // ── Canonical table updates ─────────────────────────────────────────
    for record in records {
        if record.scope != "production" {
            continue;
        }
        let Some(round_uuid) = record.round_uuid.as_deref() else {
            continue;
        };
        let (field, batch_table) = match record.kind.as_str() {
            "touch" => ("touches", "mp_round_touch_batches"),
            "judge" => ("judges", "mp_round_judge_batches"),
            _ => continue,
        };

        let items: Vec<Value> = record
            .payload
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| vec![record.payload.clone()]);

        let payload_json =
            serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string());
        let mut first_game_time: Option<f64> = None;
        let mut last_game_time: Option<f64> = None;
        for v in &items {
            if let Some(time) = v.get("time").and_then(Value::as_f64) {
                first_game_time = Some(first_game_time.map_or(time, |cur| cur.min(time)));
                last_game_time = Some(last_game_time.map_or(time, |cur| cur.max(time)));
            }
        }

        let canonical_sql = format!(
            "INSERT INTO mp_round_player_data \
               (round_uuid, player_id, {field}, created_at, updated_at, sequence) \
             VALUES ($1, $2, $3::jsonb, $4, $4, nextval('mp_persist_sequence')) \
             ON CONFLICT (round_uuid, player_id) DO UPDATE SET \
               {field} = mp_round_player_data.{field} || $3::jsonb, \
               updated_at = $4, sequence = nextval('mp_persist_sequence')"
        );
        sqlx::query(&canonical_sql)
            .bind(round_uuid)
            .bind(record.player_id)
            .bind(&payload_json)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(|e| format!("canonical update {round_uuid}: {e}"))?;

        let batch_sql = format!(
            "INSERT INTO {batch_table} \
               (round_uuid, player_id, count, first_game_time, last_game_time, \
                payload, created_at, sequence) \
             VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7, nextval('mp_persist_sequence'))"
        );
        sqlx::query(&batch_sql)
            .bind(round_uuid)
            .bind(record.player_id)
            .bind(i32::try_from(items.len()).unwrap_or(i32::MAX))
            .bind(first_game_time)
            .bind(last_game_time)
            .bind(&payload_json)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(|e| format!("batch insert {round_uuid}: {e}"))?;
    }

    // ── Commit ──────────────────────────────────────────────────────────
    transaction
        .commit()
        .await
        .map_err(|e| format!("commit transaction: {e}"))?;

    Ok(())
}

// ── Utilities ────────────────────────────────────────────────────────────────

/// Generate a deterministic batch idempotency key from the admission sequence
/// range of the items in the batch.  The same range always produces the same
/// key, so retrying the same batch of items is idempotent at the database
/// level (ON CONFLICT DO NOTHING).
pub(crate) fn batch_uuid(min_seq: u64, max_seq: u64, instance_id: &str) -> String {
    // Include the server instance ID so the batch key is unique across boots:
    // the HF admission sequence restarts at 1 each boot, so a bare hf-{min}-{max}
    // key would collide with a pre-restart batch and be deduplicated wrongly (P1).
    format!("hf-{instance_id}-{min_seq}-{max_seq}")
}

/// Convert HF items to the `RuntimeTelemetryBatchRecord` form expected by
/// the existing `record_runtime_telemetry_batches` method.
pub(crate) fn extract_runtime_records(
    batch_id: &str,
    items: &[HighFrequencyItem],
) -> Vec<RuntimeTelemetryBatchRecord> {
    items
        .iter()
        .map(|item| {
            let count = item.item_count();
            RuntimeTelemetryBatchRecord {
                event_id: item.event_id(),
                batch_uuid: batch_id.to_string(),
                run_id: None,
                scope: "production".to_string(),
                pipeline: "runtime.high_frequency.writer".to_string(),
                source: "high_frequency_writer".to_string(),
                flush_reason: "batch".to_string(),
                schema_version: HF_SCHEMA_VERSION,
                kind: item.kind.as_str().to_string(),
                room_id: item.room_id(),
                round_uuid: Some(item.round_id.clone()),
                player_id: item.user_id,
                item_count: i32::try_from(count).unwrap_or(i32::MAX),
                payload: item.payload.clone(),
            }
        })
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::high_frequency::HighFrequencyKind;
    use serde_json::json;

    fn make_item(kind: HighFrequencyKind, user_id: i32) -> HighFrequencyItem {
        let event_id = uuid::Uuid::new_v4().to_string();
        HighFrequencyItem {
            kind,
            round_id: "round-1".to_string(),
            user_id,
            payload: json!({
                "event_id": event_id,
                "room_id": "room-1",
                "round_id": "round-1",
                "user_id": user_id,
                "count": 3,
                "data": [
                    {"time": 1.0, "x": 0.1, "y": 0.2},
                    {"time": 1.5, "x": 0.3, "y": 0.4},
                    {"time": 2.0, "x": 0.5, "y": 0.6},
                ],
            }),
            created_at_ms: now_ms(),
            admission_seq: 0,
        }
    }

    #[test]
    fn extract_runtime_records_contains_expected_fields() {
        let items = vec![make_item(HighFrequencyKind::Touch, 42)];
        let records = extract_runtime_records("test-batch", &items);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].player_id, 42);
        assert_eq!(records[0].kind, "touch");
        assert_eq!(records[0].scope, "production");
        assert_eq!(records[0].pipeline, "runtime.high_frequency.writer");
        assert_eq!(records[0].source, "high_frequency_writer");
        assert_eq!(records[0].round_uuid.as_deref(), Some("round-1"));
        assert_eq!(records[0].item_count, 3);
        assert!(!records[0].event_id.is_empty());
    }
}

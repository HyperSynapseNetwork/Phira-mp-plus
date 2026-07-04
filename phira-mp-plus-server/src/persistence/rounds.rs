//! Round persistence — round lifecycle, touches, judges, results.
//!
//! Extracted from db.rs. Writes to mp_rounds, mp_round_touch_batches,
//! mp_round_judge_batches, mp_round_player_data, mp_round_results.

use crate::db::DbManager;
use serde_json::Value;
#[cfg(feature = "postgres")]
use sqlx::Row;

impl DbManager {
    pub async fn open_round(&self, meta: &crate::round_store::RoundMeta) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let players = serde_json::to_string(&meta.players).unwrap_or_else(|_| "[]".to_string());
            let now = crate::db::now_ms();
            let _ = sqlx::query(
                "INSERT INTO mp_rounds (round_uuid, room_id, chart_id, chart_name, players, started_at, finished_at, created_at, updated_at, sequence)
                 VALUES ($1, $2, $3, $4, $5::jsonb, $6, $7, $8, $8, nextval('mp_persist_sequence'))
                 ON CONFLICT (round_uuid) DO UPDATE SET
                   room_id = EXCLUDED.room_id, chart_id = EXCLUDED.chart_id,
                   chart_name = EXCLUDED.chart_name, players = EXCLUDED.players,
                   started_at = EXCLUDED.started_at, finished_at = EXCLUDED.finished_at,
                   updated_at = EXCLUDED.updated_at, sequence = EXCLUDED.sequence"
            )
            .bind(&meta.round_uuid).bind(&meta.room_id).bind(meta.chart_id)
            .bind(&meta.chart_name).bind(players).bind(meta.started_at)
            .bind(meta.finished_at).bind(now)
            .execute(pool).await;
            let _ = crate::db::append_event_pg(pool, "round.open", Some(&meta.room_id), None,
                serde_json::to_value(meta).unwrap_or_default()).await;
        }
    }

    pub async fn close_round(&self, round_uuid: &str) {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let now = crate::db::now_ms();
            let _ = sqlx::query(
                "UPDATE mp_rounds SET finished_at = COALESCE(finished_at, $2), updated_at = $2, sequence = nextval('mp_persist_sequence')
                 WHERE round_uuid = $1"
            ).bind(round_uuid).bind(now).execute(pool).await;
            let _ = crate::db::append_event_pg(pool, "round.close", None, None,
                serde_json::json!({"round_uuid": round_uuid})).await;
        }
    }

    pub async fn append_touches(&self, round_uuid: &str, player_id: i32, data: &[crate::plugin::TouchEventPoint]) {
        if data.is_empty() { return; }
        let payload_json = serde_json::to_string(data).unwrap_or_else(|_| "[]".to_string());
        self.append_player_array_json(round_uuid, player_id, "touches", &payload_json).await;
        self.append_touch_batch_str(round_uuid, player_id, data, &payload_json).await;
    }

    pub async fn append_judges(&self, round_uuid: &str, player_id: i32, data: &[crate::plugin::JudgeEventItem]) {
        if data.is_empty() { return; }
        let payload_json = serde_json::to_string(data).unwrap_or_else(|_| "[]".to_string());
        self.append_player_array_json(round_uuid, player_id, "judges", &payload_json).await;
        self.append_judge_batch_str(round_uuid, player_id, data, &payload_json).await;
    }

    pub async fn record_round_result(&self, round_uuid: &str, player_id: i32, score: i64, combo: i64) -> bool {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let now = crate::db::now_ms();
            return sqlx::query(
                "INSERT INTO mp_round_results (round_uuid, player_id, score, max_combo, created_at, updated_at, sequence)
                 VALUES ($1, $2, $3, $4, $5, $5, nextval('mp_persist_sequence'))
                 ON CONFLICT (round_uuid, player_id) DO UPDATE SET
                   score = $3, max_combo = $4, updated_at = $5"
            ).bind(round_uuid).bind(player_id).bind(score).bind(combo).bind(now)
            .execute(pool).await.is_ok();
        }
        false
    }

    pub async fn list_rounds(&self, limit: i64) -> Vec<crate::round_store::RoundMeta> {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let limit = limit.clamp(1, 200);
            let rows = sqlx::query(
                "SELECT round_uuid, room_id, chart_id, chart_name, players::text AS players,
                        started_at, finished_at, created_at, updated_at
                 FROM mp_rounds ORDER BY sequence DESC LIMIT $1"
            ).bind(limit).fetch_all(pool).await.unwrap_or_default();
            return rows.iter().filter_map(|row| {
                let raw = row.try_get::<String, _>("players").ok()?;
                let players: Vec<i32> = serde_json::from_str(&raw).ok()?;
                Some(crate::round_store::RoundMeta {
                    round_uuid: row.try_get::<String, _>("round_uuid").ok()?,
                    room_id: row.try_get::<String, _>("room_id").ok()?,
                    chart_id: row.try_get::<i32, _>("chart_id").ok()?,
                    chart_name: row.try_get::<String, _>("chart_name").ok()?,
                    players,
                    started_at: row.try_get::<i64, _>("started_at").unwrap_or(0),
                    finished_at: row.try_get::<i64, _>("finished_at").ok(),
                    created_at: row.try_get::<i64, _>("created_at").unwrap_or(0),
                    updated_at: row.try_get::<i64, _>("updated_at").unwrap_or(0),
                })
            }).collect();
        }
        Vec::new()
    }

    pub async fn read_round_player_data(&self, round_uuid: &str, player_id: i32) -> Option<Value> {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let row = sqlx::query(
                "SELECT touches::text AS touches, judges::text AS judges, score, max_combo
                 FROM mp_round_player_data WHERE round_uuid = $1 AND player_id = $2"
            ).bind(round_uuid).bind(player_id)
            .fetch_optional(pool).await.ok()??;
            return Some(serde_json::json!({
                "round_uuid": round_uuid,
                "player_id": player_id,
                "touches": row.try_get::<String, _>("touches").ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(Value::Array(Vec::new())),
                "judges": row.try_get::<String, _>("judges").ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(Value::Array(Vec::new())),
                "score": row.try_get::<i64, _>("score").unwrap_or(0),
                "max_combo": row.try_get::<i64, _>("max_combo").unwrap_or(0),
            }));
        }
        None
    }
}

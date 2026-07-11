//! Round persistence — round lifecycle, touches, judges, results.
//!
//! Extracted from db.rs. Writes to mp_rounds, mp_round_touch_batches,
//! mp_round_judge_batches, mp_round_player_data, mp_round_results.

use crate::db::DbManager;
#[cfg(feature = "postgres")]
use sqlx::Row;

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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl DbManager {
    pub async fn open_round(&self, meta: &crate::round_store::RoundMeta) -> bool {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let players =
                serde_json::to_value(&meta.players).unwrap_or_else(|_| serde_json::json!([]));
            let payload = serde_json::to_value(meta).unwrap_or_default();
            let now = now_ms();
            let Ok(mut transaction) = pool.begin().await else {
                return false;
            };
            let round_write = sqlx::query(
                "INSERT INTO mp_rounds
                   (round_uuid, room_id, chart_id, chart_name, players, started_at,
                    finished_at, created_at, updated_at, sequence)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8, nextval('mp_persist_sequence'))
                 ON CONFLICT (round_uuid) DO UPDATE SET
                   room_id = EXCLUDED.room_id,
                   chart_id = EXCLUDED.chart_id,
                   chart_name = EXCLUDED.chart_name,
                   players = EXCLUDED.players,
                   started_at = EXCLUDED.started_at,
                   finished_at = EXCLUDED.finished_at,
                   updated_at = EXCLUDED.updated_at,
                   sequence = EXCLUDED.sequence
                 WHERE mp_rounds.room_id IS DISTINCT FROM EXCLUDED.room_id
                    OR mp_rounds.chart_id IS DISTINCT FROM EXCLUDED.chart_id
                    OR mp_rounds.chart_name IS DISTINCT FROM EXCLUDED.chart_name
                    OR mp_rounds.players IS DISTINCT FROM EXCLUDED.players
                    OR mp_rounds.started_at IS DISTINCT FROM EXCLUDED.started_at
                    OR mp_rounds.finished_at IS DISTINCT FROM EXCLUDED.finished_at",
            )
            .bind(&meta.round_uuid)
            .bind(&meta.room_id)
            .bind(meta.chart_id)
            .bind(&meta.chart_name)
            .bind(players)
            .bind(meta.started_at)
            .bind(meta.finished_at)
            .bind(now)
            .execute(&mut *transaction)
            .await;
            let round_write = match round_write {
                Ok(result) => result,
                Err(_) => return false,
            };
            if round_write.rows_affected() == 0 {
                return transaction.commit().await.is_ok();
            }
            if sqlx::query(
                "INSERT INTO mp_events (kind, room_id, user_id, payload, created_at)
                 VALUES ('round.open', $1, NULL, $2, $3)",
            )
            .bind(&meta.room_id)
            .bind(payload)
            .bind(now)
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

    pub async fn close_round(&self, round_uuid: &str) -> bool {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let now = now_ms();
            let Ok(mut transaction) = pool.begin().await else {
                return false;
            };
            let update = sqlx::query(
                "UPDATE mp_rounds
                 SET finished_at = $2,
                     updated_at = $2,
                     sequence = nextval('mp_persist_sequence')
                 WHERE round_uuid = $1 AND finished_at IS NULL
                 RETURNING room_id",
            )
            .bind(round_uuid)
            .bind(now)
            .fetch_optional(&mut *transaction)
            .await;
            let row = match update {
                Ok(row) => row,
                Err(_) => return false,
            };
            let Some(row) = row else {
                return transaction.commit().await.is_ok();
            };
            let room_id = row.try_get::<String, _>("room_id").ok();
            if sqlx::query(
                "INSERT INTO mp_events (kind, room_id, user_id, payload, created_at)
                 VALUES ('round.close', $1, NULL, $2, $3)",
            )
            .bind(room_id.as_deref())
            .bind(serde_json::json!({"round_uuid": round_uuid}))
            .bind(now)
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

    pub async fn append_touches(
        &self,
        round_uuid: &str,
        player_id: i32,
        data: &[crate::plugin::TouchEventPoint],
    ) -> bool {
        if data.is_empty() {
            return true;
        }
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let payload_json = serde_json::to_string(data).unwrap_or_else(|_| "[]".to_string());
            let (first_game_time, last_game_time) =
                telemetry_time_range(data.iter().map(|point| point.time as f64));
            let now = now_ms();
            let Ok(mut transaction) = pool.begin().await else {
                return false;
            };
            if sqlx::query(
                "INSERT INTO mp_round_player_data
                   (round_uuid, player_id, touches, created_at, updated_at, sequence)
                 VALUES ($1, $2, $3::jsonb, $4, $4, nextval('mp_persist_sequence'))
                 ON CONFLICT (round_uuid, player_id) DO UPDATE SET
                   touches = mp_round_player_data.touches || $3::jsonb,
                   updated_at = $4, sequence = nextval('mp_persist_sequence')",
            )
            .bind(round_uuid)
            .bind(player_id)
            .bind(&payload_json)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .is_err()
            {
                return false;
            }
            if sqlx::query(
                "INSERT INTO mp_round_touch_batches
                   (round_uuid, player_id, count, first_game_time, last_game_time, payload, created_at, sequence)
                 VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7, nextval('mp_persist_sequence'))"
            )
            .bind(round_uuid)
            .bind(player_id)
            .bind(i32::try_from(data.len()).unwrap_or(i32::MAX))
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
            return transaction.commit().await.is_ok();
        }
        false
    }

    pub async fn append_judges(
        &self,
        round_uuid: &str,
        player_id: i32,
        data: &[crate::plugin::JudgeEventItem],
    ) -> bool {
        if data.is_empty() {
            return true;
        }
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let payload_json = serde_json::to_string(data).unwrap_or_else(|_| "[]".to_string());
            let (first_game_time, last_game_time) =
                telemetry_time_range(data.iter().map(|item| item.time as f64));
            let now = now_ms();
            let Ok(mut transaction) = pool.begin().await else {
                return false;
            };
            if sqlx::query(
                "INSERT INTO mp_round_player_data
                   (round_uuid, player_id, judges, created_at, updated_at, sequence)
                 VALUES ($1, $2, $3::jsonb, $4, $4, nextval('mp_persist_sequence'))
                 ON CONFLICT (round_uuid, player_id) DO UPDATE SET
                   judges = mp_round_player_data.judges || $3::jsonb,
                   updated_at = $4, sequence = nextval('mp_persist_sequence')",
            )
            .bind(round_uuid)
            .bind(player_id)
            .bind(&payload_json)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .is_err()
            {
                return false;
            }
            if sqlx::query(
                "INSERT INTO mp_round_judge_batches
                   (round_uuid, player_id, count, first_game_time, last_game_time, payload, created_at, sequence)
                 VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7, nextval('mp_persist_sequence'))"
            )
            .bind(round_uuid)
            .bind(player_id)
            .bind(i32::try_from(data.len()).unwrap_or(i32::MAX))
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
            return transaction.commit().await.is_ok();
        }
        false
    }

    pub async fn record_round_result(
        &self,
        round_uuid: &str,
        room_id: &str,
        result: &crate::room::PlayResult,
    ) -> bool {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let now = now_ms();
            let payload = serde_json::to_value(result).unwrap_or_default();
            return sqlx::query(
                "INSERT INTO mp_round_results
                   (round_uuid, user_id, room_id, score, accuracy, payload, created_at, updated_at, sequence)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $7, nextval('mp_persist_sequence'))
                 ON CONFLICT (round_uuid, user_id) DO UPDATE SET
                   room_id = EXCLUDED.room_id, score = EXCLUDED.score,
                   accuracy = EXCLUDED.accuracy, payload = EXCLUDED.payload,
                   updated_at = EXCLUDED.updated_at, sequence = EXCLUDED.sequence"
            )
            .bind(round_uuid)
            .bind(result.user_id)
            .bind(room_id)
            .bind(result.score)
            .bind(f64::from(result.accuracy))
            .bind(payload)
            .bind(now)
            .execute(pool)
            .await
            .is_ok();
        }
        false
    }

    pub async fn list_rounds(&self, limit: i64) -> Vec<crate::round_store::RoundMeta> {
        #[cfg(feature = "postgres")]
        if let Self::Pg(pool) = self {
            let limit = limit.clamp(1, 200);
            let rows = sqlx::query(
                "SELECT round_uuid, room_id, chart_id, chart_name, players::text AS players,
                        started_at, finished_at
                 FROM mp_rounds ORDER BY sequence DESC LIMIT $1",
            )
            .bind(limit)
            .fetch_all(pool)
            .await
            .unwrap_or_default();
            return rows
                .iter()
                .filter_map(|row| {
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
            if let Ok(Some(row)) = sqlx::query(
                "SELECT touches::text AS touches, judges::text AS judges
                 FROM mp_round_player_data WHERE round_uuid = $1 AND player_id = $2",
            )
            .bind(round_uuid)
            .bind(player_id)
            .fetch_optional(pool)
            .await
            {
                if let (Ok(touches), Ok(judges)) = (
                    row.try_get::<String, _>("touches"),
                    row.try_get::<String, _>("judges"),
                ) {
                    return Some(crate::round_store::RoundPlayerData {
                        round_uuid: round_uuid.to_string(),
                        player_id,
                        touches: serde_json::from_str(&touches).unwrap_or_default(),
                        judges: serde_json::from_str(&judges).unwrap_or_default(),
                    });
                }
            }
        }
        None
    }
}

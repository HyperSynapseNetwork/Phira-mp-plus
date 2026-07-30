//! Round persistence — round lifecycle, touches, judges, results.
//!
//! Extracted from db.rs. Writes to mp_rounds, mp_round_touch_batches,
//! mp_round_judge_batches, mp_round_player_data, mp_round_results.

use crate::db::DbManager;
use sqlx::Row;

/// A round that was never finished — found during crash recovery.
#[derive(Debug, Clone)]
pub struct UnfinishedRound {
    pub round_uuid: String,
    pub room_id: String,
    pub chart_id: i32,
    pub chart_name: String,
    pub started_at: i64,
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

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl DbManager {
    pub async fn open_round(&self, meta: &crate::round_store::RoundMeta) -> bool {
        let Self::Pg(pool) = self;
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
        transaction.commit().await.is_ok()
    }

    pub async fn close_round(&self, round_uuid: &str) -> bool {
        let Self::Pg(pool) = self;
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
        transaction.commit().await.is_ok()
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
        let Self::Pg(pool) = self;
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
        transaction.commit().await.is_ok()
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
        let Self::Pg(pool) = self;
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
        transaction.commit().await.is_ok()
    }

    pub async fn record_round_result(
        &self,
        round_uuid: &str,
        room_id: &str,
        result: &crate::room::PlayResult,
    ) -> bool {
        let Self::Pg(pool) = self;
        let now = now_ms();
        let payload = serde_json::to_value(result).unwrap_or_default();
        sqlx::query(
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
        .is_ok()
    }

    /// Atomically commit all round results and close the round in a single
    /// PostgreSQL transaction.
    ///
    /// This replaces the previous pattern of per-result INSERT + separate
    /// close_round call, eliminating the window where `mp_rounds.finished_at`
    /// could be left NULL after partial failures.
    ///
    /// The transaction:
    /// 1. Verifies the round exists (`SELECT ... FOR UPDATE`)
    /// 2. Upserts all results (`ON CONFLICT DO NOTHING`)
    /// 3. Inserts minimal aborted-user records for any not already in results
    /// 4. Closes the round (`UPDATE mp_rounds SET finished_at`)
    /// 5. Records a `round.completed` event
    ///
    /// Returns `true` if the entire transaction committed successfully.
    pub async fn commit_round_completed(
        &self,
        round_uuid: &str,
        room_id: &str,
        event_id: &str,
        results: &[crate::room::PlayResult],
        finished_at: i64,
        aborted_users: &[i32],
    ) -> bool {
        let Self::Pg(pool) = self;
        let now = now_ms();
        let Ok(mut transaction) = pool.begin().await else {
            return false;
        };

        // 1. Verify round exists and lock it (prevents concurrent writes).
        if sqlx::query("SELECT 1 FROM mp_rounds WHERE round_uuid = $1 FOR UPDATE")
            .bind(round_uuid)
            .fetch_optional(&mut *transaction)
            .await
            .ok()
            .flatten()
            .is_none()
        {
            // Round does not exist — create a stub row so the rest of the
            // transaction can succeed.  This handles edge cases where the
            // round metadata was never fully opened (e.g. crash recovery).
            if sqlx::query(
                "INSERT INTO mp_rounds (round_uuid, room_id, chart_id, chart_name, players,
                                        started_at, finished_at, created_at, updated_at, sequence)
                 VALUES ($1, $2, 0, '', '[]'::jsonb, $3, $3, $3, $3,
                         nextval('mp_persist_sequence'))
                 ON CONFLICT (round_uuid) DO UPDATE SET
                   finished_at = EXCLUDED.finished_at,
                   updated_at  = EXCLUDED.updated_at",
            )
            .bind(round_uuid)
            .bind(room_id)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .is_err()
            {
                return false;
            }
        }

        // 2. Upsert all results (ON CONFLICT DO NOTHING ensures first-write-wins).
        for result in results {
            let payload = serde_json::to_value(result).unwrap_or_default();
            if sqlx::query(
                "INSERT INTO mp_round_results
                       (round_uuid, user_id, room_id, score, accuracy,
                        payload, created_at, updated_at, sequence)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $7,
                         nextval('mp_persist_sequence'))
                 ON CONFLICT (round_uuid, user_id) DO NOTHING",
            )
            .bind(round_uuid)
            .bind(result.user_id)
            .bind(room_id)
            .bind(result.score)
            .bind(f64::from(result.accuracy))
            .bind(payload)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .is_err()
            {
                return false;
            }
        }

        // 3. Mark aborted users not already in results (defensive).
        for uid in aborted_users {
            let already = results.iter().any(|r| r.user_id == *uid);
            if !already {
                let payload = serde_json::json!({"user_id": uid, "aborted": true});
                if sqlx::query(
                    "INSERT INTO mp_round_results
                           (round_uuid, user_id, room_id, score, accuracy,
                            payload, created_at, updated_at, sequence)
                     VALUES ($1, $2, $3, 0, 0.0, $4, $5, $5,
                             nextval('mp_persist_sequence'))
                     ON CONFLICT (round_uuid, user_id) DO NOTHING",
                )
                .bind(round_uuid)
                .bind(uid)
                .bind(room_id)
                .bind(payload)
                .bind(now)
                .execute(&mut *transaction)
                .await
                .is_err()
                {
                    return false;
                }
            }
        }

        // 4. Close the round.
        if sqlx::query(
            "UPDATE mp_rounds
                SET finished_at = $2,
                    updated_at  = $2,
                    sequence    = nextval('mp_persist_sequence')
              WHERE round_uuid = $1",
        )
        .bind(round_uuid)
        .bind(finished_at)
        .execute(&mut *transaction)
        .await
        .is_err()
        {
            return false;
        }

        // 5. Record a round.completed event for audit.
        if sqlx::query(
            "INSERT INTO mp_events (event_id, kind, room_id, user_id, payload, created_at)
             VALUES ($1, 'round.completed', $2, NULL, $3, $4)
             ON CONFLICT (event_id) WHERE event_id IS NOT NULL DO NOTHING",
        )
        .bind(event_id)
        .bind(room_id)
        .bind(serde_json::json!({
            "round_uuid": round_uuid,
            "result_count": results.len(),
            "aborted_count": aborted_users.len(),
        }))
        .bind(now)
        .execute(&mut *transaction)
        .await
        .is_err()
        {
            return false;
        }

        transaction.commit().await.is_ok()
    }

    pub async fn list_rounds(&self, limit: i64) -> Vec<crate::round_store::RoundMeta> {
        let Self::Pg(pool) = self;
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
        rows.iter()
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
            .collect()
    }

    pub async fn read_round_player_data(
        &self,
        round_uuid: &str,
        player_id: i32,
    ) -> Option<crate::round_store::RoundPlayerData> {
        let Self::Pg(pool) = self;
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
        None
    }

    /// Find all rounds that were started but never finished (crash recovery).
    ///
    /// Returns rows from `mp_rounds` where `finished_at IS NULL`, ordered by
    /// started_at ascending (oldest first).
    pub async fn find_unfinished_rounds(&self) -> Result<Vec<UnfinishedRound>, sqlx::Error> {
        let Self::Pg(pool) = self;
        let rows = sqlx::query(
            "SELECT round_uuid, room_id, chart_id, chart_name, started_at
                 FROM mp_rounds
                 WHERE finished_at IS NULL
                 ORDER BY started_at ASC",
        )
        .fetch_all(pool)
        .await?;
        Ok(rows.iter()
            .filter_map(|row| {
                Some(UnfinishedRound {
                    round_uuid: row.try_get::<String, _>("round_uuid").ok()?,
                    room_id: row.try_get::<String, _>("room_id").ok()?,
                    chart_id: row.try_get::<i32, _>("chart_id").ok()?,
                    chart_name: row.try_get::<String, _>("chart_name").ok()?,
                    started_at: row.try_get::<i64, _>("started_at").ok()?,
                })
            })
            .collect())
    }

    /// Mark an unfinished round as aborted (crash recovery).
    ///
    /// Sets `finished_at` to the current timestamp and `aborted = true`.
    /// Returns `true` if a row was updated successfully, `false` if the round
    /// was already finished or did not exist.
    pub async fn abort_round(&self, round_uuid: &str) -> bool {
        let Self::Pg(pool) = self;
        let now = now_ms();
        sqlx::query(
            "UPDATE mp_rounds
                 SET finished_at = $2,
                     aborted = TRUE,
                     updated_at = $2,
                     sequence = nextval('mp_persist_sequence')
                 WHERE round_uuid = $1 AND finished_at IS NULL",
        )
        .bind(round_uuid)
        .bind(now)
        .execute(pool)
        .await
        .is_ok_and(|r| r.rows_affected() > 0)
    }

    /// Return the highest schema version recorded in `_pmp_schema_version`.
    pub async fn get_schema_version(&self) -> Option<i32> {
        let Self::Pg(pool) = self;
        let row = sqlx::query(
            "SELECT MAX(version) AS version FROM _pmp_schema_version",
        )
        .fetch_optional(pool)
        .await
        .ok()??;
        row.try_get::<i32, _>("version").ok()
    }

    /// Count total rows in `mp_users`.
    pub async fn count_users(&self) -> i64 {
        let Self::Pg(pool) = self;
        sqlx::query("SELECT COUNT(*) AS cnt FROM mp_users")
            .fetch_one(pool)
            .await
            .ok()
            .and_then(|r| r.try_get::<i64, _>("cnt").ok())
            .unwrap_or(0)
    }

    /// Count total rows in `playtime`.
    pub async fn count_playtime(&self) -> i64 {
        let Self::Pg(pool) = self;
        sqlx::query("SELECT COUNT(*) AS cnt FROM playtime")
            .fetch_one(pool)
            .await
            .ok()
            .and_then(|r| r.try_get::<i64, _>("cnt").ok())
            .unwrap_or(0)
    }
}

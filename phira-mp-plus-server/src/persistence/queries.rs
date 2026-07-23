//! Read-only persistence queries — events, snapshots, touches, judges.
//!
//! Extracted from db.rs to keep read-path SQL separate from write-path logic.

use crate::db::DbManager;
use serde_json::Value;
use sqlx::Row;

impl DbManager {
    pub async fn query_events(
        &self,
        since_sequence: i64,
        limit: i64,
        kind: Option<String>,
        room_id: Option<String>,
        user_id: Option<i32>,
    ) -> Vec<Value> {
        let Self::Pg(pool) = self;
        let limit = limit.clamp(1, 500);
        let mut query_text = String::from(
            "SELECT sequence, kind, room_id, user_id, payload::text AS payload, created_at
                 FROM mp_events WHERE sequence > $1",
        );
        let mut bind_count = 1u8;
        if kind.is_some() {
            bind_count += 1;
            query_text.push_str(&format!(" AND kind = ${bind_count}"));
        }
        if room_id.is_some() {
            bind_count += 1;
            query_text.push_str(&format!(" AND room_id = ${bind_count}"));
        }
        if user_id.is_some() {
            bind_count += 1;
            query_text.push_str(&format!(" AND user_id = ${bind_count}"));
        }
        bind_count += 1;
        query_text.push_str(&format!(" ORDER BY sequence ASC LIMIT ${bind_count}"));

        let mut query = sqlx::query(&query_text).bind(since_sequence);
        if let Some(ref value) = kind {
            query = query.bind(value);
        }
        if let Some(ref value) = room_id {
            query = query.bind(value);
        }
        if let Some(value) = user_id {
            query = query.bind(value);
        }
        query = query.bind(limit);

        let rows = query.fetch_all(pool).await.unwrap_or_default();
        rows.iter()
            .map(|row| {
                serde_json::json!({
                    "sequence": row.try_get::<i64, _>("sequence").unwrap_or_default(),
                    "kind": row.try_get::<String, _>("kind").unwrap_or_default(),
                    "room_id": row.try_get::<Option<String>, _>("room_id").ok().flatten(),
                    "user_id": row.try_get::<Option<i32>, _>("user_id").ok().flatten(),
                    "payload": row.try_get::<String, _>("payload").ok()
                        .and_then(|value| serde_json::from_str(&value).ok())
                        .unwrap_or(Value::Null),
                    "created_at": row.try_get::<i64, _>("created_at").unwrap_or_default(),
                })
            })
            .collect()
    }

    pub async fn query_room_snapshots(&self, since_sequence: i64, limit: i64) -> Vec<Value> {
        let Self::Pg(pool) = self;
        let limit = limit.clamp(1, 500);
        let rows = sqlx::query(
            "SELECT sequence, room_id, room_uuid, payload::text AS payload, created_at, updated_at
                 FROM mp_room_snapshots WHERE sequence > $1 ORDER BY sequence ASC LIMIT $2"
        )
        .bind(since_sequence)
        .bind(limit)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        rows.iter()
            .map(|row| {
                serde_json::json!({
                    "sequence": row.try_get::<i64, _>("sequence").unwrap_or_default(),
                    "room_id": row.try_get::<String, _>("room_id").unwrap_or_default(),
                    "room_uuid": row.try_get::<String, _>("room_uuid").unwrap_or_default(),
                    "payload": row.try_get::<String, _>("payload").ok()
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or(Value::Null),
                    "created_at": row.try_get::<i64, _>("created_at").unwrap_or_default(),
                    "updated_at": row.try_get::<i64, _>("updated_at").unwrap_or_default(),
                })
            })
            .collect()
    }

    pub async fn query_touch_batches(
        &self,
        since_sequence: i64,
        limit: i64,
        round_uuid: Option<String>,
        player_id: Option<i32>,
    ) -> Vec<Value> {
        let Self::Pg(pool) = self;
        let limit = limit.clamp(1, 500);
        let mut q = String::from(
            "SELECT sequence, round_uuid, player_id, payload::text AS payload, count, created_at,
                        first_game_time, last_game_time
                 FROM mp_round_touch_batches WHERE sequence > $1"
        );
        let mut bind_count = 1u8;
        if round_uuid.is_some() {
            bind_count += 1;
            q.push_str(&format!(" AND round_uuid = ${bind_count}"));
        }
        if player_id.is_some() {
            bind_count += 1;
            q.push_str(&format!(" AND player_id = ${bind_count}"));
        }
        q.push_str(" ORDER BY sequence ASC LIMIT $");
        bind_count += 1;
        q.push_str(&bind_count.to_string());
        let mut query = sqlx::query(&q).bind(since_sequence);
        if let Some(ref ru) = round_uuid {
            query = query.bind(ru);
        }
        if let Some(pid) = player_id {
            query = query.bind(pid);
        }
        query = query.bind(limit);
        let rows = query.fetch_all(pool).await.unwrap_or_default();
        rows.iter().map(|row| {
            serde_json::json!({
                "sequence": row.try_get::<i64, _>("sequence").unwrap_or_default(),
                "round_uuid": row.try_get::<String, _>("round_uuid").unwrap_or_default(),
                "player_id": row.try_get::<i32, _>("player_id").unwrap_or_default(),
                "count": row.try_get::<i32, _>("count").unwrap_or_default(),
                "first_game_time": row.try_get::<Option<f64>, _>("first_game_time").ok().flatten(),
                "last_game_time": row.try_get::<Option<f64>, _>("last_game_time").ok().flatten(),
                "payload": row.try_get::<String, _>("payload").ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(Value::Null),
                "created_at": row.try_get::<i64, _>("created_at").unwrap_or_default(),
            })
        }).collect()
    }

    pub async fn query_judge_batches(
        &self,
        since_sequence: i64,
        limit: i64,
        round_uuid: Option<String>,
        player_id: Option<i32>,
    ) -> Vec<Value> {
        let Self::Pg(pool) = self;
        let limit = limit.clamp(1, 500);
        let mut q = String::from(
            "SELECT sequence, round_uuid, player_id, payload::text AS payload, count, created_at,
                        first_game_time, last_game_time
                 FROM mp_round_judge_batches WHERE sequence > $1"
        );
        let mut bind_count = 1u8;
        if round_uuid.is_some() {
            bind_count += 1;
            q.push_str(&format!(" AND round_uuid = ${bind_count}"));
        }
        if player_id.is_some() {
            bind_count += 1;
            q.push_str(&format!(" AND player_id = ${bind_count}"));
        }
        q.push_str(" ORDER BY sequence ASC LIMIT $");
        bind_count += 1;
        q.push_str(&bind_count.to_string());
        let mut query = sqlx::query(&q).bind(since_sequence);
        if let Some(ref ru) = round_uuid {
            query = query.bind(ru);
        }
        if let Some(pid) = player_id {
            query = query.bind(pid);
        }
        query = query.bind(limit);
        let rows = query.fetch_all(pool).await.unwrap_or_default();
        rows.iter().map(|row| {
            serde_json::json!({
                "sequence": row.try_get::<i64, _>("sequence").unwrap_or_default(),
                "round_uuid": row.try_get::<String, _>("round_uuid").unwrap_or_default(),
                "player_id": row.try_get::<i32, _>("player_id").unwrap_or_default(),
                "count": row.try_get::<i32, _>("count").unwrap_or_default(),
                "first_game_time": row.try_get::<Option<f64>, _>("first_game_time").ok().flatten(),
                "last_game_time": row.try_get::<Option<f64>, _>("last_game_time").ok().flatten(),
                "payload": row.try_get::<String, _>("payload").ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(Value::Null),
                "created_at": row.try_get::<i64, _>("created_at").unwrap_or_default(),
            })
        }).collect()
    }
}

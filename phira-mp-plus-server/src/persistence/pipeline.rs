//! Persistence pipeline adapters for simulation, production telemetry and benchmark reports.
//!
//! All DB-facing helpers in this module await the concrete database method when
//! one exists. That makes the latency metrics in `PersistenceStats` represent
//! write acknowledgement instead of only `tokio::spawn` dispatch cost.

use crate::persistence::{PersistenceEvent, PersistencePipeline};
use crate::telemetry_batcher::TelemetryBatcher;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductionTelemetryStage {
    NotTelemetry,
    Staged,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistenceWriteStage {
    NotApplicable,
    SkippedNoDatabase {
        pipeline: PersistencePipeline,
    },
    Acknowledged {
        pipeline: PersistencePipeline,
        elapsed_ms: u64,
    },
    Failed {
        pipeline: PersistencePipeline,
        elapsed_ms: u64,
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BenchmarkReportStage {
    NotBenchmark,
    SkippedNoDatabase,
    Acknowledged { elapsed_ms: u64 },
    Failed { elapsed_ms: u64, error: String },
}

const DB_WRITE_ATTEMPTS: usize = 3;
const DB_RETRY_BACKOFF_MS: [u64; DB_WRITE_ATTEMPTS - 1] = [50, 250];

async fn wait_before_retry(attempt: usize) {
    if let Some(delay_ms) = DB_RETRY_BACKOFF_MS.get(attempt) {
        tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub async fn persist_simulation_event_if_needed(event: &PersistenceEvent) -> PersistenceWriteStage {
    if !event.is_simulation() {
        return PersistenceWriteStage::NotApplicable;
    }
    let Some(db) = crate::internal_hooks::DB.get().filter(|db| db.is_active()) else {
        return PersistenceWriteStage::SkippedNoDatabase {
            pipeline: PersistencePipeline::Simulation,
        };
    };

    let started = Instant::now();
    let event_id = payload_event_id(event).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let mut result = false;
    for attempt in 0..DB_WRITE_ATTEMPTS {
        result = match event {
            PersistenceEvent::ServerEvent { kind, payload, .. } => {
                let payload = with_runtime_event_id((**payload).clone(), &event_id);
                db.record_sim_event(extract_run_id(&payload), kind, payload)
                    .await
            }
            PersistenceEvent::RoomSnapshot {
                room_id, payload, ..
            } => {
                let mut payload_owned = with_runtime_event_id((**payload).clone(), &event_id);
                if let Some(obj) = payload_owned.as_object_mut() {
                    obj.entry("room_id".to_string())
                        .or_insert_with(|| serde_json::json!(room_id));
                }
                db.record_sim_event(
                    extract_run_id(&payload_owned),
                    "simulation.room_snapshot",
                    payload_owned,
                )
                .await
            }
            PersistenceEvent::TouchBatch {
                round_id,
                user_id,
                payload,
                ..
            } => {
                let mut payload_owned = with_runtime_event_id((**payload).clone(), &event_id);
                if let Some(obj) = payload_owned.as_object_mut() {
                    obj.entry("round_id".to_string())
                        .or_insert_with(|| serde_json::json!(round_id));
                    obj.entry("user_id".to_string())
                        .or_insert_with(|| serde_json::json!(user_id));
                }
                db.record_sim_event(
                    extract_run_id(&payload_owned),
                    "simulation.touch_batch",
                    payload_owned,
                )
                .await
            }
            PersistenceEvent::JudgeBatch {
                round_id,
                user_id,
                payload,
                ..
            } => {
                let mut payload_owned = with_runtime_event_id((**payload).clone(), &event_id);
                if let Some(obj) = payload_owned.as_object_mut() {
                    obj.entry("round_id".to_string())
                        .or_insert_with(|| serde_json::json!(round_id));
                    obj.entry("user_id".to_string())
                        .or_insert_with(|| serde_json::json!(user_id));
                }
                db.record_sim_event(
                    extract_run_id(&payload_owned),
                    "simulation.judge_batch",
                    payload_owned,
                )
                .await
            }
            PersistenceEvent::UserOnline { .. }
            | PersistenceEvent::UserOffline { .. }
            | PersistenceEvent::UserDisconnect { .. }
            | PersistenceEvent::UserSeen { .. }
            | PersistenceEvent::UserRoomHistory { .. }
            | PersistenceEvent::BenchmarkReport { .. }
            | PersistenceEvent::Flush
            | PersistenceEvent::Shutdown => {
                return PersistenceWriteStage::NotApplicable;
            }
        };
        if result {
            break;
        }
        wait_before_retry(attempt).await;
    }
    if result {
        PersistenceWriteStage::Acknowledged {
            pipeline: PersistencePipeline::Simulation,
            elapsed_ms: elapsed_ms(started),
        }
    } else {
        PersistenceWriteStage::Failed {
            pipeline: PersistencePipeline::Simulation,
            elapsed_ms: elapsed_ms(started),
            error: "simulation event database write failed".to_string(),
        }
    }
}

/// Stage production telemetry for the Runtime v2 batcher. The payload records
/// whether this item is a migration mirror or the authoritative Worker write.
pub async fn stage_production_telemetry_if_needed(
    wal_id: uuid::Uuid,
    event: &PersistenceEvent,
    batcher: &Arc<TelemetryBatcher>,
) -> ProductionTelemetryStage {
    if event.is_simulation() {
        return ProductionTelemetryStage::NotTelemetry;
    }
    let item = match event {
        PersistenceEvent::TouchBatch {
            round_id,
            user_id,
            payload,
            ..
        } => crate::telemetry::TelemetryItem {
            event_id: payload
                .get("event_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            wal_id: Some(wal_id),
            kind: crate::telemetry::TelemetryKind::Touch,
            room_id: extract_room_id(payload),
            round_id: Some(round_id.clone()),
            user_id: *user_id,
            item_count: crate::persistence::telemetry::telemetry_point_count(payload),
            dual_write: payload
                .get("dual_write")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            persistence_mode: payload
                .get("persistence_mode")
                .and_then(Value::as_str)
                .unwrap_or("worker_authoritative")
                .to_string(),
            payload: (**payload).clone(),
        },
        PersistenceEvent::JudgeBatch {
            round_id,
            user_id,
            payload,
            ..
        } => crate::telemetry::TelemetryItem {
            event_id: payload
                .get("event_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            wal_id: Some(wal_id),
            kind: crate::telemetry::TelemetryKind::Judge,
            room_id: extract_room_id(payload),
            round_id: Some(round_id.clone()),
            user_id: *user_id,
            item_count: crate::persistence::telemetry::telemetry_point_count(payload),
            dual_write: payload
                .get("dual_write")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            persistence_mode: payload
                .get("persistence_mode")
                .and_then(Value::as_str)
                .unwrap_or("worker_authoritative")
                .to_string(),
            payload: (**payload).clone(),
        },
        _ => return ProductionTelemetryStage::NotTelemetry,
    };

    match batcher.enqueue(item).await {
        Ok(()) => ProductionTelemetryStage::Staged,
        Err(item) => ProductionTelemetryStage::Failed(format!(
            "telemetry batcher rejected {} item for user {}",
            item.kind.as_str(),
            item.user_id
        )),
    }
}

pub async fn persist_production_event_if_needed(event: &PersistenceEvent) -> PersistenceWriteStage {
    if event.is_simulation() {
        return PersistenceWriteStage::NotApplicable;
    }
    let Some(db) = crate::internal_hooks::DB.get().filter(|db| db.is_active()) else {
        return PersistenceWriteStage::SkippedNoDatabase {
            pipeline: PersistencePipeline::EventMirror,
        };
    };

    let started = Instant::now();
    let server_event_id = match event {
        PersistenceEvent::ServerEvent { .. } => {
            Some(payload_event_id(event).unwrap_or_else(|| uuid::Uuid::new_v4().to_string()))
        }
        _ => None,
    };
    let mut result = false;
    for attempt in 0..DB_WRITE_ATTEMPTS {
        result = match event {
            PersistenceEvent::ServerEvent { kind, payload, .. } => {
                let payload = with_persistence_meta(
                    (**payload).clone(),
                    server_event_id.as_deref(),
                );
                db.record_room_event(
                    kind,
                    extract_room_id(&payload),
                    extract_user_id(&payload),
                    payload,
                )
                .await
            }
            PersistenceEvent::RoomSnapshot {
                room_id, payload, ..
            } => {
                let mut payload = with_persistence_meta((**payload).clone(), None);
                if let Some(obj) = payload.as_object_mut() {
                    obj.entry("room_id".to_string())
                        .or_insert_with(|| serde_json::json!(room_id));
                }
                let room_uuid = payload
                    .get("room_uuid")
                    .and_then(Value::as_str)
                    .unwrap_or(room_id)
                    .to_owned();
                db.record_room_snapshot(room_id, &room_uuid, payload).await
            }
            PersistenceEvent::TouchBatch { .. } | PersistenceEvent::JudgeBatch { .. } => {
                return PersistenceWriteStage::NotApplicable;
            }
            PersistenceEvent::UserRoomHistory {
                user_id,
                room_id,
                room_uuid,
                joined_at,
            } => {
                db.record_user_room_history(*user_id, room_id, room_uuid, *joined_at)
                    .await
            }
            PersistenceEvent::UserOnline { user_id } => db.set_online(*user_id).await,
            PersistenceEvent::UserOffline { user_id } => db.set_offline(*user_id).await,
            PersistenceEvent::UserDisconnect { user_id, user_name } => {
                db.record_user_disconnect(*user_id, user_name).await
            }
            PersistenceEvent::UserSeen {
                user_id,
                user_name,
                language,
                ip,
            } => {
                db.record_user_seen(*user_id, user_name, language, Some(ip.clone()))
                    .await
            }
            PersistenceEvent::BenchmarkReport { .. }
            | PersistenceEvent::Flush
            | PersistenceEvent::Shutdown => {
                return PersistenceWriteStage::NotApplicable;
            }
        };
        if result {
            break;
        }
        wait_before_retry(attempt).await;
    }
    if result {
        PersistenceWriteStage::Acknowledged {
            pipeline: PersistencePipeline::EventMirror,
            elapsed_ms: elapsed_ms(started),
        }
    } else {
        PersistenceWriteStage::Failed {
            pipeline: PersistencePipeline::EventMirror,
            elapsed_ms: elapsed_ms(started),
            error: "production event database write failed".to_string(),
        }
    }
}

pub async fn persist_benchmark_report_if_needed(event: &PersistenceEvent) -> BenchmarkReportStage {
    let PersistenceEvent::BenchmarkReport { report } = event else {
        return BenchmarkReportStage::NotBenchmark;
    };
    let Some(db) = crate::internal_hooks::DB.get().filter(|db| db.is_active()) else {
        return BenchmarkReportStage::SkippedNoDatabase;
    };
    let started = Instant::now();
    let record = crate::persistence::BenchmarkReportPersistenceRecord::from_report(
        report,
        "benchmark.completed.event_bus",
    );
    let mut persisted = false;
    for attempt in 0..DB_WRITE_ATTEMPTS {
        persisted = db.record_runtime_benchmark_report(record.clone()).await;
        if persisted {
            break;
        }
        wait_before_retry(attempt).await;
    }
    if persisted {
        BenchmarkReportStage::Acknowledged {
            elapsed_ms: elapsed_ms(started),
        }
    } else {
        BenchmarkReportStage::Failed {
            elapsed_ms: elapsed_ms(started),
            error: "benchmark report database write failed".to_string(),
        }
    }
}

fn payload_event_id(event: &PersistenceEvent) -> Option<String> {
    let payload = match event {
        PersistenceEvent::RoomSnapshot { payload, .. }
        | PersistenceEvent::ServerEvent { payload, .. }
        | PersistenceEvent::TouchBatch { payload, .. }
        | PersistenceEvent::JudgeBatch { payload, .. } => payload,
        _ => return None,
    };
    payload
        .get("event_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn with_runtime_event_id(mut payload: Value, event_id: &str) -> Value {
    if let Some(obj) = payload.as_object_mut() {
        obj.entry("event_id".to_string())
            .or_insert_with(|| serde_json::json!(event_id));
    }
    payload
}

fn with_persistence_meta(mut payload: Value, event_id: Option<&str>) -> Value {
    if let Some(obj) = payload.as_object_mut() {
        if let Some(event_id) = event_id {
            obj.entry("event_id".to_string())
                .or_insert_with(|| serde_json::json!(event_id));
        }
        obj.entry("source".to_string())
            .or_insert_with(|| serde_json::json!("persistence_worker"));
        obj.entry("dual_write".to_string())
            .or_insert_with(|| serde_json::json!(false));
        obj.entry("persistence_mode".to_string())
            .or_insert_with(|| serde_json::json!("worker_exclusive"));
    }
    payload
}

pub(crate) fn extract_room_id(payload: &Value) -> Option<String> {
    payload
        .get("room_id")
        .and_then(Value::as_str)
        .filter(|room_id| !room_id.is_empty())
        .map(ToString::to_string)
}

fn extract_user_id(payload: &Value) -> Option<i32> {
    payload
        .get("user_id")
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

fn extract_run_id(payload: &Value) -> Option<String> {
    payload
        .get("run_id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

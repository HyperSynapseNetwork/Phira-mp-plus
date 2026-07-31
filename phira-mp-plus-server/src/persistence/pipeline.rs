//! Persistence pipeline adapters for production telemetry and benchmark reports.
//!
//! All DB-facing helpers in this module await the concrete database method when
//! one exists. That makes the latency metrics in `PersistenceStats` represent
//! write acknowledgement instead of only `tokio::spawn` dispatch cost.

use crate::persistence::{PersistenceEvent, PersistencePipeline};
use serde_json::Value;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistenceWriteStage {
    NotApplicable,
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
    Acknowledged { elapsed_ms: u64 },
    Failed { elapsed_ms: u64, error: String },
}

const DB_WRITE_ATTEMPTS: usize = 5;
const DB_RETRY_BACKOFF_MS: [u64; DB_WRITE_ATTEMPTS - 1] = [100, 200, 500, 2000];

async fn wait_before_retry(attempt: usize) {
    if let Some(delay_ms) = DB_RETRY_BACKOFF_MS.get(attempt) {
        tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub async fn persist_production_event_if_needed(event: &PersistenceEvent) -> PersistenceWriteStage {
    let db = crate::internal_hooks::DB.get().expect("DB must be initialized before persistence worker starts");

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
            PersistenceEvent::UserRoomHistory {
                user_id,
                room_id,
                room_uuid,
                joined_at,
            } => {
                db.record_user_room_history(*user_id, room_id, room_uuid, *joined_at)
                    .await
            }
            PersistenceEvent::UserOffline { user_id, server_instance_id, session_id } => {
                db.set_offline(*user_id, server_instance_id, session_id).await
            }
            PersistenceEvent::UserDisconnect { user_id, user_name, .. } => {
                db.record_user_disconnect(*user_id, user_name).await
            }
            PersistenceEvent::UserAuthenticated {
                event_id,
                session_id,
                user_id,
                user_name,
                language,
                ip,
                connected_at,
                server_instance_id,
            } => {
                db.commit_user_authenticated(
                    event_id,
                    session_id,
                    *user_id,
                    user_name,
                    language,
                    ip,
                    *connected_at,
                    server_instance_id,
                ).await
            }
            PersistenceEvent::RoundResult {
                round_uuid,
                room_id,
                result,
            } => {
                db.record_round_result(round_uuid, room_id, result).await
            }
            PersistenceEvent::RoundCompleted {
                round_uuid,
                room_id,
                event_id,
                results,
                finished_at,
                aborted_users,
            } => {
                // Atomic single-transaction commit: all results + close_round
                // in one PostgreSQL transaction.  No more partial admission.
                db.commit_round_completed(
                    round_uuid,
                    room_id,
                    event_id,
                    results,
                    *finished_at,
                    aborted_users,
                )
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
            pipeline: PersistencePipeline::ProductionEvent,
            elapsed_ms: elapsed_ms(started),
        }
    } else {
        PersistenceWriteStage::Failed {
            pipeline: PersistencePipeline::ProductionEvent,
            elapsed_ms: elapsed_ms(started),
            error: "production event database write failed".to_string(),
        }
    }
}

pub async fn persist_benchmark_report_if_needed(event: &PersistenceEvent) -> BenchmarkReportStage {
    let PersistenceEvent::BenchmarkReport { report } = event else {
        return BenchmarkReportStage::NotBenchmark;
    };
    let db = crate::internal_hooks::DB.get().expect("DB must be initialized before persistence worker starts");
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
        | PersistenceEvent::ServerEvent { payload, .. } => payload,
        _ => return None,
    };
    payload
        .get("event_id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn with_persistence_meta(mut payload: Value, event_id: Option<&str>) -> Value {
    if let Some(obj) = payload.as_object_mut() {
        if let Some(event_id) = event_id {
            obj.entry("event_id".to_string())
                .or_insert_with(|| serde_json::json!(event_id));
        }
        obj.entry("source".to_string())
            .or_insert_with(|| serde_json::json!("persistence_worker"));
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


//! Persistence pipeline adapters for simulation, production telemetry and benchmark reports.
//!
//! All DB-facing helpers in this module await the concrete database method when
//! one exists. That makes the latency metrics in `PersistenceStats` represent
//! write acknowledgement instead of only `tokio::spawn` dispatch cost.

use crate::persistence::{PersistenceEvent, PersistencePipeline};
use crate::telemetry_batcher::{TelemetryBatcher, TelemetryItem, TelemetryKind};
use serde_json::Value;
use std::{sync::Arc, time::Instant};

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

fn elapsed_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub async fn persist_simulation_event_if_needed(event: &PersistenceEvent) -> PersistenceWriteStage {
    if !event.is_simulation() {
        return PersistenceWriteStage::NotApplicable;
    }
    let Some(db) = crate::internal_hooks::DB.get() else {
        return PersistenceWriteStage::SkippedNoDatabase {
            pipeline: PersistencePipeline::Simulation,
        };
    };

    let started = Instant::now();
    let result = match event {
        PersistenceEvent::ServerEvent { kind, payload, .. } => {
            db.record_sim_event(extract_run_id(payload), kind, payload.clone())
                .await
        }
        PersistenceEvent::RoomSnapshot {
            room_id, payload, ..
        } => {
            let mut payload = payload.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.entry("room_id".to_string())
                    .or_insert_with(|| serde_json::json!(room_id));
            }
            db.record_sim_event(
                extract_run_id(&payload),
                "simulation.room_snapshot",
                payload,
            )
            .await
        }
        PersistenceEvent::TouchBatch {
            round_id,
            user_id,
            payload,
            ..
        } => {
            let mut payload = payload.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.entry("round_id".to_string())
                    .or_insert_with(|| serde_json::json!(round_id));
                obj.entry("user_id".to_string())
                    .or_insert_with(|| serde_json::json!(user_id));
            }
            db.record_sim_event(extract_run_id(&payload), "simulation.touch_batch", payload)
                .await
        }
        PersistenceEvent::JudgeBatch {
            round_id,
            user_id,
            payload,
            ..
        } => {
            let mut payload = payload.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.entry("round_id".to_string())
                    .or_insert_with(|| serde_json::json!(round_id));
                obj.entry("user_id".to_string())
                    .or_insert_with(|| serde_json::json!(user_id));
            }
            db.record_sim_event(extract_run_id(&payload), "simulation.judge_batch", payload)
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

pub async fn stage_production_telemetry_if_needed(
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
        } => Some(TelemetryItem {
            kind: TelemetryKind::Touch,
            room_id: extract_room_id(payload),
            round_id: Some(round_id.clone()),
            user_id: *user_id,
            item_count: extract_item_count(payload),
            payload: payload.clone(),
        }),
        PersistenceEvent::JudgeBatch {
            round_id,
            user_id,
            payload,
            ..
        } => Some(TelemetryItem {
            kind: TelemetryKind::Judge,
            room_id: extract_room_id(payload),
            round_id: Some(round_id.clone()),
            user_id: *user_id,
            item_count: extract_item_count(payload),
            payload: payload.clone(),
        }),
        _ => None,
    };

    let Some(item) = item else {
        return ProductionTelemetryStage::NotTelemetry;
    };

    let kind = item.kind.as_str().to_string();
    let user_id = item.user_id;
    match batcher.enqueue(item).await {
        Ok(()) => ProductionTelemetryStage::Staged,
        Err(_) => ProductionTelemetryStage::Failed(format!(
            "telemetry batcher rejected {kind} batch for user_id={user_id}"
        )),
    }
}

pub async fn persist_production_event_if_needed(event: &PersistenceEvent) -> PersistenceWriteStage {
    if event.is_simulation() {
        return PersistenceWriteStage::NotApplicable;
    }
    let Some(db) = crate::internal_hooks::DB.get() else {
        return PersistenceWriteStage::SkippedNoDatabase {
            pipeline: PersistencePipeline::EventMirror,
        };
    };

    let started = Instant::now();
    let result = match event {
        PersistenceEvent::ServerEvent { kind, payload, .. } => {
            let payload = with_runtime_v2_persistence_meta(payload.clone());
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
            let mut payload = with_runtime_v2_persistence_meta(payload.clone());
            if let Some(obj) = payload.as_object_mut() {
                obj.entry("room_id".to_string())
                    .or_insert_with(|| serde_json::json!(room_id));
            }
            db.record_room_event(
                "runtime.room_snapshot",
                Some(room_id.clone()),
                extract_user_id(&payload),
                payload,
            )
            .await
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
            db.record_user_room_history_sync(*user_id, room_id.clone(), room_uuid.clone(), *joined_at);
            return PersistenceWriteStage::Acknowledged {
                pipeline: PersistencePipeline::EventMirror,
                elapsed_ms: started.elapsed().as_millis() as u64,
            };
        }
        PersistenceEvent::UserOnline { user_id } => {
            db.set_online_sync(*user_id);
            return PersistenceWriteStage::Acknowledged {
                pipeline: PersistencePipeline::EventMirror,
                elapsed_ms: started.elapsed().as_millis() as u64,
            };
        }
        PersistenceEvent::UserOffline { user_id } => {
            db.set_offline_sync(*user_id);
            return PersistenceWriteStage::Acknowledged {
                pipeline: PersistencePipeline::EventMirror,
                elapsed_ms: started.elapsed().as_millis() as u64,
            };
        }
        PersistenceEvent::UserDisconnect { user_id, user_name } => {
            db.record_user_disconnect_sync(*user_id, user_name);
            return PersistenceWriteStage::Acknowledged {
                pipeline: PersistencePipeline::EventMirror,
                elapsed_ms: started.elapsed().as_millis() as u64,
            };
        }
        PersistenceEvent::UserSeen { user_id, user_name, language, ip } => {
            db.record_user_seen_sync(*user_id, user_name, language, Some(ip.clone()));
            return PersistenceWriteStage::Acknowledged {
                pipeline: PersistencePipeline::EventMirror,
                elapsed_ms: started.elapsed().as_millis() as u64,
            };
        }
        PersistenceEvent::BenchmarkReport { .. }
        | PersistenceEvent::Flush
        | PersistenceEvent::Shutdown => {
            return PersistenceWriteStage::NotApplicable;
        }
    };
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
    let Some(db) = crate::internal_hooks::DB.get() else {
        return BenchmarkReportStage::SkippedNoDatabase;
    };
    let started = Instant::now();
    let record = crate::persistence::BenchmarkReportPersistenceRecord::from_report(
        report,
        "benchmark.completed.event_bus",
    );
    if db.record_runtime_benchmark_report(record).await {
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

fn with_runtime_v2_persistence_meta(mut payload: Value) -> Value {
    if let Some(obj) = payload.as_object_mut() {
        obj.entry("runtime_v2_source".to_string())
            .or_insert_with(|| serde_json::json!("persistence_worker"));
        obj.entry("runtime_v2_dual_write".to_string())
            .or_insert_with(|| serde_json::json!(true));
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

fn extract_item_count(payload: &Value) -> usize {
    payload
        .get("count")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(1)
        .max(1)
}

fn extract_run_id(payload: &Value) -> Option<String> {
    payload
        .get("run_id")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

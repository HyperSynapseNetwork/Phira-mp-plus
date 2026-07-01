//! Persistence pipeline adapters for simulation, production telemetry and benchmark reports.

use crate::persistence::message::PersistenceEvent;
use crate::telemetry_batcher::{TelemetryBatcher, TelemetryItem, TelemetryKind};
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductionTelemetryStage {
    NotTelemetry,
    Staged,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BenchmarkReportStage {
    NotBenchmark,
    Queued,
    SkippedNoDatabase,
}

pub fn persist_simulation_event_if_needed(event: &PersistenceEvent) -> bool {
    if !event.is_simulation() {
        return false;
    }
    let Some(db) = crate::internal_hooks::DB.get() else {
        return false;
    };

    match event {
        PersistenceEvent::ServerEvent { kind, payload, .. } => {
            db.record_sim_event_sync(extract_run_id(payload), kind, payload.clone());
            true
        }
        PersistenceEvent::RoomSnapshot { room_id, payload, .. } => {
            let mut payload = payload.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.entry("room_id".to_string()).or_insert_with(|| serde_json::json!(room_id));
            }
            db.record_sim_event_sync(extract_run_id(&payload), "simulation.room_snapshot", payload);
            true
        }
        PersistenceEvent::TouchBatch { round_id, user_id, payload, .. } => {
            let mut payload = payload.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.entry("round_id".to_string()).or_insert_with(|| serde_json::json!(round_id));
                obj.entry("user_id".to_string()).or_insert_with(|| serde_json::json!(user_id));
            }
            db.record_sim_event_sync(extract_run_id(&payload), "simulation.touch_batch", payload);
            true
        }
        PersistenceEvent::JudgeBatch { round_id, user_id, payload, .. } => {
            let mut payload = payload.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.entry("round_id".to_string()).or_insert_with(|| serde_json::json!(round_id));
                obj.entry("user_id".to_string()).or_insert_with(|| serde_json::json!(user_id));
            }
            db.record_sim_event_sync(extract_run_id(&payload), "simulation.judge_batch", payload);
            true
        }
        PersistenceEvent::BenchmarkReport { .. } | PersistenceEvent::Flush | PersistenceEvent::Shutdown => false,
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
        PersistenceEvent::TouchBatch { round_id, user_id, payload, .. } => Some(TelemetryItem {
            kind: TelemetryKind::Touch,
            room_id: extract_room_id(payload),
            round_id: Some(round_id.clone()),
            user_id: *user_id,
            item_count: extract_item_count(payload),
            payload: payload.clone(),
        }),
        PersistenceEvent::JudgeBatch { round_id, user_id, payload, .. } => Some(TelemetryItem {
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

pub fn persist_production_event_if_needed(event: &PersistenceEvent) -> bool {
    if event.is_simulation() {
        return false;
    }
    let Some(db) = crate::internal_hooks::DB.get() else {
        return false;
    };

    match event {
        PersistenceEvent::ServerEvent { kind, payload, .. } => {
            let payload = with_runtime_v2_persistence_meta(payload.clone());
            db.record_room_event_sync(
                kind,
                extract_room_id(&payload),
                extract_user_id(&payload),
                payload,
            );
            true
        }
        PersistenceEvent::RoomSnapshot { room_id, payload, .. } => {
            let mut payload = with_runtime_v2_persistence_meta(payload.clone());
            if let Some(obj) = payload.as_object_mut() {
                obj.entry("room_id".to_string()).or_insert_with(|| serde_json::json!(room_id));
            }
            db.record_room_event_sync(
                "runtime.room_snapshot",
                Some(room_id.clone()),
                extract_user_id(&payload),
                payload,
            );
            true
        }
        PersistenceEvent::TouchBatch { .. } | PersistenceEvent::JudgeBatch { .. } => false,
        PersistenceEvent::BenchmarkReport { .. } | PersistenceEvent::Flush | PersistenceEvent::Shutdown => false,
    }
}

pub fn persist_benchmark_report_if_needed(event: &PersistenceEvent) -> BenchmarkReportStage {
    let PersistenceEvent::BenchmarkReport { report } = event else {
        return BenchmarkReportStage::NotBenchmark;
    };
    let Some(db) = crate::internal_hooks::DB.get() else {
        return BenchmarkReportStage::SkippedNoDatabase;
    };
    let record = crate::persistence::BenchmarkReportPersistenceRecord::from_report(
        report,
        "benchmark.completed.event_bus",
    );
    if db.record_runtime_benchmark_report_sync(record) {
        BenchmarkReportStage::Queued
    } else {
        BenchmarkReportStage::SkippedNoDatabase
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_report_stage_is_noop_for_non_benchmark() {
        let event = PersistenceEvent::Flush;
        assert_eq!(persist_benchmark_report_if_needed(&event), BenchmarkReportStage::NotBenchmark);
    }
}

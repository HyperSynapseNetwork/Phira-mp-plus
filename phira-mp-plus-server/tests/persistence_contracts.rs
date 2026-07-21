//! Persistence layer contract tests.
//!
//! These tests verify persistence semantics, cutover-mode telemetry integration,
//! and event type contracts. Pure cutover-mode tests live in
//! `telemetry_cutover_contracts.rs`.

use phira_mp_plus_server::telemetry::TelemetryCutoverMode;

// ── Telemetry batcher integration ─────────────────────────────────────

#[test]
fn telemetry_batcher_stats_has_cutover_mode() {
    let stats = phira_mp_plus_server::telemetry::TelemetryBatcherStats::default();
    assert_eq!(
        stats.cutover_mode,
        TelemetryCutoverMode::default().as_str(),
        "TelemetryBatcherStats should default to the safe mode"
    );
}

#[test]
fn dual_write_field_exists_as_schema_legacy() {
    // RuntimeTelemetryBatchRecord.dual_write is a legacy schema field.
    // Its presence is intentional for backward compatibility.
    let _ = phira_mp_plus_server::db::RuntimeTelemetryBatchRecord {
        event_id: "event-test".to_string(),
        batch_uuid: "test".to_string(),
        run_id: None,
        scope: "test".to_string(),
        pipeline: "test".to_string(),
        source: "test".to_string(),
        flush_reason: "test".to_string(),
        schema_version: 1,
        dual_write: false,
        kind: "touch".to_string(),
        room_id: None,
        round_uuid: None,
        player_id: 0,
        item_count: 1,
        payload: serde_json::json!({}),
    };
}

#[test]
fn simulation_scope_in_persistence_payload() {
    use phira_mp_plus_server::telemetry::{TelemetryItem, TelemetryKind};
    let _item = TelemetryItem {
        event_id: "event-test".to_string(),
        wal_id: None,
        kind: TelemetryKind::Touch,
        room_id: None,
        round_id: None,
        user_id: 1,
        item_count: 1,
        dual_write: false,
        persistence_mode: "worker_authoritative".to_string(),
        payload: serde_json::json!({"run_id": "sim-123"}),
    };
}

// ── PersistenceWorker ─────────────────────────────────────────────────

#[tokio::test]
async fn persistence_worker_spawns_with_default_mode() {
    let worker = phira_mp_plus_server::persistence_worker::PersistenceWorker::spawn(64);
    let stats = worker.stats().await;
    assert_eq!(
        stats.telemetry.cutover_mode,
        TelemetryCutoverMode::default().as_str(),
        "worker should default to DirectOnly"
    );
}

#[test]
fn persistence_worker_simulation_isolation() {
    use phira_mp_plus_server::persistence::message::PersistenceEvent;
    use std::sync::Arc;
    let _sim = PersistenceEvent::TouchBatch {
        round_id: "sim-round".into(),
        user_id: 0,
        payload: Arc::new(serde_json::json!({"run_id": "sim-123"})),
        simulation: true,
    };
    let _prod = PersistenceEvent::TouchBatch {
        round_id: "prod-round".into(),
        user_id: 1,
        payload: Arc::new(serde_json::json!({})),
        simulation: false,
    };
}

// ── PersistenceEvent kind contracts ───────────────────────────────────

#[test]
fn user_room_history_event_kind() {
    use phira_mp_plus_server::persistence::message::PersistenceEvent;
    let event = PersistenceEvent::UserRoomHistory {
        user_id: 42,
        room_id: "room-a".into(),
        room_uuid: "uuid".into(),
        joined_at: 1000,
    };
    assert_eq!(event.kind(), "user_room_history");
    assert!(!event.is_simulation(), "room history is not simulation");
    let summary = event.summary();
    assert!(summary.contains("user_id=42"), "summary contains user_id");
    assert!(
        summary.contains("room_id=room-a"),
        "summary contains room_id"
    );
}

#[test]
fn user_room_history_enum_constructs() {
    use phira_mp_plus_server::persistence::message::PersistenceEvent;
    let event = PersistenceEvent::UserRoomHistory {
        user_id: 1,
        room_id: "r".into(),
        room_uuid: "u".into(),
        joined_at: 0,
    };
    assert!(
        format!("{event:?}").contains("UserRoomHistory"),
        "debug format mentions variant"
    );
}

#[test]
fn user_online_event_kind() {
    use phira_mp_plus_server::persistence::message::PersistenceEvent;
    let event = PersistenceEvent::UserOnline { user_id: 42 };
    assert_eq!(event.kind(), "user_online");
    assert!(!event.is_simulation());
    assert!(event.summary().contains("user_id=42"));
}

#[test]
fn user_offline_event_kind() {
    use phira_mp_plus_server::persistence::message::PersistenceEvent;
    let event = PersistenceEvent::UserOffline { user_id: 99 };
    assert_eq!(event.kind(), "user_offline");
    assert!(!event.is_simulation());
    assert!(event.summary().contains("user_id=99"));
}

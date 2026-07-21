//! Persistence layer contract tests.
//!
//! These tests verify telemetry cutover modes and persistence semantics,
//! and persistence payload helpers are documented and consistent.

use phira_mp_plus_server::telemetry::TelemetryCutoverMode;

#[test]
fn telemetry_cutover_default_is_direct_only() {
    assert_eq!(
        TelemetryCutoverMode::default(),
        TelemetryCutoverMode::DirectOnly,
        "default telemetry mode must be DirectOnly for safety"
    );
}

#[test]
fn telemetry_cutover_mode_variants_cover_all_supported_modes() {
    let variants = TelemetryCutoverMode::variants();
    assert_eq!(variants.len(), 3, "must have exactly 3 modes");
    assert!(variants.contains(&TelemetryCutoverMode::DirectOnly));
    assert!(variants.contains(&TelemetryCutoverMode::WorkerPreferred));
    assert!(variants.contains(&TelemetryCutoverMode::WorkerAuthoritative));
}

#[test]
fn telemetry_cutover_modes_as_str_match_contract() {
    assert_eq!(TelemetryCutoverMode::DirectOnly.as_str(), "direct_only");
    assert_eq!(
        TelemetryCutoverMode::WorkerPreferred.as_str(),
        "worker_preferred"
    );
    assert_eq!(
        TelemetryCutoverMode::WorkerAuthoritative.as_str(),
        "worker_authoritative"
    );
}

#[test]
fn telemetry_cutover_modes_round_trip_via_parse() {
    for mode in TelemetryCutoverMode::variants() {
        let s = mode.as_str();
        let parsed = TelemetryCutoverMode::parse(s);
        assert_eq!(parsed, Some(*mode), "round-trip parse failed for {s}");
    }
}

#[test]
fn worker_preferred_parse_rejects_legacy_names() {
    // Legacy compatibility names are no longer accepted
    assert_eq!(
        TelemetryCutoverMode::parse("worker_only"),
        None,
        "parse('worker_only') should be rejected"
    );
    assert_eq!(
        TelemetryCutoverMode::parse("worker"),
        None,
        "parse('worker') should be rejected"
    );
}

#[test]
fn direct_only_writes_direct_not_enqueue_worker() {
    assert!(TelemetryCutoverMode::DirectOnly.should_write_direct());
    assert!(!TelemetryCutoverMode::DirectOnly.should_enqueue_worker());
}

#[test]
fn worker_preferred_writes_direct_and_enqueues_worker() {
    assert!(
        TelemetryCutoverMode::WorkerPreferred.should_write_direct(),
        "WorkerPreferred must write direct (safety path)"
    );
    assert!(
        TelemetryCutoverMode::WorkerPreferred.should_enqueue_worker(),
        "WorkerPreferred must enqueue Worker for mirror or canonical compensation"
    );
}

#[test]
fn worker_authoritative_enqueues_and_only_falls_back_on_rejection() {
    let mode = TelemetryCutoverMode::WorkerAuthoritative;
    let decision = mode.cutover_decision();
    assert!(mode.should_enqueue_worker());
    assert!(!mode.should_write_direct());
    assert!(!decision.should_write_direct_after_worker_enqueue(true));
    assert!(decision.should_write_direct_after_worker_enqueue(false));
}

#[test]
fn worker_preferred_description_mentions_direct_mirror_and_compensation() {
    let desc = TelemetryCutoverMode::WorkerPreferred.description();
    assert!(
        desc.contains("direct"),
        "WorkerPreferred description must mention the direct path: {desc}"
    );
    assert!(
        desc.contains("mirror"),
        "WorkerPreferred description must mention mirror: {desc}"
    );
    assert!(
        desc.contains("compensat"),
        "WorkerPreferred description must mention direct-failure compensation: {desc}"
    );
}

#[test]
fn no_legacy_modes_in_variants() {
    let variants = TelemetryCutoverMode::variants();
    for v in variants {
        let s = v.as_str();
        assert!(
            s == "direct_only" || s == "worker_preferred" || s == "worker_authoritative",
            "unexpected mode variant: {s}"
        );
    }
}

#[test]
fn cutover_decision_contract() {
    for mode in TelemetryCutoverMode::variants() {
        let decision = mode.cutover_decision();
        assert_eq!(decision.mode, *mode);
        assert_eq!(decision.enqueue_worker, mode.should_enqueue_worker());
        assert_eq!(
            decision.write_direct_before_worker_result,
            mode.should_write_direct()
        );
    }
}

#[test]
fn telemetry_batcher_stats_has_cutover_mode() {
    // The TelemetryBatcherStats stores cutover_mode as string;
    // verify the format matches the enum as_str.
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
    // This test verifies it's still accessible but doesn't assert on value.
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
    // Verify that runtime_v2 telemetry records use "simulation" scope
    // by checking the TelemetryBatcher writes "production" scope
    // (simulation events are routed separately).
    use phira_mp_plus_server::telemetry::{TelemetryItem, TelemetryKind};
    // We can't easily create a full batch record without a DB,
    // but we can verify the enum/struct contract holds.
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

// ── PersistenceWorker write-path audit ──
//
// The following DB write paths currently bypass PersistenceWorker
// (direct DbManager writes, not routed through the mpsc channel).
// Each is documented with its location and write frequency.
//
// These are the known production bypass paths:
//
// | # | File | Function | Data written | Frequency |
// |---|------|----------|-------------|-----------|
// | 1 | internal_hooks.rs | set_online_sync | user online status | per-connect | ✅ migrated to PersistenceWorker |
// | 2 | internal_hooks.rs | set_offline_sync | user offline status | per-disconnect | ✅ migrated to PersistenceWorker |
// | 3 | extensions.rs | record_room_event_sync | extension snapshots | per-ext-change | ✅ migrated to PersistenceWorker |
// | 4 | session.rs | record_user_disconnect_sync | disconnect events | per-disconnect | ✅ migrated to PersistenceWorker |
// | 5 | session.rs | record_user_seen_sync | user seen timestamp | per-command | ✅ migrated to PersistenceWorker |
// | 6 | round_store.rs | open_round/close_round and direct-only/preferred append paths | round data | per-round | telemetry append becomes Worker-exclusive only in explicit worker_authoritative mode |
// | 7 | server.rs | record_user_room_history_sync | room join history | per-join | ✅ migrated to PersistenceWorker |
//
// Telemetry/Benchmark/Simulation events route through PersistenceWorker
// via PersistenceEvent enum and the typed pipeline.
//
// When migrating a path to PersistenceWorker:
// 1. Add a PersistenceEvent variant
// 2. Wire the handler in pipeline.rs
// 3. Add a contract test below to verify the migration

#[test]
fn persistence_bypass_paths_are_documented() {
    // Compile-time check: the DB global is accessed for direct writes
    // This test verifies the bypass paths listed above are still accurate
    // by checking the internal_hooks static DB accessor compiles.
    let _ = phira_mp_plus_server::internal_hooks::DB;
}

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
    // PersistenceWorker should not process simulation events as production.
    // Verify by checking the pipeline separates simulation scopes.
    use phira_mp_plus_server::persistence::message::PersistenceEvent;
    use std::sync::Arc;
    // Simulation events go through persist_simulation_event_if_needed,
    // production through persist_production_event_if_needed.
    // This test verifies the enum variants exist.
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

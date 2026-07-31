//! Persistence layer contract tests.
//!
//! These tests verify persistence semantics and event type contracts.

#[test]
fn runtime_telemetry_batch_record_constructs() {
    // Verify that RuntimeTelemetryBatchRecord constructs without dual_write.
    let _ = phira_mp_plus_server::db::RuntimeTelemetryBatchRecord {
        event_id: "event-test".to_string(),
        batch_uuid: "test".to_string(),
        run_id: None,
        scope: "test".to_string(),
        pipeline: "test".to_string(),
        source: "test".to_string(),
        flush_reason: "test".to_string(),
        schema_version: 1,
        kind: "touch".to_string(),
        room_id: None,
        round_uuid: None,
        player_id: 0,
        item_count: 1,
        payload: serde_json::json!({}),
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
fn user_offline_event_kind() {
    use phira_mp_plus_server::persistence::message::PersistenceEvent;
    let event = PersistenceEvent::UserOffline {
        user_id: 99,
        server_instance_id: String::new(),
        session_id: String::new(),
        occurred_at: 0,
    };
    assert_eq!(event.kind(), "user_offline");
    assert!(event.summary().contains("user_id=99"));
}

#[test]
fn user_disconnect_event_kind() {
    use phira_mp_plus_server::persistence::message::PersistenceEvent;
    let event = PersistenceEvent::UserDisconnect {
        user_id: 98,
        user_name: "alice".to_string(),
        server_instance_id: "inst-1".to_string(),
        session_id: "sess-1".to_string(),
        occurred_at: 1_000_000,
    };
    assert_eq!(event.kind(), "user_disconnect");
    assert!(event.summary().contains("user_id=98"));
    // The payload must carry the session generation identity used by the
    // disconnect persistence SQL to gate on the current session (PMP41 P1).
    let payload = event.dead_letter_payload().expect("disconnect has payload");
    assert_eq!(payload.get("session_id").and_then(|v| v.as_str()), Some("sess-1"));
    assert_eq!(payload.get("server_instance_id").and_then(|v| v.as_str()), Some("inst-1"));
    assert_eq!(payload.get("occurred_at").and_then(|v| v.as_i64()), Some(1_000_000));
}

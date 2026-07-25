//! SSE stream integration tests.
//!
//! Tests SseHub publish/subscribe and event streaming.
//! Full axum HTTP SSE test requires PlusServerState which is
//! too heavyweight for integration tests — SseHub is tested
//! directly here and the HTTP layer is exercised by
//! the existing general_sse_handler in production.

use phira_mp_plus_server::plugin_http::SseStreamConfig;
use phira_mp_plus_server::plugin_http::{SseEvent, SseHub};
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::test]
async fn sse_publish_subscribe_works() {
    let hub = Arc::new(SseHub::new());
    let mut rx = hub.subscribe_general();

    hub.publish(SseEvent::new("test_event", "{\"key\": \"value\"}"));

    let received = rx.recv().await.expect("should receive event");
    assert_eq!(received.event_type, "test_event");
    assert_eq!(received.data, "{\"key\": \"value\"}");
}

#[tokio::test]
async fn sse_multiple_events_in_order() {
    let hub = Arc::new(SseHub::new());
    let mut rx = hub.subscribe_general();

    hub.publish(SseEvent::new("e1", "1".to_string()));
    hub.publish(SseEvent::new("e2", "2".to_string()));
    hub.publish(SseEvent::new("e3", "3".to_string()));

    for i in 1..=3 {
        let received = rx.recv().await.expect("should receive event");
        assert_eq!(received.event_type, format!("e{i}"));
        assert_eq!(received.data, format!("{i}"));
    }
}

#[tokio::test]
async fn sse_room_events_format() {
    use phira_mp_common::{RoomData, RoomEvent, RoomId, StrippedRoomState};
    let hub = Arc::new(SseHub::new());
    let mut rx = hub.subscribe_general();

    hub.publish_room_event(RoomEvent::CreateRoom {
        room: RoomId::try_from("test-room".to_string()).unwrap(),
        data: RoomData {
            host: 42,
            users: vec![1, 2, 3],
            lock: false,
            cycle: false,
            chart: Some(100),
            state: StrippedRoomState::SelectingChart,
            rounds: Vec::new(),
        },
    });

    let received = rx.recv().await.expect("should receive room event");
    assert_eq!(received.event_type, "create_room");
    assert!(
        received.data.contains("test-room"),
        "data should contain room name"
    );
}

#[tokio::test]
async fn sse_stream_registration_integration() {
    // Test that we can create and store stream configs
    let config = SseStreamConfig {
        plugin: "test-plugin".to_string(),
        event_types: vec!["RoomCreate".to_string(), "RoomJoin".to_string()],
    };
    let streams = Arc::new(RwLock::new(std::collections::HashMap::new()));
    streams
        .write()
        .await
        .insert("/test/stream".to_string(), config);

    let entry = streams.read().await.get("/test/stream").cloned().unwrap();
    assert_eq!(entry.plugin, "test-plugin");
    assert!(entry.event_types.contains(&"RoomCreate".to_string()));
}

use axum::response::sse::Event;
use futures::{stream, Stream, StreamExt};
use phira_mp_common::RoomEvent;
use serde::Serialize;
use serde_json::json;
use std::convert::Infallible;
use std::pin::Pin;
use tokio::sync::broadcast;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;

const CHANNEL_CAPACITY: usize = 1024;

pub type EventStream = Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;

#[derive(Debug, Clone, Serialize)]
pub struct SseEvent {
    pub event_type: String,
    pub data: String,
}

impl SseEvent {
    pub fn new(event_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            event_type: event_type.into(),
            data: data.into(),
        }
    }

    fn into_axum(self) -> Event {
        Event::default().event(self.event_type).data(self.data)
    }
}

#[derive(Debug)]
pub struct SseHub {
    general: broadcast::Sender<SseEvent>,
}

impl Default for SseHub {
    fn default() -> Self {
        Self::new()
    }
}

impl SseHub {
    pub fn new() -> Self {
        let (general, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self { general }
    }

    pub fn general_sender(&self) -> broadcast::Sender<SseEvent> {
        self.general.clone()
    }

    pub fn subscribe_general(&self) -> broadcast::Receiver<SseEvent> {
        self.general.subscribe()
    }

    pub fn publish(&self, event: SseEvent) {
        let _ = self.general.send(event);
    }

    pub fn publish_room_event(&self, event: RoomEvent) {
        let event = SseEvent::new(event.event_type(), event.inner().to_string());
        let _ = self.general.send(event);
    }
}

pub fn general_stream(rx: broadcast::Receiver<SseEvent>) -> EventStream {
    let ready = SseEvent::new(
        "ready",
        json!({"stream": "events", "version": env!("CARGO_PKG_VERSION")}).to_string(),
    );
    Box::pin(stream::once(async move { Ok(ready.into_axum()) }).chain(updates(rx)))
}

fn updates(rx: broadcast::Receiver<SseEvent>) -> impl Stream<Item = Result<Event, Infallible>> {
    BroadcastStream::new(rx).filter_map(|message| async move {
        match message {
            Ok(event) => Some(Ok(event.into_axum())),
            Err(BroadcastStreamRecvError::Lagged(skipped)) => Some(Ok(Event::default()
                .event("stream_lagged")
                .data(json!({"skipped": skipped}).to_string()))),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use phira_mp_common::{RoomData, RoomId, StrippedRoomState};

    #[tokio::test]
    async fn room_events_reach_general_stream() {
        let hub = SseHub::new();
        let mut general = hub.subscribe_general();

        hub.publish_room_event(RoomEvent::CreateRoom {
            room: RoomId::try_from("test".to_string()).unwrap(),
            data: RoomData {
                host: 0,
                users: Vec::new(),
                lock: false,
                cycle: false,
                chart: None,
                state: StrippedRoomState::SelectingChart,
                rounds: Vec::new(),
            },
        });

        assert_eq!(general.recv().await.unwrap().event_type, "create_room");
    }
}

use crate::server::PlusServerState;
use axum::response::sse::Event;
use futures::{stream, Stream, StreamExt};
use phira_mp_common::RoomEvent;
use serde_json::json;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;

const CHANNEL_CAPACITY: usize = 1024;

pub type EventStream = Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;

#[derive(Debug, Clone)]
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
    rooms: broadcast::Sender<SseEvent>,
}

impl Default for SseHub {
    fn default() -> Self {
        Self::new()
    }
}

impl SseHub {
    pub fn new() -> Self {
        let (general, _) = broadcast::channel(CHANNEL_CAPACITY);
        let (rooms, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self { general, rooms }
    }

    pub fn general_sender(&self) -> broadcast::Sender<SseEvent> {
        self.general.clone()
    }

    pub fn room_sender(&self) -> broadcast::Sender<SseEvent> {
        self.rooms.clone()
    }

    pub fn subscribe_general(&self) -> broadcast::Receiver<SseEvent> {
        self.general.subscribe()
    }

    pub fn subscribe_rooms(&self) -> broadcast::Receiver<SseEvent> {
        self.rooms.subscribe()
    }

    pub fn publish(&self, event: SseEvent) {
        let _ = self.general.send(event);
    }

    pub fn publish_room_event(&self, event: RoomEvent) {
        let event = SseEvent::new(event.event_type(), event.inner().to_string());
        let _ = self.rooms.send(event.clone());
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

pub async fn room_stream(
    server: Arc<PlusServerState>,
    rx: broadcast::Receiver<SseEvent>,
) -> EventStream {
    let rooms = server
        .rooms
        .read()
        .await
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let room_count = rooms.len();

    let ready = stream::once(async move {
        Ok::<_, Infallible>(SseEvent::new(
            "ready",
            json!({"stream": "rooms", "rooms": room_count}).to_string(),
        )
        .into_axum())
    });
    let snapshots = stream::iter(rooms).then(|room| async move {
        let data = crate::room::Room::into_data(&room).await;
        let payload = json!({"room": room.id.to_string(), "data": data}).to_string();
        Ok::<_, Infallible>(SseEvent::new("update_room", payload).into_axum())
    });

    Box::pin(ready.chain(snapshots).chain(updates(rx)))
}

fn updates(rx: broadcast::Receiver<SseEvent>) -> impl Stream<Item = Result<Event, Infallible>> {
    BroadcastStream::new(rx).filter_map(|message| async move {
        match message {
            Ok(event) => Some(Ok(event.into_axum())),
            Err(BroadcastStreamRecvError::Lagged(skipped)) => Some(Ok(
                Event::default()
                    .event("stream_lagged")
                    .data(json!({"skipped": skipped}).to_string()),
            )),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use phira_mp_common::RoomId;

    #[tokio::test]
    async fn room_events_reach_both_streams() {
        let hub = SseHub::new();
        let mut rooms = hub.subscribe_rooms();
        let mut general = hub.subscribe_general();
        let room = RoomId::try_from("alpha".to_string()).unwrap();

        hub.publish_room_event(RoomEvent::JoinRoom { room, user: 42 });

        assert_eq!(rooms.recv().await.unwrap().event_type, "join_room");
        assert_eq!(general.recv().await.unwrap().event_type, "join_room");
    }
}

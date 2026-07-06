//! WebSocket live event stream.
//!
//! Provides a WebSocket endpoint at `/api/ws` that streams the same live events
//! as the SSE endpoint (`/api/events`), but over a binary WebSocket connection.
//! Messages are serialized SseEvent JSON bytes.

use super::HttpAppState;
use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    extract::State,
    response::IntoResponse,
};
use std::sync::Arc;
use tokio::sync::broadcast;

pub async fn handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<HttpAppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| run(socket, state))
}

async fn run(mut socket: WebSocket, state: Arc<HttpAppState>) {
    let mut events = state.events.general_sender().subscribe();
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(data) => {
                    let payload = serde_json::to_vec(&data).unwrap_or_default();
                    if socket.send(Message::Binary(payload.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("WebSocket client lagged behind by {n} events");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            message = socket.recv() => match message {
                Some(Ok(Message::Ping(data))) => {
                    if socket.send(Message::Pong(data)).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
        }
    }
}

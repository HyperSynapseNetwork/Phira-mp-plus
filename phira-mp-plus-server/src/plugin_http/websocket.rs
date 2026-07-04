//! WebSocket live event stream (work-in-progress).
//!
//! TODO: This module is a draft and is NOT yet integrated into the router.
//! - `HttpAppState` is missing the `ws_live_tx` broadcast channel
//! - Once added and wired into the router, this provides live event streaming
//!   over WebSocket as an alternative to SSE.

use super::HttpAppState;
use axum::{
    body::Bytes,
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, State},
    response::IntoResponse,
};
use std::sync::Arc;

pub async fn handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<HttpAppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| run(socket, state))
}

async fn run(mut socket: WebSocket, state: Arc<HttpAppState>) {
    // FIXME: Add ws_live_tx broadcast channel to HttpAppState
    // let mut events = state.ws_live_tx.subscribe();
    loop {
        tokio::select! {
            // event = events.recv() => match event {
            //     Ok(data) => {
            //         if socket.send(Message::Binary(Bytes::from(data))).await.is_err() {
            //             break;
            //         }
            //     }
            //     Err(_) => break,
            // },
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

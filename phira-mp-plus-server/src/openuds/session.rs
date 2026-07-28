//! Per-connection session handling for OpenUDS.
//!
//! Each connected client gets a `Session` that manages:
//! - Authentication state machine (Pending -> Authenticated -> Active)
//! - Event subscription list
//! - Frame send channel with backpressure
//!
//! Fields use interior mutability so the Session can be shared via Arc<Session>
//! across the reader, writer, and event dispatch tasks.

use crate::openuds::protocol;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use uuid::Uuid;

/// A connected OpenUDS client session.
///
/// All mutable fields use interior mutability so the Session can be
/// behind an `Arc<Session>` and accessed from multiple tasks simultaneously.
pub struct Session {
    /// Unique session identifier.
    pub id: Uuid,
    /// Whether the session has completed authentication.
    authenticated: AtomicBool,
    /// Sender for outgoing frames. Used by dispatch/events/streams to push
    /// responses back to the client.
    pub tx: mpsc::Sender<Value>,
    /// Subscribed event types (e.g., "room.*", "user.online").
    subscriptions: RwLock<HashSet<String>>,
    /// Subscribed data streams (e.g., "touches", "judges").
    stream_subscriptions: RwLock<HashSet<String>>,
    /// Client-provided name (for CLI approve mode display).
    pub client_name: RwLock<String>,
    /// Whether this session is for a toolbar/bot connection.
    pub is_bot: AtomicBool,
}

impl Session {
    pub fn new(send_buffer: usize) -> (Self, mpsc::Receiver<Value>) {
        let (tx, rx) = mpsc::channel(send_buffer.max(64));
        let id = Uuid::new_v4();

        (
            Self {
                id,
                authenticated: AtomicBool::new(false),
                tx,
                subscriptions: RwLock::new(HashSet::new()),
                stream_subscriptions: RwLock::new(HashSet::new()),
                client_name: RwLock::new(String::new()),
                is_bot: AtomicBool::new(false),
            },
            rx,
        )
    }

    /// Check if the session is authenticated.
    pub fn is_authenticated(&self) -> bool {
        self.authenticated.load(Ordering::Acquire)
    }

    /// Mark session as authenticated.
    pub fn set_authenticated(&self) {
        self.authenticated.store(true, Ordering::Release);
    }

    /// Check if this session subscribes to a given event type.
    ///
    /// Supports wildcard matching: `room.*` matches `room.created`, `room.joined`, etc.
    pub fn subscribes_to(&self, event_type: &str) -> bool {
        let Ok(subscriptions) = self.subscriptions.read() else {
            return false;
        };
        if subscriptions.contains("*") || subscriptions.contains(event_type) {
            return true;
        }
        // Check wildcard patterns: "room.*" matches "room.created"
        for pattern in subscriptions.iter() {
            if let Some(prefix) = pattern.strip_suffix(".*") {
                if event_type.starts_with(prefix)
                    && (event_type.len() == prefix.len()
                        || event_type.as_bytes().get(prefix.len()) == Some(&b'.'))
                {
                    return true;
                }
            }
        }
        false
    }

    /// Check if this session subscribes to a given stream.
    pub fn subscribes_to_stream(&self, stream: &str) -> bool {
        self.stream_subscriptions
            .read()
            .map(|s| s.contains(stream))
            .unwrap_or(false)
    }

    /// Add event type subscriptions.
    pub fn add_subscriptions(&self, types: &[String]) {
        if let Ok(mut subscriptions) = self.subscriptions.write() {
            for t in types {
                subscriptions.insert(t.clone());
            }
        }
    }

    /// Remove event type subscriptions.
    pub fn remove_subscriptions(&self, types: &[String]) {
        if let Ok(mut subscriptions) = self.subscriptions.write() {
            for t in types {
                subscriptions.remove(t);
            }
        }
    }

    /// Add stream subscriptions.
    pub fn add_stream_subscriptions(&self, streams: &[String]) {
        if let Ok(mut subs) = self.stream_subscriptions.write() {
            for s in streams {
                subs.insert(s.clone());
            }
        }
    }

    /// Remove stream subscriptions.
    pub fn remove_stream_subscriptions(&self, streams: &[String]) {
        if let Ok(mut subs) = self.stream_subscriptions.write() {
            for s in streams {
                subs.remove(s);
            }
        }
    }

    /// Set the client name.
    pub fn set_client_name(&self, name: &str) {
        if let Ok(mut n) = self.client_name.write() {
            *n = name.to_string();
        }
    }

    /// Get the client name.
    pub fn get_client_name(&self) -> String {
        self.client_name
            .read()
            .map(|n| n.clone())
            .unwrap_or_default()
    }

    /// Send a frame to the client. Returns false if the send channel is closed.
    pub async fn send(&self, value: Value) -> bool {
        self.tx.send(value).await.is_ok()
    }

    /// Build a standardized error response.
    pub fn error_response(id: Option<&str>, code: &str, message: &str) -> Value {
        let mut resp = serde_json::json!({
            "type": "response",
            "ok": false,
            "error": {
                "code": code,
                "message": message,
            }
        });
        if let Some(req_id) = id {
            resp["id"] = serde_json::json!(req_id);
        }
        resp
    }

    /// Build a standardized success response.
    pub fn success_response(id: Option<&str>, data: Value) -> Value {
        let mut resp = serde_json::json!({
            "type": "response",
            "ok": true,
            "data": data,
        });
        if let Some(req_id) = id {
            resp["id"] = serde_json::json!(req_id);
        }
        resp
    }

    /// Build a standardized event frame.
    pub fn event_response(event_type: &str, data: Value) -> Value {
        let event_id = Uuid::new_v4().to_string();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        serde_json::json!({
            "type": "event",
            "event_type": event_type,
            "data": data,
            "event_id": event_id,
            "timestamp": timestamp,
        })
    }

    /// Build a stream frame.
    pub fn stream_response(stream: &str, user_id: i32, frames: Value) -> Value {
        serde_json::json!({
            "type": "stream",
            "stream": stream,
            "user_id": user_id,
            "frames": frames,
        })
    }
}

/// Writer task: reads from the mpsc receiver and writes frames to the UDS stream.
pub async fn session_writer(
    mut write_half: tokio::net::unix::OwnedWriteHalf,
    mut rx: mpsc::Receiver<Value>,
) {
    while let Some(value) = rx.recv().await {
        match protocol::encode(&value) {
            Ok(buf) => {
                if let Err(e) = write_half.write_all(&buf).await {
                    tracing::debug!("session_writer: write error: {e}");
                    break;
                }
                if let Err(e) = write_half.flush().await {
                    tracing::debug!("session_writer: flush error: {e}");
                    break;
                }
            }
            Err(e) => {
                tracing::warn!("session_writer: encode error: {e}");
            }
        }
    }
}

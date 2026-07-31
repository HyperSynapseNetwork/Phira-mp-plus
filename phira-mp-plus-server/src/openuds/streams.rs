//! High-frequency data stream channels for OpenUDS.
//!
//! Provides dedicated channels for touch/judge data that would otherwise
//! cause head-of-line blocking on the control event channel.
//!
//! Stream frames use the same length-prefixed format but the JSON body
//! is a batch array:
//! ```json
//! {"type":"stream","stream":"touches","user_id":1001,"frames":[...]}
//! {"type":"stream","stream":"judges","user_id":1001,"events":[...]}
//! ```

use crate::openuds::session::Session;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Manages active stream subscriptions and delivers stream data.
pub struct StreamManager {
    /// Active sessions indexed by session ID.
    sessions: Arc<RwLock<HashMap<Uuid, Arc<Session>>>>,
}

impl StreamManager {
    pub fn new(sessions: Arc<RwLock<HashMap<Uuid, Arc<Session>>>>) -> Self {
        Self { sessions }
    }

    /// Deliver a touch data frame to all sessions subscribed to "touches".
    pub async fn deliver_touches(&self, user_id: i32, frames: serde_json::Value) {
        let sessions = self.sessions.read().await;
        for (_id, session) in sessions.iter() {
            if session.is_authenticated() && session.subscribes_to_stream("touches") {
                let frame = Session::stream_response("touches", user_id, frames.clone());
                let _ = session.send(frame).await;
            }
        }
    }

    /// Deliver a judge data frame to all sessions subscribed to "judges".
    pub async fn deliver_judges(&self, user_id: i32, events: serde_json::Value) {
        let sessions = self.sessions.read().await;
        for (_id, session) in sessions.iter() {
            if session.is_authenticated() && session.subscribes_to_stream("judges") {
                let frame = Session::stream_response("judges", user_id, events.clone());
                let _ = session.send(frame).await;
            }
        }
    }

    /// Deliver a formatted server log line to all sessions subscribed to "logs".
    pub async fn deliver_logs(&self, line: String) {
        let sessions = self.sessions.read().await;
        for (_id, session) in sessions.iter() {
            if session.is_authenticated() && session.subscribes_to_stream("logs") {
                let frame = Session::stream_response(
                    "logs",
                    0,
                    serde_json::json!({ "line": line }),
                );
                let _ = session.send(frame).await;
            }
        }
    }
}

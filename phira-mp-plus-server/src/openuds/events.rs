//! Event subscription and delivery for OpenUDS.
//!
//! Hooks into the PMP event bus (EventBus / MpEvent) and delivers filtered
//! events to connected UDS clients based on their subscription list.

use crate::event_bus::MpEvent;
use crate::openuds::session::Session;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Shared event dispatcher state.
pub struct EventDispatcher {
    /// Active sessions indexed by session ID.
    sessions: Arc<RwLock<HashMap<Uuid, Arc<Session>>>>,
    /// EventBus receiver for subscribing to PMP events.
    rx: tokio::sync::broadcast::Receiver<MpEvent>,
}

impl EventDispatcher {
    pub fn new(
        event_bus: &crate::event_bus::EventBus,
        sessions: Arc<RwLock<HashMap<Uuid, Arc<Session>>>>,
    ) -> Self {
        let rx = event_bus.subscribe();
        Self { sessions, rx }
    }

    /// Start the event dispatch loop. Runs forever; call in a spawned task.
    pub async fn run(mut self) {
        loop {
            match self.rx.recv().await {
                Ok(event) => {
                    self.dispatch_event(event).await;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("openuds event dispatcher lagged by {n} events");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    tracing::info!("openuds event dispatcher: event bus closed");
                    break;
                }
            }
        }
    }

    /// Convert an MpEvent to an OpenUDS event frame and deliver to
    /// subscribed sessions.
    async fn dispatch_event(&self, event: MpEvent) {
        let (event_type, data) = match Self::convert_event(&event) {
            Some(pair) => pair,
            None => return, // Event type not exposed via OpenUDS
        };

        let sessions = self.sessions.read().await;
        for (_id, session) in sessions.iter() {
            if session.is_authenticated() && session.subscribes_to(event_type) {
                let frame = Session::event_response(event_type, data.clone());
                let _ = session.send(frame).await;
            }
        }
    }

    /// Convert an MpEvent to an (event_type, data) pair suitable for
    /// OpenUDS event frames.
    fn convert_event(event: &MpEvent) -> Option<(&'static str, Value)> {
        match event {
            MpEvent::UserConnected {
                user_id,
                user_name,
                user_ip,
                ..
            } => Some((
                "user.online",
                serde_json::json!({
                    "user_id": user_id,
                    "name": user_name,
                    "ip": user_ip,
                }),
            )),
            MpEvent::UserDisconnected {
                user_id, user_name, ..
            } => Some((
                "user.offline",
                serde_json::json!({
                    "user_id": user_id,
                    "name": user_name,
                }),
            )),
            MpEvent::RoomCreated {
                room_id, room_uuid, ..
            } => Some((
                "room.created",
                serde_json::json!({
                    "room_id": room_id.to_string(),
                    "uuid": room_uuid.to_string(),
                }),
            )),
            MpEvent::RoomJoined { room_id, user_id } => Some((
                "room.joined",
                serde_json::json!({
                    "room_id": room_id.to_string(),
                    "user_id": user_id,
                }),
            )),
            MpEvent::RoomLeft { room_id, user_id } => Some((
                "room.left",
                serde_json::json!({
                    "room_id": room_id.to_string(),
                    "user_id": user_id,
                }),
            )),
            MpEvent::RoomUpdated { room_id } => Some((
                "room.updated",
                serde_json::json!({
                    "room_id": room_id.to_string(),
                }),
            )),
            MpEvent::RoomLocked { room_id, locked } => Some((
                "room.updated",
                serde_json::json!({
                    "room_id": room_id.to_string(),
                    "locked": locked,
                }),
            )),
            MpEvent::RoomCycled { room_id, cycle } => Some((
                "room.updated",
                serde_json::json!({
                    "room_id": room_id.to_string(),
                    "cycle": cycle,
                }),
            )),
            MpEvent::GameStarted {
                room_id, round_id, ..
            } => Some((
                "round.started",
                serde_json::json!({
                    "room_id": room_id.to_string(),
                    "round_id": round_id,
                }),
            )),
            MpEvent::RoundCompleted {
                room_id, round_id, ..
            } => Some((
                "round.completed",
                serde_json::json!({
                    "room_id": room_id.to_string(),
                    "round_id": round_id,
                }),
            )),
            MpEvent::RoomStateChanged { room_id, state } => Some((
                "room.updated",
                serde_json::json!({
                    "room_id": room_id.to_string(),
                    "state": state,
                }),
            )),
            MpEvent::HostChanged { room_id, host } => Some((
                "room.updated",
                serde_json::json!({
                    "room_id": room_id.to_string(),
                    "host_id": host,
                }),
            )),
            MpEvent::ChartSelected {
                room_id, chart_id, ..
            } => Some((
                "room.updated",
                serde_json::json!({
                    "room_id": room_id.to_string(),
                    "chart_id": chart_id,
                }),
            )),
            MpEvent::PlayerReadyChanged {
                room_id,
                user_id,
                ready,
            } => Some((
                "room.updated",
                serde_json::json!({
                    "room_id": room_id.to_string(),
                    "user_id": user_id,
                    "ready": ready,
                }),
            )),
            // Events not exposed via OpenUDS
            MpEvent::TouchesReceived { .. }
            | MpEvent::JudgesReceived { .. }
            | MpEvent::ChatMessage { .. }
            | MpEvent::AdminCommandExecuted { .. }
            | MpEvent::SimulationStarted { .. }
            | MpEvent::SimulationStopped { .. }
            | MpEvent::PersistenceWritten { .. }
            | MpEvent::BenchmarkCompleted { .. }
            | MpEvent::PluginEventDispatched(..)
            | MpEvent::Custom { .. } => None,
        }
    }
}

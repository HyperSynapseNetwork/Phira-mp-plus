//! Runtime v2 event bus skeleton.
//!
//! The current server still uses direct calls for most side effects.  This bus
//! is introduced as an opt-in spine for new Runtime v2 features first, so old
//! room/session/plugin behavior can be migrated gradually instead of rewritten
//! in one risky patch.

use phira_mp_common::RoomId;
use serde_json::Value;
use tokio::sync::broadcast;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum MpEvent {
    UserConnected { user_id: i32 },
    UserDisconnected { user_id: i32 },
    RoomCreated { room_id: RoomId, room_uuid: Uuid },
    RoomJoined { room_id: RoomId, user_id: i32 },
    RoomLeft { room_id: RoomId, user_id: i32 },
    RoomUpdated { room_id: RoomId },
    HostChanged { room_id: RoomId, host: Option<i32> },
    ChartSelected { room_id: RoomId, chart_id: i32 },
    GameStarted { room_id: RoomId, round_id: String },
    TouchesReceived { room_id: RoomId, user_id: i32, count: usize },
    JudgesReceived { room_id: RoomId, user_id: i32, count: usize },
    RoundCompleted { room_id: RoomId, round_id: String },
    ChatMessage { room_id: Option<RoomId>, user_id: i32 },
    AdminCommandExecuted { user_id: Option<i32>, command: String },
    SimulationStarted { run_id: Uuid },
    SimulationStopped { run_id: Uuid, reason: String },
    PersistenceWritten { table: String, rows: usize },
    Custom { kind: String, payload: Value },
}

#[derive(Debug)]
pub struct EventBus {
    tx: broadcast::Sender<MpEvent>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(16));
        Self { tx }
    }

    pub fn publish(&self, event: MpEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MpEvent> {
        self.tx.subscribe()
    }

    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

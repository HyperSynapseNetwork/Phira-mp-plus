//! Runtime v2 room-command gateway.
//!
//! This is the first production-facing seam for the future `room-actor`.
//! It deliberately does **not** own room state yet.  Instead, CLI/admin/WIT-like
//! room write commands route through one facade while the existing `Room` state
//! machine continues to own behavior.  The migration path is:
//!
//! 1. route duplicate room write commands through this gateway;
//! 2. add metrics, tests, and simulation coverage around the gateway;
//! 3. route low-risk commands through a mailbox-backed path;
//! 4. replace the remaining inline implementation with per-room actors;
//! 5. remove the old direct calls from `cli.rs`, `server.rs`, and `session.rs`.

mod audit;
mod command;
mod mailbox;
mod ops;
mod result;

pub use self::result::{RoomCommandDelivery, RoomCommandResult};

use self::command::RoomActorCommand;
use crate::server::PlusServerState;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    sync::atomic::AtomicU64,
    sync::{RwLock as StdRwLock, Weak},
};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomCommandGatewayStats {
    pub routed: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub audited: u64,
    pub latency_total_us: u64,
    pub latency_max_us: u64,
    pub mailbox_enabled: bool,
    pub mailbox_enqueued: u64,
    pub mailbox_completed: u64,
    pub mailbox_failed: u64,
    pub mailbox_fallback: u64,
    pub mailbox_closed: u64,
    pub room_mailboxes: usize,
    pub mailbox_created: u64,
    pub mailbox_registry_hit: u64,
    pub mailbox_registry_miss: u64,
    pub recent_commands: Vec<RoomCommandAuditEntry>,
    pub phase: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomCommandAuditEntry {
    pub command_id: u64,
    pub room_id: String,
    pub action: String,
    pub ok: bool,
    pub latency_us: u64,
    pub error: Option<String>,
    pub delivery: String,
}

const MAX_ROOM_COMMAND_AUDIT: usize = 128;

pub struct RoomCommandGateway {
    routed: AtomicU64,
    succeeded: AtomicU64,
    failed: AtomicU64,
    self_ref: StdRwLock<Option<Weak<RoomCommandGateway>>>,
    state_ref: StdRwLock<Option<Weak<PlusServerState>>>,
    mailbox_tx: StdRwLock<Option<mpsc::Sender<RoomActorCommand>>>,
    room_mailboxes: StdRwLock<HashMap<String, mpsc::Sender<RoomActorCommand>>>,
    mailbox_enqueued: AtomicU64,
    mailbox_completed: AtomicU64,
    mailbox_failed: AtomicU64,
    mailbox_fallback: AtomicU64,
    mailbox_closed: AtomicU64,
    mailbox_created: AtomicU64,
    mailbox_registry_hit: AtomicU64,
    mailbox_registry_miss: AtomicU64,
    command_seq: AtomicU64,
    audited: AtomicU64,
    latency_total_us: AtomicU64,
    latency_max_us: AtomicU64,
    recent_commands: StdRwLock<VecDeque<RoomCommandAuditEntry>>,
}

impl RoomCommandGateway {
    pub fn new() -> Self {
        Self {
            routed: AtomicU64::new(0),
            succeeded: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            self_ref: StdRwLock::new(None),
            state_ref: StdRwLock::new(None),
            mailbox_tx: StdRwLock::new(None),
            room_mailboxes: StdRwLock::new(HashMap::new()),
            mailbox_enqueued: AtomicU64::new(0),
            mailbox_completed: AtomicU64::new(0),
            mailbox_failed: AtomicU64::new(0),
            mailbox_fallback: AtomicU64::new(0),
            mailbox_closed: AtomicU64::new(0),
            mailbox_created: AtomicU64::new(0),
            mailbox_registry_hit: AtomicU64::new(0),
            mailbox_registry_miss: AtomicU64::new(0),
            command_seq: AtomicU64::new(0),
            audited: AtomicU64::new(0),
            latency_total_us: AtomicU64::new(0),
            latency_max_us: AtomicU64::new(0),
            recent_commands: StdRwLock::new(VecDeque::with_capacity(MAX_ROOM_COMMAND_AUDIT)),
        }
    }
}

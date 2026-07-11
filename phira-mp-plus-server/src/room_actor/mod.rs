//! Runtime v2 room-command gateway（迁移中）。
//!
//! 9 个房间管理命令已通过 per-room mailbox 串行化，
//! 包括 Lock/Cycle/Host/Hidden/Endpoint/Close/Kick/Start/Cancel。
//! Room 本身是 lock/cycle/host 的唯一真理源；mailbox 只负责串行化写命令，
//! 不再维护第二份影子状态。
//!
//! 但房间状态仍由 `room.rs` + `PlusServerState.rooms` + `RwLock`/`Atomic` 拥有，
//! Room Actor 尚未完全获得状态所有权。
//!
//! 架构：
//!
//! RoomCommandGateway
//!     ↓
//! per-room mailbox
//!     ↓
//! 调用旧 Room 对象完成操作
//!
//! 迁移路径：
//!
//! 1. 9 个管理写命令通过此网关路由 ✅
//! 2. 已入队命令的不确定结果不再重放 ✅
//! 3. mailbox 容量由运行时配置传入 ✅
//! 4. 房间完整状态迁入单一 actor-owned `RoomState` ❌
//! 5. 删除跨字段共享锁和剩余直接状态写入 ❌

pub mod actor;
mod audit;
mod command;
mod context;
mod handler;
mod mailbox;
mod ops;
mod result;

pub use self::actor::RoomSnapshot;
pub use self::result::{RoomCommandDelivery, RoomCommandPayload, RoomCommandResult};

use self::command::RoomActorCommand;
use crate::server::PlusServerState;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize},
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
    pub mailbox_retried: u64,
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
    mailbox_started: AtomicBool,
    mailbox_capacity: AtomicUsize,
    room_mailboxes: StdRwLock<HashMap<String, mpsc::Sender<RoomActorCommand>>>,
    /// Latest room snapshots, updated after each mailbox command execution.
    snapshots: StdRwLock<HashMap<String, actor::RoomSnapshot>>,
    mailbox_enqueued: AtomicU64,
    mailbox_completed: AtomicU64,
    mailbox_failed: AtomicU64,
    mailbox_retried: AtomicU64,
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
            mailbox_started: AtomicBool::new(false),
            mailbox_capacity: AtomicUsize::new(128),
            room_mailboxes: StdRwLock::new(HashMap::new()),
            snapshots: StdRwLock::new(HashMap::new()),
            mailbox_enqueued: AtomicU64::new(0),
            mailbox_completed: AtomicU64::new(0),
            mailbox_failed: AtomicU64::new(0),
            mailbox_retried: AtomicU64::new(0),
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

    /// Get the latest snapshot for a room, if available.
    pub fn room_snapshot(&self, room_id: &str) -> Option<actor::RoomSnapshot> {
        self.snapshots.read().ok().and_then(|map| map.get(room_id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_is_disabled_before_runtime_start() {
        let gateway = RoomCommandGateway::new();
        assert!(!gateway.mailbox_enabled());
    }
}

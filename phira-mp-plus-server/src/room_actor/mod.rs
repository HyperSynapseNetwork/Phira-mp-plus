//! Runtime v2 room-command gateway — actor_state as snapshot authority.
//!
//! Status (Issue #9): actor_state is now the authoritative source for
//! snapshots, with bidirectional sync keeping Room consistent.
//!
//! 架构：
//!
//! RoomCommandGateway
//!     ↓
//! per-room mailbox
//!     ↓
//! RoomActor.execute_command()
//!     ├─ 直接修改 actor_state（SetLock/SetCycle/SetHidden ✅）
//!     │   └─ 之后 sync_to_room() 推送到 Room
//!     ├─ 委派给 Room 对象（其他命令，逐步迁移中）
//!     │   └─ 之后 from_room() 读回 actor_state
//!     └─ Snapshot 始终从 actor_state 派生（原子性 ✅）
//!
//! 迁移状态：
//!
//! 1. 9 个管理写命令通过此网关路由 ✅
//! 2. 已入队命令的不确定结果不再重放 ✅
//! 3. mailbox 容量由运行时配置传入 ✅
//! 4. mailbox/快照注册表绑定房间 UUID 代次 ✅
//! 5. actor_state ↔ Room 双向同步 ✅（sync_to_room / from_room）
//! 6. Snapshot 从 actor_state 派生（不再从 Room 独立锁读取）✅
//! 7. 所有命令写 actor_state 或 Room 后交叉同步 ✅
//!    ❌ 尚未完成：
//!    - player_data / display_names 仍是实时流，不在 actor_state 中
//!    - Room 仍持有可变状态（未降级为纯广播接口）
//!    - Gateway path 命令仍通过 Room 读写（需逐步迁移到 actor_state 优先）

pub mod actor;
mod audit;
mod command;
mod context;
mod handler;
mod mailbox;
mod ops;
mod result;

pub use self::actor::{RoomMembers, RoomSnapshot, RoomState, RoundInfo};
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

#[derive(Clone)]
struct RoomMailboxEntry {
    room_uuid: uuid::Uuid,
    tx: mpsc::Sender<RoomActorCommand>,
}

#[derive(Clone)]
struct RoomSnapshotEntry {
    room_uuid: uuid::Uuid,
    snapshot: actor::RoomSnapshot,
}

pub struct RoomCommandGateway {
    routed: AtomicU64,
    succeeded: AtomicU64,
    failed: AtomicU64,
    self_ref: StdRwLock<Option<Weak<RoomCommandGateway>>>,
    state_ref: StdRwLock<Option<Weak<PlusServerState>>>,
    mailbox_started: AtomicBool,
    mailbox_capacity: AtomicUsize,
    room_mailboxes: StdRwLock<HashMap<String, RoomMailboxEntry>>,
    /// Latest room snapshots, updated after each mailbox command execution.
    snapshots: StdRwLock<HashMap<String, RoomSnapshotEntry>>,
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
        self.snapshots
            .read()
            .ok()
            .and_then(|map| map.get(room_id).map(|entry| entry.snapshot.clone()))
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

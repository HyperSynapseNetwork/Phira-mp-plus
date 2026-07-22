//! Runtime v2 room-command gateway（迁移中）。
//!
//! WARNING: RoomActorState 当前仍是镜像状态（Issue #9）。
//!
//! 源码仍同时持有 `room: Arc<Room>` 和 `actor_state: Option<RoomActorState>`。
//! ActorState 在命令执行后会从共享 `Room` 对象重新读取刷新。这意味着：
//!
//! - Room 仍是真实状态源
//! - ActorState 只读、写后刷新、非权威
//! - snapshot 不是原子的（Room 中跨字段仍使用独立锁）
//! - 成员、谱面、轮次和玩家数据仍由共享锁持有
//!
//! 所有房间管理命令已通过 per-room mailbox 串行化。
//! Room Actor 已获得 `RoomState` 所有权（control/lifecycle/members/chart/round/live），
//! handler 直接修改 actor-owned state，不再完全依赖 `Room` 对象。
//!
//! 架构：
//!
//! RoomCommandGateway
//!     ↓
//! per-room mailbox
//!     ↓
//! RoomActor.execute_command()
//!     ├─ 直接修改 actor_state（SetLock/SetCycle/SetHidden ✅）
//!     └─ 委派给 Room 对象（其他命令，逐步迁移中）
//!
//! 迁移路径：
//!
//! 1. 9 个管理写命令通过此网关路由 ✅
//! 2. 已入队命令的不确定结果不再重放 ✅
//! 3. mailbox 容量由运行时配置传入 ✅
//! 4. mailbox/快照注册表绑定房间 UUID 代次 ✅
//! 5. 房间完整状态迁入单一 actor-owned `RoomState` 🔶（actor_state 仍是镜像）
//! 6. 删除跨字段共享锁和剩余直接状态写入 ❌
//!
//! 完成迁移的标志：
//! - `RoomActor::room` 字段删除，`actor_state` 成为唯一状态源
//! - 命令响应直接写 `actor_state`，不再需要 `refresh_snapshot()`
//! - `RoomSnapshot` 始终从 `RoomActorState` 派生，保证原子性
//! - `Room` 对象降级为纯通信/广播接口，不再保存可变状态

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

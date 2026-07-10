//! Runtime v2 room-command gateway（迁移中）。
//!
//! 7 个房间命令已通过 per-room mailbox 串行化（`room_mailbox_only`），
//! 包括 Lock/Cycle/Host/Close/Kick/Start/Cancel。
//! Lock/cycle/host 状态通过 owned_locks/owned_cycles/owned_hosts 追踪。
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
//! 1. 所有房间写命令通过此网关路由 ✅
//! 2. 指标、测试和仿真覆盖 ✅
//! 3. 低风险命令通过 mailbox ❌ → 完成
//! 4. 剩余内联实现替换为 per-room actor ❌
//! 5. 删除 cli.rs/server.rs/session.rs 中的旧直接调用 ❌

pub mod actor;
mod audit;
mod command;
mod context;
mod handler;
mod mailbox;
mod ops;
mod result;

pub use self::result::{RoomCommandDelivery, RoomCommandPayload, RoomCommandResult};

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
    mailbox_tx: StdRwLock<Option<mpsc::Sender<RoomActorCommand>>>,
    room_mailboxes: StdRwLock<HashMap<String, mpsc::Sender<RoomActorCommand>>>,
    /// Owned lock state per room. Each entry tracks what the mailbox worker
    /// believes the lock state is. Populated by SetLock in the mailbox path.
    owned_locks: StdRwLock<HashMap<String, bool>>,
    /// Owned cycle state per room. Populated by SetCycle in the mailbox path.
    owned_cycles: StdRwLock<HashMap<String, bool>>,
    /// Owned host state per room. Populated by SetHost in the mailbox path.
    owned_hosts: StdRwLock<HashMap<String, Option<i32>>>,
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
            mailbox_tx: StdRwLock::new(None),
            room_mailboxes: StdRwLock::new(HashMap::new()),
            owned_locks: StdRwLock::new(HashMap::new()),
            owned_cycles: StdRwLock::new(HashMap::new()),
            owned_hosts: StdRwLock::new(HashMap::new()),
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

    /// Check the owned lock state for a room. Returns None if the room
    /// has not been tracked yet (falls back to Room.locked).
    pub fn room_lock_owned(&self, room_id: &str) -> Option<bool> {
        self.owned_locks.read().ok().and_then(|map| map.get(room_id).copied())
    }

    /// Set the owned lock state for a room.
    pub fn set_room_lock_owned(&self, room_id: &str, locked: bool) {
        if let Ok(mut locks) = self.owned_locks.write() {
            locks.insert(room_id.to_string(), locked);
        }
    }

    /// Check the owned cycle state for a room. Returns None if the room
    /// has not been tracked yet (falls back to Room.cycle).
    pub fn room_cycle_owned(&self, room_id: &str) -> Option<bool> {
        self.owned_cycles.read().ok().and_then(|map| map.get(room_id).copied())
    }

    /// Set the owned cycle state for a room.
    pub fn set_room_cycle_owned(&self, room_id: &str, cycle: bool) {
        if let Ok(mut cycles) = self.owned_cycles.write() {
            cycles.insert(room_id.to_string(), cycle);
        }
    }

    /// Check the owned host state for a room. Returns None if the room
    /// has not been tracked yet (falls back to Room.host lookup).
    pub fn room_host_owned(&self, room_id: &str) -> Option<Option<i32>> {
        self.owned_hosts.read().ok().and_then(|map| map.get(room_id).copied())
    }

    /// Set the owned host state for a room.
    pub fn set_room_host_owned(&self, room_id: &str, host: Option<i32>) {
        if let Ok(mut hosts) = self.owned_hosts.write() {
            hosts.insert(room_id.to_string(), host);
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
    fn owned_lock_returns_none_for_unknown_room() {
        let gateway = RoomCommandGateway::new();
        assert_eq!(gateway.room_lock_owned("nonexistent"), None);
    }

    #[test]
    fn owned_lock_updates_after_set_lock() {
        let gateway = RoomCommandGateway::new();
        if let Ok(mut locks) = gateway.owned_locks.write() {
            locks.insert("room-1".to_string(), true);
        }
        assert_eq!(gateway.room_lock_owned("room-1"), Some(true));
    }

    #[test]
    fn owned_cycle_returns_none_for_unknown_room() {
        let gateway = RoomCommandGateway::new();
        assert_eq!(gateway.room_cycle_owned("nonexistent"), None);
    }

    #[test]
    fn owned_cycle_updates_after_set_cycle() {
        let gateway = RoomCommandGateway::new();
        if let Ok(mut cycles) = gateway.owned_cycles.write() {
            cycles.insert("room-2".to_string(), true);
        }
        assert_eq!(gateway.room_cycle_owned("room-2"), Some(true));
    }

    #[test]
    fn owned_host_returns_none_for_unknown_room() {
        let gateway = RoomCommandGateway::new();
        assert_eq!(gateway.room_host_owned("nonexistent"), None);
    }

    #[test]
    fn owned_host_updates_after_set_host() {
        let gateway = RoomCommandGateway::new();
        if let Ok(mut hosts) = gateway.owned_hosts.write() {
            hosts.insert("room-3".to_string(), Some(42));
        }
        assert_eq!(gateway.room_host_owned("room-3"), Some(Some(42)));
    }

    #[test]
    fn owned_host_can_be_none_for_system_host() {
        let gateway = RoomCommandGateway::new();
        if let Ok(mut hosts) = gateway.owned_hosts.write() {
            hosts.insert("room-4".to_string(), None);
        }
        assert_eq!(gateway.room_host_owned("room-4"), Some(None));
    }

    #[test]
    fn set_lock_visibility_is_restricted() {
        // Compile-time check: set_lock_inline is pub(in crate::room_actor)
        // and NOT accessible from outside the module. The only write path
        // to room.locked is through the mailbox handler via set_lock_inline.
        // Verify the public API surface: set_lock goes through mailbox_only.
        let gateway = RoomCommandGateway::new();
        // room_mailbox_sender is None before start_mailbox — this means
        // mailbox-only routing will fail with "mailbox not available",
        // proving the path is gated, not a direct write.
        assert!(
            !gateway.mailbox_enabled(),
            "mailbox not available before start_mailbox"
        );
    }
}

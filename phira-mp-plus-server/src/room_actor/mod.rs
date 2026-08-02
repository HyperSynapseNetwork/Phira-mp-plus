//! Room command gateway — actor_state as primary snapshot authority.
//!
//! Status: 主要业务状态已 actor-owned；管理编排和连接 registry cutover 尚未完成。
//! Room is a pure broadcast bus (connection references only).
//!
//! 架构：
//!
//! RoomCommandGateway
//!     ↓
//! per-room mailbox
//!     ↓
//! RoomActor.execute_command()
//!     └─ 直接修改 actor_state → refresh_snapshot_from_state()
//!        └─ Snapshot 始终从 actor_state 派生（原子性 ✅）
//!
//! 迁移状态：
//!
//! 1. 所有管理写命令通过此网关路由 ✅
//! 2. 已入队命令的不确定结果不再重放 ✅
//! 3. mailbox 容量由运行时配置传入 ✅
//! 4. mailbox/快照注册表绑定房间 UUID 代次 ✅
//! 5. actor_state 始终非空 ✅（RoomActor.actor_state: RoomActorState）
//! 6. Snapshot 从 actor_state 派生（不再从 Room 独立锁读取）✅
//! 7. 所有命令写 actor_state 后更新 snapshot cache ✅
//! 8. live 状态由 SetLive 命令管理 ✅
//! 9. members/monitors 由 AddUser/RemoveUser 命令管理 ✅
//!
//! All room mutations MUST route through `RoomCommandGateway` (per-room
//! mailbox) rather than touching `state.rooms` / `room.*` directly.
//!
//! Known bypasses without a gateway equivalent (TODO):
//! - `room.create_empty` in state query dispatch → `s.create_empty_room()`
//!   directly; no `RoomActorCommand::CreateRoom` variant exists.
//! - `send_room_chat` in state query dispatch → `room.send()` directly
//!   (read-only message broadcast, but bypasses the mailbox).
//! - `create_empty_room` / `force_move_user_to_room` / `assign_room_host_if_missing`
//!   in PlusServer: direct Room manipulation; complex multi-step operations that
//!   may need new command variants.

#![allow(clippy::too_many_arguments, clippy::type_complexity)]

pub mod actor;
mod audit;
/// `pub(crate)`: `session.rs` names `crate::room_actor::command::RoomOrigin`
/// when projecting a `CommandOrigin` into the room-actor token (PMP44 P0-C).
pub(crate) mod command;
mod context;
mod handler;
mod lifecycle;
mod mailbox;
mod ops;
mod result;

pub use self::actor::{RoomMembers, RoomSnapshot, RoomState, RoundInfo};
pub use self::result::{
    BindAndSnapshotData, BindAndSnapshotUser, RoomCommandDelivery, RoomCommandPayload,
    RoomCommandResult, RoomCommandTerminal,
};

use self::command::RoomActorCommand;
use crate::server::PlusServerState;
use phira_mp_common::ServerCommand;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    sync::{RwLock as StdRwLock, Weak},
};
use tokio::sync::{broadcast, mpsc};

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
    /// Fire-and-forget telemetry channel (审计 P0).
    telemetry_tx: mpsc::Sender<RoomActorCommand>,
    /// Bounded broadcast channel for monitor telemetry (审计 P1-A).
    /// try_send drops if full — slow monitors never block the hot path.
    /// A per-room relay task reads from this channel and forwards to connected
    /// monitor sessions via Room::broadcast_monitors.
    monitor_telemetry_tx: broadcast::Sender<ServerCommand>,
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

impl Default for RoomCommandGateway {
    fn default() -> Self {
        Self::new()
    }
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

    /// Get the fire-and-forget telemetry sender for a room, if available.
    /// Returns `None` when the mailbox is not yet started, the room is
    /// unknown, or the telemetry channel is closed.
    pub(crate) async fn telemetry_sender(&self, room_id: &str) -> Option<mpsc::Sender<RoomActorCommand>> {
        if !self.mailbox_enabled() {
            return None;
        }
        let mailboxes = self.room_mailboxes.read().ok()?;
        mailboxes.get(room_id).map(|entry| entry.telemetry_tx.clone())
    }

    /// Get the bounded broadcast sender for monitor telemetry, if available.
    /// Returns `None` when the mailbox is not yet started, the room is
    /// unknown, or the broadcast channel is closed.
    ///
    /// The sender uses `try_send` internally — if the channel is full the
    /// oldest telemetry frame is dropped, so slow monitors never block the
    /// game hot path.
    pub(crate) fn monitor_telemetry_sender(&self, room_id: &str) -> Option<broadcast::Sender<ServerCommand>> {
        if !self.mailbox_enabled() {
            return None;
        }
        let mailboxes = self.room_mailboxes.read().ok()?;
        mailboxes.get(room_id).map(|entry| entry.monitor_telemetry_tx.clone())
    }

    /// PMP45 P0-F: 网关单调命令序号（Room Actor 排序点观测）。`finish_command`
    /// 在每条命令完成后 `fetch_add`；`BindAndSnapshot` 处理器读取它作为
    /// cutover token——快照反映的是所有 `command_id <= token` 命令提交后的
    /// 权威状态。
    pub fn command_seq(&self) -> u64 {
        self.command_seq.load(Ordering::Relaxed)
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

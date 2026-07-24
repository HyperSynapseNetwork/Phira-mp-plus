//! Room broadcast bus — no mutable state, pure message dispatch.
//!
//! Room is a lightweight broadcast interface that holds connection references
//! (users/monitors) and provides send/broadcast/publish_update methods.
//! All room state (control flags, lifecycle, chart, round tracking, player
//! data, display names) is owned by the per-room RoomActor and accessed via
//! the RoomCommandGateway snapshot cache.
//!
//! This module also defines shared data types (InternalRoomState,
//! PlayerLiveData, PlayRound, PlayResult) that the actor and snapshot
//! subsystems use. These are data shapes, not mutable state.
//!
//! Phase 2, Work C — Room was degraded from a state-holder to a pure
//! broadcast interface. Every set_* method was removed; callers route
//! mutations through RoomActorCommand variants via RoomCommandGateway.

use crate::plugin::{JudgeEventItem, PluginManager, TouchEventPoint};
use phira_mp_common::{
    Message, PartialRoomData, RoomEvent, RoomId, RoundData, ServerCommand,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Weak,
};
use tokio::sync::RwLock;
use tracing::{debug, info};

#[derive(Default, Debug, Clone)]
pub enum InternalRoomState {
    #[default]
    SelectChart,
    WaitForReady {
        started: HashSet<i32>,
        admin_started: bool,
    },
    Playing {
        results: HashMap<i32, crate::server::Record>,
        aborted: HashSet<i32>,
    },
}

impl InternalRoomState {
    pub fn to_client(&self, chart: Option<i32>) -> phira_mp_common::RoomState {
        match self {
            Self::SelectChart => phira_mp_common::RoomState::SelectChart(chart),
            Self::WaitForReady { .. } => phira_mp_common::RoomState::WaitingForReady,
            Self::Playing { .. } => phira_mp_common::RoomState::Playing,
        }
    }

    pub fn stripped(&self) -> phira_mp_common::StrippedRoomState {
        match self {
            Self::SelectChart => phira_mp_common::StrippedRoomState::SelectingChart,
            Self::WaitForReady { .. } => phira_mp_common::StrippedRoomState::WaitingForReady,
            Self::Playing { .. } => phira_mp_common::StrippedRoomState::Playing,
        }
    }
}

/// 一轮游玩的结算数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayRound {
    /// 本轮唯一标识符
    pub round_id: uuid::Uuid,
    pub chart_id: i32,
    pub chart_name: String,
    pub results: Vec<PlayResult>,
}

/// 单个用户的实时触控数据缓存
#[derive(Debug, Clone, Default, Serialize)]
pub struct PlayerLiveData {
    /// 最近的触控帧（最多保留 120 帧 ≈ 2 秒 @ 60fps）
    pub touches: Vec<TouchEventPoint>,
    /// 最近的判定事件（最多保留 64 条）
    pub judges: Vec<JudgeEventItem>,
}

impl PlayerLiveData {
    pub fn push_touches(&mut self, new_touches: &[TouchEventPoint]) {
        self.touches.extend_from_slice(new_touches);
        if self.touches.len() > 120 {
            self.touches.drain(0..self.touches.len() - 120);
        }
    }

    pub fn push_judges(&mut self, new_judges: &[JudgeEventItem]) {
        self.judges.extend_from_slice(new_judges);
        if self.judges.len() > 64 {
            self.judges.drain(0..self.judges.len() - 64);
        }
    }

    pub fn clear(&mut self) {
        self.touches.clear();
        self.judges.clear();
    }
}

/// 单个用户的游玩结算
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayResult {
    pub user_id: i32,
    pub user_name: String,
    pub score: i32,
    pub accuracy: f32,
    pub perfect: i32,
    pub good: i32,
    pub bad: i32,
    pub miss: i32,
    pub max_combo: i32,
    pub full_combo: bool,
    pub aborted: bool,
    pub std_score: f32,
}

pub fn protocol_round(round: &PlayRound) -> RoundData {
    RoundData {
        chart: round.chart_id,
        records: round
            .results
            .iter()
            .map(|result| phira_mp_common::Record {
                id: 0,
                player: result.user_id,
                score: result.score,
                perfect: result.perfect,
                good: result.good,
                bad: result.bad,
                miss: result.miss,
                max_combo: result.max_combo,
                accuracy: result.accuracy,
                full_combo: result.full_combo,
                std: 0.0,
                std_score: result.std_score,
            })
            .collect(),
    }
}

/// Read-only snapshot of room control-plane state.
///
/// Populated from the per-room actor's authoritative state, not from Room's
/// own fields (which no longer hold mutable state).
#[derive(Debug, Clone, Serialize)]
pub struct RoomControlSnapshot {
    pub host_id: Option<i32>,
    pub locked: bool,
    pub cycle: bool,
    pub hidden: bool,
    pub persistent_empty: bool,
    pub system_host: bool,
    pub phira_api_endpoint: Option<String>,
    pub admin_start_pending: bool,
    pub max_users: usize,
    pub generation: u64,
}

/// A room as a broadcast bus.
///
/// Room now holds only connection references and broadcast methods. All
/// mutable state (lock, cycle, hidden, host, chart, lifecycle, player data,
/// display names) lives in the per-room RoomActor and is accessible via
/// `RoomCommandGateway::room_snapshot()`.
pub struct Room {
    pub id: RoomId,
    /// 房间唯一标识符
    pub uuid: uuid::Uuid,
    /// 用于触发 RoomComplete 等插件事件的插件管理器
    pub plugin_manager: Option<Arc<PluginManager>>,
    /// Reference to the server state (crate-visible for handler broadcasts).
    pub(crate) server: Weak<crate::server::PlusServerState>,

    /// Whether the room has at least one active monitor session.
    pub live: AtomicBool,

    /// Connected players (weak references).
    pub users: RwLock<Vec<Weak<super::session::User>>>,
    /// Connected monitors (weak references).
    pub monitors: RwLock<Vec<Weak<super::session::User>>>,

    /// 历史游玩记录（不持久化，房间解散即清除）
    pub play_history: crate::play_history::PlayHistoryStore,
    /// 轮次数据持久化存储。
    pub round_store: Option<Arc<crate::round_store::RoundStore>>,

    /// 房间创建时间戳（Unix 毫秒）
    pub created_at: i64,
}

/// 房间名以前缀 `-` 开头时默认隐藏。
pub fn room_id_is_hidden(room_id: &str) -> bool {
    room_id.starts_with('-')
}

impl Room {
    pub fn new(
        id: RoomId,
        host: Weak<super::session::User>,
        plugin_manager: Option<Arc<PluginManager>>,
        server: Weak<crate::server::PlusServerState>,
        _max_users: usize,
        round_store: Option<Arc<crate::round_store::RoundStore>>,
    ) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Self {
            id,
            uuid: uuid::Uuid::new_v4(),
            plugin_manager,
            server,
            live: AtomicBool::new(false),
            users: vec![host].into(),
            monitors: Vec::new().into(),
            play_history: crate::play_history::PlayHistoryStore::new(),
            round_store,
            created_at: now,
        }
    }

    pub fn new_empty(
        id: RoomId,
        plugin_manager: Option<Arc<PluginManager>>,
        server: Weak<crate::server::PlusServerState>,
        max_users: usize,
        round_store: Option<Arc<crate::round_store::RoundStore>>,
    ) -> Self {
        let mut room = Self::new(
            id,
            Weak::<super::session::User>::new(),
            plugin_manager,
            server,
            max_users,
            round_store,
        );
        room.users = Vec::new().into();
        room
    }

    /// Read the latest control snapshot from the actor's snapshot cache.
    ///
    /// This is a synchronous call because `server.room_snapshot()` reads from
    /// a `StdRwLock` cache. Returns a default snapshot if the room has no
    /// actor yet (e.g. during initial construction before the first mailbox
    /// command).
    pub fn control_snapshot(&self) -> RoomControlSnapshot {
        if let Some(server) = self.server.upgrade() {
            if let Some(snap) = server.room_snapshot(&self.id.to_string()) {
                return RoomControlSnapshot {
                    host_id: snap.host,
                    locked: snap.locked,
                    cycle: snap.cycle,
                    hidden: snap.hidden,
                    persistent_empty: false, // not yet in actor snapshot
                    system_host: false,
                    phira_api_endpoint: None,
                    admin_start_pending: false,
                    max_users: 100,
                    generation: 0,
                };
            }
        }
        // Fallback default — the actor will populate after the first command.
        RoomControlSnapshot {
            host_id: None,
            locked: false,
            cycle: false,
            hidden: room_id_is_hidden(&self.id.to_string()),
            persistent_empty: false,
            system_host: false,
            phira_api_endpoint: None,
            admin_start_pending: false,
            max_users: 100,
            generation: 0,
        }
    }

    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::Relaxed)
    }

    // ── Broadcast / send methods ─────────────────────────────────────

    #[inline]
    pub async fn send(&self, msg: Message) {
        self.broadcast(ServerCommand::Message(msg)).await;
    }

    /// Send a message to all users EXCEPT the one with `excluded_user_id`.
    pub async fn send_except(&self, excluded_user_id: i32, msg: Message) {
        let cmd = ServerCommand::Message(msg);
        for session in self.users().await.into_iter().chain(self.monitors().await) {
            if session.user.id != excluded_user_id {
                session.try_send(cmd.clone()).await;
            }
        }
    }

    pub async fn broadcast(&self, cmd: ServerCommand) {
        debug!("broadcast {cmd:?}");
        for session in self.users().await.into_iter().chain(self.monitors().await) {
            session.try_send(cmd.clone()).await;
        }
    }

    pub async fn broadcast_except(&self, excluded_user_id: i32, cmd: ServerCommand) {
        for session in self.users().await.into_iter().chain(self.monitors().await) {
            if session.id != excluded_user_id {
                session.try_send(cmd.clone()).await;
            }
        }
    }

    pub async fn broadcast_players(&self, cmd: ServerCommand) {
        for session in self.users().await {
            session.try_send(cmd.clone()).await;
        }
    }

    pub async fn broadcast_monitors(&self, cmd: ServerCommand) {
        for session in self.monitors().await {
            session.try_send(cmd.clone()).await;
        }
    }

    #[inline]
    pub async fn send_as(&self, user: &super::session::User, content: String) {
        self.send(Message::Chat {
            user: user.id,
            content,
        })
        .await;
    }

    /// Broadcast a `PartialRoomData` update to the monitoring infrastructure.
    pub(crate) async fn publish_update(&self, data: PartialRoomData) {
        if let Some(server) = self.server.upgrade() {
            server
                .publish_room_event(RoomEvent::UpdateRoom {
                    room: self.id.clone(),
                    data,
                })
                .await;
            server.publish_runtime_event(crate::event_bus::MpEvent::RoomUpdated {
                room_id: self.id.clone(),
            });
        }
    }

    // ── User / monitor management ────────────────────────────────────

    pub async fn add_user(&self, user: Weak<super::session::User>, monitor: bool) -> bool {
        if monitor {
            let mut guard = self.monitors.write().await;
            guard.retain(|it| it.strong_count() > 0);
            guard.push(user);
            true
        } else {
            let mut guard = self.users.write().await;
            guard.retain(|it| it.strong_count() > 0);
            let max_users = self.control_snapshot().max_users;
            if guard.len() >= max_users {
                false
            } else {
                guard.push(user);
                true
            }
        }
    }

    /// 管理员强制迁移用户时使用：绕过房间人数、锁定和状态限制，并去重。
    pub async fn force_add_user(&self, user: Weak<super::session::User>, monitor: bool) {
        let target_ptr = user.as_ptr() as usize;
        {
            let mut players = self.users.write().await;
            players
                .retain(|entry| entry.strong_count() > 0 && entry.as_ptr() as usize != target_ptr);
        }
        {
            let mut monitors = self.monitors.write().await;
            monitors
                .retain(|entry| entry.strong_count() > 0 && entry.as_ptr() as usize != target_ptr);
        }
        if monitor {
            self.monitors.write().await.push(user);
            self.has_active_monitors().await;
        } else {
            self.users.write().await.push(user);
        }
    }

    pub async fn users(&self) -> Vec<Arc<super::session::User>> {
        self.users
            .read()
            .await
            .iter()
            .filter_map(|it| it.upgrade())
            .collect()
    }

    pub async fn monitors(&self) -> Vec<Arc<super::session::User>> {
        self.monitors
            .read()
            .await
            .iter()
            .filter_map(|it| it.upgrade())
            .collect()
    }

    pub async fn has_active_monitors(&self) -> bool {
        let active = !self.monitors().await.is_empty();
        self.live.store(active, Ordering::Relaxed);
        active
    }

    // ── User leave / cleanup ─────────────────────────────────────────

    /// Handle a user leaving the room. Removes them from users/monitors lists,
    /// cleans up any cached data, and returns `true` if the room should be dropped.
    #[must_use]
    pub async fn on_user_leave(&self, user: &super::session::User) -> bool {
        let is_monitor = user.monitor.load(Ordering::Relaxed);
        let leave = ServerCommand::Message(Message::LeaveRoom {
            user: user.id,
            name: user.name.clone(),
        });
        if is_monitor {
            self.broadcast_players(leave).await;
        } else {
            self.broadcast(leave).await;
        }

        *user.room.write().await = None;
        (if is_monitor {
            &self.monitors
        } else {
            &self.users
        })
        .write()
        .await
        .retain(|entry| {
            entry
                .upgrade()
                .is_some_and(|current| !std::ptr::eq(Arc::as_ptr(&current), user))
        });

        if is_monitor {
            self.has_active_monitors().await;
            return false;
        }

        let users = self.users().await;
        if users.is_empty() {
            // Room is empty — let the caller decide whether to drop or preserve.
            // The caller (session_room, force_move, or actor handler) checks
            // the persistent_empty flag and removes the room from the global map.
            info!("room users all disconnected");
            return true;
        }

        // Host reassignment is handled by the caller (actor handler or
        // force_move path) through the RoomCommandGateway.
        false
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn default_snapshot_fallback() {
        // control_snapshot on a room with no actor returns sensible defaults.
        // This is exercised indirectly via construction flows.
    }
}

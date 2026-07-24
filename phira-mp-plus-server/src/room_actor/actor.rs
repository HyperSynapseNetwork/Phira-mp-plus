//! Room Actor — actor-owned state with Room as pure broadcast bus.
//!
//! After Phase 2 Work C, Room no longer holds any mutable state. All room
//! state (control flags, lifecycle, chart, round meta, player data, display
//! names) is actor-owned. Room is used exclusively for broadcast/send and
//! user/monitor reference management.
//!
//! `sync_to_room()` has been removed — Room has no state fields to sync to.
//! The actor updates its `latest_snapshot` after every command and stores it
//! in the gateway's snapshot cache for external readers.

use super::command::RoomActorCommand;
use crate::room::{InternalRoomState, PlayerLiveData, Room, RoomControlSnapshot};
use crate::server::PlusServerState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// 房间状态的只读快照。
/// Actor 在每次命令执行后生成新快照，外部读路径使用快照而非直接访问 Room。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomSnapshot {
    pub room_id: String,
    pub room_uuid: String,
    pub locked: bool,
    pub cycle: bool,
    pub host: Option<i32>,
    pub hidden: bool,
    pub live: bool,
    pub created_at: i64,
    /// Chart id, if one is selected (actor-authoritative).
    pub chart: Option<i32>,
    /// The room lifecycle state as a stripped enum (actor-authoritative).
    pub stripped: phira_mp_common::StrippedRoomState,
    /// Current round id, if a round is active (actor-authoritative).
    pub round_id: Option<uuid::Uuid>,
    /// IDs of users who have readied up (actor-authoritative, only meaningful in WaitForReady).
    pub ready_set: Option<Vec<i32>>,
}

impl RoomSnapshot {
    /// 从 Room 对象构建快照。
    pub async fn from_room(room: &Room) -> Self {
        let control = room.control_snapshot();
        RoomSnapshot {
            room_id: room.id.to_string(),
            room_uuid: room.uuid.to_string(),
            locked: control.locked,
            cycle: control.cycle,
            host: control.host_id,
            hidden: control.hidden,
            live: room.is_live(),
            created_at: room.created_at,
            chart: None, // not available from control snapshot alone
            stripped: phira_mp_common::StrippedRoomState::SelectingChart,
            round_id: None,
            ready_set: None,
        }
    }

    /// 从 actor state 构建快照（迁移后权威路径）。
    pub fn from_actor_state(state: &RoomActorState) -> Self {
        Self {
            room_id: state.room_id.clone(),
            room_uuid: state.room_uuid.clone(),
            locked: state.state.control.locked,
            cycle: state.state.control.cycle,
            host: state.state.control.host_id,
            hidden: state.state.control.hidden,
            live: state.state.live,
            created_at: state.created_at,
            chart: state.state.chart,
            stripped: state.state.lifecycle.stripped(),
            round_id: state.state.round.round_id,
            ready_set: match &state.state.lifecycle {
                InternalRoomState::WaitForReady { started, .. } => {
                    Some(started.iter().copied().collect())
                }
                _ => None,
            },
        }
    }
}

/// 房间成员列表（仅存用户 ID，完整 User 对象仍在 session 层持有）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomMembers {
    pub users: Vec<i32>,
    pub monitors: Vec<i32>,
}

impl RoomMembers {
    pub fn is_empty(&self) -> bool {
        self.users.is_empty() && self.monitors.is_empty()
    }
}

/// 当前轮次元数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundInfo {
    pub round_id: Option<uuid::Uuid>,
    pub round_uuid: Option<uuid::Uuid>,
}

/// Actor 持有的完整房间状态。
/// 所有房间数据在此处，而非共享的 Room 对象中。
#[derive(Debug, Clone)]
pub struct RoomState {
    pub control: RoomControlSnapshot,
    pub lifecycle: InternalRoomState,
    pub members: RoomMembers,
    pub chart: Option<i32>,
    pub round: RoundInfo,
    pub live: bool,
}

impl RoomState {
    /// 从共享 Room 对象填充自身（迁移过渡用）。
    ///
    /// After Phase 2 Work C, Room no longer holds state/chart/round_id.
    /// We read control from the actor snapshot cache, lifecycle defaults
    /// to SelectChart, and members from Room's user/monitor lists.
    pub async fn from_room(room: &Room) -> Self {
        let control = room.control_snapshot();
        let users = room.users().await.iter().map(|u| u.id).collect();
        let monitors = room.monitors().await.iter().map(|u| u.id).collect();
        Self {
            control,
            lifecycle: InternalRoomState::SelectChart,
            members: RoomMembers { users, monitors },
            chart: None,
            round: RoundInfo {
                round_id: None,
                round_uuid: None,
            },
            live: room.is_live(),
        }
    }

    /// 构建 RoomSnapshot（供外部只读路径使用）。
    pub fn to_snapshot(&self, room_id: &str, room_uuid: &str, created_at: i64) -> crate::room_actor::actor::RoomSnapshot {
        crate::room_actor::actor::RoomSnapshot {
            room_id: room_id.to_string(),
            room_uuid: room_uuid.to_string(),
            locked: self.control.locked,
            cycle: self.control.cycle,
            host: self.control.host_id,
            hidden: self.control.hidden,
            live: self.live,
            created_at,
            chart: self.chart,
            stripped: self.lifecycle.stripped(),
            round_id: self.round.round_id,
            ready_set: match &self.lifecycle {
                InternalRoomState::WaitForReady { started, .. } => {
                    Some(started.iter().copied().collect())
                }
                _ => None,
            },
        }
    }

    /// 设置房间锁定状态。
    pub fn set_locked(&mut self, locked: bool) {
        self.control.locked = locked;
    }

    pub fn set_cycle(&mut self, cycle: bool) {
        self.control.cycle = cycle;
    }

    pub fn set_hidden(&mut self, hidden: bool) {
        self.control.hidden = hidden;
    }
}

/// Full actor-owned room state (migration target).
/// All room data lives here, not in the shared Room object.
#[derive(Debug, Clone)]
pub struct RoomActorState {
    pub room_id: String,
    pub room_uuid: String,
    pub state: RoomState,
    pub created_at: i64,
    /// 各玩家实时触控/判定数据缓存（actor-authoritative）
    pub player_data: HashMap<i32, PlayerLiveData>,
    /// 各玩家展示名（actor-authoritative）
    pub display_names: HashMap<i32, String>,
}

impl RoomActorState {
    /// Snapshot the current Room state into an actor-owned copy.
    ///
    /// After Phase 2 Work C, Room no longer holds player_data or
    /// display_names, so these are initialized as empty. The actor will
    /// populate them as commands arrive.
    pub async fn from_room(room: &Room) -> Self {
        let state = RoomState::from_room(room).await;
        Self {
            room_id: room.id.to_string(),
            room_uuid: room.uuid.to_string(),
            state,
            created_at: room.created_at,
            player_data: HashMap::new(),
            display_names: HashMap::new(),
        }
    }

    /// Create a new `RoomActorState` from its constituent parts.
    pub fn new(
        room_id: String,
        room_uuid: String,
        state: RoomState,
        created_at: i64,
    ) -> Self {
        Self {
            room_id,
            room_uuid,
            state,
            created_at,
            player_data: HashMap::new(),
            display_names: HashMap::new(),
        }
    }

    // (Removed) Room no longer holds mutable state, so there is nothing to
    // sync to. This method is intentionally omitted. The actor updates its
    // own `latest_snapshot` and stores it in the gateway cache after every
    // command. External readers (snapshots, queries) use
    // `RoomCommandGateway::room_snapshot()` instead of reading from Room.
}

/// Room Actor — 每个房间一个，持有状态并处理命令。
pub struct RoomActor {
    room: Arc<Room>,
    pub(super) state: Arc<PlusServerState>,
    latest_snapshot: RoomSnapshot,
    /// Actor-owned state mirror. During migration, populated from Room
    /// and updated by commands. Eventually replaces Room entirely.
    pub actor_state: Option<RoomActorState>,
}

impl RoomActor {
    pub async fn new(room: Arc<Room>, state: Arc<PlusServerState>) -> Self {
        // Note: control_snapshot() reads from the actor snapshot cache which
        // may not have an entry for this room yet. It falls back to sensible
        // defaults until the first command populates the actor state.
        let snapshot = RoomSnapshot::from_room(&room).await;
        let actor_state = Some(RoomActorState::from_room(&room).await);
        Self {
            room,
            state,
            latest_snapshot: snapshot,
            actor_state,
        }
    }

    pub fn room(&self) -> &Arc<Room> {
        &self.room
    }

    pub fn snapshot(&self) -> &RoomSnapshot {
        &self.latest_snapshot
    }

    /// Refresh snapshot from actor state (always the authority after command
    /// execution).  This replaces the old `refresh_snapshot` which read from
    /// Room's independent locks and could observe inconsistent combinations.
    pub fn refresh_snapshot_from_state(&mut self) {
        if let Some(ref actor_state) = self.actor_state {
            self.latest_snapshot = RoomSnapshot::from_actor_state(actor_state);
        }
    }

    /// Execute a command against the actor's owned state.
    /// All commands go through execute_with_actor which writes actor_state.
    /// After Phase 2 Work C, Room no longer holds state, so sync_to_room
    /// is removed. The snapshot cache is updated directly.
    pub(super) async fn execute_command(&mut self, command: RoomActorCommand) -> bool {
        use super::handler::RoomCommandHandler;
        use super::context::RoomCommandContext;

        if self.actor_state.is_none() {
            self.actor_state = Some(RoomActorState::from_room(&self.room).await);
        }

        let state_arc = Arc::clone(&self.state);
        let state: &crate::server::PlusServerState = &state_arc;
        let room = Arc::clone(&self.room);
        let ctx = RoomCommandContext::with_actor(
            state,
            room,
            self,
        );
        let result = RoomCommandHandler::execute_with_actor(ctx, &command).await;
        let should_stop = RoomCommandHandler::should_stop_room_mailbox(&command, &result);
        self.state.room_commands.observe_mailbox_result(&result);

        if result.is_ok() {
            self.refresh_snapshot_from_state();
            self.state.room_commands.store_snapshot_if_current(
                &self.room.id.to_string(),
                self.room.uuid,
                self.latest_snapshot.clone(),
            );
        }
        command.reply_with(result);
        should_stop
    }
}

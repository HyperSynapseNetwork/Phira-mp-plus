//! Room Actor — 每个房间一个 actor，持有房间状态快照。
//!
//! 迁移中：actor 正从 `RoomSnapshot` + `Room` 引用迁移到完整
//! `RoomActorState` 所有权。当前 actor 仍通过 `Room` 对象读取状态，
//! 但新代码应优先使用 `self.actor_state`。
//!
//! 迁移已完成部分：
//! - `RoomState` 定义：包含 control/lifecycle/members/chart/round/live
//! - `RoomActorState::from_room()` 直接持有这些字段
//! - `RoomActor::execute_command()` 入口：支持直接修改 actor_state

use super::command::RoomActorCommand;
use crate::room::{InternalRoomState, Room, RoomControlSnapshot};
use crate::server::PlusServerState;
use serde::{Deserialize, Serialize};
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
}

impl RoomSnapshot {
    /// 从 Room 对象构建快照。
    pub async fn from_room(room: &Room) -> Self {
        let control = room.control_snapshot();
        Self {
            room_id: room.id.to_string(),
            room_uuid: room.uuid.to_string(),
            locked: control.locked,
            cycle: control.cycle,
            host: control.host_id,
            hidden: control.hidden,
            live: room.is_live(),
            created_at: room.created_at,
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
    pub async fn from_room(room: &Room) -> Self {
        let control = room.control_snapshot();
        let lifecycle = room.state.read().await.clone();
        let users = room.users().await.iter().map(|u| u.id).collect();
        let monitors = room.monitors().await.iter().map(|u| u.id).collect();
        let chart = room.chart.read().await.as_ref().map(|c| c.id);
        let round_id = *room.current_round_id.read().await;
        Self {
            control,
            lifecycle,
            members: RoomMembers { users, monitors },
            chart,
            round: RoundInfo {
                round_id,
                round_uuid: None,
            },
            live: room.is_live(),
        }
    }

    /// 构建 RoomSnapshot（供外部只读路径使用）。
    pub fn to_snapshot(&self, room_id: &str, room_uuid: &str, created_at: i64) -> RoomSnapshot {
        RoomSnapshot {
            room_id: room_id.to_string(),
            room_uuid: room_uuid.to_string(),
            locked: self.control.locked,
            cycle: self.control.cycle,
            host: self.control.host_id,
            hidden: self.control.hidden,
            live: self.live,
            created_at,
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
}

impl RoomActorState {
    /// Snapshot the current Room state into an actor-owned copy.
    /// During migration, this is called after each command to sync
    /// from the shared Room into the actor's local state.
    pub async fn from_room(room: &Room) -> Self {
        let state = RoomState::from_room(room).await;
        Self {
            room_id: room.id.to_string(),
            room_uuid: room.uuid.to_string(),
            state,
            created_at: room.created_at,
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
        }
    }
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

    /// 刷新快照和 actor 状态（命令执行后调用）。
    pub async fn refresh_snapshot(&mut self) {
        self.latest_snapshot = RoomSnapshot::from_room(&self.room).await;
        self.actor_state = Some(RoomActorState::from_room(&self.room).await);
    }

    /// 从 actor_state 刷新快照（actor_state 为权威来源时使用）。
    pub fn refresh_snapshot_from_state(&mut self) {
        if let Some(ref actor_state) = self.actor_state {
            self.latest_snapshot = RoomSnapshot::from_actor_state(actor_state);
        }
    }

    /// Execute a command against the actor's owned state.
    /// Returns `true` if the mailbox should stop after this command.
    pub(super) async fn execute_command(&mut self, command: RoomActorCommand) -> bool {
        use super::handler::RoomCommandHandler;
        use super::context::RoomCommandContext;
        let gateway = Arc::clone(&self.state.room_commands);
        let state = Arc::clone(&self.state);
        let room = self.room.clone();
        let ctx = RoomCommandContext::with_actor(
            gateway.as_ref(),
            state.as_ref(),
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

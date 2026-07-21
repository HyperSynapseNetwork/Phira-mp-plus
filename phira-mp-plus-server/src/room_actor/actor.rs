//! Room Actor — 每个房间一个 actor，持有房间状态快照。
//!
//! 迁移中：actor 正从 `RoomSnapshot` + `Room` 引用迁移到完整
//! `RoomActorState` 所有权。当前 actor 仍通过 `Room` 对象读取状态，
//! 但新代码应优先使用 `self.actor_state`。

use crate::room::{InternalRoomState, Room};
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
}

/// Full actor-owned room state (migration target).
/// All room data lives here, not in the shared Room object.
#[derive(Debug, Clone)]
pub struct RoomActorState {
    pub room_id: String,
    pub room_uuid: String,
    pub control: crate::room::RoomControlSnapshot,
    pub lifecycle: InternalRoomState,
    pub users: Vec<i32>,
    pub monitors: Vec<i32>,
    pub chart: Option<i32>,
    pub current_round_id: Option<uuid::Uuid>,
    pub live: bool,
    pub created_at: i64,
}

impl RoomActorState {
    /// Snapshot the current Room state into an actor-owned copy.
    /// During migration, this is called after each command to sync
    /// from the shared Room into the actor's local state.
    pub async fn from_room(room: &Room) -> Self {
        let control = room.control_snapshot();
        let state = room.state.read().await;
        Self {
            room_id: room.id.to_string(),
            room_uuid: room.uuid.to_string(),
            control,
            lifecycle: state.clone(),
            users: room.users().await.iter().map(|u| u.id).collect(),
            monitors: room.monitors().await.iter().map(|u| u.id).collect(),
            chart: room.chart.read().await.as_ref().map(|c| c.id),
            current_round_id: *room.current_round_id.read().await,
            live: room.is_live(),
            created_at: room.created_at,
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
}

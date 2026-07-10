//! Room Actor — 每个房间一个 actor，持有房间状态快照。
//!
//! 当前作为读快照+写派发层：actor 持有 `RoomSnapshot` 和 `Room` 引用，
//! 快照在每次命令执行后更新。外部代码通过 gateway 获取快照，不再直接读 room fields。

use crate::room::Room;
use crate::server::PlusServerState;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// 房间状态的只读快照。
/// Actor 在每次命令执行后生成新快照，外部读路径使用快照而非直接访问 Room。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomSnapshot {
    pub room_id: String,
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
        Self {
            room_id: room.id.to_string(),
            locked: room.is_locked(),
            cycle: room.is_cycle(),
            host: room.host_id().await,
            hidden: room.is_hidden(),
            live: room.is_live(),
            created_at: room.created_at,
        }
    }
}

/// Room Actor — 每个房间一个，持有状态并处理命令。
pub struct RoomActor {
    room: Arc<Room>,
    state: Arc<PlusServerState>,
    latest_snapshot: RoomSnapshot,
}

impl RoomActor {
    pub async fn new(room: Arc<Room>, state: Arc<PlusServerState>) -> Self {
        let snapshot = RoomSnapshot::from_room(&room).await;
        Self {
            room,
            state,
            latest_snapshot: snapshot,
        }
    }

    pub fn room(&self) -> &Arc<Room> {
        &self.room
    }

    pub fn snapshot(&self) -> &RoomSnapshot {
        &self.latest_snapshot
    }

    /// 刷新快照（命令执行后调用）。
    pub async fn refresh_snapshot(&mut self) {
        self.latest_snapshot = RoomSnapshot::from_room(&self.room).await;
    }
}

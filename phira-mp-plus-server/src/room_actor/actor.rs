//! Room Actor — sole authoritative source for all mutable room state.
//!
//! The actor exclusively owns members, monitors, and live status. Room is
//! used purely as a broadcast/send bus for connection reference management.
//!
//! The actor updates its `latest_snapshot` after every command and stores it
//! in the gateway's snapshot cache for external readers.

use super::command::RoomActorCommand;
use crate::persistence::message::PersistenceEvent;
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
    pub persistent_empty: bool,
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
    /// 从 actor state 构建快照（权威路径）。
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
            persistent_empty: state.state.control.persistent_empty,
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
    pub chart_name: Option<String>,
    pub round: RoundInfo,
    pub live: bool,
    /// 准备倒计时开始时间（毫秒时间戳）。None 表示未启动倒计时。
    pub ready_countdown_started_at: Option<i64>,
    /// 对局超时截止时间（毫秒时间戳）。None 表示未启用超时或已超时。
    pub playing_timeout_deadline: Option<i64>,
}

impl RoomState {
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
            persistent_empty: self.control.persistent_empty,
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
}

/// Room Actor — 每个房间一个，持有状态并处理命令。
pub struct RoomActor {
    pub(super) room: Arc<Room>,
    pub(super) state: Arc<PlusServerState>,
    latest_snapshot: RoomSnapshot,
    /// Actor-owned state (always present).
    pub actor_state: RoomActorState,
}

impl RoomActor {
    pub fn new(room: Arc<Room>, state: Arc<PlusServerState>) -> Self {
        // Initialize actor state from Room fields (first-time population).
        let control = room.control_snapshot();
        let actor_state = RoomActorState::new(
            room.id.to_string(),
            room.uuid.to_string(),
            RoomState {
                control,
                lifecycle: InternalRoomState::SelectChart,
                members: RoomMembers {
                    users: Vec::new(),
                    monitors: Vec::new(),
                },
                chart: None,
                chart_name: None,
                round: RoundInfo {
                    round_id: None,
                    round_uuid: None,
                },
                live: false,
                ready_countdown_started_at: None,
                playing_timeout_deadline: None,
            },
            room.created_at,
        );
        let snapshot = RoomSnapshot::from_actor_state(&actor_state);
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
    /// execution).
    pub fn refresh_snapshot_from_state(&mut self) {
        self.latest_snapshot = RoomSnapshot::from_actor_state(&self.actor_state);
    }

    /// Execute a fire-and-forget telemetry command.
    /// 审计 P0: telemetry path is lighter — no snapshot refresh, no audit,
    /// no oneshot reply, no `finish_command`. Only updates `player_data`.
    pub(super) async fn execute_telemetry(&mut self, command: RoomActorCommand) {
        match command {
            RoomActorCommand::TelemetryTouches { room_id: _, user_id, touches } => {
                let entry = self.actor_state.player_data.entry(user_id).or_default();
                entry.push_touches(&touches);
            }
            RoomActorCommand::TelemetryJudges { room_id: _, user_id, judges } => {
                let entry = self.actor_state.player_data.entry(user_id).or_default();
                entry.push_judges(&judges);
            }
            _ => {}
        }
    }

    /// Execute a command against the actor's owned state.
    /// All commands go through execute_with_actor which writes actor_state.
    /// The snapshot cache is updated directly after execution.
    pub(super) async fn execute_command(&mut self, command: RoomActorCommand) -> bool {
        use super::handler::RoomCommandHandler;
        use super::context::RoomCommandContext;
        use super::lifecycle::DefaultRoomLifecycle;

        let state: &PlusServerState = &*self.state;
        let room = Arc::clone(&self.room);
        let lc = DefaultRoomLifecycle::new(room, state);
        let ctx = RoomCommandContext::new(&lc, &mut self.actor_state);
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
            // Enqueue a RoomSnapshot to the persistence worker for the
            // mp_room_snapshots table (P0-E audit).
            if let Ok(payload) = serde_json::to_value(&self.latest_snapshot) {
                let _ = self
                    .state
                    .persistence_worker
                    .enqueue(PersistenceEvent::RoomSnapshot {
                        room_id: self.room.id.to_string(),
                        payload: Arc::new(payload),
                    })
                    .await;
            }
        }
        command.reply_with(result);
        should_stop
    }
}

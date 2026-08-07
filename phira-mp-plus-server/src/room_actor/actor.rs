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
use tracing::warn;
use std::collections::HashMap;
use std::sync::Arc;

/// Debounce interval (ms) for RoomSnapshot persistence enqueues.
/// After a state change, a debounce timer is started. If no further
/// state change occurs within this interval, the snapshot is enqueued.
const ROOM_SNAPSHOT_DEBOUNCE_MS: u64 = 500;

/// 房间状态的只读快照。
/// Actor 在每次命令执行后生成新快照，外部读路径使用快照而非直接访问 Room。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomSnapshot {
    pub room_id: String,
    pub room_uuid: String,
    pub locked: bool,
    pub cycle: bool,
    pub tournament: bool,
    pub host: Option<i32>,
    pub system_host: bool,
    pub hidden: bool,
    pub live: bool,
    pub created_at: i64,
    pub persistent_empty: bool,
    /// Chart id, if one is selected (actor-authoritative).
    pub chart: Option<i32>,
    /// Chart name, if one is selected (actor-authoritative).
    pub chart_name: Option<String>,
    /// The room lifecycle state as a stripped enum (actor-authoritative).
    pub stripped: phira_mp_common::StrippedRoomState,
    /// Current round id, if a round is active (actor-authoritative).
    pub round_id: Option<uuid::Uuid>,
    /// IDs of users who have readied up (actor-authoritative, only meaningful in WaitForReady).
    pub ready_set: Option<Vec<i32>>,
    /// Keys of the results map — user IDs who submitted results (finished playing).
    pub results_keys: Vec<i32>,
    /// User IDs who aborted the round.
    pub aborted_users: Vec<i32>,
    /// User IDs who are still playing (in members but not finished or aborted).
    pub playing_users: Vec<i32>,
    /// Actor-authoritative member lists (actor-state members, not Room connection refs).
    pub members: RoomMembers,
    /// PMP45 P0-K: degraded 标志——Join 补偿失败（Ghost member 待清理）时房间
    /// 不再接受新的 Join，直到操作员 / 未来 reconcile 清空。`#[serde(default)]`
    /// 保证旧的持久化快照可解析。
    #[serde(default)]
    pub degraded: bool,
}

impl RoomSnapshot {
    /// 从 actor state 构建快照（权威路径）。
    pub fn from_actor_state(state: &RoomActorState) -> Self {
        Self {
            room_id: state.room_id.clone(),
            room_uuid: state.room_uuid.clone(),
            locked: state.state.control.locked,
            cycle: state.state.control.cycle,
            tournament: state.state.control.tournament,
            host: state.state.control.host_id,
            system_host: state.state.control.system_host,
            hidden: state.state.control.hidden,
            live: state.state.live,
            created_at: state.created_at,
            persistent_empty: state.state.control.persistent_empty,
            chart: state.state.chart,
            chart_name: state.state.chart_name.clone(),
            stripped: state.state.lifecycle.stripped(),
            round_id: state.state.round.round_id,
            ready_set: match &state.state.lifecycle {
                InternalRoomState::WaitForReady { started, .. } => {
                    Some(started.iter().copied().collect())
                }
                _ => None,
            },
            results_keys: match &state.state.lifecycle {
                InternalRoomState::Playing { results, .. } => {
                    results.keys().copied().collect()
                }
                _ => Vec::new(),
            },
            aborted_users: match &state.state.lifecycle {
                InternalRoomState::Playing { aborted, .. } => {
                    aborted.iter().copied().collect()
                }
                _ => Vec::new(),
            },
            playing_users: match &state.state.lifecycle {
                InternalRoomState::Playing { results, aborted } => {
                    state.state.members.users.iter()
                        .filter(|u| !results.contains_key(u) && !aborted.contains(u))
                        .copied()
                        .collect()
                }
                _ => Vec::new(),
            },
            members: state.state.members.clone(),
            degraded: state.state.degraded,
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
    /// PMP45 P0-K: degraded 标志——Join 补偿失败（Ghost member 待清理）时置
    /// true，AddUser 将拒绝新的 Join，直到被显式清空（操作员 / 未来 reconcile）。
    pub degraded: bool,
    /// 准备倒计时开始时间（毫秒时间戳）。None 表示未启动倒计时。
    pub ready_countdown_started_at: Option<i64>,
    /// 对局超时截止时间（毫秒时间戳）。None 表示未启用超时或已超时。
    pub playing_timeout_deadline: Option<i64>,
    /// 当前谱面时长（秒）。选谱时异步解析写入，结算时清空（PMP48：谱面
    /// 时时可能更新，不长期缓存——每次选谱解析，每轮结算后释放）。
    pub chart_duration: Option<f64>,
    /// 本轮游玩开始时间（毫秒时间戳）。进入 Playing 时设置，结算/超时清除。
    /// 进度通知按谱面时长 + 该时间计算进度百分比与剩余分钟（deadline 会被
    /// 首个玩家完成时延长，不能反推开始时间）。
    pub playing_started_at: Option<i64>,
    /// 进度通知订阅者：user_id → 上次通知时间（毫秒）。加入游玩中房间时
    /// 注册，轮次结束或用户离开时移除；mailbox 周期维护每 30 秒推送一次。
    pub progress_subscribers: HashMap<i32, i64>,
    /// PMP46 Blocker 2: Room Actor 权威状态事件序号。每次权威状态变更递增；
    /// `BindAndSnapshot` 返回快照时刻的该序号作为 `snapshot_seq`。发往 Session
    /// Gate 的状态事件携带它，认证激活时 `room_seq <= snapshot_seq` 才可剔除。
    /// Room Actor 序号与 Gate 自身序号是两个无关数字，绝不能用 Gate 序号对齐
    /// 快照（audit §7）。
    pub room_event_seq: u64,
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
            system_host: self.control.system_host,
            hidden: self.control.hidden,
            live: self.live,
            created_at,
            persistent_empty: self.control.persistent_empty,
            chart: self.chart,
            chart_name: self.chart_name.clone(),
            stripped: self.lifecycle.stripped(),
            round_id: self.round.round_id,
            ready_set: match &self.lifecycle {
                InternalRoomState::WaitForReady { started, .. } => {
                    Some(started.iter().copied().collect())
                }
                _ => None,
            },
            results_keys: match &self.lifecycle {
                InternalRoomState::Playing { results, .. } => {
                    results.keys().copied().collect()
                }
                _ => Vec::new(),
            },
            aborted_users: match &self.lifecycle {
                InternalRoomState::Playing { aborted, .. } => {
                    aborted.iter().copied().collect()
                }
                _ => Vec::new(),
            },
            playing_users: match &self.lifecycle {
                InternalRoomState::Playing { results, aborted } => {
                    self.members.users.iter()
                        .filter(|u| !results.contains_key(u) && !aborted.contains(u))
                        .copied()
                        .collect()
                }
                _ => Vec::new(),
            },
            members: self.members.clone(),
            degraded: self.degraded,
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

    /// PMP46 Blocker 2: 权威状态事件序号递增（返回新值）。每次权威状态变更前
    /// 调用——`BindAndSnapshot` 返回快照时刻的该序号作为 `snapshot_seq`；发往
    /// Session Gate 的状态事件以它打戳，认证激活时 `room_seq <= snapshot_seq`
    /// 才剔除（快照已包含），快照点之后的事件绝不误删（audit §7.5）。
    pub fn bump_room_event_seq(&mut self) -> u64 {
        self.room_event_seq += 1;
        self.room_event_seq
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
    /// Handle for the debounce timer that enqueues the RoomSnapshot for
    /// persistence. Cancelled and replaced on every state mutation so that
    /// rapid-fire commands produce at most one enqueue 500ms after the last change.
    snapshot_debounce_handle: Option<tokio::task::JoinHandle<()>>,
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
                degraded: false,
                ready_countdown_started_at: None,
                playing_timeout_deadline: None,
                chart_duration: None,
                playing_started_at: None,
                progress_subscribers: HashMap::new(),
                room_event_seq: 0,
            },
            room.created_at,
        );
        let snapshot = RoomSnapshot::from_actor_state(&actor_state);
        Self {
            room,
            state,
            latest_snapshot: snapshot,
            actor_state,
            snapshot_debounce_handle: None,
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

        let room = Arc::clone(&self.room);
        let lc = DefaultRoomLifecycle::new(room, Arc::clone(&self.state));
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
            // P1-E: Debounced RoomSnapshot persistence — cancel any previous
            // debounce timer, then schedule a new one that waits 500ms before
            // enqueuing. This ensures rapid-fire state mutations produce at
            // most one enqueue after the last change settles, without flooding
            // the persistence worker.
            if let Some(handle) = self.snapshot_debounce_handle.take() {
                handle.abort();
            }
            if self.actor_state.state.control.persistent_empty {
                let persistence = self.state.persistence_worker.clone();
                let room_id = self.room.id.to_string();
                let snapshot = self.latest_snapshot.clone();
                self.snapshot_debounce_handle = Some(tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(ROOM_SNAPSHOT_DEBOUNCE_MS)).await;
                    let room_id_for_msg = room_id.clone();
                    if let Ok(payload) = serde_json::to_value(&snapshot) {
                        if let Err(e) = persistence
                            .enqueue(PersistenceEvent::RoomSnapshot {
                                room_id,
                                payload: Arc::new(payload),
                            })
                            .await
                        {
                            warn!(
                                room = %room_id_for_msg,
                                kind = %e.kind(),
                                "debounced room snapshot enqueue failed"
                            );
                        }
                    }
                }));
            }
        }
        command.reply_with(result);
        should_stop
    }
}

//! Phira-mp+ 房间管理
//!
//! 增强的房间管理，集成插件事件系统。记录每轮游玩结算数据，
//! 支持查询历史、转移房主等管理功能。

use crate::plugin::{PluginEvent, PluginManager};
use crate::server::Chart;
use anyhow::{Result, bail};
use phira_mp_common::{ClientRoomState, Message, RoomId, RoomState, ServerCommand};
use rand::seq::IndexedRandom;
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    ops::Deref,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::RwLock;
use tracing::{debug, info};

#[derive(Default, Debug)]
pub enum InternalRoomState {
    #[default]
    SelectChart,
    WaitForReady {
        started: HashSet<i32>,
    },
    Playing {
        results: HashMap<i32, crate::server::Record>,
        aborted: HashSet<i32>,
    },
}

impl InternalRoomState {
    pub fn to_client(&self, chart: Option<i32>) -> RoomState {
        match self {
            Self::SelectChart => RoomState::SelectChart(chart),
            Self::WaitForReady { .. } => RoomState::WaitingForReady,
            Self::Playing { .. } => RoomState::Playing,
        }
    }
}

/// 一轮游玩的结算数据
#[derive(Debug, Clone, Serialize)]
pub struct PlayRound {
    /// 本轮唯一标识符
    pub round_id: uuid::Uuid,
    pub chart_id: i32,
    pub chart_name: String,
    pub results: Vec<PlayResult>,
}

/// 单个用户的游玩结算
#[derive(Debug, Clone, Serialize)]
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

pub struct Room {
    pub id: RoomId,
    /// 房间唯一标识符
    pub uuid: uuid::Uuid,
    /// 用于触发 RoundComplete 事件的插件管理器
    pub plugin_manager: Option<Arc<PluginManager>>,
    pub host: RwLock<Weak<super::session::User>>,
    pub state: RwLock<InternalRoomState>,

    pub live: AtomicBool,
    pub locked: AtomicBool,
    pub cycle: AtomicBool,

    pub users: RwLock<Vec<Weak<super::session::User>>>,
    pub monitors: RwLock<Vec<Weak<super::session::User>>>,
    pub chart: RwLock<Option<Chart>>,

    /// 历史游玩记录（不持久化，房间解散即清除）
    pub play_history: RwLock<Vec<PlayRound>>,
    /// 当前轮次 ID（游戏开始时生成，结算时使用）
    pub current_round_id: RwLock<Option<uuid::Uuid>>,

    /// 房间最大玩家数（来自服务器配置或默认值）
    pub max_users: usize,
}

impl Room {
    pub fn new(id: RoomId, host: Weak<super::session::User>, plugin_manager: Option<Arc<PluginManager>>, max_users: usize) -> Self {
        Self {
            id,
            host: host.clone().into(),
            state: RwLock::default(),

            live: AtomicBool::new(false),
            locked: AtomicBool::new(false),
            cycle: AtomicBool::new(false),

            users: vec![host].into(),
            monitors: Vec::new().into(),
            chart: RwLock::default(),
            uuid: uuid::Uuid::new_v4(),
            play_history: RwLock::new(Vec::new()),
            current_round_id: RwLock::new(None),
            plugin_manager,
            max_users,
        }
    }

    // ── 查询方法 ──

    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::SeqCst)
    }

    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::SeqCst)
    }

    pub fn is_cycle(&self) -> bool {
        self.cycle.load(Ordering::SeqCst)
    }

    /// 获取房主用户 ID
    pub async fn host_id(&self) -> Option<i32> {
        self.host.read().await.upgrade().map(|u| u.id)
    }

    /// 获取当前谱面 ID
    pub async fn chart_id(&self) -> Option<i32> {
        self.chart.read().await.as_ref().map(|c| c.id)
    }

    /// 获取当前谱面名称
    pub async fn chart_name(&self) -> Option<String> {
        self.chart.read().await.as_ref().map(|c| c.name.clone())
    }

    pub async fn client_room_state(&self) -> RoomState {
        self.state
            .read()
            .await
            .to_client(self.chart.read().await.as_ref().map(|it| it.id))
    }

    pub async fn client_state(&self, user: &super::session::User) -> ClientRoomState {
        ClientRoomState {
            id: self.id.clone(),
            state: self.client_room_state().await,
            live: self.is_live(),
            locked: self.is_locked(),
            cycle: self.is_cycle(),
            is_host: self.check_host(user).await.is_ok(),
            is_ready: matches!(&*self.state.read().await, InternalRoomState::WaitForReady { started } if started.contains(&user.id)),
            users: self
                .users
                .read()
                .await
                .iter()
                .chain(self.monitors.read().await.iter())
                .filter_map(|it| it.upgrade().map(|it| (it.id, it.to_info())))
                .collect(),
        }
    }

    pub async fn on_state_change(&self) {
        self.broadcast(ServerCommand::ChangeState(self.client_room_state().await))
            .await;
    }

    // ── 用户管理 ──

    pub async fn add_user(&self, user: Weak<super::session::User>, monitor: bool) -> bool {
        if monitor {
            let mut guard = self.monitors.write().await;
            guard.retain(|it| it.strong_count() > 0);
            guard.push(user);
            true
        } else {
            let mut guard = self.users.write().await;
            guard.retain(|it| it.strong_count() > 0);
            if guard.len() >= self.max_users {
                false
            } else {
                guard.push(user);
                true
            }
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

    pub async fn check_host(&self, user: &super::session::User) -> Result<()> {
        if self.host.read().await.upgrade().map(|it| it.id) != Some(user.id) {
            bail!("only host can do this");
        }
        Ok(())
    }

    /// 转移房主
    pub async fn transfer_host(&self, new_host_id: i32) -> Result<()> {
        // 通知旧房主
        if let Some(old_host) = self.host.read().await.upgrade() {
            if old_host.id != new_host_id {
                old_host.try_send(ServerCommand::ChangeHost(false)).await;
            }
        }
        let user = self
            .users()
            .await
            .into_iter()
            .find(|u| u.id == new_host_id)
            .ok_or_else(|| anyhow::anyhow!("user not in room"))?;
        let weak = Arc::downgrade(&user);
        *self.host.write().await = weak;
        self.send(Message::NewHost { user: new_host_id }).await;
        user.try_send(ServerCommand::ChangeHost(true)).await;
        Ok(())
    }

    // ── 消息广播 ──

    #[inline]
    pub async fn send(&self, msg: Message) {
        self.broadcast(ServerCommand::Message(msg)).await;
    }

    pub async fn broadcast(&self, cmd: ServerCommand) {
        debug!("broadcast {cmd:?}");
        for session in self
            .users()
            .await
            .into_iter()
            .chain(self.monitors().await.into_iter())
        {
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

    /// Return: should the room be dropped
    #[must_use]
    pub async fn on_user_leave(&self, user: &super::session::User) -> bool {
        self.send(Message::LeaveRoom {
            user: user.id,
            name: user.name.clone(),
        })
        .await;
        *user.room.write().await = None;
        (if user.monitor.load(Ordering::SeqCst) {
            &self.monitors
        } else {
            &self.users
        })
        .write()
        .await
        .retain(|it| it.upgrade().is_some_and(|it| it.id != user.id));
        if self.check_host(user).await.is_ok() {
            info!("host disconnected!");
            let users = self.users().await;
            if users.is_empty() {
                info!("room users all disconnected, dropping room");
                return true;
            } else {
                let user = users.choose(&mut rand::rng()).unwrap();
                debug!("selected {} as host", user.id);
                *self.host.write().await = Arc::downgrade(user);
                self.send(Message::NewHost { user: user.id }).await;
                user.try_send(ServerCommand::ChangeHost(true)).await;
            }
        }
        self.check_all_ready().await;
        false
    }

    // ── 游戏流程 ──

    pub async fn reset_game_time(&self) {
        for user in self.users().await {
            user.game_time
                .store(f32::NEG_INFINITY.to_bits(), Ordering::SeqCst);
        }
    }

    /// 保存当前轮次的结算数据到历史记录
    async fn save_round_history(&self) {
        let round_id = self.current_round_id.read().await.unwrap_or(uuid::Uuid::nil());
        let (chart_id, chart_name, results, aborted) = {
            let guard = self.state.read().await;
            match guard.deref() {
                InternalRoomState::Playing { results, aborted } => {
                    let cid = self.chart.read().await.as_ref().map(|c| (c.id, c.name.clone()));
                    let (cid, cn) = match cid {
                        Some((id, name)) => (id, name),
                        None => return, // 没有谱面信息，跳过
                    };
                    let results = results.clone();
                    let aborted = aborted.clone();
                    (cid, cn, results, aborted)
                }
                _ => return,
            }
        };

        // 收集用户名
        let users_map: HashMap<i32, String> = self
            .users()
            .await
            .into_iter()
            .map(|u| (u.id, u.name.clone()))
            .collect();

        let mut play_results = Vec::new();
        for (uid, rec) in &results {
            play_results.push(PlayResult {
                user_id: *uid,
                user_name: users_map.get(uid).cloned().unwrap_or_else(|| format!("{}", uid)),
                score: rec.score,
                accuracy: rec.accuracy,
                perfect: rec.perfect,
                good: rec.good,
                bad: rec.bad,
                miss: rec.miss,
                max_combo: rec.max_combo,
                full_combo: rec.full_combo,
                aborted: false,
                std_score: rec.std_score,
            });
        }
        for uid in &aborted {
            if !results.contains_key(uid) {
                play_results.push(PlayResult {
                    user_id: *uid,
                    user_name: users_map.get(uid).cloned().unwrap_or_else(|| format!("{}", uid)),
                    score: 0,
                    accuracy: 0.0,
                    perfect: 0,
                    good: 0,
                    bad: 0,
                    miss: 0,
                    max_combo: 0,
                    full_combo: false,
                    aborted: true,
                    std_score: 0.0,
                });
            }
        }

        let round = PlayRound {
            round_id,
            chart_id,
            chart_name,
            results: play_results,
        };

        self.play_history.write().await.push(round);
        info!(
            room = self.id.to_string(),
            "saved play round history (total {})",
            self.play_history.read().await.len()
        );
    }

    pub async fn check_all_ready(&self) {
        let guard = self.state.read().await;
        match guard.deref() {
            InternalRoomState::WaitForReady { started } => {
                if self
                    .users()
                    .await
                    .into_iter()
                    .chain(self.monitors().await.into_iter())
                    .all(|it| started.contains(&it.id))
                {
                    drop(guard);
                    let round_id = uuid::Uuid::new_v4();
                    *self.current_round_id.write().await = Some(round_id);
                    info!(room = self.id.to_string(), round = %round_id, "game start");
                    self.send(Message::StartPlaying).await;
                    self.reset_game_time().await;
                    *self.state.write().await = InternalRoomState::Playing {
                        results: HashMap::new(),
                        aborted: HashSet::new(),
                    };
                    self.on_state_change().await;
                }
            }
            InternalRoomState::Playing { results, aborted } => {
                if self
                    .users()
                    .await
                    .into_iter()
                    .all(|it| results.contains_key(&it.id) || aborted.contains(&it.id))
                {
                    drop(guard);
                    // 保存历史记录
                    let rid = *self.current_round_id.read().await;
                    self.save_round_history().await;

                    if let Some(rid) = rid {
                        info!("round complete: {}", rid);
                    }

                    // 触发 RoundComplete 事件
                    if let Some(pm) = &self.plugin_manager {
                        let chart = self.chart.read().await;
                        let (cid, cn) = chart.as_ref().map(|c| (c.id, c.name.clone())).unwrap_or((0, "?".into()));
                        pm.trigger(&PluginEvent::RoundComplete {
                            room_id: self.id.to_string(),
                            chart_id: cid,
                            chart_name: cn,
                        }).await;
                    }

                    self.send(Message::GameEnd).await;
                    *self.state.write().await = InternalRoomState::SelectChart;
                    if self.is_cycle() {
                        debug!(room = self.id.to_string(), "cycling");
                        let host = Weak::clone(&*self.host.read().await);
                        let new_host = {
                            let users = self.users().await;
                            let index = users
                                .iter()
                                .position(|it| host.ptr_eq(&Arc::downgrade(it)))
                                .map(|it| (it + 1) % users.len())
                                .unwrap_or_default();
                            users.into_iter().nth(index).unwrap()
                        };
                        *self.host.write().await = Arc::downgrade(&new_host);
                        self.send(Message::NewHost { user: new_host.id }).await;
                        if let Some(old) = host.upgrade() {
                            old.try_send(ServerCommand::ChangeHost(false)).await;
                        }
                        new_host.try_send(ServerCommand::ChangeHost(true)).await;
                    }
                    self.on_state_change().await;
                }
            }
            _ => {}
        }
    }
}

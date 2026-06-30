//! Room state, gameplay lifecycle, and live telemetry.

use crate::plugin::{JudgeEventItem, PluginEvent, PluginManager, TouchEventPoint};
use crate::server::Chart;
use anyhow::{bail, Result};
use phira_mp_common::{
    ClientRoomState, Message, PartialRoomData, RoomEvent, RoomId, RoomState, RoundData,
    ServerCommand, StrippedRoomState,
};
use rand::seq::IndexedRandom;
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    ops::Deref,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Weak,
    },
};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

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

    fn stripped(&self) -> StrippedRoomState {
        match self {
            Self::SelectChart => StrippedRoomState::SelectingChart,
            Self::WaitForReady { .. } => StrippedRoomState::WaitingForReady,
            Self::Playing { .. } => StrippedRoomState::Playing,
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

fn protocol_round(round: &PlayRound) -> RoundData {
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

pub struct Room {
    pub id: RoomId,
    /// 房间唯一标识符
    pub uuid: uuid::Uuid,
    /// 用于触发 RoundComplete 事件的插件管理器
    pub plugin_manager: Option<Arc<PluginManager>>,
    server: Weak<crate::server::PlusServerState>,
    pub host: RwLock<Weak<super::session::User>>,
    pub state: RwLock<InternalRoomState>,

    pub live: AtomicBool,
    pub locked: AtomicBool,
    pub cycle: AtomicBool,
    hidden: AtomicBool,
    persistent_empty: AtomicBool,
    system_host: AtomicBool,
    phira_api_endpoint: RwLock<Option<String>>,
    display_names: RwLock<HashMap<i32, String>>,
    admin_start_pending: AtomicBool,

    pub users: RwLock<Vec<Weak<super::session::User>>>,
    pub monitors: RwLock<Vec<Weak<super::session::User>>>,
    pub chart: RwLock<Option<Chart>>,

    /// 历史游玩记录（不持久化，房间解散即清除）
    pub play_history: RwLock<Vec<PlayRound>>,
    /// 当前轮次 ID（游戏开始时生成，结算时使用）
    pub current_round_id: RwLock<Option<uuid::Uuid>>,

    /// 房间最大玩家数（来自服务器配置或默认值）
    pub max_users: AtomicUsize,

    /// 各玩家实时触控/判定数据缓存（供插件 WASM host API 查询）
    pub player_data: RwLock<HashMap<i32, PlayerLiveData>>,

    /// 轮次数据持久化存储。
    pub round_store: Option<Arc<crate::round_store::RoundStore>>,

    /// 房间创建时间戳（Unix 毫秒）
    pub created_at: i64,
}

/// 房间名以前缀 `-`（兼容旧版 `+-`）开头时默认隐藏。
pub fn room_id_is_hidden(room_id: &str) -> bool {
    room_id.starts_with('-') || room_id.starts_with("+-")
}

impl Room {
    pub fn new(
        id: RoomId,
        host: Weak<super::session::User>,
        plugin_manager: Option<Arc<PluginManager>>,
        server: Weak<crate::server::PlusServerState>,
        max_users: usize,
        round_store: Option<Arc<crate::round_store::RoundStore>>,
    ) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let hidden = room_id_is_hidden(&id.to_string());
        Self {
            id,
            host: host.clone().into(),
            state: RwLock::default(),

            live: AtomicBool::new(false),
            locked: AtomicBool::new(false),
            cycle: AtomicBool::new(false),
            hidden: AtomicBool::new(hidden),
            persistent_empty: AtomicBool::new(false),
            system_host: AtomicBool::new(false),
            phira_api_endpoint: RwLock::new(None),
            display_names: RwLock::new(HashMap::new()),
            admin_start_pending: AtomicBool::new(false),

            users: vec![host].into(),
            monitors: Vec::new().into(),
            chart: RwLock::default(),
            uuid: uuid::Uuid::new_v4(),
            play_history: RwLock::new(Vec::new()),
            current_round_id: RwLock::new(None),
            plugin_manager,
            server,
            max_users: AtomicUsize::new(max_users),
            player_data: RwLock::new(HashMap::new()),
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
        room.persistent_empty.store(true, Ordering::SeqCst);
        room
    }

    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::SeqCst)
    }

    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::SeqCst)
    }

    pub fn is_cycle(&self) -> bool {
        self.cycle.load(Ordering::SeqCst)
    }

    pub fn is_hidden(&self) -> bool {
        self.hidden.load(Ordering::SeqCst)
    }

    pub fn set_hidden(&self, hidden: bool) {
        self.hidden.store(hidden, Ordering::SeqCst);
    }

    pub fn is_persistent_empty(&self) -> bool {
        self.persistent_empty.load(Ordering::SeqCst)
    }

    pub fn set_persistent_empty(&self, persistent: bool) {
        self.persistent_empty.store(persistent, Ordering::SeqCst);
    }

    pub async fn set_display_name(&self, user_id: i32, name: String) {
        self.display_names.write().await.insert(user_id, name);
    }

    pub async fn display_name(&self, user: &super::session::User) -> String {
        self.display_names
            .read()
            .await
            .get(&user.id)
            .cloned()
            .unwrap_or_else(|| user.name.clone())
    }

    pub(crate) fn display_name_sync(&self, user: &super::session::User) -> String {
        self.display_names
            .try_read()
            .ok()
            .and_then(|names| names.get(&user.id).cloned())
            .unwrap_or_else(|| user.name.clone())
    }

    pub async fn phira_api_endpoint_override(&self) -> Option<String> {
        self.phira_api_endpoint.read().await.clone()
    }

    pub async fn set_phira_api_endpoint_override(&self, endpoint: Option<String>) {
        *self.phira_api_endpoint.write().await = endpoint;
    }

    pub async fn effective_phira_api_endpoint(&self, server: &crate::server::PlusServerState) -> String {
        self.phira_api_endpoint
            .read()
            .await
            .clone()
            .unwrap_or_else(|| server.config.phira_api_endpoint.clone())
    }

    pub(crate) fn phira_api_endpoint_override_sync(&self) -> Option<String> {
        self.phira_api_endpoint.try_read().ok().and_then(|guard| guard.clone())
    }

    pub(crate) fn effective_phira_api_endpoint_sync(&self, fallback: &str) -> String {
        match self.phira_api_endpoint.try_read() {
            Ok(guard) => guard.clone().unwrap_or_else(|| fallback.to_string()),
            Err(_) => fallback.to_string(),
        }
    }

    /// 获取房主用户 ID
    pub async fn host_id(&self) -> Option<i32> {
        self.host.read().await.upgrade().map(|u| u.id)
    }

    pub fn is_system_host(&self) -> bool {
        self.system_host.load(Ordering::SeqCst)
    }

    pub async fn has_host(&self) -> bool {
        self.is_system_host() || self.host.read().await.upgrade().is_some()
    }

    /// 获取房间最大玩家数
    pub fn max_users_count(&self) -> usize {
        self.max_users.load(Ordering::Relaxed)
    }

    /// 设置房间最大玩家数
    pub fn set_max_users(&self, n: usize) {
        self.max_users.store(n, Ordering::Relaxed);
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
        let users = self
            .users()
            .await
            .into_iter()
            .chain(self.monitors().await)
            .map(|user| (user.id, user.to_info()))
            .collect();

        ClientRoomState {
            id: self.id.clone(),
            state: self.client_room_state().await,
            live: self.is_live(),
            locked: self.is_locked(),
            cycle: self.is_cycle(),
            is_host: self.check_host(user).await.is_ok(),
            is_ready: matches!(&*self.state.read().await, InternalRoomState::WaitForReady { started } if started.contains(&user.id)),
            users,
        }
    }

    /// 转换为 RoomData（供 monitor 协议使用）
    pub async fn into_data(room: &Arc<Self>) -> phira_mp_common::RoomData {
        let host = room.host.read().await.upgrade().map(|u| u.id).unwrap_or(-1);
        let users: Vec<i32> = room.users().await.into_iter().map(|u| u.id).collect();
        let chart = room.chart.read().await.as_ref().map(|c| c.id);
        let state = room.state.read().await.stripped();
        let rounds = room
            .play_history
            .read()
            .await
            .iter()
            .map(protocol_round)
            .collect();
        phira_mp_common::RoomData {
            host,
            users,
            lock: room.locked.load(Ordering::SeqCst),
            cycle: room.cycle.load(Ordering::SeqCst),
            chart,
            state,
            rounds,
        }
    }

    pub(crate) async fn publish_update(&self, data: PartialRoomData) {
        if let Some(server) = self.server.upgrade() {
            server
                .publish_room_event(RoomEvent::UpdateRoom {
                    room: self.id.clone(),
                    data,
                })
                .await;
        }
    }

    pub async fn on_state_change(&self) {
        self.broadcast(ServerCommand::ChangeState(self.client_room_state().await))
            .await;
        let state = self.state.read().await.stripped();
        self.publish_update(PartialRoomData {
            state: Some(state),
            ..Default::default()
        })
        .await;
    }

    /// 存储玩家的触控帧数据（供 WASM 插件 host API 查询）
    pub async fn store_player_touches(&self, user_id: i32, data: &[TouchEventPoint]) {
        if data.is_empty() { return; }
        let mut guard = self.player_data.write().await;
        guard.entry(user_id).or_default().push_touches(data);
    }

    /// 存储玩家的判定事件数据（供 WASM 插件 host API 查询）
    pub async fn store_player_judges(&self, user_id: i32, data: &[JudgeEventItem]) {
        if data.is_empty() { return; }
        let mut guard = self.player_data.write().await;
        guard.entry(user_id).or_default().push_judges(data);
    }

    /// 获取玩家的触控数据（WASM host API 用）
    pub async fn get_player_touches(&self, user_id: i32) -> Vec<TouchEventPoint> {
        self.player_data.read().await.get(&user_id)
            .map(|d| d.touches.clone())
            .unwrap_or_default()
    }

    /// 获取玩家的判定数据（WASM host API 用）
    pub async fn get_player_judges(&self, user_id: i32) -> Vec<JudgeEventItem> {
        self.player_data.read().await.get(&user_id)
            .map(|d| d.judges.clone())
            .unwrap_or_default()
    }

    pub async fn add_user(&self, user: Weak<super::session::User>, monitor: bool) -> bool {
        if monitor {
            let mut guard = self.monitors.write().await;
            guard.retain(|it| it.strong_count() > 0);
            guard.push(user);
            true
        } else {
            let mut guard = self.users.write().await;
            guard.retain(|it| it.strong_count() > 0);
            if guard.len() >= self.max_users.load(Ordering::Relaxed) {
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
            players.retain(|entry| entry.strong_count() > 0 && entry.as_ptr() as usize != target_ptr);
        }
        {
            let mut monitors = self.monitors.write().await;
            monitors.retain(|entry| entry.strong_count() > 0 && entry.as_ptr() as usize != target_ptr);
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
        self.live.store(active, Ordering::SeqCst);
        active
    }

    pub async fn check_host(&self, user: &super::session::User) -> Result<()> {
        if self.host.read().await.upgrade().map(|it| it.id) != Some(user.id) {
            bail!("only host can do this");
        }
        Ok(())
    }

    pub fn admin_start_pending(&self) -> bool {
        self.admin_start_pending.load(Ordering::SeqCst)
    }

    /// Start a round from the administrative console without bypassing client loading.
    ///
    /// The official client assumes that the room host has already downloaded the chart
    /// when it receives `WaitingForReady`. An administrative start has no such client-side
    /// preparation step, so the host is temporarily presented as a regular player until it
    /// reports `Ready`. The real server-side host never changes.
    pub async fn begin_admin_start(&self) -> Result<()> {
        if self
            .admin_start_pending
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            bail!("administrative start is already in progress");
        }

        let result: Result<()> = async {
            if !matches!(*self.state.read().await, InternalRoomState::SelectChart) {
                bail!("room is not selecting a chart");
            }
            if self.chart.read().await.is_none() {
                bail!("no chart selected");
            }

            // Message::SelectChart is informational in the official client. It learns the
            // active chart id from SelectChart(Some(id)) in ChangeState.
            self.on_state_change().await;

            let host = self
                .host
                .read()
                .await
                .upgrade()
                .ok_or_else(|| anyhow::anyhow!("room host disconnected"))?;
            host.try_send(ServerCommand::ChangeHost(false)).await;

            self.reset_game_time().await;
            self.send(Message::GameStart { user: 0 }).await;
            self.send(Message::Chat {
                user: 0,
                content: "服务器已发起游戏，请加载谱面并点击准备".to_string(),
            })
            .await;
            *self.state.write().await = InternalRoomState::WaitForReady {
                started: HashSet::new(),
            };
            self.on_state_change().await;
            self.check_all_ready().await;
            Ok(())
        }
        .await;

        if result.is_err() {
            self.admin_start_pending.store(false, Ordering::SeqCst);
        }
        result
    }

    /// Restore the client's host flag after an administrative start completes or is cancelled.
    pub async fn finish_admin_start(&self) {
        if !self.admin_start_pending.swap(false, Ordering::SeqCst) {
            return;
        }
        if let Some(host) = self.host.read().await.upgrade() {
            host.try_send(ServerCommand::ChangeHost(true)).await;
        }
    }

    pub async fn transfer_host(&self, new_host_id: i32) -> Result<()> {
        self.set_host(Some(new_host_id), true).await
    }

    /// 设置房主。`None` 表示显式设置为系统 `?` 房主；这种状态不会被后续加入者自动接管。
    pub async fn set_host(&self, new_host_id: Option<i32>, announce: bool) -> Result<()> {
        let old_host = self.host.read().await.upgrade();
        match new_host_id {
            Some(new_host_id) => {
                let user = self
                    .users()
                    .await
                    .into_iter()
                    .find(|u| u.id == new_host_id)
                    .ok_or_else(|| anyhow::anyhow!("user not in room"))?;
                let old_id = old_host.as_ref().map(|u| u.id);
                if old_id != Some(new_host_id) {
                    if let Some(old_host) = old_host {
                        old_host.try_send(ServerCommand::ChangeHost(false)).await;
                    }
                    if announce {
                        self.send(Message::NewHost { user: new_host_id }).await;
                    }
                }
                self.system_host.store(false, Ordering::SeqCst);
                *self.host.write().await = Arc::downgrade(&user);
                user.try_send(ServerCommand::ChangeHost(true)).await;
                self.publish_update(PartialRoomData {
                    host: Some(new_host_id),
                    ..Default::default()
                })
                .await;
            }
            None => {
                if let Some(old_host) = old_host {
                    old_host.try_send(ServerCommand::ChangeHost(false)).await;
                }
                self.system_host.store(true, Ordering::SeqCst);
                *self.host.write().await = Weak::<super::session::User>::new();
                if announce {
                    self.send(Message::NewHost { user: -1 }).await;
                }
                self.publish_update(PartialRoomData {
                    host: Some(-1),
                    ..Default::default()
                })
                .await;
            }
        }
        Ok(())
    }

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
            .chain(self.monitors().await)
        {
            session.try_send(cmd.clone()).await;
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

    /// Return: should the room be dropped
    #[must_use]
    pub async fn on_user_leave(&self, user: &super::session::User) -> bool {
        let is_monitor = user.monitor.load(Ordering::SeqCst);
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
        (if is_monitor { &self.monitors } else { &self.users })
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
            if self.is_persistent_empty() {
                info!("room users all disconnected, preserving persistent empty room");
                if !self.is_system_host() {
                    *self.host.write().await = Weak::<super::session::User>::new();
                }
                return false;
            }
            info!("room users all disconnected, dropping room");
            return true;
        }

        if self.check_host(user).await.is_ok() {
            info!("host disconnected!");
            let user = users.choose(&mut rand::rng()).unwrap();
            debug!("selected {} as host", user.id);
            self.system_host.store(false, Ordering::SeqCst);
            *self.host.write().await = Arc::downgrade(user);
            self.send(Message::NewHost { user: user.id }).await;
            user.try_send(ServerCommand::ChangeHost(true)).await;
            self.publish_update(PartialRoomData {
                host: Some(user.id),
                ..Default::default()
            })
            .await;
        }
        self.check_all_ready().await;
        false
    }

    pub async fn reset_game_time(&self) {
        for user in self.users().await {
            user.game_time
                .store(f32::NEG_INFINITY.to_bits(), Ordering::SeqCst);
        }
    }

    async fn save_round_history(&self) -> Option<RoundData> {
        let round_id = self.current_round_id.read().await.unwrap_or(uuid::Uuid::nil());
        let (chart_id, chart_name, results, aborted) = {
            let guard = self.state.read().await;
            match guard.deref() {
                InternalRoomState::Playing { results, aborted } => {
                    let cid = self.chart.read().await.as_ref().map(|c| (c.id, c.name.clone()));
                    let (cid, cn) = match cid {
                        Some((id, name)) => (id, name),
                        None => return None,
                    };
                    let results = results.clone();
                    let aborted = aborted.clone();
                    (cid, cn, results, aborted)
                }
                _ => return None,
            }
        };

        // 收集用户名
        let mut users_map: HashMap<i32, String> = HashMap::new();
        for u in self.users().await {
            users_map.insert(u.id, self.display_name(&u).await);
        }

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

        if let Some(db) = crate::internal_hooks::DB.get() {
            for result in &round.results {
                db.record_round_result(&round.round_id.to_string(), &self.id.to_string(), result).await;
            }
        }
        let event = protocol_round(&round);
        self.play_history.write().await.push(round);
        info!(
            room = self.id.to_string(),
            "saved play round history (total {})",
            self.play_history.read().await.len()
        );
        Some(event)
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
                    self.finish_admin_start().await;
                    let round_id = uuid::Uuid::new_v4();
                    *self.current_round_id.write().await = Some(round_id);
                    info!(room = self.id.to_string(), round = %round_id, "game start");
                    if let Some(server) = self.server.upgrade() {
                        server.publish_runtime_event(crate::event_bus::MpEvent::GameStarted {
                            room_id: self.id.clone(),
                            round_id: round_id.to_string(),
                        });
                    }
                    self.send(Message::StartPlaying).await;
                    self.reset_game_time().await;
                    *self.state.write().await = InternalRoomState::Playing {
                        results: HashMap::new(),
                        aborted: HashSet::new(),
                    };
                    self.on_state_change().await;

                    // 打开轮次数据存储
                    let rid = round_id.to_string();
                    let chart_info = self.chart.read().await;
                    let (cid, cn) = chart_info.as_ref().map(|c| (c.id, c.name.clone())).unwrap_or((0, "?".into()));
                    drop(chart_info);
                    let players: Vec<i32> = self.users().await.into_iter().map(|u| u.id).collect();
                    if let Some(rs) = &self.round_store {
                        let meta = crate::round_store::RoundMeta {
                            round_uuid: rid,
                            chart_id: cid,
                            chart_name: cn,
                            room_id: self.id.to_string(),
                            players: players.clone(),
                            started_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as i64)
                                .unwrap_or(0),
                            finished_at: None,
                        };
                        if let Err(e) = rs.open_round(&meta).await {
                            warn!("round store: failed to open round: {e}");
                        }
                    }
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
                    let rid = *self.current_round_id.read().await;
                    let completed_round = self.save_round_history().await;
                    if let (Some(server), Some(round)) =
                        (self.server.upgrade(), completed_round)
                    {
                        server
                            .publish_room_event(RoomEvent::NewRound {
                                room: self.id.clone(),
                                round,
                            })
                            .await;
                    }

                    // 关闭轮次数据存储
                    if let Some(rid) = rid {
                        info!("round complete: {}", rid);
                        if let Some(rs) = &self.round_store {
                            rs.close_round(&rid.to_string()).await;
                        }
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

                    // 发送结算排行
                    {
                        let history = self.play_history.read().await;
                        if let Some(last) = history.last() {
                            let mut sorted = last.results.clone();
                            sorted.sort_by(|a, b| b.score.cmp(&a.score));
                            let mut lines: Vec<String> = vec![format!("▸ {chart} 排行", chart = last.chart_name)];
                            for (i, r) in sorted.iter().enumerate() {
                                let status = if r.aborted { " 放弃" } else { "" };
                                let fc = if r.full_combo { " FC" } else { "" };
                                lines.push(format!("#{} {} | {}分 | {:.1}%{}{}", i + 1, r.user_name, r.score, r.accuracy * 100.0, fc, status));
                            }
                            for line in &lines {
                                self.send(Message::Chat { user: 0, content: line.clone() }).await;
                            }
                        }
                    }
                    self.send(Message::GameEnd).await;
                    *self.state.write().await = InternalRoomState::SelectChart;
                    if self.is_cycle() && !self.is_system_host() {
                        debug!(room = self.id.to_string(), "cycling");
                        let host = Weak::clone(&*self.host.read().await);
                        let new_host = {
                            let users = self.users().await;
                            if users.is_empty() {
                                None
                            } else {
                                let index = users
                                    .iter()
                                    .position(|it| host.ptr_eq(&Arc::downgrade(it)))
                                    .map(|it| (it + 1) % users.len())
                                    .unwrap_or_default();
                                users.into_iter().nth(index)
                            }
                        };
                        if let Some(new_host) = new_host {
                            self.system_host.store(false, Ordering::SeqCst);
                            *self.host.write().await = Arc::downgrade(&new_host);
                            self.send(Message::NewHost { user: new_host.id }).await;
                            if let Some(old) = host.upgrade() {
                                old.try_send(ServerCommand::ChangeHost(false)).await;
                            }
                            new_host.try_send(ServerCommand::ChangeHost(true)).await;
                            self.publish_update(PartialRoomData {
                                host: Some(new_host.id),
                                ..Default::default()
                            })
                            .await;
                        }
                    }
                    self.on_state_change().await;
                }
            }
            _ => {}
        }
    }
}

//! Client sessions, authentication, and command dispatch.

use crate::l10n::{Language, LANGUAGE};
use crate::phira_client::PhiraRetryNoticeTarget;
use crate::plugin::PluginEvent;
use crate::server::PlusServerState;
use crate::session_auth::{authenticate_remote_with_notice, send_auth_rejection, AuthUserInfo};
use anyhow::{anyhow, bail, Result};
use phira_mp_common::{ClientCommand, RoomEvent, ServerCommand, Stream, UserInfo};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Weak,
    },
    time::{Duration, Instant},
};
use tokio::{
    net::TcpStream,
    sync::{Mutex, Notify, OnceCell, RwLock},
    task::JoinHandle,
    time,
};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Constant-time byte slice comparison to resist timing side-channel attacks.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

const HEARTBEAT_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(600);
pub struct User {
    pub id: i32,
    pub name: String,
    pub lang: Language,

    pub server: Arc<PlusServerState>,
    pub auth_token: RwLock<Option<String>>,
    pub session: RwLock<Option<Weak<Session>>>,
    pub room: RwLock<Option<Arc<super::room::Room>>>,

    pub monitor: AtomicBool,
    pub game_time: AtomicU32,

    pub dangle_mark: Mutex<Option<Arc<()>>>,
    pub admin_cli_pending: Mutex<Option<String>>,
    /// 用户确认加入进行中游戏的房间 ID（第一次请求时设置，第二次直接加入）。
    pub join_pending_game: RwLock<Option<String>>,
}

impl User {
    pub fn new(
        id: i32,
        name: String,
        lang: Language,
        server: Arc<PlusServerState>,
        auth_token: Option<String>,
    ) -> Self {
        Self {
            id,
            name,
            lang,

            server,
            auth_token: RwLock::new(auth_token),
            session: RwLock::default(),
            room: RwLock::default(),

            monitor: AtomicBool::default(),
            game_time: AtomicU32::default(),

            dangle_mark: Mutex::default(),
            admin_cli_pending: Mutex::default(),
            join_pending_game: RwLock::default(),
        }
    }

    pub fn to_info(&self) -> UserInfo {
        UserInfo {
            id: self.id,
            name: self.name.clone(),
            monitor: self.monitor.load(Ordering::Relaxed),
        }
    }

    pub fn can_monitor(&self) -> bool {
        self.server.config.monitors.contains(&self.id)
    }

    pub async fn set_session(&self, session: Weak<Session>) {
        *self.session.write().await = Some(session);
        *self.dangle_mark.lock().await = None;
    }

    pub async fn set_auth_token(&self, token: Option<String>) {
        *self.auth_token.write().await = token;
    }

    pub async fn auth_token(&self) -> Option<String> {
        self.auth_token.read().await.clone()
    }

    pub fn auth_token_sync(&self) -> Option<String> {
        self.auth_token
            .try_read()
            .ok()
            .and_then(|token| token.clone())
    }

    pub async fn try_send(&self, cmd: ServerCommand) {
        if let Some(session) = self.session.read().await.as_ref().and_then(Weak::upgrade) {
            session.try_send(cmd).await;
        } else {
            warn!("sending {:?} to dangling user {}", cmd, self.id);
        }
    }

    pub async fn dangle(self: Arc<Self>) {
        warn!(user = self.id, "user dangling");
        let guard = self.room.read().await;
        let room = guard.as_ref().map(Arc::clone);
        drop(guard);

        // Monitor sessions are transient and never enter the player lifecycle.
        if self.id < 0 {
            if let Some(room) = room {
                if room.on_user_leave(&self).await {
                    self.server.rooms.write().await.remove(&room.id);
                }
            }
            let mut users = self.server.users.write().await;
            if users
                .get(&self.id)
                .is_some_and(|current| Arc::ptr_eq(current, &self))
            {
                users.remove(&self.id);
            }
            drop(users);
            let mut monitors = self.server.game_monitors.write().await;
            if monitors
                .get(&self.id)
                .and_then(Weak::upgrade)
                .is_some_and(|session| Arc::ptr_eq(&session.user, &self))
            {
                monitors.remove(&self.id);
            }
            return;
        }

        if let Some(room) = room {
            let guard = room.state.read().await;
            if matches!(*guard, crate::room::InternalRoomState::Playing { .. }) {
                warn!(user = self.id, "lost connection on playing, aborting");
                {
                    let mut users = self.server.users.write().await;
                    if users
                        .get(&self.id)
                        .is_some_and(|current| Arc::ptr_eq(current, &self))
                    {
                        users.remove(&self.id);
                    }
                }
                drop(guard);
                let room_id = room.id.clone();
                let was_monitor = self.monitor.load(Ordering::Relaxed);
                if room.on_user_leave(&self).await {
                    self.server.rooms.write().await.remove(&room_id);
                }
                if !was_monitor {
                    self.server
                        .publish_room_event(RoomEvent::LeaveRoom {
                            room: room_id,
                            user: self.id,
                        })
                        .await;
                }
                self.server
                    .plugin_manager
                    .trigger(&PluginEvent::UserDisconnect {
                        user_id: self.id,
                        user_name: self.name.clone(),
                    })
                    .await;
                self.server
                    .publish_runtime_event(crate::event_bus::MpEvent::UserDisconnected {
                        user_id: self.id,
                    });
                crate::internal_hooks::playtime_disconnect(self.id);
                // Primary: PersistenceWorker
                let worker_ok = self.server.persistence_worker.enqueue(
                    crate::persistence::message::PersistenceEvent::UserDisconnect {
                        user_id: self.id,
                        user_name: self.name.clone(),
                    }
                ).await.is_ok();
                // Fallback: direct DB write
                if !worker_ok {
                    if let Some(db) = crate::internal_hooks::DB.get() {
                        db.record_user_disconnect_sync(self.id, &self.name);
                    }
                }
                // Also mirror offline status to worker
                let _ = self.server.persistence_worker.enqueue(
                    crate::persistence::message::PersistenceEvent::UserOffline {
                        user_id: self.id,
                    }
                ).await;
                return;
            }
        }
        self.server
            .plugin_manager
            .trigger(&PluginEvent::UserDisconnect {
                user_id: self.id,
                user_name: self.name.clone(),
            })
            .await;
        self.server
            .publish_runtime_event(crate::event_bus::MpEvent::UserDisconnected {
                user_id: self.id,
            });
        if let Some(db) = crate::internal_hooks::DB.get() {
            db.record_user_disconnect_sync(self.id, &self.name);
        }

        let dangle_mark = Arc::new(());
        *self.dangle_mark.lock().await = Some(Arc::clone(&dangle_mark));
        let weak_self = Arc::downgrade(&self);
        tokio::spawn(async move {
            time::sleep(Duration::from_secs(10)).await;
            if Arc::strong_count(&dangle_mark) > 1 {
                if let Some(self_) = weak_self.upgrade() {
                    let guard = self_.room.read().await;
                    let room = guard.as_ref().map(Arc::clone);
                    drop(guard);
                    if let Some(room) = room {
                        {
                            let mut users = self_.server.users.write().await;
                            if users
                                .get(&self_.id)
                                .is_some_and(|current| Arc::ptr_eq(current, &self_))
                            {
                                users.remove(&self_.id);
                            }
                        }
                        let room_id = room.id.clone();
                        let was_monitor = self_.monitor.load(Ordering::Relaxed);
                        if room.on_user_leave(&self_).await {
                            self_.server.rooms.write().await.remove(&room_id);
                        }
                        if !was_monitor {
                            self_
                                .server
                                .publish_room_event(RoomEvent::LeaveRoom {
                                    room: room_id,
                                    user: self_.id,
                                })
                                .await;
                        }
                    }
                }
            }
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionCategory {
    Normal,
    Console,
    RoomMonitor,
    GameMonitor,
}

enum AuthenticationOutcome {
    Accepted(Arc<User>, SessionCategory),
    Rejected,
}

pub struct Session {
    pub id: Uuid,
    pub ip: String,
    pub stream: Stream<ServerCommand, ClientCommand>,
    pub user: Arc<User>,
    pub category: SessionCategory,

    monitor_task_handle: JoinHandle<()>,
    /// 命令速率限制器（按类别）
    cmd_limiter: crate::rate_limiter::CommandRateLimiter,
}

impl Session {
    pub async fn new(
        id: Uuid,
        addr: std::net::SocketAddr,
        stream: TcpStream,
        server: Arc<PlusServerState>,
    ) -> Result<Arc<Self>> {
        stream.set_nodelay(true)?;
        let this = Arc::new(OnceCell::<Arc<Session>>::new());
        let this_inited = Arc::new(Notify::new());
        let (tx, rx) = tokio::sync::oneshot::channel::<AuthenticationOutcome>();
        let last_recv: Arc<Mutex<Instant>> = Arc::new(Mutex::new(Instant::now()));
        let server_clone = Arc::clone(&server);

        let stream = Stream::<ServerCommand, ClientCommand>::new(
            None,
            stream,
            Box::new({
                let this = Arc::clone(&this);
                let this_inited = Arc::clone(&this_inited);
                let mut tx = Some(tx);
                let server = Arc::clone(&server);
                let last_recv = Arc::clone(&last_recv);
                let waiting_for_authenticate = Arc::new(AtomicBool::new(true));
                let panicked = Arc::new(AtomicBool::new(false));
                move |send_tx, cmd| {
                    let this = Arc::clone(&this);
                    let this_inited = Arc::clone(&this_inited);
                    let tx = if matches!(
                        &cmd,
                        ClientCommand::Authenticate { .. }
                            | ClientCommand::ConsoleAuthenticate { .. }
                            | ClientCommand::RoomMonitorAuthenticate { .. }
                            | ClientCommand::GameMonitorAuthenticate { .. }
                    ) {
                        tx.take()
                    } else {
                        None
                    };
                    let server = Arc::clone(&server);
                    let last_recv = Arc::clone(&last_recv);
                    let waiting_for_authenticate = Arc::clone(&waiting_for_authenticate);
                    let panicked = Arc::clone(&panicked);
                    async move {
                        if panicked.load(Ordering::SeqCst) {
                            return;
                        }
                        *last_recv.lock().await = Instant::now();
                        if matches!(&cmd, ClientCommand::Ping) {
                            let _ = send_tx.send(ServerCommand::Pong).await;
                            return;
                        }
                        if waiting_for_authenticate.load(Ordering::SeqCst) {
                            if let ClientCommand::Authenticate { token } = &cmd {
                                let Some(tx) = tx else { return };
                                let mut auth_tx = Some(tx);
                                let retry_send_tx = Arc::clone(&send_tx);
                                let res: Result<()> = {
                                    let this = Arc::clone(&this);
                                    let server = Arc::clone(&server);
                                    let auth_tx = &mut auth_tx;
                                    async move {
                                        let token = token.clone().into_inner();
                                        if token.len() > 32 {
                                            bail!("invalid token");
                                        }
                                        debug!("session {id}: authenticating");
                                        // 计算 token SHA256 哈希作为缓存键
                                        use sha2::{Digest, Sha256};
                                        let token_hash =
                                            format!("{:x}", Sha256::digest(token.as_bytes()));

                                        let user_info = {
                                            let ac = server.extensions.get_auth_cache().await;
                                            ac.get(&token_hash).cloned()
                                        };
                                        let user_info = if let Some(entry) = user_info {
                                            // 封禁检查（拒绝前不走 API，毫秒级响应）
                                            if let Some(_reason) =
                                                server.ban_manager.ban_reason(entry.user_id).await
                                            {
                                                warn!(
                                                    "banned user {}({}) tried to connect (cache)",
                                                    entry.name, entry.user_id
                                                );
                                                if let Some(tx) = auth_tx.take() {
                                                    let _ =
                                                        tx.send(AuthenticationOutcome::Rejected);
                                                }
                                                return Ok(());
                                            }
                                            debug!("cache hit for user {}", entry.user_id);
                                            AuthUserInfo {
                                                id: entry.user_id,
                                                name: entry.name,
                                                language: entry.language,
                                            }
                                        } else {
                                            // 缓存未命中，请求 API；遇到 “认证失败 502错误”/502/5xx 时重试并提示该客户端。
                                            match server
                                                .phira_client
                                                .get_json::<AuthUserInfo>(
                                                    &server.config.phira_api_endpoint,
                                                    None,
                                                    "/me",
                                                    Some(&token),
                                                    PhiraRetryNoticeTarget::Stream(
                                                        retry_send_tx.as_ref(),
                                                    ),
                                                )
                                                .await
                                            {
                                                Ok(info) => {
                                                    // API 成功，更新缓存并持久化
                                                    let _cached_at = std::time::SystemTime::now()
                                                        .duration_since(std::time::UNIX_EPOCH)
                                                        .map(|d| d.as_millis() as i64)
                                                        .unwrap_or(0);
                                                    server
                                                        .extensions
                                                        .update_auth_cache(
                                                            token_hash,
                                                            crate::extensions::AuthCacheEntry {
                                                                user_id: info.id,
                                                                name: info.name.clone(),
                                                                language: info.language.clone(),
                                                                cached_at: _cached_at,
                                                            },
                                                        )
                                                        .await;
                                                    // 封禁检查
                                                    if let Some(_reason) =
                                                        server.ban_manager.ban_reason(info.id).await
                                                    {
                                                        warn!(
                                                            "banned user {}({}) tried to connect",
                                                            info.name, info.id
                                                        );
                                                        if let Some(tx) = auth_tx.take() {
                                                            let _ = tx.send(
                                                                AuthenticationOutcome::Rejected,
                                                            );
                                                        }
                                                        return Ok(());
                                                    }
                                                    info
                                                }
                                                Err(_err) => {
                                                    if let Some(tx) = auth_tx.take() {
                                                        let _ = tx
                                                            .send(AuthenticationOutcome::Rejected);
                                                    }
                                                    return Ok(());
                                                }
                                            }
                                        };

                                        // Primary: PersistenceWorker
                                        let seen_ok = self.server.persistence_worker.enqueue(
                                            crate::persistence::message::PersistenceEvent::UserSeen {
                                                user_id: user_info.id,
                                                user_name: user_info.name.clone(),
                                                language: user_info.language.clone(),
                                                ip: addr.ip().to_string(),
                                            }
                                        ).await.is_ok();
                                        // Fallback: direct DB write
                                        if !seen_ok {
                                            if let Some(db) = crate::internal_hooks::DB.get() {
                                                db.record_user_seen_sync(
                                                    user_info.id,
                                                    &user_info.name,
                                                    &user_info.language,
                                                    Some(addr.ip().to_string()),
                                                );
                                            }
                                        }

                                        let user_ip =
                                            this.get().map(|s| s.ip.clone()).unwrap_or_default();
                                        let existing_user = {
                                            let guard = server.users.write().await;
                                            guard.get(&user_info.id).map(Arc::clone)
                                        };
                                        if let Some(existing) = existing_user {
                                            info!("reconnect");
                                            let _ = auth_tx.take().unwrap().send(
                                                AuthenticationOutcome::Accepted(
                                                    existing.clone(),
                                                    SessionCategory::Normal,
                                                ),
                                            );
                                            this_inited.notified().await;
                                            existing
                                                .set_session(Arc::downgrade(this.get().unwrap()))
                                                .await;
                                            existing.set_auth_token(Some(token.to_string())).await;
                                            server.publish_runtime_event(
                                                crate::event_bus::MpEvent::UserConnected {
                                                    user_id: user_info.id,
                                                    user_name: user_info.name.clone(),
                                                    user_ip: user_ip.clone(),
                                                    user_language: user_info.language.clone(),
                                                },
                                            );
                                        } else {
                                            if server.ban_manager.is_banned(user_info.id).await {
                                                let _ = auth_tx
                                                    .take()
                                                    .unwrap()
                                                    .send(AuthenticationOutcome::Rejected);
                                                return Ok(());
                                            }
                                            let user = Arc::new(User::new(
                                                user_info.id,
                                                user_info.name,
                                                user_info
                                                    .language
                                                    .parse()
                                                    .map(Language)
                                                    .unwrap_or_default(),
                                                Arc::clone(&server),
                                                Some(token.to_string()),
                                            ));
                                            let _ = auth_tx.take().unwrap().send(
                                                AuthenticationOutcome::Accepted(
                                                    Arc::clone(&user),
                                                    SessionCategory::Normal,
                                                ),
                                            );
                                            this_inited.notified().await;
                                            user.set_session(Arc::downgrade(this.get().unwrap()))
                                                .await;
                                            {
                                                let mut guard = server.users.write().await;
                                                guard.insert(user_info.id, Arc::clone(&user));
                                            }
                                            server.publish_runtime_event(
                                                crate::event_bus::MpEvent::UserConnected {
                                                    user_id: user_info.id,
                                                    user_name: user.name.clone(),
                                                    user_ip: user_ip.clone(),
                                                    user_language: user.lang.0.to_string(),
                                                },
                                            );
                                        }
                                        Ok(())
                                    }
                                }
                                .await;
                                if let Err(err) = res {
                                    warn!("failed to authenticate: {err:?}");
                                    send_auth_rejection(&send_tx, err.to_string()).await;
                                    if let Some(tx) = auth_tx.take() {
                                        let _ = tx.send(AuthenticationOutcome::Rejected);
                                    }
                                    panicked.store(true, Ordering::SeqCst);
                                } else {
                                    let user = &this.get().unwrap().user;
                                    let room_state = match user.room.read().await.as_ref() {
                                        Some(room) => Some(room.client_state(user).await),
                                        None => None,
                                    };
                                    debug!("sending auth OK to user {}", user.id);
                                    let _ = send_tx
                                        .send(ServerCommand::Authenticate(Ok((
                                            user.to_info(),
                                            room_state,
                                        ))))
                                        .await;
                                    debug!("auth response sent");
                                    // 通知 room monitor 新用户
                                    let uid = user.id;
                                    let _session = this.get().map(|s| Arc::clone(s));
                                    tokio::spawn(async move {
                                        if let Some(mon) = server.get_room_monitor().await {
                                            mon.stream
                                                .send(ServerCommand::UserVisit(uid))
                                                .await
                                                .ok();
                                        }
                                    });
                                    waiting_for_authenticate.store(false, Ordering::SeqCst);
                                }
                                return;
                            } else if let ClientCommand::ConsoleAuthenticate { token } = &cmd {
                                let Some(tx) = tx else { return };
                                match authenticate_remote_with_notice(
                                    &server,
                                    token,
                                    PhiraRetryNoticeTarget::Stream(send_tx.as_ref()),
                                )
                                .await
                                {
                                    Ok(info) => {
                                        let user = Arc::new(User::new(
                                            info.id,
                                            info.name,
                                            info.language.parse().map(Language).unwrap_or_default(),
                                            Arc::clone(&server),
                                            Some(token.to_string()),
                                        ));
                                        let _ = tx.send(AuthenticationOutcome::Accepted(
                                            Arc::clone(&user),
                                            SessionCategory::Console,
                                        ));
                                        this_inited.notified().await;
                                        user.set_session(Arc::downgrade(this.get().unwrap())).await;
                                        let _ = send_tx
                                            .send(ServerCommand::Authenticate(Ok((
                                                user.to_info(),
                                                None,
                                            ))))
                                            .await;
                                        waiting_for_authenticate.store(false, Ordering::SeqCst);
                                    }
                                    Err(err) => {
                                        warn!("console authentication failed: {err}");
                                        send_auth_rejection(
                                            &send_tx,
                                            "authentication failed".into(),
                                        )
                                        .await;
                                        let _ = tx.send(AuthenticationOutcome::Rejected);
                                        panicked.store(true, Ordering::SeqCst);
                                    }
                                }
                                return;
                            } else if let ClientCommand::RoomMonitorAuthenticate { key } = &cmd {
                                let Some(tx) = tx else { return };
                                if server
                                    .room_monitor
                                    .read()
                                    .await
                                    .as_ref()
                                    .and_then(|w| w.upgrade())
                                    .is_some()
                                {
                                    send_auth_rejection(
                                        &send_tx,
                                        "more than one room monitor".into(),
                                    )
                                    .await;
                                    let _ = tx.send(AuthenticationOutcome::Rejected);
                                    panicked.store(true, Ordering::SeqCst);
                                    return;
                                }
                                if !constant_time_eq(server.room_monitor_key.as_slice(), key.as_slice()) {
                                    send_auth_rejection(&send_tx, "secret key mismatch".into())
                                        .await;
                                    let _ = tx.send(AuthenticationOutcome::Rejected);
                                    panicked.store(true, Ordering::SeqCst);
                                    return;
                                }
                                info!("new room monitor connected");
                                let user = Arc::new(User::new(
                                    -1,
                                    "$server_room_monitor".into(),
                                    Language::default(),
                                    Arc::clone(&server),
                                    None,
                                ));
                                let _ = tx.send(AuthenticationOutcome::Accepted(
                                    Arc::clone(&user),
                                    SessionCategory::RoomMonitor,
                                ));
                                this_inited.notified().await;
                                user.set_session(Arc::downgrade(this.get().unwrap())).await;
                                *server.room_monitor.write().await =
                                    Some(Arc::downgrade(this.get().unwrap()));
                                let _ = send_tx
                                    .send(ServerCommand::Authenticate(Ok((user.to_info(), None))))
                                    .await;
                                waiting_for_authenticate.store(false, Ordering::SeqCst);
                                return;
                            } else if let ClientCommand::GameMonitorAuthenticate { token } = &cmd {
                                let Some(tx) = tx else { return };
                                match authenticate_remote_with_notice(
                                    &server,
                                    token,
                                    PhiraRetryNoticeTarget::Stream(send_tx.as_ref()),
                                )
                                .await
                                {
                                    Ok(info) => {
                                        let Some(monitor_id) =
                                            info.id.checked_neg().filter(|id| *id < 0)
                                        else {
                                            send_auth_rejection(
                                                &send_tx,
                                                "invalid monitor identity".into(),
                                            )
                                            .await;
                                            let _ = tx.send(AuthenticationOutcome::Rejected);
                                            panicked.store(true, Ordering::SeqCst);
                                            return;
                                        };
                                        let user = Arc::new(User::new(
                                            monitor_id,
                                            format!("{} (monitor)", info.name),
                                            info.language.parse().map(Language).unwrap_or_default(),
                                            Arc::clone(&server),
                                            Some(token.to_string()),
                                        ));
                                        let _ = tx.send(AuthenticationOutcome::Accepted(
                                            Arc::clone(&user),
                                            SessionCategory::GameMonitor,
                                        ));
                                        this_inited.notified().await;
                                        user.set_session(Arc::downgrade(this.get().unwrap())).await;
                                        server
                                            .users
                                            .write()
                                            .await
                                            .insert(monitor_id, Arc::clone(&user));
                                        server
                                            .set_game_monitor(
                                                monitor_id,
                                                Arc::downgrade(this.get().unwrap()),
                                            )
                                            .await;
                                        let _ = send_tx
                                            .send(ServerCommand::Authenticate(Ok((
                                                user.to_info(),
                                                None,
                                            ))))
                                            .await;
                                        waiting_for_authenticate.store(false, Ordering::SeqCst);
                                    }
                                    Err(err) => {
                                        warn!("game monitor authentication failed: {err}");
                                        send_auth_rejection(
                                            &send_tx,
                                            "game monitor auth failed".into(),
                                        )
                                        .await;
                                        let _ = tx.send(AuthenticationOutcome::Rejected);
                                        panicked.store(true, Ordering::SeqCst);
                                    }
                                }
                                return;
                            } else {
                                warn!("packet before authentication, ignoring: {cmd:?}");
                                return;
                            }
                        }
                        let user = this.get().map(|it| Arc::clone(&it.user)).unwrap();

                        // 命令速率限制
                        let session_ref = this.get().map(|s| Arc::clone(s));
                        let needs_limiting = matches!(
                            &cmd,
                            ClientCommand::Chat { .. }
                                | ClientCommand::CreateRoom { .. }
                                | ClientCommand::JoinRoom { .. }
                                | ClientCommand::SelectChart { .. }
                        );
                        if needs_limiting {
                            if let Some(session) = session_ref {
                                let category = match &cmd {
                                    ClientCommand::Chat { .. } => {
                                        crate::rate_limiter::CommandCategory::Chat
                                    }
                                    _ => crate::rate_limiter::CommandCategory::RoomOp,
                                };
                                if !session.cmd_limiter.check(category).await {
                                    warn!("command rate limited for user {}", user.id);
                                    return;
                                }
                            }
                        }

                        let result = LANGUAGE
                            .scope(
                                Arc::new(user.lang.clone()),
                                crate::session_dispatch::process(
                                    user,
                                    this.get().unwrap().category,
                                    cmd,
                                ),
                            )
                            .await;
                        if let Some(resp) = result {
                            if let Err(err) = send_tx.send(resp).await {
                                error!(
                                    "failed to handle message, aborting connection {id}: {err:?}",
                                );
                                panicked.store(true, Ordering::SeqCst);
                                if let Err(err) = server.lost_con_tx.send(id).await {
                                    error!("failed to mark lost connection ({id}): {err:?}");
                                }
                            }
                        }
                    }
                }
            }),
        )
        .await?;
        let monitor_task_handle = tokio::spawn({
            let server_clone = Arc::clone(&server_clone);
            async move {
                loop {
                    let recv = *last_recv.lock().await;
                    time::sleep_until((recv + HEARTBEAT_DISCONNECT_TIMEOUT).into()).await;

                    if *last_recv.lock().await + HEARTBEAT_DISCONNECT_TIMEOUT > Instant::now() {
                        continue;
                    }

                    if let Err(err) = server_clone.lost_con_tx.send(id).await {
                        error!("failed to mark lost connection ({id}): {err:?}");
                    }
                    break;
                }
            }
        });

        let (user, category) = match rx.await {
            Ok(AuthenticationOutcome::Accepted(user, category)) => (user, category),
            Ok(AuthenticationOutcome::Rejected) => {
                return Err(anyhow!("authentication rejected after response flush"));
            }
            Err(_) => return Err(anyhow!("authentication channel closed")),
        };

        let ip = addr.ip().to_string();
        let res = Arc::new(Self {
            id,
            ip,
            stream,
            user,
            category,
            monitor_task_handle,
            cmd_limiter: crate::rate_limiter::CommandRateLimiter::new(),
        });
        let _ = this.set(Arc::clone(&res));
        this_inited.notify_one();
        Ok(res)
    }

    pub fn version(&self) -> u8 {
        self.stream.version()
    }

    pub fn name(&self) -> &str {
        &self.user.name
    }

    pub async fn try_send(&self, cmd: ServerCommand) {
        if let Err(err) = self.stream.send(cmd).await {
            error!("failed to deliver command to {}: {err:?}", self.id);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.monitor_task_handle.abort();
    }
}

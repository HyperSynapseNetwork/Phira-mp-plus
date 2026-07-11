//! Client sessions, authentication, and command dispatch.

use crate::l10n::{Language, LANGUAGE};
use crate::phira_client::PhiraRetryNoticeTarget;
use crate::server::PlusServerState;
use crate::session_auth::{
    authenticate_remote_with_notice, ban_rejection_message, send_auth_rejection, AuthUserInfo,
};
use anyhow::{anyhow, bail, Result};
use phira_mp_common::{ClientCommand, Message, RoomEvent, ServerCommand, Stream, UserInfo};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, OnceLock, Weak,
    },
    time::{Duration, Instant},
};
use tokio::{
    net::TcpStream,
    sync::{mpsc, Mutex, Notify, OnceCell, OwnedSemaphorePermit, RwLock},
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

    pub async fn can_monitor(&self) -> bool {
        self.server
            .live_config
            .read()
            .await
            .monitors
            .contains(&self.id)
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

    pub async fn dangle(self: Arc<Self>, disconnected_session_id: Uuid) {
        warn!(user = self.id, session = %disconnected_session_id, "user dangling");

        // Normal-user registration and disconnect finalization share one gate.
        // This prevents a reconnect from racing an offline transition.
        let registration_guard = if self.id >= 0 {
            Some(self.server.user_registration_gate.lock().await)
        } else {
            None
        };
        let is_current_session = self
            .session
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|session| session.id == disconnected_session_id);
        if !is_current_session {
            debug!(
                user = self.id,
                session = %disconnected_session_id,
                "ignoring stale disconnect after transport replacement"
            );
            return;
        }

        let room = self.room.read().await.as_ref().map(Arc::clone);

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

        if let Some(room) = room.as_ref() {
            let playing = matches!(
                *room.state.read().await,
                crate::room::InternalRoomState::Playing { .. }
            );
            if playing {
                warn!(
                    user = self.id,
                    "lost connection while playing; removing immediately"
                );
                let room_id = room.id.clone();
                let was_monitor = self.monitor.load(Ordering::Relaxed);
                if room.on_user_leave(&self).await {
                    self.server.rooms.write().await.remove(&room_id);
                }
                let mut users = self.server.users.write().await;
                if users
                    .get(&self.id)
                    .is_some_and(|current| Arc::ptr_eq(current, &self))
                {
                    users.remove(&self.id);
                }
                drop(users);
                drop(registration_guard);

                if !was_monitor {
                    self.server
                        .publish_room_event(RoomEvent::LeaveRoom {
                            room: room_id,
                            user: self.id,
                        })
                        .await;
                }
                self.server
                    .publish_user_disconnected(self.id, self.name.clone())
                    .await;
                crate::internal_hooks::playtime_disconnect(self.id);
                let _ = self
                    .server
                    .persistence_worker
                    .enqueue(
                        crate::persistence::message::PersistenceEvent::UserDisconnect {
                            user_id: self.id,
                            user_name: self.name.clone(),
                        },
                    )
                    .await;
                let _ = self
                    .server
                    .persistence_worker
                    .enqueue(crate::persistence::message::PersistenceEvent::UserOffline {
                        user_id: self.id,
                    })
                    .await;
                return;
            }
        }

        let dangle_mark = Arc::new(());
        *self.dangle_mark.lock().await = Some(Arc::clone(&dangle_mark));
        drop(registration_guard);

        self.server
            .publish_user_disconnected(self.id, self.name.clone())
            .await;
        let _ = self
            .server
            .persistence_worker
            .enqueue(
                crate::persistence::message::PersistenceEvent::UserDisconnect {
                    user_id: self.id,
                    user_name: self.name.clone(),
                },
            )
            .await;

        let weak_self = Arc::downgrade(&self);
        crate::supervisor_actor::spawn_named(format!("dangle-grace-{}", self.id), async move {
            time::sleep(Duration::from_secs(10)).await;
            let Some(self_) = weak_self.upgrade() else {
                return;
            };
            let registration_guard = self_.server.user_registration_gate.lock().await;
            let expired = {
                let mut current = self_.dangle_mark.lock().await;
                if current
                    .as_ref()
                    .is_some_and(|mark| Arc::ptr_eq(mark, &dangle_mark))
                {
                    current.take();
                    true
                } else {
                    false
                }
            };
            if !expired {
                return;
            }

            let room = self_.room.read().await.as_ref().map(Arc::clone);
            let mut room_leave_event = None;
            if let Some(room) = room {
                let room_id = room.id.clone();
                let was_monitor = self_.monitor.load(Ordering::Relaxed);
                if room.on_user_leave(&self_).await {
                    self_.server.rooms.write().await.remove(&room_id);
                }
                if !was_monitor {
                    room_leave_event = Some(RoomEvent::LeaveRoom {
                        room: room_id,
                        user: self_.id,
                    });
                }
            }

            let mut users = self_.server.users.write().await;
            if users
                .get(&self_.id)
                .is_some_and(|current| Arc::ptr_eq(current, &self_))
            {
                users.remove(&self_.id);
            }
            drop(users);
            drop(registration_guard);

            if let Some(event) = room_leave_event {
                self_.server.publish_room_event(event).await;
            }
            crate::internal_hooks::playtime_disconnect(self_.id);
            let _ = self_
                .server
                .persistence_worker
                .enqueue(crate::persistence::message::PersistenceEvent::UserOffline {
                    user_id: self_.id,
                })
                .await;
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

    /// Per-session actor mailbox sender. Set after authentication.
    pub(crate) actor_tx: OnceLock<mpsc::Sender<crate::session_actor::SessionActorCmd>>,
    monitor_task_handle: JoinHandle<()>,
    /// Releases one authenticated-session capacity slot on drop.
    _session_permit: OwnedSemaphorePermit,
    /// 命令速率限制器（按类别）
    cmd_limiter: crate::rate_limiter::CommandRateLimiter,
}

impl Session {
    pub async fn new(
        id: Uuid,
        addr: std::net::SocketAddr,
        stream: TcpStream,
        server: Arc<PlusServerState>,
        session_permit: OwnedSemaphorePermit,
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
                                            if let Some(reason) =
                                                server.ban_manager.ban_reason(entry.user_id).await
                                            {
                                                warn!(
                                                    "banned user {}({}) tried to connect (cache)",
                                                    entry.name, entry.user_id
                                                );
                                                let rejection = ban_rejection_message(
                                                    &entry.language,
                                                    &reason,
                                                );
                                                send_auth_rejection(
                                                    retry_send_tx.as_ref(),
                                                    rejection,
                                                )
                                                .await;
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
                                                    if let Some(reason) =
                                                        server.ban_manager.ban_reason(info.id).await
                                                    {
                                                        warn!(
                                                            "banned user {}({}) tried to connect",
                                                            info.name, info.id
                                                        );
                                                        let rejection = ban_rejection_message(
                                                            &info.language,
                                                            &reason,
                                                        );
                                                        send_auth_rejection(
                                                            retry_send_tx.as_ref(),
                                                            rejection,
                                                        )
                                                        .await;
                                                        if let Some(tx) = auth_tx.take() {
                                                            let _ = tx.send(
                                                                AuthenticationOutcome::Rejected,
                                                            );
                                                        }
                                                        return Ok(());
                                                    }
                                                    info
                                                }
                                                Err(err) => {
                                                    warn!(?err, "remote authentication failed");
                                                    send_auth_rejection(
                                                        retry_send_tx.as_ref(),
                                                        "authentication failed".to_string(),
                                                    )
                                                    .await;
                                                    if let Some(tx) = auth_tx.take() {
                                                        let _ = tx
                                                            .send(AuthenticationOutcome::Rejected);
                                                    }
                                                    return Ok(());
                                                }
                                            }
                                        };

                                        // PersistenceWorker (exclusive — no direct fallback)
                                        let _ = server.persistence_worker.enqueue(
                                            crate::persistence::message::PersistenceEvent::UserSeen {
                                                user_id: user_info.id,
                                                user_name: user_info.name.clone(),
                                                language: user_info.language.clone(),
                                                ip: addr.ip().to_string(),
                                            }
                                        ).await;

                                        // Keep the final reconnect/new-user decision atomic across
                                        // Session construction. Cancellation releases this guard,
                                        // so a failed handshake cannot leave a reserved user entry.
                                        let _registration_guard =
                                            server.user_registration_gate.lock().await;
                                        let existing_user = {
                                            let guard = server.users.write().await;
                                            guard.get(&user_info.id).map(Arc::clone)
                                        };
                                        if let Some(existing) = existing_user {
                                            info!("reconnect");
                                            // Replace the transport atomically. The old socket can
                                            // otherwise remain active until heartbeat timeout and
                                            // issue commands concurrently with the new session.
                                            let previous_session = {
                                                let guard = existing.session.read().await;
                                                guard.as_ref().and_then(std::sync::Weak::upgrade)
                                            };
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
                                            if let Some(previous) = previous_session {
                                                if previous.id != id {
                                                    previous.stream.close();
                                                    let _ = server
                                                        .lost_con_tx
                                                        .try_send(previous.id);
                                                }
                                            }
                                            existing.set_auth_token(Some(token.to_string())).await;
                                        } else {
                                            if let Some(reason) =
                                                server.ban_manager.ban_reason(user_info.id).await
                                            {
                                                let rejection = ban_rejection_message(
                                                    &user_info.language,
                                                    &reason,
                                                );
                                                send_auth_rejection(
                                                    retry_send_tx.as_ref(),
                                                    rejection,
                                                )
                                                .await;
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
                                } else if this.get().is_none() {
                                    // Authentication was deliberately rejected and the Session was
                                    // never initialized. Do not fall through into the success path.
                                    panicked.store(true, Ordering::SeqCst);
                                } else {
                                    // Initialize per-session mailbox
                                    if let Some(session) = this.get() {
                                        let tx = crate::session_actor::init_session_mailbox(session);
                                        let _ = session.actor_tx.set(tx);
                                    }
                                    let user = &this.get().unwrap().user;
                                    let room_state = match user.room.read().await.as_ref() {
                                        Some(room) => Some(room.client_state(user).await),
                                        None => None,
                                    };
                                    debug!("sending auth OK to user {}", user.id);
                                    if let Err(err) = send_tx
                                        .send_and_flush(ServerCommand::Authenticate(Ok((
                                            user.to_info(),
                                            room_state,
                                        ))))
                                        .await
                                    {
                                        warn!(user = user.id, ?err, "failed to flush auth response");
                                        panicked.store(true, Ordering::SeqCst);
                                        return;
                                    }
                                    debug!("auth response sent");
                                    server
                                        .publish_user_connected(
                                            user.id,
                                            user.name.clone(),
                                            addr.ip().to_string(),
                                            user.lang.0.to_string(),
                                        )
                                        .await;
                                    // Welcome chat must follow the successful authentication frame;
                                    // otherwise clients may discard it before room/user state exists.
                                    let online = server.users.read().await.len();
                                    crate::internal_hooks::send_welcome(
                                        user.id,
                                        &user.name,
                                        online,
                                        &server,
                                    );
                                    // 通知 room monitor 新用户
                                    let uid = user.id;
                                    crate::supervisor_actor::spawn_named(
                                        format!("room-monitor-visit-{uid}"),
                                        async move {
                                            if let Some(mon) = server.get_room_monitor().await {
                                                mon.stream
                                                    .send(ServerCommand::UserVisit(uid))
                                                    .await
                                                    .ok();
                                            }
                                        },
                                    );
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

                        let joining_player = matches!(
                            &cmd,
                            ClientCommand::JoinRoom { monitor: false, .. }
                        )
                        .then(|| Arc::clone(&user));
                        let creating_player = matches!(&cmd, ClientCommand::CreateRoom { .. })
                            .then(|| Arc::clone(&user));
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
                            let joined_room = joining_player.is_some()
                                && matches!(&resp, ServerCommand::JoinRoom(Ok(_)));
                            let created_room = creating_player.is_some()
                                && matches!(&resp, ServerCommand::CreateRoom(Ok(())));
                            if let Err(err) = send_tx.send(resp).await {
                                error!(
                                    "failed to handle message, aborting connection {id}: {err:?}",
                                );
                                panicked.store(true, Ordering::SeqCst);
                                if let Err(err) = server.lost_con_tx.send(id).await {
                                    error!("failed to mark lost connection ({id}): {err:?}");
                                }
                            } else if created_room {
                                let creating_player = creating_player.expect("checked above");
                                if let Err(err) = send_tx
                                    .send(ServerCommand::Message(Message::CreateRoom {
                                        user: creating_player.id,
                                    }))
                                    .await
                                {
                                    error!(
                                        "failed to deliver post-create room event to {id}: {err:?}"
                                    );
                                }
                            } else if joined_room {
                                let joining_player = joining_player.expect("checked above");
                                let room = joining_player
                                    .room
                                    .read()
                                    .await
                                    .as_ref()
                                    .map(Arc::clone);
                                if let Some(room) = room {
                                    if room.check_host(&joining_player).await.is_ok() {
                                        // Preserve protocol order for the first player entering an
                                        // administratively-created empty room.
                                        if let Err(err) = send_tx
                                            .send(ServerCommand::ChangeHost(true))
                                            .await
                                        {
                                            error!(
                                                "failed to deliver post-join host state to {id}: {err:?}"
                                            );
                                        }
                                    }
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
                let timeout =
                    Duration::from_secs(server_clone.config.idle.heartbeat_timeout_secs.max(10));
                loop {
                    let recv = *last_recv.lock().await;
                    time::sleep_until((recv + timeout).into()).await;

                    if *last_recv.lock().await + timeout > Instant::now() {
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
            actor_tx: OnceLock::new(),
            monitor_task_handle,
            _session_permit: session_permit,
            cmd_limiter: crate::rate_limiter::CommandRateLimiter::new(),
        });
        let _ = this.set(Arc::clone(&res));
        this_inited.notify_one();

        if category == SessionCategory::Normal {
            crate::internal_hooks::playtime_connect(res.user.id);
            if let Err(event) = server
                .persistence_worker
                .enqueue(crate::persistence::message::PersistenceEvent::UserOnline {
                    user_id: res.user.id,
                })
                .await
            {
                warn!(
                    user = res.user.id,
                    kind = %event.kind(),
                    "failed to enqueue authoritative online state"
                );
            }
        }

        Ok(res)
    }

    pub fn version(&self) -> u8 {
        self.stream.version()
    }

    pub fn name(&self) -> &str {
        &self.user.name
    }

    pub async fn try_send(&self, cmd: ServerCommand) {
        if let Err(err) = self.stream.try_send(cmd) {
            // A full outbound queue means this client is no longer keeping up.
            // Disconnect it instead of allowing one slow consumer to stall a
            // room-wide broadcast or actor command.
            warn!(session = %self.id, user = self.user.id, ?err, "disconnecting slow client");
            self.stream.close();
            let _ = self.user.server.lost_con_tx.try_send(self.id);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.monitor_task_handle.abort();
    }
}

//! Client sessions, authentication, and command dispatch.

use crate::l10n::{Language, LANGUAGE};
use crate::plugin::PluginEvent;
use crate::server::PlusServerState;
use crate::tl;
use anyhow::{anyhow, bail, Result};
use phira_mp_common::{
    ClientCommand, JoinRoomResponse, Message, PartialRoomData, RoomEvent, ServerCommand, Stream,
    StreamSender, UserInfo,
};
use serde::Deserialize;
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
use tracing::{debug, debug_span, error, info, trace, warn, Instrument};
use uuid::Uuid;

const HEARTBEAT_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(600);
const AUTH_FAILURE_RESPONSE_DELAY: Duration = Duration::from_millis(50);
const AUTH_FAILURE_FLUSH_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_CLIENT_BAN_REASON_CHARS: usize = 160;
const PHIRA_RETRY_NOTICE: &str = "Phira服务器太烂了，我们正在重试以保证你的流畅体验";
const PHIRA_MAX_RETRIES: usize = 3;

#[derive(Debug, Deserialize)]
struct AuthUserInfo {
    id: i32,
    name: String,
    language: String,
}

enum RetryNoticeTarget<'a> {
    None,
    Stream(&'a StreamSender<ServerCommand>),
    User(&'a User),
}

async fn send_phira_retry_notice(target: &RetryNoticeTarget<'_>) {
    let cmd = ServerCommand::Message(Message::Chat {
        user: 0,
        content: PHIRA_RETRY_NOTICE.to_string(),
    });
    match target {
        RetryNoticeTarget::None => {}
        RetryNoticeTarget::Stream(sender) => {
            if let Err(err) = sender.send(cmd).await {
                warn!("failed to send Phira retry notice: {err:?}");
            }
        }
        RetryNoticeTarget::User(user) => user.try_send(cmd).await,
    }
}

fn phira_status_retryable(status: reqwest::StatusCode, body: &str) -> bool {
    status == reqwest::StatusCode::BAD_GATEWAY
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
        || body.contains("认证失败 502错误")
}

fn phira_error_retryable(err: &reqwest::Error) -> bool {
    err.is_timeout()
        || err.is_connect()
        || err.status().is_some_and(|status| phira_status_retryable(status, ""))
        || err.to_string().contains("认证失败 502错误")
}

async fn phira_get_json_with_retry<T>(
    server: &PlusServerState,
    endpoint_override: Option<String>,
    path: &str,
    bearer: Option<&str>,
    target: RetryNoticeTarget<'_>,
) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let endpoint = endpoint_override
        .as_deref()
        .unwrap_or(&server.config.phira_api_endpoint)
        .trim_end_matches('/');
    let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
    let url = format!("{endpoint}{path}");

    for attempt in 0..=PHIRA_MAX_RETRIES {
        let mut request = client.get(&url);
        if let Some(token) = bearer {
            request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"));
        }
        match request.send().await {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    return response.json::<T>().await.map_err(Into::into);
                }
                let body = response.text().await.unwrap_or_default();
                let retryable = phira_status_retryable(status, &body);
                if retryable && attempt < PHIRA_MAX_RETRIES {
                    send_phira_retry_notice(&target).await;
                    time::sleep(Duration::from_millis(200 * (attempt as u64 + 1))).await;
                    continue;
                }
                if status == reqwest::StatusCode::BAD_GATEWAY || body.contains("认证失败 502错误") {
                    bail!("认证失败 502错误");
                }
                bail!("Phira API request failed: {status} {body}");
            }
            Err(err) if phira_error_retryable(&err) && attempt < PHIRA_MAX_RETRIES => {
                send_phira_retry_notice(&target).await;
                time::sleep(Duration::from_millis(200 * (attempt as u64 + 1))).await;
            }
            Err(err) => return Err(err.into()),
        }
    }
    bail!("Phira API request failed after retries")
}

async fn authenticate_remote(server: &PlusServerState, token: &str) -> Result<AuthUserInfo> {
    authenticate_remote_with_notice(server, token, RetryNoticeTarget::None).await
}

async fn authenticate_remote_with_notice(
    server: &PlusServerState,
    token: &str,
    target: RetryNoticeTarget<'_>,
) -> Result<AuthUserInfo> {
    if token.len() > 128 {
        bail!("invalid token");
    }
    phira_get_json_with_retry(server, None, "/me", Some(token), target).await
}

fn ban_rejection_message(language: &str, reason: &str) -> String {
    let language = language.parse::<Language>().unwrap_or_default();
    let mut reason = reason.split_whitespace().collect::<Vec<_>>().join(" ");

    if reason.is_empty() || reason == "你的账号已被封禁" {
        reason = crate::l10n::try_translate(&language.0, "auth-banned-default-reason");
    }

    if reason.chars().count() > MAX_CLIENT_BAN_REASON_CHARS {
        reason = reason
            .chars()
            .take(MAX_CLIENT_BAN_REASON_CHARS.saturating_sub(1))
            .collect::<String>();
        reason.push('…');
    }

    let mut args = fluent::FluentArgs::new();
    args.set("reason", reason);
    crate::l10n::try_translate_with_args(&language.0, "auth-banned", args)
        .chars()
        .filter(|ch| !matches!(ch, '\u{2068}' | '\u{2069}'))
        .collect()
}

async fn send_auth_rejection(send_tx: &StreamSender<ServerCommand>, message: String) {
    time::sleep(AUTH_FAILURE_RESPONSE_DELAY).await;
    match time::timeout(
        AUTH_FAILURE_FLUSH_TIMEOUT,
        send_tx.send_and_flush(ServerCommand::Authenticate(Err(message))),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(err)) => warn!("failed to deliver authentication rejection: {err:?}"),
        Err(_) => warn!("timed out while delivering authentication rejection"),
    }
}

pub struct User {
    pub id: i32,
    pub name: String,
    pub lang: Language,

    pub server: Arc<PlusServerState>,
    pub session: RwLock<Option<Weak<Session>>>,
    pub room: RwLock<Option<Arc<super::room::Room>>>,

    pub monitor: AtomicBool,
    pub game_time: AtomicU32,

    pub dangle_mark: Mutex<Option<Arc<()>>>,
}

impl User {
    pub fn new(id: i32, name: String, lang: Language, server: Arc<PlusServerState>) -> Self {
        Self {
            id,
            name,
            lang,

            server,
            session: RwLock::default(),
            room: RwLock::default(),

            monitor: AtomicBool::default(),
            game_time: AtomicU32::default(),

            dangle_mark: Mutex::default(),
        }
    }

    pub fn to_info(&self) -> UserInfo {
        UserInfo {
            id: self.id,
            name: self.name.clone(),
            monitor: self.monitor.load(Ordering::SeqCst),
        }
    }

    pub fn can_monitor(&self) -> bool {
        self.server.config.monitors.contains(&self.id)
    }

    pub async fn set_session(&self, session: Weak<Session>) {
        *self.session.write().await = Some(session);
        *self.dangle_mark.lock().await = None;
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
                let was_monitor = self.monitor.load(Ordering::SeqCst);
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
                self.server.plugin_manager.trigger(&PluginEvent::UserDisconnect {
                    user_id: self.id,
                    user_name: self.name.clone(),
                }).await;
                crate::internal_hooks::playtime_disconnect(self.id);
                return;
            }
        }
        self.server.plugin_manager.trigger(&PluginEvent::UserDisconnect {
            user_id: self.id,
            user_name: self.name.clone(),
        }).await;

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
                        let was_monitor = self_.monitor.load(Ordering::SeqCst);
                        if room.on_user_leave(&self_).await {
                            self_.server.rooms.write().await.remove(&room_id);
                        }
                        if !was_monitor {
                            self_.server
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
                                        // 计算 token 哈希作为缓存键（使用 SHA-256）
                                        use sha2::{Sha256, Digest};
                                        let token_hash = u64::from_le_bytes(
                                            Sha256::digest(token.as_bytes())[..8].try_into().unwrap()
                                        );

                                        let user_info = {
                                            let ac = server.extensions.get_auth_cache().await;
                                            ac.get(&token_hash).cloned()
                                        };
                                        let user_info = if let Some(entry) = user_info {
                                            // 封禁检查（拒绝前不走 API，毫秒级响应）
                                            if let Some(reason) = server.ban_manager.ban_reason(entry.user_id).await {
                                                warn!("banned user {}({}) tried to connect (cache)", entry.name, entry.user_id);
                                                bail!("{}", ban_rejection_message(&entry.language, &reason));
                                            }
                                            debug!("cache hit for user {}", entry.user_id);
                                            AuthUserInfo {
                                                id: entry.user_id,
                                                name: entry.name,
                                                language: entry.language,
                                            }
                                        } else {
                                            // 缓存未命中，请求 API；遇到 “认证失败 502错误”/502/5xx 时重试并提示该客户端。
                                            match phira_get_json_with_retry::<AuthUserInfo>(
                                                &server,
                                                None,
                                                "/me",
                                                Some(&token),
                                                RetryNoticeTarget::Stream(retry_send_tx.as_ref()),
                                            ).await {
                                                Ok(info) => {
                                                    // API 成功，更新缓存并持久化
                                                    server.extensions.update_auth_cache(
                                                        token_hash,
                                                        crate::extensions::AuthCacheEntry {
                                                            user_id: info.id,
                                                            name: info.name.clone(),
                                                            language: info.language.clone(),
                                                        },
                                                    ).await;
                                                    // 封禁检查
                                                    if let Some(reason) = server.ban_manager.ban_reason(info.id).await {
                                                        warn!("banned user {}({}) tried to connect", info.name, info.id);
                                                        bail!("{}", ban_rejection_message(&info.language, &reason));
                                                    }
                                                    info
                                                }
                                                Err(err) => {
                                                    bail!("{}", err);
                                                }
                                            }
                                        };

                                        let mut users_guard = server.users.write().await;
                                        if let Some(user) = users_guard.get(&user_info.id) {
                                            info!("reconnect");
                                            let _ = auth_tx.take().unwrap().send(
                                                AuthenticationOutcome::Accepted(
                                                    Arc::clone(user),
                                                    SessionCategory::Normal,
                                                ),
                                            );
                                            this_inited.notified().await;
                                            user.set_session(Arc::downgrade(this.get().unwrap()))
                                                .await;
                                            // 重连也需要触发 UserConnect（欢迎语等依赖此事件）
                                            let user_ip = this.get().map(|s| s.ip.clone()).unwrap_or_default();
                                            server.plugin_manager
                                                .trigger(&PluginEvent::UserConnect {
                                                    user_id: user_info.id,
                                                    user_name: user_info.name.clone(),
                                                    user_ip,
                                                })
                                                .await;
                                            let online = {
                                                let rooms = server.rooms.read().await;
                                                let mut in_room: std::collections::HashSet<i32> = std::collections::HashSet::new();
                                                for (_id, room) in rooms.iter() {
                                                    for u in room.users().await {
                                                        in_room.insert(u.id);
                                                    }
                                                }
                                                in_room.len()
                                            };
                                            drop(users_guard);
                                            crate::internal_hooks::track_player(user_info.id, &user_info.name);
                                            crate::internal_hooks::playtime_connect(user_info.id);
                                            crate::internal_hooks::send_welcome(user_info.id, &user_info.name, online, &server);
                                        } else {
                                            let user = Arc::new(User::new(
                                                user_info.id,
                                                user_info.name,
                                                user_info.language
                                                    .parse()
                                                    .map(Language)
                                                    .unwrap_or_default(),
                                                Arc::clone(&server),
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
                                            users_guard.insert(user_info.id, Arc::clone(&user));

                                            let user_ip = this.get().map(|s| s.ip.clone()).unwrap_or_default();
                                            server.plugin_manager
                                                .trigger(&PluginEvent::UserConnect {
                                                    user_id: user_info.id,
                                                    user_name: user.name.clone(),
                                                    user_ip,
                                                })
                                                .await;
                                            let online = {
                                                let rooms = server.rooms.read().await;
                                                let mut in_room: std::collections::HashSet<i32> = std::collections::HashSet::new();
                                                for (_id, room) in rooms.iter() {
                                                    for u in room.users().await {
                                                        in_room.insert(u.id);
                                                    }
                                                }
                                                in_room.len()
                                            };
                                            drop(users_guard);
                                            crate::internal_hooks::track_player(user_info.id, &user.name);
                                            crate::internal_hooks::playtime_connect(user_info.id);
                                            crate::internal_hooks::send_welcome(user_info.id, &user.name, online, &server);
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
                                            mon.stream.send(ServerCommand::UserVisit(uid)).await.ok();
                                        }
                                    });
                                    waiting_for_authenticate.store(false, Ordering::SeqCst);
                                }
                                return;
                            } else if let ClientCommand::ConsoleAuthenticate { token } = &cmd {
                                let Some(tx) = tx else { return };
                                match authenticate_remote_with_notice(&server, token, RetryNoticeTarget::Stream(send_tx.as_ref())).await {
                                    Ok(info) => {
                                        let user = Arc::new(User::new(
                                            info.id,
                                            info.name,
                                            info.language.parse().map(Language).unwrap_or_default(),
                                            Arc::clone(&server),
                                        ));
                                        let _ = tx.send(AuthenticationOutcome::Accepted(
                                            Arc::clone(&user),
                                            SessionCategory::Console,
                                        ));
                                        this_inited.notified().await;
                                        user.set_session(Arc::downgrade(this.get().unwrap())).await;
                                        let _ = send_tx
                                            .send(ServerCommand::Authenticate(Ok((user.to_info(), None))))
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
                                if server.room_monitor.read().await.as_ref().and_then(|w| w.upgrade()).is_some() {
                                    send_auth_rejection(
                                        &send_tx,
                                        "more than one room monitor".into(),
                                    )
                                    .await;
                                    let _ = tx.send(AuthenticationOutcome::Rejected);
                                    panicked.store(true, Ordering::SeqCst);
                                    return;
                                }
                                if server.room_monitor_key.as_slice() != key.as_slice() {
                                    send_auth_rejection(&send_tx, "secret key mismatch".into())
                                        .await;
                                    let _ = tx.send(AuthenticationOutcome::Rejected);
                                    panicked.store(true, Ordering::SeqCst);
                                    return;
                                }
                                info!("new room monitor connected");
                                let user = Arc::new(User::new(-1, "$server_room_monitor".into(), Language::default(), Arc::clone(&server)));
                                let _ = tx.send(AuthenticationOutcome::Accepted(
                                    Arc::clone(&user),
                                    SessionCategory::RoomMonitor,
                                ));
                                this_inited.notified().await;
                                user.set_session(Arc::downgrade(this.get().unwrap())).await;
                                *server.room_monitor.write().await = Some(Arc::downgrade(this.get().unwrap()));
                                let _ = send_tx.send(ServerCommand::Authenticate(Ok((user.to_info(), None)))).await;
                                waiting_for_authenticate.store(false, Ordering::SeqCst);
                                return;
                            } else if let ClientCommand::GameMonitorAuthenticate { token } = &cmd {
                                let Some(tx) = tx else { return };
                                match authenticate_remote_with_notice(&server, token, RetryNoticeTarget::Stream(send_tx.as_ref())).await {
                                    Ok(info) => {
                                        let Some(monitor_id) = info.id.checked_neg().filter(|id| *id < 0) else {
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
                                        ));
                                        let _ = tx.send(AuthenticationOutcome::Accepted(
                                            Arc::clone(&user),
                                            SessionCategory::GameMonitor,
                                        ));
                                        this_inited.notified().await;
                                        user.set_session(Arc::downgrade(this.get().unwrap())).await;
                                        server.users.write().await.insert(monitor_id, Arc::clone(&user));
                                        server
                                            .set_game_monitor(monitor_id, Arc::downgrade(this.get().unwrap()))
                                            .await;
                                        let _ = send_tx
                                            .send(ServerCommand::Authenticate(Ok((user.to_info(), None))))
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
                        let needs_limiting = matches!(&cmd,
                            ClientCommand::Chat { .. }
                            | ClientCommand::CreateRoom { .. }
                            | ClientCommand::JoinRoom { .. }
                            | ClientCommand::SelectChart { .. }
                        );
                        if needs_limiting {
                            if let Some(session) = session_ref {
                                let category = match &cmd {
                                    ClientCommand::Chat { .. } => crate::rate_limiter::CommandCategory::Chat,
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
                                process(user, this.get().unwrap().category, cmd),
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

async fn process(user: Arc<User>, category: SessionCategory, cmd: ClientCommand) -> Option<ServerCommand> {
    #[inline]
    fn err_to_str<T>(result: Result<T>) -> Result<T, String> {
        result.map_err(|it| it.to_string())
    }

    macro_rules! get_room {
        (~ $d:ident) => {
            let $d = match user.room.read().await.as_ref().map(Arc::clone) {
                Some(room) => room,
                None => {
                    warn!("no room");
                    return None;
                }
            };
        };
        ($d:ident) => {
            let $d = user
                .room
                .read()
                .await
                .as_ref()
                .map(Arc::clone)
                .ok_or_else(|| anyhow!("{}", tl!("no-room")))?;
        };
        ($d:ident, $($pt:tt)*) => {
            let $d = user
                .room
                .read()
                .await
                .as_ref()
                .map(Arc::clone)
                .ok_or_else(|| anyhow!("{}", tl!("no-room")))?;
            if !matches!(&*$d.state.read().await, $($pt)*) {
                bail!("{}", tl!("invalid-state"));
            }
        };
    }
    let permitted = match category {
        SessionCategory::Normal => !matches!(&cmd, ClientCommand::QueryRoomInfo),
        SessionCategory::GameMonitor => matches!(
            &cmd,
            ClientCommand::JoinRoom { monitor: true, .. }
                | ClientCommand::LeaveRoom
                | ClientCommand::Ready
                | ClientCommand::CancelReady
                | ClientCommand::Authenticate { .. }
                | ClientCommand::ConsoleAuthenticate { .. }
                | ClientCommand::RoomMonitorAuthenticate { .. }
                | ClientCommand::GameMonitorAuthenticate { .. }
        ),
        SessionCategory::RoomMonitor => matches!(
            &cmd,
            ClientCommand::QueryRoomInfo
                | ClientCommand::Authenticate { .. }
                | ClientCommand::ConsoleAuthenticate { .. }
                | ClientCommand::RoomMonitorAuthenticate { .. }
                | ClientCommand::GameMonitorAuthenticate { .. }
        ),
        SessionCategory::Console => matches!(
            &cmd,
            ClientCommand::Authenticate { .. }
                | ClientCommand::ConsoleAuthenticate { .. }
                | ClientCommand::RoomMonitorAuthenticate { .. }
                | ClientCommand::GameMonitorAuthenticate { .. }
        ),
    };
    if !permitted {
        warn!(user = user.id, ?category, ?cmd, "command rejected for session category");
        return None;
    }

    match cmd {
        ClientCommand::Ping => unreachable!(),
        ClientCommand::Authenticate { .. } => Some(ServerCommand::Authenticate(Err(
            tl!("repeated-authenticate"),
        ))),
        ClientCommand::Chat { message } => {
            let res: Result<()> = async move {
                get_room!(room);
                room.send_as(&user, message.into_inner()).await;
                Ok(())
            }
            .await;
            Some(ServerCommand::Chat(err_to_str(res)))
        }
        ClientCommand::Touches { frames } => {
            get_room!(~ room);
            if room.has_active_monitors().await {
                debug!("received {} touch events from {}", frames.len(), user.id);
                if let Some(frame) = frames.last() {
                    user.game_time.store(frame.time.to_bits(), Ordering::SeqCst);
                }
                let touch_data: Vec<crate::plugin::TouchEventPoint> = frames
                    .iter()
                    .flat_map(|frame| {
                        frame.points.iter().map(|(finger, pos)| {
                            crate::plugin::TouchEventPoint {
                                time: frame.time,
                                finger: *finger,
                                x: pos.x(),
                                y: pos.y(),
                            }
                        })
                    })
                    .collect();
                if !touch_data.is_empty() {
                    room.store_player_touches(user.id, &touch_data).await;
                    if let Some(rid) = room.current_round_id.read().await.as_ref() {
                        if let Some(rs) = &room.round_store {
                            rs.append_touches(&rid.to_string(), user.id, &touch_data).await;
                        }
                    }
                }
                let pm = Arc::clone(&user.server.plugin_manager);
                let room_id = room.id.to_string();
                let touch_data_for_event = touch_data.clone();
                let uid = user.id;
                tokio::spawn(async move {
                    pm.trigger(&PluginEvent::PlayerTouches {
                        user_id: uid,
                        room_id,
                        data: touch_data_for_event,
                    }).await;
                });
                tokio::spawn(async move {
                    room.broadcast_monitors(ServerCommand::Touches {
                        player: user.id,
                        frames,
                    })
                    .await;
                });
            } else {
                warn!("received touch events in non-live mode");
            }
            None
        }
        ClientCommand::Judges { judges } => {
            get_room!(~ room);
            if room.has_active_monitors().await {
                debug!("received {} judge events from {}", judges.len(), user.id);
                let judge_data: Vec<crate::plugin::JudgeEventItem> = judges
                    .iter()
                    .map(|j| crate::plugin::JudgeEventItem {
                        time: j.time,
                        line_id: j.line_id,
                        note_id: j.note_id,
                        judgement: format!("{:?}", j.judgement),
                    })
                    .collect();
                if !judge_data.is_empty() {
                    room.store_player_judges(user.id, &judge_data).await;
                    if let Some(rid) = room.current_round_id.read().await.as_ref() {
                        if let Some(rs) = &room.round_store {
                            rs.append_judges(&rid.to_string(), user.id, &judge_data).await;
                        }
                    }
                }
                let pm = Arc::clone(&user.server.plugin_manager);
                let room_id = room.id.to_string();
                let judge_data_for_event = judge_data.clone();
                let uid = user.id;
                tokio::spawn(async move {
                    pm.trigger(&PluginEvent::PlayerJudges {
                        user_id: uid,
                        room_id,
                        data: judge_data_for_event,
                    }).await;
                });
                tokio::spawn(async move {
                    room.broadcast_monitors(ServerCommand::Judges {
                        player: user.id,
                        judges,
                    })
                    .await;
                });
            } else {
                warn!("received judge events in non-live mode");
            }
            None
        }
        ClientCommand::CreateRoom { id } => {
            let res: Result<()> = async move {
                let mut room_guard = user.room.write().await;
                if room_guard.is_some() {
                    bail!(tl!("already-in-room"));
                }

                let mut map_guard = user.server.rooms.write().await;
                let max_users = user.server.config.max_users_per_room.unwrap_or(8);
                let room = Arc::new(crate::room::Room::new(
                    id.clone(),
                    Arc::downgrade(&user),
                    Some(Arc::clone(&user.server.plugin_manager)),
                    Arc::downgrade(&user.server),
                    max_users,
                    Some(Arc::clone(&user.server.round_store)),
                ));
                match map_guard.entry(id.clone()) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(Arc::clone(&room));
                    }
                    std::collections::hash_map::Entry::Occupied(_) => {
                        bail!(tl!("create-id-occupied"));
                    }
                }
                let room_uuid = room.uuid;
                room.send(Message::CreateRoom { user: user.id }).await;
                drop(map_guard);
                user.server
                    .publish_room_event(RoomEvent::CreateRoom {
                        room: id.clone(),
                        data: crate::room::Room::into_data(&room).await,
                    })
                    .await;
                *room_guard = Some(room);

                info!(user = user.id, room = id.to_string(), room_uuid = %room_uuid, "user create room");
                info!("房间 '{}' 唯一标识: {}", id, room_uuid);

                user.server.plugin_manager
                    .trigger(&PluginEvent::RoomCreate {
                        user_id: user.id,
                        room_id: id.to_string(),
                    })
                    .await;

                Ok(())
            }
            .await;
            Some(ServerCommand::CreateRoom(err_to_str(res)))
        }
        ClientCommand::JoinRoom { id, monitor } => {
            let res: Result<JoinRoomResponse> = async move {
                let mut room_guard = user.room.write().await;
                if room_guard.is_some() {
                    bail!(tl!("already-in-room"));
                }
                let room = user.server.rooms.read().await.get(&id).map(Arc::clone);
                let Some(room) = room else {
                    bail!(tl!("room-not-found"))
                };
                if room.locked.load(Ordering::SeqCst) {
                    bail!(tl!("join-room-locked"));
                }
                if !matches!(*room.state.read().await, crate::room::InternalRoomState::SelectChart) {
                    bail!(tl!("join-game-ongoing"));
                }
                // GameMonitor 会话（user.id < 0）可以旁观任意房间
                if monitor && !user.can_monitor() && user.id > 0 {
                    bail!(tl!("join-cant-monitor"));
                }
                if !room.add_user(Arc::downgrade(&user), monitor).await {
                    bail!(tl!("join-room-full"));
                }
                info!(
                    user = user.id,
                    room = id.to_string(),
                    monitor,
                    "user join room"
                );
                user.monitor.store(monitor, Ordering::SeqCst);
                if monitor && !room.live.fetch_or(true, Ordering::SeqCst) {
                    info!(room = id.to_string(), "room goes live");
                }
                let join = ServerCommand::OnJoinRoom(user.to_info());
                let message = ServerCommand::Message(Message::JoinRoom {
                    user: user.id,
                    name: user.name.clone(),
                });
                if monitor {
                    room.broadcast_players(join).await;
                    room.broadcast_players(message).await;
                } else {
                    room.broadcast(join).await;
                    room.broadcast(message).await;
                    user.server
                        .publish_room_event(RoomEvent::JoinRoom {
                            room: id.clone(),
                            user: user.id,
                        })
                        .await;
                }
                *room_guard = Some(Arc::clone(&room));
                if !monitor {
                    room.set_host_if_missing(&user).await;
                }
                user.server.refresh_room_display_names(&room).await;

                // Protocol-only game monitors are not exposed as players to plugins.
                if category == SessionCategory::Normal {
                    user.server.plugin_manager
                        .trigger(&PluginEvent::RoomJoin {
                            user_id: user.id,
                            room_id: id.to_string(),
                            is_monitor: monitor,
                        })
                        .await;
                }

                let mut users = room.users().await;
                if category != SessionCategory::GameMonitor {
                    users.extend(room.monitors().await);
                }

                Ok(JoinRoomResponse {
                    state: room.client_room_state().await,
                    users: users.into_iter().map(|user| user.to_info()).collect(),
                    live: room.is_live(),
                })
            }
            .await;
            Some(ServerCommand::JoinRoom(err_to_str(res)))
        }
        ClientCommand::LeaveRoom => {
            let res: Result<()> = async move {
                get_room!(room);
                let room_id = room.id.clone();
                info!(
                    user = user.id,
                    room = room.id.to_string(),
                    "user leave room"
                );
                let was_monitor = user.monitor.load(Ordering::SeqCst);
                if room.on_user_leave(&user).await {
                    user.server.rooms.write().await.remove(&room.id);
                }
                if category == SessionCategory::Normal && !was_monitor {
                    user.server
                        .publish_room_event(RoomEvent::LeaveRoom {
                            room: room.id.clone(),
                            user: user.id,
                        })
                        .await;
                }

                if category == SessionCategory::Normal {
                    user.server.plugin_manager
                        .trigger(&PluginEvent::RoomLeave {
                            user_id: user.id,
                            room_id: room_id.to_string(),
                        })
                        .await;
                }

                Ok(())
            }
            .await;
            Some(ServerCommand::LeaveRoom(err_to_str(res)))
        }
        ClientCommand::LockRoom { lock } => {
            let res: Result<()> = async move {
                get_room!(room);
                room.check_host(&user).await?;
                info!(
                    user = user.id,
                    room = room.id.to_string(),
                    lock,
                    "lock room"
                );
                room.locked.store(lock, Ordering::SeqCst);
                room.send(Message::LockRoom { lock }).await;
                room.publish_update(PartialRoomData {
                    lock: Some(lock),
                    ..Default::default()
                })
                .await;

                user.server.plugin_manager
                    .trigger(&PluginEvent::RoomModify {
                        user_id: user.id,
                        room_id: room.id.to_string(),
                        data: format!(r#"{{"action":"lock","value":{lock}}}"#),
                    })
                    .await;

                Ok(())
            }
            .await;
            Some(ServerCommand::LockRoom(err_to_str(res)))
        }
        ClientCommand::CycleRoom { cycle } => {
            let res: Result<()> = async move {
                get_room!(room);
                room.check_host(&user).await?;
                info!(
                    user = user.id,
                    room = room.id.to_string(),
                    cycle,
                    "cycle room"
                );
                room.cycle.store(cycle, Ordering::SeqCst);
                room.send(Message::CycleRoom { cycle }).await;
                room.publish_update(PartialRoomData {
                    cycle: Some(cycle),
                    ..Default::default()
                })
                .await;

                user.server.plugin_manager
                    .trigger(&PluginEvent::RoomModify {
                        user_id: user.id,
                        room_id: room.id.to_string(),
                        data: format!(r#"{{"action":"cycle","value":{cycle}}}"#),
                    })
                    .await;

                Ok(())
            }
            .await;
            Some(ServerCommand::CycleRoom(err_to_str(res)))
        }
        ClientCommand::SelectChart { id } => {
            let res: Result<()> = async move {
                get_room!(room, crate::room::InternalRoomState::SelectChart);
                room.check_host(&user).await?;
                let span = debug_span!(
                    "select chart",
                    user = user.id,
                    room = room.id.to_string(),
                    chart = id,
                );
                async move {
                    trace!("fetch");
                    let endpoint = room.effective_phira_api_endpoint(&user.server).await;
                    let res: crate::server::Chart = phira_get_json_with_retry(
                        &user.server,
                        Some(endpoint),
                        &format!("/chart/{id}"),
                        None,
                        RetryNoticeTarget::User(user.as_ref()),
                    ).await?;
                    debug!("chart is {res:?}");
                    room.send(Message::SelectChart {
                        user: user.id,
                        name: res.name.clone(),
                        id: res.id,
                    })
                    .await;
                    *room.chart.write().await = Some(res);
                    room.on_state_change().await;
                    room.publish_update(PartialRoomData {
                        chart: Some(id),
                        ..Default::default()
                    })
                    .await;
                    Ok(())
                }
                .instrument(span)
                .await
            }
            .await;
            Some(ServerCommand::SelectChart(err_to_str(res)))
        }
        ClientCommand::RequestStart => {
            let res: Result<()> = async move {
                get_room!(room, crate::room::InternalRoomState::SelectChart);
                room.check_host(&user).await?;
                if room.admin_start_pending() {
                    bail!("administrative start is already in progress");
                }
                if room.chart.read().await.is_none() {
                    bail!(tl!("start-no-chart-selected"));
                }
                debug!(room = room.id.to_string(), "room wait for ready");
                room.finish_admin_start().await;
                room.reset_game_time().await;
                room.send(Message::GameStart { user: user.id }).await;
                *room.state.write().await = crate::room::InternalRoomState::WaitForReady {
                    started: std::iter::once(user.id).collect(),
                };
                room.on_state_change().await;
                room.check_all_ready().await;

                user.server.plugin_manager
                    .trigger(&PluginEvent::GameStart {
                        user_id: user.id,
                        room_id: room.id.to_string(),
                    })
                    .await;

                Ok(())
            }
            .await;
            Some(ServerCommand::RequestStart(err_to_str(res)))
        }
        ClientCommand::Ready => {
            let res: Result<()> = async move {
                get_room!(room);
                let mut guard = room.state.write().await;
                if let crate::room::InternalRoomState::WaitForReady { started } = &mut *guard {
                    if !started.insert(user.id) {
                        bail!(tl!("already-ready"));
                    }
                    room.send(Message::Ready { user: user.id }).await;
                    drop(guard);
                    room.check_all_ready().await;
                }
                Ok(())
            }
            .await;
            Some(ServerCommand::Ready(err_to_str(res)))
        }
        ClientCommand::CancelReady => {
            let res: Result<()> = async move {
                get_room!(room);
                let mut guard = room.state.write().await;
                if let crate::room::InternalRoomState::WaitForReady { started } = &mut *guard {
                    if !started.remove(&user.id) {
                        bail!(tl!("not-ready"));
                    }
                    if room.check_host(&user).await.is_ok() {
                        room.send(Message::CancelGame { user: user.id }).await;
                        *guard = crate::room::InternalRoomState::SelectChart;
                        drop(guard);
                        room.finish_admin_start().await;
                        room.on_state_change().await;
                    } else {
                        room.send(Message::CancelReady { user: user.id }).await;
                    }
                }
                Ok(())
            }
            .await;
            Some(ServerCommand::CancelReady(err_to_str(res)))
        }
        ClientCommand::Played { id } => {
            let res: Result<()> = async move {
                get_room!(room);
                let endpoint = room.effective_phira_api_endpoint(&user.server).await;
                let res: crate::server::Record = phira_get_json_with_retry(
                    &user.server,
                    Some(endpoint),
                    &format!("/record/{id}"),
                    None,
                    RetryNoticeTarget::User(user.as_ref()),
                ).await?;
                if res.player != user.id {
                    bail!(tl!("invalid-record"));
                }
                debug!(
                    room = room.id.to_string(),
                    user = user.id,
                    "user played: {res:?}"
                );
                room.send(Message::Played {
                    user: user.id,
                    score: res.score,
                    accuracy: res.accuracy,
                    full_combo: res.full_combo,
                })
                .await;
                let score = res.score;
                let accuracy = res.accuracy;
                let perfect = res.perfect;
                let good = res.good;
                let bad = res.bad;
                let miss = res.miss;
                let max_combo = res.max_combo;
                let full_combo = res.full_combo;
                let mut guard = room.state.write().await;
                if let crate::room::InternalRoomState::Playing { results, aborted } = &mut *guard {
                    if aborted.contains(&user.id) {
                        bail!("aborted");
                    }
                    if results.insert(user.id, res).is_some() {
                        bail!(tl!("already-uploaded"));
                    }
                    drop(guard);
                    room.check_all_ready().await;

                    user.server.plugin_manager
                        .trigger(&PluginEvent::GameEnd {
                            user_id: user.id,
                            user_name: user.name.clone(),
                            room_id: room.id.to_string(),
                            score,
                            accuracy,
                            perfect,
                            good,
                            bad,
                            miss,
                            max_combo,
                            full_combo,
                        })
                        .await;
                }
                Ok(())
            }
            .await;
            Some(ServerCommand::Played(err_to_str(res)))
        }
        ClientCommand::Abort => {
            let res: Result<()> = async move {
                get_room!(room);
                let mut guard = room.state.write().await;
                if let crate::room::InternalRoomState::Playing { results, aborted } = &mut *guard {
                    if results.contains_key(&user.id) {
                        bail!(tl!("already-uploaded"));
                    }
                    if !aborted.insert(user.id) {
                        bail!(tl!("aborted"));
                    }
                    drop(guard);
                    room.send(Message::Abort { user: user.id }).await;
                    room.check_all_ready().await;
                }
                Ok(())
            }
            .await;
            Some(ServerCommand::Abort(err_to_str(res)))
        }
        ClientCommand::QueryRoomInfo => {
            let res: Result<ServerCommand> = async move {
                let rooms_guard = user.server.rooms.read().await;
                let mut info: std::collections::HashMap<phira_mp_common::RoomId, phira_mp_common::RoomData> = std::collections::HashMap::new();
                let mut user_room_map: std::collections::HashMap<i32, phira_mp_common::RoomId> = std::collections::HashMap::new();
                for (id, room) in rooms_guard.iter() {
                    for u in room.users().await {
                        user_room_map.insert(u.id, id.clone());
                    }
                    info.insert(id.clone(), crate::room::Room::into_data(room).await);
                }
                drop(rooms_guard);
                Ok(ServerCommand::RoomResponse(Ok((info, user_room_map))))
            }
            .await;
            match res {
                Ok(cmd) => Some(cmd),
                Err(_) => None,
            }
        }
        ClientCommand::RoomMonitorAuthenticate { .. } | ClientCommand::GameMonitorAuthenticate { .. } | ClientCommand::ConsoleAuthenticate { .. } => {
            Some(ServerCommand::Authenticate(Err("already authenticated".into())))
        }
    }
}

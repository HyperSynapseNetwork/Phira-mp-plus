//! Client sessions, authentication, and command dispatch.

pub use crate::session_lifecycle::User;

use crate::l10n::{Language, LANGUAGE};
use crate::phira_client::PhiraRetryNoticeTarget;
use crate::server::PlusServerState;
use crate::session_auth::{
    authenticate_remote_with_notice, ban_rejection_message, resolve_phira_api_endpoint,
    send_auth_rejection, AuthUserInfo,
};
use anyhow::{anyhow, bail, Result};
use phira_mp_common::{ClientCommand, Message, ServerCommand, Stream};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    time::{Duration, Instant},
};
use tokio::{
    net::TcpStream,
    sync::{mpsc, Mutex, Notify, OnceCell, OwnedSemaphorePermit},
    task::JoinHandle,
    time,
};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

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
                                            let endpoint = resolve_phira_api_endpoint(&server).await;
                                            match server
                                                .phira_client
                                                .get_json::<AuthUserInfo>(
                                                    &endpoint,
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
                                        Some(room) => Some(crate::session_room::build_client_room_state(room, user).await),
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
                                    crate::internal_hooks::track_player(user.id, &user.name);
                                    crate::internal_hooks::send_welcome(
                                        user.id,
                                        &user.name,
                                        online,
                                        &server,
                                    );
                                            },
                                        );
                                    }
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
                                        // Initialize per-session mailbox for command routing
                                        if let Some(session) = this.get() {
                                            let tx = crate::session_actor::init_session_mailbox(session);
                                            let _ = session.actor_tx.set(tx);
                                        }
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
                                // Authenticate via shared key derived from HSN_SECRET_KEY.
                                let expected = phira_mp_common::generate_secret_key("room_monitor", 64);
                                match expected {
                                    Ok(expected_key) if expected_key.as_slice() == key.as_ref() => {
                                        info!("room monitor authenticated");
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
                                        if let Some(session) = this.get() {
                                            let tx = crate::session_actor::init_session_mailbox(session);
                                            let _ = session.actor_tx.set(tx);
                                        }
                                        *server.room_monitor.write().await =
                                            Some(Arc::downgrade(this.get().unwrap()));
                                        let _ = send_tx
                                            .send(ServerCommand::Authenticate(Ok((user.to_info(), None))))
                                            .await;
                                        waiting_for_authenticate.store(false, Ordering::SeqCst);
                                    }
                                    _ => {
                                        warn!("room monitor authentication failed (shared key mismatch)");
                                        send_auth_rejection(
                                            &send_tx,
                                            "room monitor authentication failed".into(),
                                        )
                                        .await;
                                        let _ = tx.send(AuthenticationOutcome::Rejected);
                                        panicked.store(true, Ordering::SeqCst);
                                    }
                                }
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
                                        if !server.config.monitors.contains(&info.id) {
                                            send_auth_rejection(
                                                &send_tx,
                                                format!("user {} is not in the monitor whitelist", info.id),
                                            )
                                            .await;
                                            let _ = tx.send(AuthenticationOutcome::Rejected);
                                            panicked.store(true, Ordering::SeqCst);
                                            return;
                                        }
                                        let user = Arc::new(User::new(
                                            info.id,
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
                                        // Initialize per-session mailbox for command routing
                                        if let Some(session) = this.get() {
                                            let tx = crate::session_actor::init_session_mailbox(session);
                                            let _ = session.actor_tx.set(tx);
                                        }
                                        server
                                            .users
                                            .write()
                                            .await
                                            .insert(info.id, Arc::clone(&user));
                                        server
                                            .set_game_monitor(
                                                info.id,
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
                            }
                            // NOTE: Do NOT send ChangeHost(true) after JoinRoom(Ok) here.
                            // assign_room_host_if_missing() in join_room already broadcasts
                            // ChangeHost through the room mailbox SetHost handler. Sending it
                            // again here duplicates the message and can arrive out of order
                            // (before JoinRoom(Ok)), confusing the client.
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

//! Client sessions, authentication, and command dispatch.

pub use crate::session_lifecycle::User;

use crate::l10n::{Language, LANGUAGE};
use crate::official_client_compat::protocol_trace::ProtocolTrace;
use crate::phira_client::PhiraRetryNoticeTarget;
use crate::server::PlusServerState;
use crate::session_auth::{
    authenticate_remote_with_notice, ban_rejection_message, resolve_phira_api_endpoint,
    send_auth_rejection, AuthUserInfo,
};
use anyhow::{anyhow, bail, Result};
use phira_mp_common::{ClientCommand, Message, ServerCommand, Stream, StreamSender};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock, Weak,
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

/// Deliver a dispatch response honoring the P0-B minimum-latency window (the
/// caller waits for that separately) and the P0-E critical flush.  Returns
/// `true` when the packet entered the outbound send queue.  A critical flush
/// failure closes the transport and enters the existing lost-connection path.
///
/// PMP44 P0-J: when an absolute `deadline` is present, the critical flush is
/// bounded by the remaining budget — a flush that outlives the deadline is
/// treated as a send failure (mark panicked, close transport).
async fn send_dispatch_response(
    send_tx: &StreamSender<ServerCommand>,
    resp: ServerCommand,
    critical: bool,
    received_at: Instant,
    id: Uuid,
    server: &PlusServerState,
    panicked: &AtomicBool,
    deadline: Option<Instant>,
) -> bool {
    let result = if critical {
        let fut = send_tx.send_and_flush(resp);
        match deadline {
            Some(d) => {
                let remaining = d.saturating_duration_since(Instant::now());
                match tokio::time::timeout(remaining, fut).await {
                    Ok(r) => r,
                    Err(_) => {
                        // 响应 flush 超出绝对预算——视为发送失败。
                        Err(anyhow!("response flush exceeded deadline"))
                    }
                }
            }
            None => fut.await,
        }
    } else {
        send_tx.send(resp).await
    };
    match result {
        Ok(()) => {
            ProtocolTrace::get().response_queued.fetch_add(1, Ordering::Relaxed);
            if critical {
                ProtocolTrace::get().response_flushed.fetch_add(1, Ordering::Relaxed);
            }
            ProtocolTrace::get().record_response_latency(received_at);
            true
        }
        Err(err) => {
            error!("failed to handle message, aborting connection {id}: {err:?}");
            panicked.store(true, Ordering::SeqCst);
            if let Err(err) = server.lost_con_tx.send(id).await {
                error!("failed to mark lost connection ({id}): {err:?}");
            }
            false
        }
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

/// A stable handle to the Session that initiated a command, captured at route
/// time. Every response, error, transport close and post-response compensation
/// for that command is bound to this origin — never to the user's *current*
/// session, which may have been replaced by a reconnect (P0-A).
///
/// Two independent checks guard freshness:
/// - a generation counter snapshot compared against the user's current binding
///   generation (`user.binding.generation`, bumped by `User::set_session`), and
/// - pointer identity between the captured weak ref and the weak ref the user
///   currently stores.
#[derive(Debug, Clone)]
pub struct CommandOrigin {
    pub(crate) session: Weak<Session>,
    pub(crate) generation: u64,
}

/// Pure two-part staleness decision, factored out for unit testing: an origin
/// is current only when its generation snapshot still matches the user's
/// current generation AND the captured Session is still the one the user is on.
pub(crate) fn origin_is_current(
    snapshot_generation: u64,
    current_generation: u64,
    same_session: bool,
) -> bool {
    snapshot_generation == current_generation && same_session
}

impl CommandOrigin {
    pub(crate) async fn is_current(&self) -> bool {
        let Some(session) = self.session.upgrade() else {
            return false;
        };
        let user = &session.user;
        let binding = user.binding.read().await;
        let current_generation = binding.generation;
        let current = binding.session.as_ref().and_then(Weak::upgrade);
        let same_session = current.as_ref().is_some_and(|cur| Arc::ptr_eq(cur, &session));
        origin_is_current(self.generation, current_generation, same_session)
    }

    /// Send a best-effort packet to the origin session. Returns `false` and
    /// drops the packet when the origin is stale — a superseded session must
    /// never receive a response intended for an old command (P0-A).
    pub(crate) async fn try_send(&self, cmd: ServerCommand) -> bool {
        if !self.is_current().await {
            debug!(generation = self.generation, "dropping response for stale session origin");
            return false;
        }
        let Some(session) = self.session.upgrade() else {
            debug!("dropping response: origin session already dropped");
            return false;
        };
        session.try_send(cmd).await;
        true
    }

    /// Send a critical packet to the origin session, waiting for capacity and
    /// flushing it to the socket. Returns `Err` when the origin is stale or the
    /// send queue failed.
    pub(crate) async fn send_and_flush(&self, cmd: ServerCommand) -> Result<()> {
        if !self.is_current().await {
            return Err(anyhow!("stale session origin"));
        }
        let Some(session) = self.session.upgrade() else {
            return Err(anyhow!("origin session gone"));
        };
        session.send_and_flush(cmd).await
    }

    /// Close ONLY this origin's transport. A superseded origin is still safe to
    /// close — it is the old connection being torn down — while the user's
    /// current session is never touched (P0-A / P0-D).
    pub(crate) async fn close_uncertain(&self) {
        let Some(session) = self.session.upgrade() else {
            return;
        };
        let stale = !self.is_current().await;
        tracing::warn!(
            user = session.user.id,
            session = %session.id,
            stale,
            "closing origin transport: uncertain command outcome"
        );
        session.stream.close();
        let _ = session.user.server.lost_con_tx.try_send(session.id);
    }

    /// PMP44 P0-C: project this origin into the lightweight room-actor token.
    /// `None` when the origin session is already dropped (the command must not
    /// commit against authoritative room state).
    pub(crate) fn to_room_origin(&self) -> crate::room_actor::command::RoomOrigin {
        self.session.upgrade().map(|s| (s.id, self.generation))
    }
}

/// Minimal outbound sink abstraction so [`SessionOutboundGate`] can be driven
/// both from `Session` methods (which hold the `Stream`) and from the auth
/// callback (which holds the `Arc<StreamSender>`).
pub(crate) trait OutboundSink {
    async fn sink_send(&self, cmd: ServerCommand) -> Result<()>;
    async fn sink_send_and_flush(&self, cmd: ServerCommand) -> Result<()>;
    fn sink_try_send(&self, cmd: ServerCommand) -> Result<()>;
}

impl OutboundSink for Stream<ServerCommand, ClientCommand> {
    async fn sink_send(&self, cmd: ServerCommand) -> Result<()> {
        self.send(cmd).await
    }
    async fn sink_send_and_flush(&self, cmd: ServerCommand) -> Result<()> {
        self.send_and_flush(cmd).await
    }
    fn sink_try_send(&self, cmd: ServerCommand) -> Result<()> {
        self.try_send(cmd)
    }
}

impl OutboundSink for StreamSender<ServerCommand> {
    async fn sink_send(&self, cmd: ServerCommand) -> Result<()> {
        self.send(cmd).await
    }
    async fn sink_send_and_flush(&self, cmd: ServerCommand) -> Result<()> {
        self.send_and_flush(cmd).await
    }
    fn sink_try_send(&self, cmd: ServerCommand) -> Result<()> {
        self.try_send(cmd)
    }
}

/// P0-B: per-session outbound activation barrier. Before the initial
/// `Authenticate(Ok)` frame is proven flushed, outbound packets (room
/// broadcasts, extension state, monitor notifications) are buffered here in
/// FIFO order instead of racing the client's authentication callback. After
/// activation they pass straight through to the transport.
pub(crate) struct SessionOutboundGate {
    activated: AtomicBool,
    pending: Mutex<VecDeque<ServerCommand>>,
}

impl SessionOutboundGate {
    pub(crate) fn new() -> Self {
        Self {
            activated: AtomicBool::new(false),
            pending: Mutex::new(VecDeque::new()),
        }
    }

    /// Queue (pre-activation) or forward (post-activation). Returns `Err` only
    /// when the forwarding send itself fails.
    pub(crate) async fn send(&self, sink: &impl OutboundSink, cmd: ServerCommand) -> Result<()> {
        let mut pending = self.pending.lock().await;
        if self.activated.load(Ordering::SeqCst) {
            drop(pending);
            sink.sink_send(cmd).await
        } else {
            pending.push_back(cmd);
            Ok(())
        }
    }

    /// Non-blocking variant used by room broadcasts. Pre-activation packets are
    /// buffered (never fail); post-activation it inherits the transport's
    /// slow-consumer failure behavior. Returns `true` when the packet was
    /// accepted (buffered or enqueued).
    pub(crate) async fn try_send(&self, sink: &impl OutboundSink, cmd: ServerCommand) -> bool {
        let mut pending = self.pending.lock().await;
        if self.activated.load(Ordering::SeqCst) {
            drop(pending);
            sink.sink_try_send(cmd).is_ok()
        } else {
            pending.push_back(cmd);
            true
        }
    }

    pub(crate) async fn send_and_flush(
        &self,
        sink: &impl OutboundSink,
        cmd: ServerCommand,
    ) -> Result<()> {
        {
            let pending = self.pending.lock().await;
            assert!(
                self.activated.load(Ordering::SeqCst),
                "send_and_flush before outbound activation"
            );
            drop(pending);
        }
        sink.sink_send_and_flush(cmd).await
    }

    /// Open the barrier and drain buffered packets in FIFO order. Must be called
    /// only after the `Authenticate(Ok)` frame has been flushed to the socket.
    pub(crate) async fn activate(&self, sink: &impl OutboundSink) {
        let mut pending = self.pending.lock().await;
        self.activated.store(true, Ordering::SeqCst);
        while let Some(cmd) = pending.pop_front() {
            if sink.sink_send(cmd).await.is_err() {
                tracing::warn!(remaining = pending.len(), "outbound gate drain failed");
                break;
            }
        }
    }
}

pub struct Session {
    pub id: Uuid,
    pub ip: String,
    pub stream: Stream<ServerCommand, ClientCommand>,
    pub user: Arc<User>,
    pub category: SessionCategory,

    /// The generation this Session was bound to its user with, set exactly once
    /// during authentication (P0-A). Network commands captured against this
    /// Session use this generation — never the user's current generation, which
    /// advances past this Session after a reconnect.
    pub bound_generation: std::sync::OnceLock<u64>,

    /// P0-B outbound activation barrier. Created before the Stream so the auth
    /// callback can open it the moment `Authenticate(Ok)` is proven flushed.
    pub(crate) gate: Arc<SessionOutboundGate>,

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
        // P0-B: outbound activation barrier. Cloned into the Stream callback so
        // the auth path can open it the moment Authenticate(Ok) is flushed.
        let gate = Arc::new(SessionOutboundGate::new());

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
                let gate = Arc::clone(&gate);
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
                    let gate = Arc::clone(&gate);
                    async move {
                        if panicked.load(Ordering::SeqCst) {
                            return;
                        }
                        *last_recv.lock().await = Instant::now();
                        if matches!(cmd, ClientCommand::Ping) {
                            let _ = send_tx.send(ServerCommand::Pong).await;
                            return;
                        }
                        if waiting_for_authenticate.load(Ordering::SeqCst) {
                            if let ClientCommand::Authenticate { token } = &cmd {
                                let Some(tx) = tx else { return };
                                // P0-B: record the Authenticate receive time at the dispatch
                                // boundary so the success response obeys the same minimum
                                // response latency as every other request-type command (§5).
                                let auth_received_at = Instant::now();
                                // PMP44 P0-D: 认证绝对预算从网络接收点开始计时，覆盖
                                // Phira API + 重试 + 退避 + WAL admission + 最低响应时延
                                // + 响应 flush。必须早于官方客户端约 7 秒 deadline。
                                let auth_deadline = auth_received_at
                                    + std::time::Duration::from_millis(
                                        server.config.compatibility.auth_deadline_ms,
                                    );
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
                                                    Some(auth_deadline),
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
                                                let guard = existing.binding.read().await;
                                                guard
                                                    .session
                                                    .as_ref()
                                                    .and_then(std::sync::Weak::upgrade)
                                            };
                                            let _ = auth_tx.take().unwrap().send(
                                                AuthenticationOutcome::Accepted(
                                                    existing.clone(),
                                                    SessionCategory::Normal,
                                                ),
                                            );
                                            this_inited.notified().await;
                                            let gen = existing
                                                .set_session(Arc::downgrade(this.get().unwrap()))
                                                .await;
                                            if let Some(session) = this.get() {
                                                let _ = session.bound_generation.set(gen);
                                            }
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
                                            let gen = user
                                                .set_session(Arc::downgrade(this.get().unwrap()))
                                                .await;
                                            if let Some(session) = this.get() {
                                                let _ = session.bound_generation.set(gen);
                                            }
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
                                    let user = Arc::clone(&this.get().unwrap().user);
                                    let room_state = match user.room.read().await.as_ref() {
                                        Some(room) => Some(crate::session_room::build_client_room_state(room, &user).await),
                                        None => None,
                                    };
                                    // ── 阻塞持久化：认证成功但尚未响应客户端 ──────────────
                                    // 在发送 Authenticate(Ok) 之前先持久化用户记录，
                                    // 确保 WAL admission 成功后才放行客户端。
                                    // PMP44 P0-D: WAL admission 前检查绝对预算——预算耗尽
                                    // 则认证失败，绝不入队（避免“服务端已注册用户但客户端
                                    // 早已超时”的幻象）。
                                    if crate::official_client_compat::timing::deadline_expired(
                                        auth_deadline,
                                    ) {
                                        warn!(
                                            user = user.id,
                                            "auth deadline elapsed before WAL admission"
                                        );
                                        send_auth_rejection(
                                            &send_tx,
                                            "authentication timed out".to_string(),
                                        )
                                        .await;
                                        panicked.store(true, Ordering::SeqCst);
                                        return;
                                    }
                                    let connected_at = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_millis() as i64)
                                        .unwrap_or(0);
                                    let event_id = Uuid::new_v4().to_string();
                                    let session_id = this.get().unwrap().id.to_string();
                                    let instance_id = crate::server_instance::current().to_string();
                                    if let Err(event) = server.persistence_worker.enqueue(
                                        crate::persistence::message::PersistenceEvent::UserAuthenticated {
                                            event_id,
                                            session_id,
                                            user_id: user.id,
                                            user_name: user.name.clone(),
                                            language: user.lang.0.to_string(),
                                            ip: addr.ip().to_string(),
                                            connected_at,
                                            server_instance_id: instance_id,
                                        }
                                    ).await {
                                        warn!(
                                            user = user.id,
                                            kind = %event.kind(),
                                            "UserAuthenticated enqueue failed — rejecting auth"
                                        );
                                        panicked.store(true, Ordering::SeqCst);
                                        return;
                                    }
                                    debug!("sending auth OK to user {}", user.id);
                                    // P0-B: the initial Authenticate response must not arrive
                                    // before the minimum response latency window (the official
                                    // client installs its Authenticate callback after send).
                                    // PMP44 P0-D: 最低响应时延等待不睡过 auth_deadline。
                                    crate::official_client_compat::timing::CompatTiming::from_config(
                                        &server.config,
                                    )
                                    .wait_until_minimum_bounded(
                                        auth_received_at,
                                        Some(auth_deadline),
                                    )
                                    .await;
                                    // PMP44 P0-D: flush 前检查——绝不发送 Authenticate(Ok)
                                    // 于预算之外。
                                    if crate::official_client_compat::timing::deadline_expired(
                                        auth_deadline,
                                    ) {
                                        warn!(
                                            user = user.id,
                                            "auth deadline elapsed before response flush"
                                        );
                                        send_auth_rejection(
                                            &send_tx,
                                            "authentication timed out".to_string(),
                                        )
                                        .await;
                                        panicked.store(true, Ordering::SeqCst);
                                        return;
                                    }
                                    let flush_remaining = auth_deadline
                                        .saturating_duration_since(Instant::now());
                                    let flush_result: Result<()> =
                                        match tokio::time::timeout(
                                            flush_remaining,
                                            send_tx.send_and_flush(ServerCommand::Authenticate(
                                                Ok((user.to_info(), room_state)),
                                            )),
                                        )
                                        .await
                                        {
                                            Ok(r) => r,
                                            Err(_) => Err(anyhow!(
                                                "auth response flush exceeded deadline"
                                            )),
                                        };
                                    if let Err(err) = flush_result {
                                        warn!(user = user.id, ?err, "failed to flush auth response");
                                        panicked.store(true, Ordering::SeqCst);
                                        return;
                                    }
                                    debug!("auth response sent");
                                    // P0-B: the auth frame is proven flushed; only now open the
                                    // outbound gate so room broadcasts buffered during the
                                    // handshake drain AFTER the client installed its callback.
                                    // PMP44 P0-D: 预算耗尽则不再激活 gate。
                                    if crate::official_client_compat::timing::deadline_expired(
                                        auth_deadline,
                                    ) {
                                        warn!(
                                            user = user.id,
                                            "auth deadline elapsed after flush; skipping gate activation"
                                        );
                                        panicked.store(true, Ordering::SeqCst);
                                        return;
                                    }
                                    gate.activate(send_tx.as_ref()).await;
                                    let auth_trace = crate::official_client_compat::protocol_trace::ProtocolTrace::get();
                                    auth_trace.response_queued.fetch_add(1, Ordering::Relaxed);
                                    auth_trace.response_flushed.fetch_add(1, Ordering::Relaxed);
                                    auth_trace.record_response_latency(auth_received_at);
                                    // ── 后台后置任务 ──────────────────────────────────────
                                    // publish_user_connected 不阻塞客户端认证响应。
                                    let uid = user.id;
                                    let uname = user.name.clone();
                                    let uip = addr.ip().to_string();
                                    let ulang = user.lang.0.to_string();
                                    let state = Arc::clone(&server);
                                    crate::supervisor_actor::spawn_named(
                                        format!("auth-post-{uid}"),
                                        async move {
                                            state.publish_user_connected(
                                                uid, uname, uip, ulang,
                                            ).await;
                                        },
                                    );
                                    // Welcome chat follows auth frame immediately (sync, fast)
                                    let online = server.users.read().await.len();
                                    crate::internal_hooks::track_player(user.id, &user.name);
                                    crate::internal_hooks::send_welcome(
                                        user.id,
                                        &user.name,
                                        online,
                                        &server,
                                    );
                                    // Room monitor notification (后台)
                                    let srv = Arc::clone(&server);
                                    crate::supervisor_actor::spawn_named(
                                        format!("room-monitor-visit-{}", user.id),
                                        async move {
                                            if let Some(mon) = srv.get_room_monitor().await {
                                                mon.stream
                                                    .send(ServerCommand::UserVisit(user.id))
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
                                let auth_received_at = Instant::now();
                                let auth_deadline = auth_received_at
                                    + std::time::Duration::from_millis(
                                        server.config.compatibility.auth_deadline_ms,
                                    );
                                match authenticate_remote_with_notice(
                                    &server,
                                    token,
                                    PhiraRetryNoticeTarget::Stream(send_tx.as_ref()),
                                    Some(auth_deadline),
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
                                        let gen = user
                                            .set_session(Arc::downgrade(this.get().unwrap()))
                                            .await;
                                        if let Some(session) = this.get() {
                                            let _ = session.bound_generation.set(gen);
                                        }
                                        // Initialize per-session mailbox for command routing
                                        if let Some(session) = this.get() {
                                            let tx = crate::session_actor::init_session_mailbox(session);
                                            let _ = session.actor_tx.set(tx);
                                        }
                                        // PMP44 P0-D: flush 前检查绝对预算——绝不发送
                                        // Authenticate(Ok) 于预算之外。
                                        if crate::official_client_compat::timing::deadline_expired(
                                            auth_deadline,
                                        ) {
                                            warn!(
                                                user = user.id,
                                                "console auth deadline elapsed before response flush"
                                            );
                                            send_auth_rejection(
                                                &send_tx,
                                                "authentication timed out".to_string(),
                                            )
                                            .await;
                                            panicked.store(true, Ordering::SeqCst);
                                            return;
                                        }
                                        crate::official_client_compat::timing::CompatTiming::from_config(
                                            &server.config,
                                        )
                                        .wait_until_minimum_bounded(
                                            auth_received_at,
                                            Some(auth_deadline),
                                        )
                                        .await;
                                        let flush_remaining = auth_deadline
                                            .saturating_duration_since(Instant::now());
                                        let flush_result: Result<()> =
                                            match tokio::time::timeout(
                                                flush_remaining,
                                                send_tx.send_and_flush(
                                                    ServerCommand::Authenticate(Ok((
                                                        user.to_info(),
                                                        None,
                                                    ))),
                                                ),
                                            )
                                            .await
                                            {
                                                Ok(r) => r,
                                                Err(_) => Err(anyhow!(
                                                    "console auth response flush exceeded deadline"
                                                )),
                                            };
                                        if let Err(err) = flush_result {
                                            warn!(user = user.id, ?err, "failed to flush console auth response");
                                            panicked.store(true, Ordering::SeqCst);
                                            return;
                                        }
                                        gate.activate(send_tx.as_ref()).await;
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
                                let auth_received_at = Instant::now();
                                let auth_deadline = auth_received_at
                                    + std::time::Duration::from_millis(
                                        server.config.compatibility.auth_deadline_ms,
                                    );
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
                                    Ok(ref expected_key) if expected_key.as_slice() == key.as_slice() => {
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
                                        let gen = user
                                            .set_session(Arc::downgrade(this.get().unwrap()))
                                            .await;
                                        if let Some(session) = this.get() {
                                            let _ = session.bound_generation.set(gen);
                                        }
                                        if let Some(session) = this.get() {
                                            let tx = crate::session_actor::init_session_mailbox(session);
                                            let _ = session.actor_tx.set(tx);
                                        }
                                        *server.room_monitor.write().await =
                                            Some(Arc::downgrade(this.get().unwrap()));
                                        // PMP44 P0-D: flush 前检查绝对预算——绝不发送
                                        // Authenticate(Ok) 于预算之外。
                                        if crate::official_client_compat::timing::deadline_expired(
                                            auth_deadline,
                                        ) {
                                            warn!(
                                                user = user.id,
                                                "room monitor auth deadline elapsed before response flush"
                                            );
                                            send_auth_rejection(
                                                &send_tx,
                                                "authentication timed out".to_string(),
                                            )
                                            .await;
                                            panicked.store(true, Ordering::SeqCst);
                                            return;
                                        }
                                        crate::official_client_compat::timing::CompatTiming::from_config(
                                            &server.config,
                                        )
                                        .wait_until_minimum_bounded(
                                            auth_received_at,
                                            Some(auth_deadline),
                                        )
                                        .await;
                                        let flush_remaining = auth_deadline
                                            .saturating_duration_since(Instant::now());
                                        let flush_result: Result<()> =
                                            match tokio::time::timeout(
                                                flush_remaining,
                                                send_tx.send_and_flush(
                                                    ServerCommand::Authenticate(Ok((
                                                        user.to_info(),
                                                        None,
                                                    ))),
                                                ),
                                            )
                                            .await
                                            {
                                                Ok(r) => r,
                                                Err(_) => Err(anyhow!(
                                                    "room monitor auth response flush exceeded deadline"
                                                )),
                                            };
                                        if let Err(err) = flush_result {
                                            warn!(user = user.id, ?err, "failed to flush room monitor auth response");
                                            panicked.store(true, Ordering::SeqCst);
                                            return;
                                        }
                                        gate.activate(send_tx.as_ref()).await;
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
                                let auth_received_at = Instant::now();
                                let auth_deadline = auth_received_at
                                    + std::time::Duration::from_millis(
                                        server.config.compatibility.auth_deadline_ms,
                                    );
                                match authenticate_remote_with_notice(
                                    &server,
                                    token,
                                    PhiraRetryNoticeTarget::Stream(send_tx.as_ref()),
                                    Some(auth_deadline),
                                )
                                .await
                                {
                                    Ok(info) => {
                                        let monitor_id = -info.id; // negative to avoid conflicting with normal session
                                        let user = Arc::new(User::new(
                                            monitor_id,
                                            info.name.clone(),
                                            info.language.parse().map(Language).unwrap_or_default(),
                                            Arc::clone(&server),
                                            Some(token.to_string()),
                                        ));
                                        let _ = tx.send(AuthenticationOutcome::Accepted(
                                            Arc::clone(&user),
                                            SessionCategory::GameMonitor,
                                        ));
                                        this_inited.notified().await;
                                        let gen = user
                                            .set_session(Arc::downgrade(this.get().unwrap()))
                                            .await;
                                        if let Some(session) = this.get() {
                                            let _ = session.bound_generation.set(gen);
                                        }
                                        // Initialize per-session mailbox for command routing
                                        if let Some(session) = this.get() {
                                            let tx = crate::session_actor::init_session_mailbox(session);
                                            let _ = session.actor_tx.set(tx);
                                        }
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
                                        // PMP44 P0-D: flush 前检查绝对预算——绝不发送
                                        // Authenticate(Ok) 于预算之外。
                                        if crate::official_client_compat::timing::deadline_expired(
                                            auth_deadline,
                                        ) {
                                            warn!(
                                                user = user.id,
                                                "game monitor auth deadline elapsed before response flush"
                                            );
                                            send_auth_rejection(
                                                &send_tx,
                                                "authentication timed out".to_string(),
                                            )
                                            .await;
                                            panicked.store(true, Ordering::SeqCst);
                                            return;
                                        }
                                        crate::official_client_compat::timing::CompatTiming::from_config(
                                            &server.config,
                                        )
                                        .wait_until_minimum_bounded(
                                            auth_received_at,
                                            Some(auth_deadline),
                                        )
                                        .await;
                                        let flush_remaining = auth_deadline
                                            .saturating_duration_since(Instant::now());
                                        let flush_result: Result<()> =
                                            match tokio::time::timeout(
                                                flush_remaining,
                                                send_tx.send_and_flush(
                                                    ServerCommand::Authenticate(Ok((
                                                        user.to_info(),
                                                        None,
                                                    ))),
                                                ),
                                            )
                                            .await
                                            {
                                                Ok(r) => r,
                                                Err(_) => Err(anyhow!(
                                                    "game monitor auth response flush exceeded deadline"
                                                )),
                                            };
                                        if let Err(err) = flush_result {
                                            warn!(user = user.id, ?err, "failed to flush game monitor auth response");
                                            panicked.store(true, Ordering::SeqCst);
                                            return;
                                        }
                                        gate.activate(send_tx.as_ref()).await;
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

                        // P0-B: record the command receive time. Responses must
                        // never arrive earlier than received_at + minimum latency
                        // (the official client installs its callback after send).
                        let received_at = Instant::now();
                        ProtocolTrace::get()
                            .request_received
                            .fetch_add(1, Ordering::Relaxed);

                        // P0-A: capture the ACTUAL session that received this
                        // command as its origin, and derive the absolute deadline
                        // from the network receive point (P0-J). After a reconnect
                        // the user's binding points at a NEW session; routing
                        // against this captured origin keeps responses, error
                        // closes and compensations bound to THIS session — never
                        // the replacement. The bound generation is the one this
                        // Session was bound with at auth time, not the user's
                        // current generation which advances past this Session.
                        let actual_session = this.get().map(Arc::clone).unwrap();
                        let origin = crate::session::CommandOrigin {
                            session: Arc::downgrade(&actual_session),
                            generation: actual_session
                                .bound_generation
                                .get()
                                .copied()
                                .unwrap_or(0),
                        };
                        let absolute_deadline = received_at
                            + std::time::Duration::from_millis(
                                user.server.config.compatibility.session_command_deadline_ms,
                            );

                        // 命令速率限制 — on failure return the matching official
                        // error response instead of silently dropping the request
                        // (P0-A).
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
                                    let critical = matches!(
                                        &cmd,
                                        ClientCommand::Authenticate { .. }
                                            | ClientCommand::CreateRoom { .. }
                                            | ClientCommand::JoinRoom { .. }
                                            | ClientCommand::RequestStart
                                            | ClientCommand::Ready
                                            | ClientCommand::CancelReady
                                            | ClientCommand::LeaveRoom
                                            | ClientCommand::Chat { .. }
                                            | ClientCommand::LockRoom { .. }
                                            | ClientCommand::CycleRoom { .. }
                                            | ClientCommand::SelectChart { .. }
                                            | ClientCommand::Played { .. }
                                            | ClientCommand::Abort
                                    );
                                    let resp =
                                        crate::official_client_compat::response::official_error_response(
                                            &cmd,
                                            "command rate limited".to_string(),
                                        );
                                    crate::official_client_compat::timing::CompatTiming::from_config(
                                        &user.server.config,
                                    )
                                    .wait_until_minimum_bounded(
                                        received_at,
                                        Some(absolute_deadline),
                                    )
                                    .await;
                                    if let Some(resp) = resp {
                                        send_dispatch_response(
                                            &send_tx,
                                            resp,
                                            critical,
                                            received_at,
                                            id,
                                            &server,
                                            &panicked,
                                            Some(absolute_deadline),
                                        )
                                        .await;
                                    }
                                    return;
                                }
                            }
                        }

                        let no_response =
                            crate::official_client_compat::response::no_response_expected(&cmd);
                        // JoinRoom(Ok) is delivered internally by join_room; a
                        // None result for JoinRoom is legitimate, not a silent drop.
                        let is_join_room = matches!(cmd, ClientCommand::JoinRoom { .. });
                        // P0-E/P0-G: every request-type response must be proven
                        // flushed to the socket (bounded send_and_flush), not
                        // merely queued — the official client waits for all of
                        // these (audit §12).
                        let critical = matches!(
                            &cmd,
                            ClientCommand::Authenticate { .. }
                                | ClientCommand::CreateRoom { .. }
                                | ClientCommand::JoinRoom { .. }
                                | ClientCommand::RequestStart
                                | ClientCommand::Ready
                                | ClientCommand::CancelReady
                                | ClientCommand::LeaveRoom
                                | ClientCommand::Chat { .. }
                                | ClientCommand::LockRoom { .. }
                                | ClientCommand::CycleRoom { .. }
                                | ClientCommand::SelectChart { .. }
                                | ClientCommand::Played { .. }
                                | ClientCommand::Abort
                        );
                        let creating_player = matches!(cmd, ClientCommand::CreateRoom { .. })
                            .then(|| Arc::clone(&user));
                        let uid = user.id;
                        let compat =
                            crate::official_client_compat::timing::CompatTiming::from_config(
                                &user.server.config,
                            );
                        let result = LANGUAGE
                            .scope(
                                Arc::new(user.lang.clone()),
                                crate::session_dispatch::process(
                                    user,
                                    actual_session.category,
                                    cmd,
                                    origin,
                                    received_at,
                                    absolute_deadline,
                                ),
                            )
                            .await;
                        if let Some(resp) = result {
                            let created_room = creating_player.is_some()
                                && matches!(resp, ServerCommand::CreateRoom(Ok(())));
                            // P0-B: never respond before the minimum latency window.
                            // PMP44 P0-J: 等待被绝对预算截断——flush 的超时随后
                            // 强制同一 deadline。
                            compat
                                .wait_until_minimum_bounded(received_at, Some(absolute_deadline))
                                .await;
                            let sent = send_dispatch_response(
                                &send_tx,
                                resp,
                                critical,
                                received_at,
                                id,
                                &server,
                                &panicked,
                                Some(absolute_deadline),
                            )
                            .await;
                            if sent && created_room {
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
                            // For the join-first-host case, join_room defers the
                            // ChangeHost(true) packet to the post-response compat queue
                            // (bound to the origin, after JoinRoom(Ok) is flushed). Sending
                            // it here would duplicate the message and can arrive out of
                            // order (before JoinRoom(Ok)), confusing the client.
                        } else if no_response.is_none() && !is_join_room {
                            // A None result that is neither NoResponseExpected nor
                            // JoinRoom's internally-delivered response is a silent
                            // drop — count it so CI/observability can assert the
                            // counter stays at zero (P1).
                            ProtocolTrace::get()
                                .silent_response_paths
                                .fetch_add(1, Ordering::Relaxed);
                            warn!(user = uid, "silent response path for request-type command");
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
            bound_generation: std::sync::OnceLock::new(),
            gate,
            actor_tx: OnceLock::new(),
            monitor_task_handle,
            _session_permit: session_permit,
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
        // P0-B: pre-authentication packets are buffered by the gate; post-
        // authentication this inherits the transport's slow-consumer behavior.
        if !self.gate.try_send(&self.stream, cmd).await {
            // A full outbound queue means this client is no longer keeping up.
            // Disconnect it instead of allowing one slow consumer to stall a
            // room-wide broadcast or actor command.
            warn!(session = %self.id, user = self.user.id, "disconnecting slow client");
            self.stream.close();
            let _ = self.user.server.lost_con_tx.try_send(self.id);
        }
    }

    /// Send a command to this session, waiting for capacity (async).
    /// Closes the connection on error (same as try_send on failure).
    pub async fn send(&self, cmd: ServerCommand) -> Result<()> {
        self.gate.send(&self.stream, cmd).await.map_err(|err| {
            warn!(session = %self.id, user = self.user.id, ?err, "disconnecting slow client (send)");
            self.stream.close();
            let _ = self.user.server.lost_con_tx.try_send(self.id);
            err
        })
    }

    /// Send a command and block until the packet has been flushed to the socket
    /// (P0-E/P0-F). Every request-type response — Authenticate, CreateRoom,
    /// JoinRoom, RequestStart, Ready, CancelReady, LeaveRoom, Chat, LockRoom,
    /// CycleRoom, SelectChart, Played, Abort — must be proven written to the
    /// wire, not merely queued (P0-G). A flush failure closes the transport and
    /// enters the existing lost-connection path.
    pub async fn send_and_flush(&self, cmd: ServerCommand) -> Result<()> {
        self.gate
            .send_and_flush(&self.stream, cmd)
            .await
            .map_err(|err| {
                warn!(session = %self.id, user = self.user.id, ?err, "disconnecting slow client (send_and_flush)");
                self.stream.close();
                let _ = self.user.server.lost_con_tx.try_send(self.id);
                err
            })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.monitor_task_handle.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// In-memory [`OutboundSink`] recording everything it receives, so the gate
    /// barrier behavior can be asserted without a live TCP transport.
    #[derive(Default)]
    struct TestSink {
        sent: Mutex<Vec<ServerCommand>>,
    }

    impl OutboundSink for TestSink {
        async fn sink_send(&self, cmd: ServerCommand) -> Result<()> {
            self.sent.lock().unwrap().push(cmd);
            Ok(())
        }
        async fn sink_send_and_flush(&self, cmd: ServerCommand) -> Result<()> {
            self.sent.lock().unwrap().push(cmd);
            Ok(())
        }
        fn sink_try_send(&self, cmd: ServerCommand) -> Result<()> {
            self.sent.lock().unwrap().push(cmd);
            Ok(())
        }
    }

    #[test]
    fn origin_generation_check_requires_matching_generation_and_session() {
        // Cross-generation => stale even when the session identity still matches.
        assert!(!origin_is_current(1, 2, true));
        // Same generation but a different session identity => stale.
        assert!(!origin_is_current(2, 2, false));
        // Generation AND identity both match => current.
        assert!(origin_is_current(2, 2, true));
    }

    #[tokio::test]
    async fn origin_with_dropped_session_is_never_current() {
        let origin = CommandOrigin {
            session: Weak::new(),
            generation: 0,
        };
        assert!(!origin.is_current().await);
        assert!(!origin.try_send(ServerCommand::Pong).await);
        // Must be a no-op — never panic, never touch any live session.
        origin.close_uncertain().await;
    }

    #[test]
    fn to_room_origin_of_dropped_session_is_none() {
        // PMP44 P0-C: an origin whose session is already dropped projects to
        // `None`, so the room actor treats it as a non-session token.
        let origin = CommandOrigin {
            session: Weak::new(),
            generation: 5,
        };
        assert!(origin.to_room_origin().is_none());
    }

    #[tokio::test]
    async fn outbound_gate_buffers_before_activation() {
        let gate = SessionOutboundGate::new();
        let sink = TestSink::default();
        assert!(gate.try_send(&sink, ServerCommand::Pong).await);
        assert!(gate.send(&sink, ServerCommand::ChangeHost(true)).await.is_ok());
        assert!(
            sink.sent.lock().unwrap().is_empty(),
            "pre-activation packets must be buffered, not forwarded"
        );
    }

    #[tokio::test]
    async fn outbound_gate_activation_drains_fifo_in_order() {
        let gate = SessionOutboundGate::new();
        let sink = TestSink::default();
        gate.try_send(&sink, ServerCommand::ChangeHost(false)).await;
        gate.send(&sink, ServerCommand::Chat(Ok(()))).await.unwrap();
        gate.try_send(&sink, ServerCommand::Pong).await;
        gate.activate(&sink).await;

        let sent = sink.sent.lock().unwrap();
        assert_eq!(sent.len(), 3);
        // FIFO: buffered order is preserved after activation.
        assert!(matches!(sent[0], ServerCommand::ChangeHost(false)));
        assert!(matches!(sent[1], ServerCommand::Chat(Ok(()))));
        assert!(matches!(sent[2], ServerCommand::Pong));
    }

    #[tokio::test]
    async fn outbound_gate_forwarding_passes_through_after_activation() {
        let gate = SessionOutboundGate::new();
        let sink = TestSink::default();
        gate.try_send(&sink, ServerCommand::Pong).await;
        gate.activate(&sink).await;
        assert!(gate.send(&sink, ServerCommand::LockRoom(Ok(()))).await.is_ok());
        assert_eq!(sink.sent.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    #[should_panic(expected = "send_and_flush before outbound activation")]
    async fn send_and_flush_before_activation_panics() {
        let gate = SessionOutboundGate::new();
        let sink = TestSink::default();
        let _ = gate.send_and_flush(&sink, ServerCommand::Pong).await;
    }
}

//! Client sessions, authentication, and command dispatch.

pub use crate::session_lifecycle::User;

use crate::l10n::{Language, LANGUAGE};
use crate::official_client_compat::post_response::PostResponseItem;
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
    sync::{mpsc, oneshot, Mutex, Notify, OnceCell, OwnedSemaphorePermit},
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
    outbound_tx: &mpsc::Sender<OutboundItem>,
    resp: ServerCommand,
    critical: bool,
    received_at: Instant,
    id: Uuid,
    server: &PlusServerState,
    panicked: &AtomicBool,
    deadline: Option<Instant>,
) -> bool {
    let result = if critical {
        let (flush_tx, flush_rx) = oneshot::channel();
        // PMP44 P0-I: 临界响应经出站队列送写，与认证帧/缓冲事件/补偿共享
        // 同一 FIFO；oneshot 回传 flush 结果以保留 P0-E/P0-J 语义。
        let send_fut = async {
            outbound_tx
                .send(OutboundItem::Critical(resp, flush_tx))
                .await
                .map_err(|err| anyhow!("outbound channel closed: {err}"))?;
            flush_rx
                .await
                .map_err(|_| anyhow!("outbound task stopped during critical flush"))?
        };
        match deadline {
            Some(d) => {
                let remaining = d.saturating_duration_since(Instant::now());
                match tokio::time::timeout(remaining, send_fut).await {
                    Ok(r) => r,
                    Err(_) => {
                        // 响应 flush 超出绝对预算——视为发送失败。
                        Err(anyhow!("response flush exceeded deadline"))
                    }
                }
            }
            None => send_fut.await,
        }
    } else {
        outbound_tx
            .send(OutboundItem::Packet(resp))
            .await
            .map_err(|err| anyhow!("outbound channel closed: {err}"))
    };
    match result {
        Ok(()) => {
            ProtocolTrace::get()
                .response_queued
                .fetch_add(1, Ordering::Relaxed);
            if critical {
                ProtocolTrace::get()
                    .response_flushed
                    .fetch_add(1, Ordering::Relaxed);
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

/// PMP44 P0-E: 认证阶段。只有进入 `Active` 后，该 Session 才算正式成为
/// User 的当前 Session；之前任何一步失败都必须撤销绑定与注册，不得留下
/// 半认证 Session。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AuthPhase {
    /// 远端认证/用户构建完成，WAL admission 尚未成功。
    Authenticating,
    /// WAL admission 成功（UserAuthenticated 已入队），但 Authenticate(Ok)
    /// 尚未 flush。
    DurableAccepted,
    /// Authenticate(Ok) 已 flush，但 OutboundGate 尚未激活。
    ResponseFlushed,
    /// Gate 激活完成，正式接管连接。
    Active,
}

/// PMP44 P0-E: 认证闭包回传的中间状态。`accepted` 为 `None` 表示认证被
/// 主动拒绝（封禁/远端失败/token 无效），Session 从未构造，无需回滚注册。
struct AuthResolved {
    accepted: Option<AuthAcceptedState>,
}

/// 认证成功（`AuthenticationOutcome::Accepted` 已发出）后的持久化决策状态，
/// 供外层 WAL/flush 阶段做回滚判断。
struct AuthAcceptedState {
    user: Arc<User>,
    /// 本次认证新建并插入 `server.users` 的用户（回滚时需用 `Arc::ptr_eq`
    /// 守卫移除）。
    newly_created: bool,
    /// reconnect 时被取代的旧 Session；其关闭推迟到新会话达到
    /// `AuthPhase::Active` 之后（PMP45 P0-D/P0-C 两阶段交接），此前任何一步
    /// 失败旧连接仍完整可回滚。
    previous_session: Option<Arc<Session>>,
}

/// PMP44 P0-E: 认证回滚判定（纯逻辑，供单测）。成功路径为
/// Authenticating --WAL 成功--> DurableAccepted --flush 成功--> ResponseFlushed
/// --gate 成功--> Active。在 `phase` 处若下一步失败则必须回滚；只有
/// `ResponseFlushed` 之后才允许不再回滚（Authenticate(Ok) 已到达客户端）。
fn should_rollback_auth(phase: AuthPhase, wal_ok: bool, flush_ok: bool) -> bool {
    match phase {
        AuthPhase::Authenticating => !wal_ok,
        AuthPhase::DurableAccepted => !flush_ok,
        AuthPhase::ResponseFlushed => !flush_ok,
        AuthPhase::Active => false,
    }
}

/// PMP44 P0-E: 认证失败回滚。撤销绑定、移除新注册用户、关闭新传输并拒绝
/// 客户端。绝不调用 `dangle()` —— 该 Session 从未成为 User 的当前 Session，
/// 完整断连路径（dangle grace / 离线事件）不适用。
///
/// PMP45 语义扩展：
/// - `bound_identity`：本次认证实际绑定到的 `(session_id, generation)`；
///   `Some(..)` 表示已 `set_session`，回滚时只精确清除该代际的绑定
///   （P0-C），`None` 表示尚未绑定（WAL 前失败，旧绑定保持不变）。
/// - `durable`：`UserAuthenticated` 是否已入队 WAL（阶段 >= DurableAccepted）。
///   为 true 时必须补发 `UserDisconnect` + `UserOffline` 补偿，否则数据库会
///   永远显示该用户在线（P0-A）。
/// - `send_err`：是否发送 `Authenticate(Err)` 拒绝帧。一旦尝试过
///   `Authenticate(Ok)` flush，结果即不确定——绝不补发 Err（P0-B），只关闭
///   传输。
///
/// 顺序很关键：先 `clear_session_if_matches`（使 lost-connection worker 的
/// `user_ref` 判定为 false，从而跳过 dangle），再移除新注册用户，再补发
/// 持久化补偿，最后（可选）发送拒绝并关闭传输。
async fn rollback_failed_auth(
    server: &PlusServerState,
    send_tx: &StreamSender<ServerCommand>,
    this: &OnceCell<Arc<Session>>,
    user: Option<&Arc<User>>,
    newly_created: bool,
    bound_identity: Option<(Uuid, u64)>,
    durable: bool,
    send_err: bool,
    reason: String,
) {
    let user_id = user.map(|u| u.id).unwrap_or_default();
    let user_name = user.map(|u| u.name.clone()).unwrap_or_default();
    if let Some(user) = user {
        // PMP45 P0-C: 只清除与本次失败认证精确同代的绑定，绝不误清新会话
        //（reconnect 路径在 WAL 失败时尚未 set_session，旧绑定保持不变，
        // 旧连接可继续存活）。
        if let Some((session_id, generation)) = bound_identity {
            user.clear_session_if_matches(session_id, generation).await;
        }
        // 本次认证新建的用户：仅当 server.users 仍指向该 User 时移除
        //（Arc::ptr_eq 守卫，与既有代码一致）。
        if newly_created {
            // PMP45 P0-E: 仅当该 User 未被更新的认证接管时才移除注册——
            // 判定绑定会话是否仍属于本次失败认证（或已无绑定）。
            let ours = {
                let binding = user.binding.read().await;
                let current_session_id = binding
                    .session
                    .as_ref()
                    .and_then(std::sync::Weak::upgrade)
                    .map(|s| s.id);
                drop(binding);
                match bound_identity {
                    Some((sid, _)) => {
                        current_session_id.is_none() || current_session_id == Some(sid)
                    }
                    None => current_session_id.is_none(),
                }
            };
            if ours {
                let mut users = server.users.write().await;
                if users
                    .get(&user.id)
                    .is_some_and(|current| Arc::ptr_eq(current, user))
                {
                    users.remove(&user.id);
                }
            }
        }
    }
    // PMP45 P0-A: WAL 事实已存在（durable）则补发断连 + 离线补偿，杜绝
    // “客户端从未完成认证但 DB 显示在线（visit + playtime）”的幻象。
    if durable {
        let session_id = this.get().map(|s| s.id.to_string()).unwrap_or_default();
        let now = crate::db::now_ms();
        if let Err(e) = server
            .persistence_worker
            .enqueue(crate::persistence::message::PersistenceEvent::UserDisconnect {
                user_id,
                user_name,
                server_instance_id: crate::server_instance::current().to_string(),
                session_id: session_id.clone(),
                occurred_at: now,
            })
            .await
        {
            warn!(user = user_id, kind = %e.kind(), "UserDisconnect enqueue failed during auth rollback");
        }
        if let Err(e) = server
            .persistence_worker
            .enqueue(crate::persistence::message::PersistenceEvent::UserOffline {
                user_id,
                server_instance_id: crate::server_instance::current().to_string(),
                session_id,
                occurred_at: now,
            })
            .await
        {
            warn!(user = user_id, kind = %e.kind(), "UserOffline enqueue failed during auth rollback");
        }
    }
    // PMP45 P0-B: 一旦尝试过 Authenticate(Ok) flush，结果即不确定，绝不补发
    // Err；仅关闭传输交由 lost-connection worker 处理。
    if send_err {
        send_auth_rejection(send_tx, reason).await;
    }
    if let Some(session) = this.get() {
        session.stream.close();
        let _ = server.lost_con_tx.try_send(session.id);
    }
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
        let same_session = current
            .as_ref()
            .is_some_and(|cur| Arc::ptr_eq(cur, &session));
        origin_is_current(self.generation, current_generation, same_session)
    }

    /// Send a best-effort packet to the origin session. Returns `false` and
    /// drops the packet when the origin is stale — a superseded session must
    /// never receive a response intended for an old command (P0-A).
    pub(crate) async fn try_send(&self, cmd: ServerCommand) -> bool {
        if !self.is_current().await {
            debug!(
                generation = self.generation,
                "dropping response for stale session origin"
            );
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
    /// P0-I 后生产路径统一经 per-session 出站通道，`send_and_flush`/`try_send`
    /// 仅测试与 gate 内部使用。
    #[allow(dead_code)]
    async fn sink_send_and_flush(&self, cmd: ServerCommand) -> Result<()>;
    #[allow(dead_code)]
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
///
/// PMP44 P0-G: the buffer is bounded — telemetry (Touches/Judges/Pong) is
/// coalesced (dropped) when over the limit, control/state events drop the
/// oldest buffered entry to make room for the newest. PMP44 P0-H: a snapshot
/// cutover token marks the boundary between events already reflected in the
/// upcoming room snapshot (dropped at activation) and the delta that must be
/// sent.
pub(crate) struct SessionOutboundGate {
    activated: AtomicBool,
    pending: Mutex<GatePending>,
    /// 认证屏障开始时间（构建 gate 时）。超过 `max_auth_duration` 未激活则
    /// 判定认证失败（fail-closed）。
    created_at: Instant,
    max_pending_events: usize,
    max_pending_bytes: usize,
    max_auth_duration: Duration,
    /// PMP45 P0-H: 控制/语义事件溢出标记。当某条非遥测事件即使清空整个缓冲
    /// 仍超字节预算时置位；`activate` 检测到它直接返回 `Err`（fail-closed），
    /// 认证路径关闭 Session 让客户端重新 Authenticate，绝不激活一个状态不
    /// 完整的连接（audit §10）。
    overflowed: AtomicBool,
}

/// PMP44 P0-G: 认证屏障的待发缓冲。`bytes` 是各条目 `real_size`（wire 编码
/// 实际字节数，PMP45 P0-H）之和，与 `events.len()` 一起构成双重上限。P0-H
/// 的序号状态也放在这里，与缓冲内容同受 `pending` 互斥锁保护。
struct GatePending {
    events: VecDeque<GateEntry>,
    bytes: usize,
    /// P0-H: 单调递增的事件序号（入队时分配）。
    next_seq: u64,
    /// P0-H: 快照切换序号。`seq <= cutover_seq` 的缓冲事件在激活时被丢弃
    /// （已包含在即将构建的快照中），`seq > cutover_seq` 的事件正常发送。
    cutover_seq: u64,
}

/// PMP45 P0-G: 缓冲事件的分类。cutover 剔除只作用于快照完整覆盖的状态类
/// 事件；Chat/语义事件与高频遥测无论序号一律发送（遥测仍受有界剔除约束）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateEventClass {
    /// 快照完整覆盖的状态类事件（lock/cycle/host/chart/state/ready/members）。
    SnapshotCovered,
    /// 快照不包含的语义/聊天事件（Chat/系统消息/GameStart/Ready 历史/Played/Abort）。
    NonSnapshot,
    /// 高频遥测（Touches/Judges/Pong）。
    Telemetry,
}

/// P0-H: 带序号与分类的事件条目，激活时用于快照切换剔除。
#[derive(Debug)]
struct GateEntry {
    cmd: ServerCommand,
    seq: u64,
    /// PMP45 P0-G: cutover 剔除只作用于 `SnapshotCovered`。
    class: GateEventClass,
}

/// `activate` 的结果。`Err` 分支即“排空期间发生发送错误”——屏障已重置为
/// 未激活并清空剩余缓冲，调用方必须 fail-closed（关闭传输）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivationOutcome {
    /// 全部缓冲事件成功转发（或按快照切换剔除后转发），屏障正式打开。
    Complete,
}

/// PMP44 P0-I/P0-O: 单 Session 出站队列条目。
///
/// 认证 Gate、官方响应与 post-response 补偿全部汇入这一条 `mpsc` 队列，
/// 由唯一的出站任务按严格 FIFO 送写 socket——绝不允许后一批补偿越过前一批
/// （audit §19/§20），也不允许缓冲的房间广播在 `Authenticate(Ok)` 之前到达
/// 客户端。
#[derive(Debug)]
pub(crate) enum OutboundItem {
    /// 常规包，按到达顺序送写。认证屏障未打开时由出站任务缓冲进
    /// `SessionOutboundGate`（沿用 P0-G 有界剔除）。
    Packet(ServerCommand),
    /// 需 flush 的临界响应（`Authenticate(Ok)`/`JoinRoom(Ok)`/...）。携带
    /// oneshot 回传 flush 结果，调用方据此保留 P0-E/P0-J 的发送成功判定。
    Critical(ServerCommand, oneshot::Sender<Result<()>>),
    /// 出站门激活——此前缓冲的包必须全部送达后才放行直通。携带 oneshot
    /// 回传排空结果（P0-G fail-closed）。
    Activate(oneshot::Sender<Result<ActivationOutcome>>),
    /// P0-O: 一组 post-response 补偿。出站任务按固定 `PostResponseKind` 顺序
    /// 串行投递；批次间由队列 FIFO 保证绝不乱序。`delay_ms` 为
    /// `protocol_hack_delay_ms`，在批次投递前于出站任务内等待（顺序保留）。
    PostResponse {
        delay_ms: u64,
        items: Vec<PostResponseItem>,
    },
}

/// PMP44 P0-I: 出站通道容量。与 `phira_mp_common::Stream` 内部 send 通道
/// 一致（1024），保证慢消费者只背压本 Session 的队列，绝不阻塞 Room Actor。
const OUTBOUND_CHANNEL_CAPACITY: usize = 1024;

/// PMP44 P0-I/P0-O: 单 Session 出站任务。拥有 `StreamSender`，按严格 FIFO
/// 处理出站队列：
/// - `Packet`：未激活时缓冲进 gate（P0-G 有界/剔除），激活后直通送写；
/// - `Critical`：`send_and_flush` 并回传结果（P0-E 证明 flush）；
/// - `Activate`：在 `Authenticate(Ok)` flush 之后排空 gate 缓冲并放行直通；
/// - `PostResponse`：先等 `protocol_hack_delay_ms`，再按固定顺序串行投递，
///   与前后批次保持 FIFO。
async fn run_outbound_task(
    mut rx: mpsc::Receiver<OutboundItem>,
    send_tx_ready: Arc<tokio::sync::OnceCell<Arc<StreamSender<ServerCommand>>>>,
    gate: Arc<SessionOutboundGate>,
) {
    let mut active = false;
    while let Some(item) = rx.recv().await {
        match item {
            OutboundItem::Packet(cmd) => {
                if !active {
                    // 认证屏障未打开：缓冲进 gate，沿用 P0-G 有界丢弃策略。
                    let mut pending = gate.pending.lock().await;
                    gate.push_bounded(&mut pending, cmd);
                } else {
                    let Some(send_tx) = send_tx_ready.get() else {
                        continue;
                    };
                    if send_tx.send(cmd).await.is_err() {
                        tracing::warn!("outbound task send failed (session teardown?)");
                    }
                }
            }
            OutboundItem::Critical(cmd, flush_tx) => {
                let Some(send_tx) = send_tx_ready.get() else {
                    let _ = flush_tx.send(Err(anyhow!("outbound sender not ready")));
                    continue;
                };
                let result = send_tx.send_and_flush(cmd).await;
                let _ = flush_tx.send(result);
            }
            OutboundItem::Activate(activate_tx) => {
                let Some(send_tx) = send_tx_ready.get() else {
                    let _ = activate_tx.send(Err(anyhow!("outbound sender not ready")));
                    continue;
                };
                match gate.activate(send_tx.as_ref()).await {
                    Ok(outcome) => {
                        active = true;
                        let _ = activate_tx.send(Ok(outcome));
                    }
                    Err(err) => {
                        active = false;
                        let _ = activate_tx.send(Err(err));
                    }
                }
            }
            OutboundItem::PostResponse { delay_ms, items } => {
                let Some(send_tx) = send_tx_ready.get() else {
                    continue;
                };
                if delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
                // 固定顺序（ChangeHost → ChangeState → PersistentRoom → Replay）
                // 且与前后批次 FIFO——batch A 全部投递完才轮到 batch B。
                let send_tx = send_tx.as_ref();
                crate::official_client_compat::post_response::run_post_response_batch(
                    items,
                    |item| async move { item.deliver_via(Some(send_tx)).await },
                )
                .await;
            }
        }
    }
}

/// 高频遥测：握手窗口内可安全 coalesce（丢弃新事件），因为它们只反映
/// 瞬时状态，后续帧会覆盖。
fn is_high_frequency_telemetry(cmd: &ServerCommand) -> bool {
    matches!(
        cmd,
        ServerCommand::Touches { .. } | ServerCommand::Judges { .. } | ServerCommand::Pong
    )
}

/// PMP45 P0-G: 对 `ServerCommand` 分类，决定 cutover 剔除是否适用。
///
/// `SnapshotCovered`：这些命令改变的状态（lock/cycle/host/chart/state/
/// membership）全部进入 `ClientRoomState`，激活时若 `seq <= cutover` 可安全
/// 剔除（快照已包含）。
/// `NonSnapshot`：Chat 与一切语义/瞬时事件（GameStart/Ready/CancelReady/
/// Played/Abort/CancelGame/StartPlaying/GameEnd 及各命令响应）——快照不包含
/// 它们，cutover 绝不丢弃，否则聊天/语义消息会在认证窗口内永久丢失（§11）。
/// `Telemetry`：高频遥测，仅受有界剔除约束。
fn classify_command(cmd: &ServerCommand) -> GateEventClass {
    if is_high_frequency_telemetry(cmd) {
        return GateEventClass::Telemetry;
    }
    match cmd {
        // 快照完整覆盖：状态变更全部进入 ClientRoomState。
        ServerCommand::ChangeHost(_)
        | ServerCommand::ChangeState(_)
        | ServerCommand::LockRoom(_)
        | ServerCommand::CycleRoom(_)
        | ServerCommand::SelectChart(_)
        | ServerCommand::OnJoinRoom(_)
        | ServerCommand::Message(Message::JoinRoom { .. })
        | ServerCommand::Message(Message::LeaveRoom { .. })
        | ServerCommand::Message(Message::NewHost { .. })
        | ServerCommand::Message(Message::LockRoom { .. })
        | ServerCommand::Message(Message::CycleRoom { .. })
        | ServerCommand::Message(Message::SelectChart { .. }) => GateEventClass::SnapshotCovered,
        // 其余（Chat/系统消息/GameStart/Ready/CancelReady/Played/Abort/
        // CancelGame/StartPlaying/GameEnd 及一切响应）不在快照覆盖范围内。
        _ => GateEventClass::NonSnapshot,
    }
}

/// PMP45 P0-H: 测量 `ServerCommand` 的真实 wire 编码字节数（§29/P1，取代旧的
/// 固定 +128/+1024 粗估）。`ServerCommand` 实现 `BinaryData`，`encode_packet`
/// 为公开 API；编码失败（类型层几乎不可能）时保守回退为枚举本体大小。
fn real_size(cmd: &ServerCommand) -> usize {
    let mut buf = Vec::new();
    match phira_mp_common::encode_packet(cmd, &mut buf) {
        Ok(()) => buf.len(),
        Err(_) => std::mem::size_of::<ServerCommand>(),
    }
}

impl SessionOutboundGate {
    pub(crate) fn new(
        max_pending_events: usize,
        max_pending_bytes: usize,
        max_auth_duration: Duration,
    ) -> Self {
        Self {
            activated: AtomicBool::new(false),
            pending: Mutex::new(GatePending {
                events: VecDeque::new(),
                bytes: 0,
                next_seq: 1,
                cutover_seq: 0,
            }),
            created_at: Instant::now(),
            max_pending_events,
            max_pending_bytes,
            max_auth_duration,
            overflowed: AtomicBool::new(false),
        }
    }

    /// 入队（未激活时）。受 `max_pending_events` / `max_pending_bytes` 约束：
    /// - 高频遥测（Touches/Judges/Pong）：超限时直接丢弃（coalesce）；
    /// - 控制/语义/聊天事件：**循环**丢弃最旧缓冲条目（FIFO）腾出空间再入队，
    ///   保留最新控制状态（P0-H：旧逻辑只弹一个，新事件较大时仍会超预算）。
    /// - 若清空缓冲后新事件仍超字节预算：控制事件溢出 → 置 `overflowed`
    ///   （fail-closed），`activate` 将拒绝激活，认证路径关闭 Session。
    fn push_bounded(&self, pending: &mut GatePending, cmd: ServerCommand) {
        let seq = pending.next_seq;
        pending.next_seq += 1;
        let class = classify_command(&cmd);
        let size = real_size(&cmd);
        let over_limit = pending.events.len() >= self.max_pending_events
            || pending.bytes.saturating_add(size) > self.max_pending_bytes;
        if over_limit {
            if class == GateEventClass::Telemetry {
                // 高频遥测：新事件直接丢弃（coalesce），保留既有缓冲。
                tracing::trace!(?cmd, "outbound gate coalesced high-frequency telemetry");
                ProtocolTrace::get()
                    .gate_dropped
                    .fetch_add(1, Ordering::Relaxed);
                return;
            }
            // 控制/语义/聊天事件：不得静默丢弃——循环丢弃最旧缓冲事件（FIFO）
            // 直到新事件能放入，既保持有界，又让客户端始终收到最新控制状态。
            loop {
                if pending.events.len() < self.max_pending_events
                    && pending.bytes.saturating_add(size) <= self.max_pending_bytes
                {
                    break;
                }
                match pending.events.pop_front() {
                    Some(oldest) => {
                        pending.bytes = pending.bytes.saturating_sub(real_size(&oldest.cmd));
                        tracing::trace!(?oldest, "outbound gate dropped oldest buffered event");
                        ProtocolTrace::get()
                            .gate_dropped
                            .fetch_add(1, Ordering::Relaxed);
                    }
                    None => {
                        // 缓冲已空且新事件单独仍超字节预算：控制事件溢出 →
                        // fail-closed。不放入缓冲（超预算单事件），交由
                        // `activate` 拒绝激活。
                        self.overflowed.store(true, Ordering::SeqCst);
                        ProtocolTrace::get()
                            .gate_control_overflow
                            .fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(
                            ?cmd,
                            size,
                            "outbound gate control overflow; session will fail-closed"
                        );
                        Self::store_pending_gauges(pending);
                        return;
                    }
                }
            }
        }
        pending
            .events
            .push_back(GateEntry { cmd, seq, class });
        pending.bytes += size;
        // PMP44 P1 §33: 每次入队/丢弃后更新认证屏障 gauge（事件数 / 字节粗估），
        // 提供预认证缓冲的实时观测视图。
        Self::store_pending_gauges(pending);
    }

    /// PMP44 P1 §33: 将认证屏障当前缓冲状态写入 gauge 计数器。在 `push_bounded`
    /// 每次入队/丢弃后、`activate` 排空后调用。
    fn store_pending_gauges(pending: &GatePending) {
        let trace = ProtocolTrace::get();
        trace
            .auth_barrier_pending_events
            .store(pending.events.len() as u64, Ordering::Relaxed);
        trace
            .auth_barrier_pending_bytes
            .store(pending.bytes as u64, Ordering::Relaxed);
    }

    /// Queue (pre-activation) or forward (post-activation). Returns `Err` only
    /// when the forwarding send itself fails.
    ///
    /// P0-I 后生产路径统一经 per-session 出站通道，此方法仅测试与 gate 直驱
    /// 使用。
    #[allow(dead_code)]
    pub(crate) async fn send(&self, sink: &impl OutboundSink, cmd: ServerCommand) -> Result<()> {
        let mut pending = self.pending.lock().await;
        if self.activated.load(Ordering::SeqCst) {
            drop(pending);
            sink.sink_send(cmd).await
        } else {
            self.push_bounded(&mut pending, cmd);
            Ok(())
        }
    }

    /// Non-blocking variant used by room broadcasts. Pre-activation packets are
    /// buffered (never fail); post-activation it inherits the transport's
    /// slow-consumer failure behavior. Returns `true` when the packet was
    /// accepted (buffered or enqueued).
    ///
    /// P0-I 后生产路径统一经 per-session 出站通道，此方法仅测试使用。
    #[allow(dead_code)]
    pub(crate) async fn try_send(&self, sink: &impl OutboundSink, cmd: ServerCommand) -> bool {
        let mut pending = self.pending.lock().await;
        if self.activated.load(Ordering::SeqCst) {
            drop(pending);
            sink.sink_try_send(cmd).is_ok()
        } else {
            self.push_bounded(&mut pending, cmd);
            true
        }
    }

    /// P0-I 后生产路径统一经 per-session 出站通道，此方法仅测试使用。
    #[allow(dead_code)]
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
    ///
    /// PMP44 P0-H: 排空时剔除 `seq <= cutover_seq` 的事件——快照在
    /// `begin_snapshot_cutover` 之后构建，凡是被快照反映的状态变更必然有
    /// `seq <= cutover`，不得重复下发；`seq > cutover`（快照构建期间到达）
    /// 的事件才是客户端需要的增量。
    ///
    /// PMP45 P0-G: cutover 剔除只作用于 `SnapshotCovered` 事件——Chat/语义/
    /// 遥测事件无论序号一律发送（修复 §11 的认证窗口聊天丢失）；遥测仍可被
    /// 有界剔除丢弃，但绝不由 cutover 丢弃。
    ///
    /// PMP45 P0-H: 控制事件溢出（`overflowed`）时直接返回 `Err`（fail-closed）
    /// ——不激活状态不完整的连接，认证路径关闭 Session，客户端重新
    /// Authenticate 获取完整快照（audit §10）。
    ///
    /// PMP44 P0-G: 若任一 `sink_send` 失败，立即 fail-closed——重置为未激活、
    /// 清空剩余缓冲并返回 `Err`，由调用方关闭传输走 lost-connection 路径，
    /// 绝不留下“已激活但丢了一半事件”的中间态。
    pub(crate) async fn activate(&self, sink: &impl OutboundSink) -> Result<ActivationOutcome> {
        let mut pending = self.pending.lock().await;
        // PMP45 P0-H: 控制事件溢出 → fail-closed。Authenticate(Ok) 可能已
        // flush（P0-B 下绝不补发 Err），但绝不打开一个缺失关键状态的连接。
        if self.overflowed.load(Ordering::SeqCst) {
            tracing::warn!("outbound gate control overflow; refusing activation (fail-closed)");
            self.activated.store(false, Ordering::SeqCst);
            pending.events.clear();
            pending.bytes = 0;
            Self::store_pending_gauges(&pending);
            return Err(anyhow!("outbound gate control overflow; fail-closed"));
        }
        self.activated.store(true, Ordering::SeqCst);
        let cutover = pending.cutover_seq;
        while let Some(entry) = pending.events.pop_front() {
            // PMP45 P0-G: 只对快照覆盖的状态类事件做 cutover 剔除。
            if entry.class == GateEventClass::SnapshotCovered && entry.seq <= cutover {
                // 快照已包含该事件，剔除以免重复。
                pending.bytes = pending.bytes.saturating_sub(real_size(&entry.cmd));
                tracing::trace!(
                    seq = entry.seq,
                    "outbound gate dropped snapshot-included event"
                );
                let trace = ProtocolTrace::get();
                trace.gate_dropped.fetch_add(1, Ordering::Relaxed);
                // PMP44 P1 §33: cutover 剔除专用计数——与 `gate_dropped`（有界
                // 丢弃策略）度量不同，此处专指快照切换屏障剔除的快照内事件。
                trace
                    .snapshot_duplicate_event
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }
            if sink.sink_send(entry.cmd).await.is_err() {
                tracing::warn!(
                    remaining = pending.events.len(),
                    "outbound gate drain failed"
                );
                self.activated.store(false, Ordering::SeqCst);
                pending.events.clear();
                pending.bytes = 0;
                // PMP44 P1 §33: fail-closed 后缓冲已清空，更新 gauge。
                Self::store_pending_gauges(&pending);
                return Err(anyhow!("outbound gate drain failed; fail-closed"));
            }
        }
        // 排空完成，缓冲为空；发送路径未逐条扣减 `bytes`，这里归零避免 gauge
        // 报出陈旧字节数。
        pending.bytes = 0;
        // PMP44 P1 §33: 排空后缓冲为空，更新 gauge（0 / 0）。
        Self::store_pending_gauges(&pending);
        Ok(ActivationOutcome::Complete)
    }

    /// PMP44 P0-H: 开始一次快照切换。返回的 `cutover` 是当前已入队事件的
    /// 最大序号。调用时机：
    ///
    /// - PMP45 P0-F 路径：收到 Room Actor 的 `BindAndSnapshot` 快照后调用，
    ///   使激活时只剔除早于该快照点的 `SnapshotCovered` 事件；
    /// - 兜底路径：在 `build_client_room_state` 之前调用。
    ///
    /// 激活时 `seq <= cutover` 的缓冲事件被视为快照已包含而被丢弃（仅限
    /// `SnapshotCovered`，PMP45 P0-G）。
    pub(crate) async fn begin_snapshot_cutover(&self) -> u64 {
        let mut pending = self.pending.lock().await;
        // next_seq 指向下一个待分配序号，减一即最后已入队事件的序号；
        // 尚无任何事件时（next_seq=1）cutover=0，不会误删后续事件。
        pending.cutover_seq = pending.next_seq.saturating_sub(1);
        pending.cutover_seq
    }

    /// PMP44 P0-G: 认证屏障持续时间是否超过上限。超时未激活即判定认证
    /// 失败（fail-closed），防止慢认证连接无界占住缓冲。
    pub(crate) fn auth_duration_exceeded(&self) -> bool {
        self.created_at.elapsed() > self.max_auth_duration
    }

    /// 当前屏障是否已激活（测试辅助；仅测试构建可见）。
    #[cfg(test)]
    pub(crate) fn is_activated(&self) -> bool {
        self.activated.load(Ordering::SeqCst)
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
    /// 构造后仅保留引用（出站任务持有同一 Arc）；`Session` 自身不再经此字段
    /// 读取 gate。
    #[allow(dead_code)]
    pub(crate) gate: Arc<SessionOutboundGate>,

    /// PMP45 P0-D: 认证是否已推进到 Active（Authenticate(Ok) 已 flush 且
    /// outbound gate 已激活）。`server::accept` 只有看到该标记才会把 Session
    /// 发布进全局 `state.sessions`——半认证 Session 绝不进入全局表（审计 §9）。
    pub(crate) active: OnceLock<()>,
    /// PMP45 P0-D: 认证激活通知。`server::accept` 在发布前等待它（带超时）；
    /// `mark_active` 用 `notify_one`（存储一个 permit），杜绝检查与等待之间的
    /// 丢失唤醒。
    pub(crate) active_notify: Arc<Notify>,

    /// PMP44 P0-I/P0-O: 单 Session 出站队列的发送端。认证 Gate、官方响应与
    /// post-response 补偿都汇入这一条 `mpsc` 队列，由唯一的出站任务按严格
    /// FIFO 送写 socket。
    pub(crate) outbound_tx: mpsc::Sender<OutboundItem>,
    /// PMP44 P0-I: 出站任务句柄，Session 销毁时 abort。
    outbound_task_handle: JoinHandle<()>,

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
        // PMP44 P0-G: 缓冲数量/字节/持续时间上限取自 config.compatibility，
        // 防止慢认证连接造成无界内存增长。
        let compat = &server.config.compatibility;
        let gate = Arc::new(SessionOutboundGate::new(
            compat.gate_max_pending_events,
            compat.gate_max_pending_bytes,
            Duration::from_millis(compat.gate_max_auth_duration_ms),
        ));

        // PMP45 P0-D: 认证激活通知。`server::accept` 等待它把 Session 发布进
        // 全局 sessions 表；`mark_active` 在认证回调推进到 Active 时调用。
        let active_notify = Arc::new(Notify::new());

        // PMP44 P0-I/P0-O: 单 Session 出站队列。发送端保存在 Session 上并克隆进
        // Stream 回调；接收端交给唯一的出站任务，后者在首个命令到达时拿到
        // `send_tx`（经 OnceCell 注入）。
        let (outbound_tx, outbound_rx) = mpsc::channel::<OutboundItem>(OUTBOUND_CHANNEL_CAPACITY);
        let outbound_sender_ready =
            Arc::new(tokio::sync::OnceCell::<Arc<StreamSender<ServerCommand>>>::new());
        let outbound_task_handle = tokio::spawn(run_outbound_task(
            outbound_rx,
            Arc::clone(&outbound_sender_ready),
            Arc::clone(&gate),
        ));

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
                let outbound_tx = outbound_tx.clone();
                let outbound_sender_ready = Arc::clone(&outbound_sender_ready);
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
                    // P0-I: 出站任务需要 `send_tx` 才能写 socket；首个命令（通常为
                    // Authenticate）到达时注入，之后不再变化。
                    let _ = outbound_sender_ready.set(Arc::clone(&send_tx));
                    let outbound_tx = outbound_tx.clone();
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
                                // PMP44 P0-E: 认证阶段机——跟踪认证推进到哪一步，
                                // 任何一步失败都必须回滚（撤销 set_session / 移除
                                // 新注册用户 / 关闭传输 / 拒绝客户端）。
                                let mut auth_phase = AuthPhase::Authenticating;
                                let res: Result<AuthResolved> = {
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
                                                return Ok(AuthResolved { accepted: None });
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
                                                    // PMP44 P0-F: 认证握手窗口内只记录重试
                                                    // 日志，绝不向官方客户端发送 PMP 扩展
                                                    // Chat 包。
                                                    PhiraRetryNoticeTarget::StreamLogOnly,
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
                                                        return Ok(AuthResolved { accepted: None });
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
                                                    return Ok(AuthResolved { accepted: None });
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
                                            // PMP44 P0-E: 捕获被取代的旧 Session；其关闭推迟到
                                            // 新会话达到 AuthPhase::Active 之后（PMP45 P0-D/P0-C
                                            // 两阶段交接）。若中途失败，旧连接仍完整可回滚。
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
                                            // 注意：set_session 推迟到外层 WAL admission 成功
                                            // 之后（DurableAccepted 阶段），这样 WAL 失败可
                                            // 完整撤销绑定，不影响旧会话。
                                            existing.set_auth_token(Some(token.to_string())).await;
                                            Ok(AuthResolved {
                                                accepted: Some(AuthAcceptedState {
                                                    user: existing,
                                                    newly_created: false,
                                                    previous_session,
                                                }),
                                            })
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
                                                return Ok(AuthResolved { accepted: None });
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
                                            {
                                                let mut guard = server.users.write().await;
                                                guard.insert(user_info.id, Arc::clone(&user));
                                            }
                                            Ok(AuthResolved {
                                                accepted: Some(AuthAcceptedState {
                                                    user,
                                                    newly_created: true,
                                                    previous_session: None,
                                                }),
                                            })
                                        }
                                    }
                                }
                                .await;
                                // 主动拒绝（封禁/远端失败/token 无效）：Session 从未构造。
                                let auth_resolved = match res {
                                    Err(err) => {
                                        warn!("failed to authenticate: {err:?}");
                                        send_auth_rejection(&send_tx, err.to_string()).await;
                                        if let Some(tx) = auth_tx.take() {
                                            let _ = tx.send(AuthenticationOutcome::Rejected);
                                        }
                                        panicked.store(true, Ordering::SeqCst);
                                        return;
                                    }
                                    Ok(resolved) => resolved,
                                };
                                let Some(AuthAcceptedState {
                                    user,
                                    newly_created,
                                    previous_session,
                                }) = auth_resolved.accepted
                                else {
                                    // Authentication was deliberately rejected and the Session
                                    // was never initialized. Do not fall through into the
                                    // success path.
                                    panicked.store(true, Ordering::SeqCst);
                                    return;
                                };
                                if this.get().is_none() {
                                    // Accepted 已发出但 Session 未构造（oneshot 被丢弃）——
                                    // 防御性终止，不进入成功路径。
                                    panicked.store(true, Ordering::SeqCst);
                                    return;
                                }
                                // Initialize per-session mailbox
                                if let Some(session) = this.get() {
                                    let tx = crate::session_actor::init_session_mailbox(session);
                                    let _ = session.actor_tx.set(tx);
                                }
                                let room_state = match user.room.read().await.as_ref() {
                                    Some(room) => {
                                        // PMP45 P0-F: 优先经 Room Actor 原子快照
                                        // （`BindAndSnapshot`）。Room Actor 在自身排序点
                                        // 一次性捕获 state/members/display_names，返回一致
                                        // 快照与 cutover token；收到快照后再对齐 gate cutover，
                                        // 使激活时只剔除快照已包含的 `SnapshotCovered` 事件
                                        //（P0-G），Chat/语义事件绝不丢失（§11）。若 mailbox
                                        // 不可用/超时/失败，回退到非原子的
                                        // `build_client_room_state`（保留作为兜底路径）。
                                        match user
                                            .server
                                            .room_commands
                                            .bind_and_snapshot(
                                                &user.server,
                                                &room.id.to_string(),
                                                user.id,
                                                Some(auth_deadline),
                                            )
                                            .await
                                        {
                                            Ok(data) => {
                                                // cutover token：Room Actor 构建快照时的网关
                                                // command_seq（actor 排序点）。cutover 在收到
                                                // 快照后对齐到当前已入队事件（含全部早于快照
                                                // 点的事件）。
                                                debug!(
                                                    user = user.id,
                                                    room = %room.id,
                                                    token = data.token,
                                                    "auth snapshot captured at room-actor sequencing point"
                                                );
                                                let _cutover = gate.begin_snapshot_cutover().await;
                                                Some(data.into_client_room_state())
                                            }
                                            Err(err) => {
                                                warn!(
                                                    user = user.id,
                                                    room = %room.id,
                                                    %err,
                                                    "BindAndSnapshot unavailable; falling back to non-atomic client room state"
                                                );
                                                let _cutover = gate.begin_snapshot_cutover().await;
                                                Some(crate::session_room::build_client_room_state(room, &user).await)
                                            }
                                        }
                                    }
                                    None => None,
                                };
                                // ── 阻塞持久化：认证成功但尚未响应客户端 ──────────────
                                // 在发送 Authenticate(Ok) 之前先持久化用户记录，
                                // 确保 WAL admission 成功后才放行客户端。
                                // PMP44 P0-D: WAL admission 前检查绝对预算——预算耗尽
                                // 则认证失败，绝不入队（避免“服务端已注册用户但客户端
                                // 早已超时”的幻象）。当前仍处于 Authenticating 阶段
                                //（尚未 set_session）。
                                if crate::official_client_compat::timing::deadline_expired(
                                    auth_deadline,
                                ) {
                                    warn!(
                                        user = user.id,
                                        "auth deadline elapsed before WAL admission"
                                    );
                                    if should_rollback_auth(auth_phase, false, true) {
                                        rollback_failed_auth(
                                            &server,
                                            &send_tx,
                                            &this,
                                            Some(&user),
                                            newly_created,
                                            None,
                                            false,
                                            true,
                                            "authentication timed out".to_string(),
                                        )
                                        .await;
                                    }
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
                                    if should_rollback_auth(auth_phase, false, true) {
                                        rollback_failed_auth(
                                            &server,
                                            &send_tx,
                                            &this,
                                            Some(&user),
                                            newly_created,
                                            None,
                                            false,
                                            true,
                                            "authentication failed: persistence unavailable"
                                                .to_string(),
                                        )
                                        .await;
                                    }
                                    panicked.store(true, Ordering::SeqCst);
                                    return;
                                }
                                // DurableAccepted：WAL 已成功。现在才把新 Session 绑定为
                                // User 的当前 Session（set_session + bound_generation）。
                                // 被取代的旧 Session 不在此时关闭——推迟到新会话达到
                                // AuthPhase::Active 之后（P0-D/P0-C 两阶段交接）。
                                auth_phase = AuthPhase::DurableAccepted;
                                debug!(user = user.id, ?auth_phase, "WAL admitted; binding session");
                                let gen = user
                                    .set_session(Arc::downgrade(this.get().unwrap()))
                                    .await;
                                if let Some(session) = this.get() {
                                    let _ = session.bound_generation.set(gen);
                                }
                                // PMP45 P0-C: 本次认证的精确绑定身份，供后续任意失败回滚
                                // 只清除该代际，绝不误清新会话。
                                let bound_identity = Some((this.get().unwrap().id, gen));
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
                                    if should_rollback_auth(auth_phase, true, false) {
                                        rollback_failed_auth(
                                            &server,
                                            &send_tx,
                                            &this,
                                            Some(&user),
                                            newly_created,
                                            bound_identity,
                                            true,
                                            true,
                                            "authentication timed out".to_string(),
                                        )
                                        .await;
                                    }
                                    panicked.store(true, Ordering::SeqCst);
                                    return;
                                }
                                // PMP44 P0-G: 认证屏障超时未激活 → fail-closed，
                                // 视为认证失败并回滚。
                                if gate.auth_duration_exceeded() {
                                    warn!(
                                        user = user.id,
                                        "outbound gate auth duration exceeded before response flush"
                                    );
                                    if should_rollback_auth(auth_phase, true, false) {
                                        rollback_failed_auth(
                                            &server,
                                            &send_tx,
                                            &this,
                                            Some(&user),
                                            newly_created,
                                            bound_identity,
                                            true,
                                            true,
                                            "authentication barrier timed out".to_string(),
                                        )
                                        .await;
                                    }
                                    panicked.store(true, Ordering::SeqCst);
                                    return;
                                }
                                let flush_remaining = auth_deadline
                                    .saturating_duration_since(Instant::now());
                                // PMP44 P0-I: `Authenticate(Ok)` 经出站队列送写——与
                                // 缓冲事件/官方响应/补偿共享同一 FIFO，且 oneshot 回传
                                // flush 结果以保留 P0-E 回滚语义（P0-D 预算不变）。
                                let flush_result: Result<()> =
                                    match tokio::time::timeout(
                                        flush_remaining,
                                        async {
                                            let (flush_tx, flush_rx) = oneshot::channel();
                                            outbound_tx
                                                .send(OutboundItem::Critical(
                                                    ServerCommand::Authenticate(Ok((
                                                        user.to_info(),
                                                        room_state,
                                                    ))),
                                                    flush_tx,
                                                ))
                                                .await
                                                .map_err(|err| {
                                                    anyhow!("outbound channel closed: {err}")
                                                })?;
                                            flush_rx.await.map_err(|_| {
                                                anyhow!("outbound task stopped during auth flush")
                                            })?
                                        },
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
                                    if should_rollback_auth(auth_phase, true, false) {
                                        // PMP45 P0-B: Ok 已尝试 flush，结果不确定——绝不补发 Err。
                                        rollback_failed_auth(
                                            &server,
                                            &send_tx,
                                            &this,
                                            Some(&user),
                                            newly_created,
                                            bound_identity,
                                            true,
                                            false,
                                            format!("authentication failed: {err}"),
                                        )
                                        .await;
                                    }
                                    panicked.store(true, Ordering::SeqCst);
                                    return;
                                }
                                debug!("auth response sent");
                                // ResponseFlushed：认证帧已证明 flush，只有现在才打开
                                // outbound gate，使握手期间缓冲的房间广播在客户端安装
                                // 回调后 drain。
                                auth_phase = AuthPhase::ResponseFlushed;
                                debug!(user = user.id, ?auth_phase, "auth frame flushed; pending gate activation");
                                // PMP44 P0-D: 预算耗尽则不再激活 gate。
                                if crate::official_client_compat::timing::deadline_expired(
                                    auth_deadline,
                                ) {
                                    warn!(
                                        user = user.id,
                                        "auth deadline elapsed after flush; skipping gate activation"
                                    );
                                    // Authenticate(Ok) 已到达客户端，客户端已进入认证后
                                    // 状态——P0-B：绝不补发 Err。但 WAL 事实已存在
                                    //（UserAuthenticated 已入队），且 P0-D 之后 accept 尚未
                                    // 把 Session 发布进 state.sessions，lost-connection worker
                                    // 找不到它、不会跑 dangle——因此必须由这里补发持久化补偿
                                    // 并做内存清理（P0-A）。rollback_failed_auth 以
                                    // durable=true + send_err=false 执行：清绑定（P0-C）、
                                    // 移除新用户（P0-E）、补发离线/断连事件、关闭传输。
                                    rollback_failed_auth(
                                        &server,
                                        &send_tx,
                                        &this,
                                        Some(&user),
                                        newly_created,
                                        bound_identity,
                                        true,
                                        false,
                                        "auth deadline elapsed after flush".to_string(),
                                    )
                                    .await;
                                    panicked.store(true, Ordering::SeqCst);
                                    return;
                                }
                                // PMP44 P0-I: 激活屏障同样经出站队列——出站任务在
                                // `Authenticate(Ok)` flush 之后才排空缓冲，绝不与认证帧
                                // 交错。oneshot 回传排空结果（P0-G fail-closed）。
                                let activate_remaining = auth_deadline
                                    .saturating_duration_since(Instant::now());
                                let activate_result: Result<()> =
                                    match tokio::time::timeout(
                                        activate_remaining,
                                        async {
                                            let (activate_tx, activate_rx) = oneshot::channel();
                                            outbound_tx
                                                .send(OutboundItem::Activate(activate_tx))
                                                .await
                                                .map_err(|err| {
                                                    anyhow!("outbound channel closed: {err}")
                                                })?;
                                            activate_rx.await.map_err(|_| {
                                                anyhow!(
                                                    "outbound task stopped during activation"
                                                )
                                            })?.map(|_| ())
                                        },
                                    )
                                    .await
                                    {
                                        Ok(r) => r,
                                        Err(_) => Err(anyhow!(
                                            "outbound gate activation exceeded deadline"
                                        )),
                                    };
                                match activate_result {
                                    Ok(()) => {}
                                    Err(err) => {
                                        // PMP44 P1 §33: 认证屏障排空失败观测计数。
                                        ProtocolTrace::get()
                                            .activation_drain_failure
                                            .fetch_add(1, Ordering::Relaxed);
                                        // PMP44 P0-G: 认证屏障排空失败 → fail-closed：
                                        // 不进入 Active。P0-B：Ok 已 flush——绝不补发 Err；
                                        // P0-A：WAL 事实已存在且 P0-D 下 accept 不会发布
                                        // 该 Session，lost-connection worker 不跑 dangle——
                                        // 必须由这里补发持久化补偿并做内存清理。
                                        warn!(
                                            user = user.id,
                                            ?err,
                                            "outbound gate activation failed; closing session"
                                        );
                                        panicked.store(true, Ordering::SeqCst);
                                        rollback_failed_auth(
                                            &server,
                                            &send_tx,
                                            &this,
                                            Some(&user),
                                            newly_created,
                                            bound_identity,
                                            true,
                                            false,
                                            format!("outbound gate activation failed: {err}"),
                                        )
                                        .await;
                                        return;
                                    }
                                }
                                auth_phase = AuthPhase::Active;
                                debug!(user = user.id, ?auth_phase, "outbound gate activated; session is live");
                                // PMP45 P0-D/P0-C: 两阶段交接——只有新会话达到 Active 才
                                // 关闭被取代的旧会话（reconnect）；此前任何一步失败旧连接
                                // 保持完整可回滚。
                                if let Some(previous) = previous_session {
                                    if previous.id != id {
                                        previous.stream.close();
                                        let _ = server.lost_con_tx.try_send(previous.id);
                                    }
                                }
                                // PMP45 P0-D: 标记认证完成，`server::accept` 据此把
                                // Session 发布进全局 sessions 表。
                                if let Some(session) = this.get() {
                                    session.mark_active();
                                }
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
                                    // PMP44 P0-F: 认证握手窗口内只记录重试日志，绝不向
                                    // 官方客户端发送 PMP 扩展 Chat 包。
                                    PhiraRetryNoticeTarget::StreamLogOnly,
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
                                        // PMP45 P0-C: 本次认证的精确绑定身份（console 不做
                                        // WAL，durable=false，无需持久化补偿）。
                                        let bound_identity = Some((this.get().unwrap().id, gen));
                                        // Initialize per-session mailbox for command routing
                                        if let Some(session) = this.get() {
                                            let tx = crate::session_actor::init_session_mailbox(session);
                                            let _ = session.actor_tx.set(tx);
                                        }
                                        // PMP44 P0-D: flush 前检查绝对预算——绝不发送
                                        // Authenticate(Ok) 于预算之外。PMP44 P0-E:
                                        // console 会话不做 WAL，flush 失败同样回滚。
                                        if crate::official_client_compat::timing::deadline_expired(
                                            auth_deadline,
                                        ) {
                                            warn!(
                                                user = user.id,
                                                "console auth deadline elapsed before response flush"
                                            );
                                            rollback_failed_auth(
                                                &server,
                                                &send_tx,
                                                &this,
                                                Some(&user),
                                                false,
                                                bound_identity,
                                                false,
                                                true,
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
                                        // PMP44 P0-G: 认证屏障超时未激活 → fail-closed。
                                        if gate.auth_duration_exceeded() {
                                            warn!(
                                                user = user.id,
                                                "console auth gate duration exceeded before response flush"
                                            );
                                            rollback_failed_auth(
                                                &server,
                                                &send_tx,
                                                &this,
                                                Some(&user),
                                                false,
                                                bound_identity,
                                                false,
                                                true,
                                                "authentication barrier timed out".to_string(),
                                            )
                                            .await;
                                            panicked.store(true, Ordering::SeqCst);
                                            return;
                                        }
                                        let flush_remaining = auth_deadline
                                            .saturating_duration_since(Instant::now());
                                        // PMP44 P0-I: Authenticate(Ok) 经出站队列送写。
                                        let flush_result: Result<()> =
                                            match tokio::time::timeout(
                                                flush_remaining,
                                                async {
                                                    let (flush_tx, flush_rx) = oneshot::channel();
                                                    outbound_tx
                                                        .send(OutboundItem::Critical(
                                                            ServerCommand::Authenticate(Ok((
                                                                user.to_info(),
                                                                None,
                                                            ))),
                                                            flush_tx,
                                                        ))
                                                        .await
                                                        .map_err(|err| {
                                                            anyhow!(
                                                                "outbound channel closed: {err}"
                                                            )
                                                        })?;
                                                    flush_rx.await.map_err(|_| {
                                                        anyhow!(
                                                            "outbound task stopped during auth flush"
                                                        )
                                                    })?
                                                },
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
                                            // PMP45 P0-B: Ok 已尝试 flush——绝不补发 Err。
                                            rollback_failed_auth(
                                                &server,
                                                &send_tx,
                                                &this,
                                                Some(&user),
                                                false,
                                                bound_identity,
                                                false,
                                                false,
                                                format!("console authentication failed: {err}"),
                                            )
                                            .await;
                                            panicked.store(true, Ordering::SeqCst);
                                            return;
                                        }
                                        // PMP44 P0-I: 激活屏障经出站队列，oneshot 回传排空结果。
                                        let activate_remaining = auth_deadline
                                            .saturating_duration_since(Instant::now());
                                        let activate_result: Result<()> =
                                            match tokio::time::timeout(
                                                activate_remaining,
                                                async {
                                                    let (activate_tx, activate_rx) =
                                                        oneshot::channel();
                                                    outbound_tx
                                                        .send(OutboundItem::Activate(activate_tx))
                                                        .await
                                                        .map_err(|err| {
                                                            anyhow!(
                                                                "outbound channel closed: {err}"
                                                            )
                                                        })?;
                                                    activate_rx.await.map_err(|_| {
                                                        anyhow!(
                                                            "outbound task stopped during activation"
                                                        )
                                                    })?.map(|_| ())
                                                },
                                            )
                                            .await
                                            {
                                                Ok(r) => r,
                                                Err(_) => Err(anyhow!(
                                                    "console outbound gate activation exceeded deadline"
                                                )),
                                            };
                                        match activate_result {
                                            Ok(()) => {}
                                            Err(err) => {
                                                // PMP44 P1 §33: 认证屏障排空失败观测计数。
                                                ProtocolTrace::get()
                                                    .activation_drain_failure
                                                    .fetch_add(1, Ordering::Relaxed);
                                                // PMP44 P0-G: 认证屏障排空失败 → fail-closed。
                                                // PMP45 P0-B: Ok 已 flush——绝不补发 Err；P0-D 下
                                                // accept 不发布该 Session，需由这里做内存清理
                                                //（console 无 WAL，durable=false，无需补偿）。
                                                warn!(
                                                    user = user.id,
                                                    ?err,
                                                    "console outbound gate activation failed; closing session"
                                                );
                                                panicked.store(true, Ordering::SeqCst);
                                                rollback_failed_auth(
                                                    &server,
                                                    &send_tx,
                                                    &this,
                                                    Some(&user),
                                                    false,
                                                    bound_identity,
                                                    false,
                                                    false,
                                                    format!(
                                                        "console outbound gate activation failed: {err}"
                                                    ),
                                                )
                                                .await;
                                                return;
                                            }
                                        }
                                        // PMP45 P0-D: console 会话达到 Active——通知 accept 发布。
                                        if let Some(session) = this.get() {
                                            session.mark_active();
                                        }
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
                                        // PMP45 P0-C: 本次认证的精确绑定身份（monitor 不做
                                        // WAL，durable=false）。
                                        let bound_identity = Some((this.get().unwrap().id, gen));
                                        if let Some(session) = this.get() {
                                            let tx = crate::session_actor::init_session_mailbox(session);
                                            let _ = session.actor_tx.set(tx);
                                        }
                                        *server.room_monitor.write().await =
                                            Some(Arc::downgrade(this.get().unwrap()));
                                        // PMP44 P0-D: flush 前检查绝对预算——绝不发送
                                        // Authenticate(Ok) 于预算之外。PMP44 P0-E:
                                        // monitor 不做 WAL，flush 失败同样回滚。
                                        if crate::official_client_compat::timing::deadline_expired(
                                            auth_deadline,
                                        ) {
                                            warn!(
                                                user = user.id,
                                                "room monitor auth deadline elapsed before response flush"
                                            );
                                            // 清除 room_monitor 弱引用，避免半认证 monitor 残留。
                                            *server.room_monitor.write().await = None;
                                            rollback_failed_auth(
                                                &server,
                                                &send_tx,
                                                &this,
                                                Some(&user),
                                                false,
                                                bound_identity,
                                                false,
                                                true,
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
                                        // PMP44 P0-G: 认证屏障超时未激活 → fail-closed。
                                        if gate.auth_duration_exceeded() {
                                            warn!(
                                                user = user.id,
                                                "room monitor auth gate duration exceeded before response flush"
                                            );
                                            // 清除 room_monitor 弱引用，避免半认证 monitor 残留。
                                            *server.room_monitor.write().await = None;
                                            rollback_failed_auth(
                                                &server,
                                                &send_tx,
                                                &this,
                                                Some(&user),
                                                false,
                                                bound_identity,
                                                false,
                                                true,
                                                "authentication barrier timed out".to_string(),
                                            )
                                            .await;
                                            panicked.store(true, Ordering::SeqCst);
                                            return;
                                        }
                                        let flush_remaining = auth_deadline
                                            .saturating_duration_since(Instant::now());
                                        // PMP44 P0-I: Authenticate(Ok) 经出站队列送写。
                                        let flush_result: Result<()> =
                                            match tokio::time::timeout(
                                                flush_remaining,
                                                async {
                                                    let (flush_tx, flush_rx) = oneshot::channel();
                                                    outbound_tx
                                                        .send(OutboundItem::Critical(
                                                            ServerCommand::Authenticate(Ok((
                                                                user.to_info(),
                                                                None,
                                                            ))),
                                                            flush_tx,
                                                        ))
                                                        .await
                                                        .map_err(|err| {
                                                            anyhow!(
                                                                "outbound channel closed: {err}"
                                                            )
                                                        })?;
                                                    flush_rx.await.map_err(|_| {
                                                        anyhow!(
                                                            "outbound task stopped during auth flush"
                                                        )
                                                    })?
                                                },
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
                                            *server.room_monitor.write().await = None;
                                            // PMP45 P0-B: Ok 已尝试 flush——绝不补发 Err。
                                            rollback_failed_auth(
                                                &server,
                                                &send_tx,
                                                &this,
                                                Some(&user),
                                                false,
                                                bound_identity,
                                                false,
                                                false,
                                                format!(
                                                    "room monitor authentication failed: {err}"
                                                ),
                                            )
                                            .await;
                                            panicked.store(true, Ordering::SeqCst);
                                            return;
                                        }
                                        // PMP44 P0-I: 激活屏障经出站队列，oneshot 回传排空结果。
                                        let activate_remaining = auth_deadline
                                            .saturating_duration_since(Instant::now());
                                        let activate_result: Result<()> =
                                            match tokio::time::timeout(
                                                activate_remaining,
                                                async {
                                                    let (activate_tx, activate_rx) =
                                                        oneshot::channel();
                                                    outbound_tx
                                                        .send(OutboundItem::Activate(activate_tx))
                                                        .await
                                                        .map_err(|err| {
                                                            anyhow!(
                                                                "outbound channel closed: {err}"
                                                            )
                                                        })?;
                                                    activate_rx.await.map_err(|_| {
                                                        anyhow!(
                                                            "outbound task stopped during activation"
                                                        )
                                                    })?.map(|_| ())
                                                },
                                            )
                                            .await
                                            {
                                                Ok(r) => r,
                                                Err(_) => Err(anyhow!(
                                                    "room monitor outbound gate activation exceeded deadline"
                                                )),
                                            };
                                        match activate_result {
                                            Ok(()) => {}
                                            Err(err) => {
                                                // PMP44 P1 §33: 认证屏障排空失败观测计数。
                                                ProtocolTrace::get()
                                                    .activation_drain_failure
                                                    .fetch_add(1, Ordering::Relaxed);
                                                // PMP44 P0-G: 认证屏障排空失败 → fail-closed。
                                                // PMP45 P0-B: Ok 已 flush——绝不补发 Err；P0-D 下
                                                // accept 不发布该 Session，需由这里做内存清理。
                                                warn!(
                                                    user = user.id,
                                                    ?err,
                                                    "room monitor outbound gate activation failed; closing session"
                                                );
                                                // 清除 room_monitor 弱引用，避免半认证 monitor 残留。
                                                *server.room_monitor.write().await = None;
                                                panicked.store(true, Ordering::SeqCst);
                                                rollback_failed_auth(
                                                    &server,
                                                    &send_tx,
                                                    &this,
                                                    Some(&user),
                                                    false,
                                                    bound_identity,
                                                    false,
                                                    false,
                                                    format!(
                                                        "room monitor outbound gate activation failed: {err}"
                                                    ),
                                                )
                                                .await;
                                                return;
                                            }
                                        }
                                        // PMP45 P0-D: room monitor 达到 Active——通知 accept 发布。
                                        if let Some(session) = this.get() {
                                            session.mark_active();
                                        }
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
                                    // PMP44 P0-F: 认证握手窗口内只记录重试日志，绝不向
                                    // 官方客户端发送 PMP 扩展 Chat 包。
                                    PhiraRetryNoticeTarget::StreamLogOnly,
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
                                        // PMP45 P0-C: 本次认证的精确绑定身份（monitor 不做
                                        // WAL，durable=false）。
                                        let bound_identity = Some((this.get().unwrap().id, gen));
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
                                        // Authenticate(Ok) 于预算之外。PMP44 P0-E:
                                        // monitor 不做 WAL，flush 失败同样回滚。
                                        if crate::official_client_compat::timing::deadline_expired(
                                            auth_deadline,
                                        ) {
                                            warn!(
                                                user = user.id,
                                                "game monitor auth deadline elapsed before response flush"
                                            );
                                            // 移除半认证 game monitor 的映射与注册。
                                            server.game_monitors.write().await.remove(&monitor_id);
                                            rollback_failed_auth(
                                                &server,
                                                &send_tx,
                                                &this,
                                                Some(&user),
                                                true,
                                                bound_identity,
                                                false,
                                                true,
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
                                        // PMP44 P0-G: 认证屏障超时未激活 → fail-closed。
                                        if gate.auth_duration_exceeded() {
                                            warn!(
                                                user = user.id,
                                                "game monitor auth gate duration exceeded before response flush"
                                            );
                                            // 移除半认证 game monitor 的映射与注册。
                                            server.game_monitors.write().await.remove(&monitor_id);
                                            rollback_failed_auth(
                                                &server,
                                                &send_tx,
                                                &this,
                                                Some(&user),
                                                true,
                                                bound_identity,
                                                false,
                                                true,
                                                "authentication barrier timed out".to_string(),
                                            )
                                            .await;
                                            panicked.store(true, Ordering::SeqCst);
                                            return;
                                        }
                                        let flush_remaining = auth_deadline
                                            .saturating_duration_since(Instant::now());
                                        // PMP44 P0-I: Authenticate(Ok) 经出站队列送写。
                                        let flush_result: Result<()> =
                                            match tokio::time::timeout(
                                                flush_remaining,
                                                async {
                                                    let (flush_tx, flush_rx) = oneshot::channel();
                                                    outbound_tx
                                                        .send(OutboundItem::Critical(
                                                            ServerCommand::Authenticate(Ok((
                                                                user.to_info(),
                                                                None,
                                                            ))),
                                                            flush_tx,
                                                        ))
                                                        .await
                                                        .map_err(|err| {
                                                            anyhow!(
                                                                "outbound channel closed: {err}"
                                                            )
                                                        })?;
                                                    flush_rx.await.map_err(|_| {
                                                        anyhow!(
                                                            "outbound task stopped during auth flush"
                                                        )
                                                    })?
                                                },
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
                                            server.game_monitors.write().await.remove(&monitor_id);
                                            // PMP45 P0-B: Ok 已尝试 flush——绝不补发 Err。
                                            rollback_failed_auth(
                                                &server,
                                                &send_tx,
                                                &this,
                                                Some(&user),
                                                true,
                                                bound_identity,
                                                false,
                                                false,
                                                format!("game monitor authentication failed: {err}"),
                                            )
                                            .await;
                                            panicked.store(true, Ordering::SeqCst);
                                            return;
                                        }
                                        // PMP44 P0-I: 激活屏障经出站队列，oneshot 回传排空结果。
                                        let activate_remaining = auth_deadline
                                            .saturating_duration_since(Instant::now());
                                        let activate_result: Result<()> =
                                            match tokio::time::timeout(
                                                activate_remaining,
                                                async {
                                                    let (activate_tx, activate_rx) =
                                                        oneshot::channel();
                                                    outbound_tx
                                                        .send(OutboundItem::Activate(activate_tx))
                                                        .await
                                                        .map_err(|err| {
                                                            anyhow!(
                                                                "outbound channel closed: {err}"
                                                            )
                                                        })?;
                                                    activate_rx.await.map_err(|_| {
                                                        anyhow!(
                                                            "outbound task stopped during activation"
                                                        )
                                                    })?.map(|_| ())
                                                },
                                            )
                                            .await
                                            {
                                                Ok(r) => r,
                                                Err(_) => Err(anyhow!(
                                                    "game monitor outbound gate activation exceeded deadline"
                                                )),
                                            };
                                        match activate_result {
                                            Ok(()) => {}
                                            Err(err) => {
                                                // PMP44 P1 §33: 认证屏障排空失败观测计数。
                                                ProtocolTrace::get()
                                                    .activation_drain_failure
                                                    .fetch_add(1, Ordering::Relaxed);
                                                // PMP44 P0-G: 认证屏障排空失败 → fail-closed。
                                                warn!(
                                                    user = user.id,
                                                    ?err,
                                                    "game monitor outbound gate activation failed; closing session"
                                                );
                                                server.game_monitors.write().await.remove(&monitor_id);
                                                // PMP45 P0-B: Ok 已 flush——绝不补发 Err；P0-D 下
                                                // accept 不发布该 Session，需由这里做内存清理并
                                                // 移除新注册的 monitor（P0-E）。
                                                panicked.store(true, Ordering::SeqCst);
                                                rollback_failed_auth(
                                                    &server,
                                                    &send_tx,
                                                    &this,
                                                    Some(&user),
                                                    true,
                                                    bound_identity,
                                                    false,
                                                    false,
                                                    format!(
                                                        "game monitor outbound gate activation failed: {err}"
                                                    ),
                                                )
                                                .await;
                                                return;
                                            }
                                        }
                                        // PMP45 P0-D: game monitor 达到 Active——通知 accept 发布。
                                        if let Some(session) = this.get() {
                                            session.mark_active();
                                        }
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
                        // PMP45 P0-I: 在唯一网络接收点拆分 commit 与 response
                        // deadline（绝不在此处之外重复减去 reserve）。`absolute_deadline`
                        // 仍是 response deadline——`send_dispatch_response` /
                        // `wait_until_minimum_bounded` / flush 全部继续使用它。
                        // `commit_deadline` 提前 reserve 毫秒，authoritative 提交
                        // 必须在它之前完成，留下 response budget 供最小响应时延与
                        // flush（audit §17：actor 在 deadline-1ms 提交后无时间 flush →
                        // 「服务端已提交、客户端已超时」）。`checked_sub` 防御性
                        // 兜底：配置已校验 reserve < 总 deadline，但极端配置下
                        // 回退到接收点（立即拒绝，安全侧）。
                        let commit_deadline = absolute_deadline
                            .checked_sub(std::time::Duration::from_millis(
                                user.server.config.compatibility.commit_response_reserve_ms,
                            ))
                            .unwrap_or(received_at);

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
                                            &outbound_tx,
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
                                    commit_deadline,
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
                                &outbound_tx,
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
                                if let Err(err) = outbound_tx
                                    .send(OutboundItem::Packet(ServerCommand::Message(
                                        Message::CreateRoom {
                                            user: creating_player.id,
                                        },
                                    )))
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
            active: OnceLock::new(),
            active_notify,
            outbound_tx,
            outbound_task_handle,
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

    /// PMP45 P0-D: 标记认证完成（Active）。认证回调在 `AuthPhase::Active`
    /// 时调用；`server::accept` 等待 `active_notify` 才把 Session 发布进全局
    /// sessions 表。使用 `notify_one`（存储一个 permit），避免 accept 检查
    /// `active` 与等待之间的丢失唤醒。
    pub(crate) fn mark_active(&self) {
        let _ = self.active.set(());
        self.active_notify.notify_one();
    }

    pub fn name(&self) -> &str {
        &self.user.name
    }

    #[allow(clippy::unused_async)]
    pub async fn try_send(&self, cmd: ServerCommand) {
        // PMP44 P0-I: 统一经出站队列。未激活时由出站任务缓冲进 gate（沿用
        // P0-G 有界剔除）；激活后直通送写。通道满说明该客户端跟不上（慢
        // 消费者），断开连接而非拖累 Room Actor 广播（P0-I §13.4）。
        if self
            .outbound_tx
            .try_send(OutboundItem::Packet(cmd))
            .is_err()
        {
            warn!(session = %self.id, user = self.user.id, "disconnecting slow client");
            self.stream.close();
            let _ = self.user.server.lost_con_tx.try_send(self.id);
        }
    }

    /// Send a command to this session, waiting for capacity (async).
    /// Closes the connection on error (same as try_send on failure).
    pub async fn send(&self, cmd: ServerCommand) -> Result<()> {
        self.outbound_tx.send(OutboundItem::Packet(cmd)).await.map_err(|err| {
            warn!(session = %self.id, user = self.user.id, ?err, "disconnecting slow client (send)");
            self.stream.close();
            let _ = self.user.server.lost_con_tx.try_send(self.id);
            anyhow!("outbound channel closed: {err}")
        })
    }

    /// Send a command and block until the packet has been flushed to the socket
    /// (P0-E/P0-F). Every request-type response — Authenticate, CreateRoom,
    /// JoinRoom, RequestStart, Ready, CancelReady, LeaveRoom, Chat, LockRoom,
    /// CycleRoom, SelectChart, Played, Abort — must be proven written to the
    /// wire, not merely queued (P0-G). The flush runs inside the single
    /// per-session outbound task (P0-I), so it is ordered against buffered
    /// events and post-response compensations. A flush failure closes the
    /// transport and enters the existing lost-connection path.
    pub async fn send_and_flush(&self, cmd: ServerCommand) -> Result<()> {
        let (flush_tx, flush_rx) = oneshot::channel();
        self.outbound_tx
            .send(OutboundItem::Critical(cmd, flush_tx))
            .await
            .map_err(|err| {
                warn!(session = %self.id, user = self.user.id, ?err, "disconnecting slow client (send_and_flush)");
                self.stream.close();
                let _ = self.user.server.lost_con_tx.try_send(self.id);
                anyhow!("outbound channel closed: {err}")
            })?;
        flush_rx.await.map_err(|_| {
            warn!(session = %self.id, user = self.user.id, "outbound task stopped during critical flush");
            self.stream.close();
            let _ = self.user.server.lost_con_tx.try_send(self.id);
            anyhow!("outbound task stopped during critical flush")
        })?
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.monitor_task_handle.abort();
        self.outbound_task_handle.abort();
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

    /// PMP44 P0-G: 测试用默认上限的 gate（数量 8 / 字节 64KiB / 时长 8000ms）。
    fn test_gate() -> SessionOutboundGate {
        SessionOutboundGate::new(8, 64 * 1024, Duration::from_millis(8000))
    }

    /// 一个永远失败的 sink，用于验证排空失败时的 fail-closed 行为。
    struct FailingSink;

    impl OutboundSink for FailingSink {
        async fn sink_send(&self, _cmd: ServerCommand) -> Result<()> {
            Err(anyhow!("sink failure"))
        }
        async fn sink_send_and_flush(&self, _cmd: ServerCommand) -> Result<()> {
            Err(anyhow!("sink failure"))
        }
        fn sink_try_send(&self, _cmd: ServerCommand) -> Result<()> {
            Err(anyhow!("sink failure"))
        }
    }

    #[tokio::test]
    async fn outbound_gate_buffers_before_activation() {
        let gate = test_gate();
        let sink = TestSink::default();
        assert!(gate.try_send(&sink, ServerCommand::Pong).await);
        assert!(gate
            .send(&sink, ServerCommand::ChangeHost(true))
            .await
            .is_ok());
        assert!(
            sink.sent.lock().unwrap().is_empty(),
            "pre-activation packets must be buffered, not forwarded"
        );
    }

    #[tokio::test]
    async fn outbound_gate_activation_drains_fifo_in_order() {
        let gate = test_gate();
        let sink = TestSink::default();
        gate.try_send(&sink, ServerCommand::ChangeHost(false)).await;
        gate.send(&sink, ServerCommand::Chat(Ok(()))).await.unwrap();
        gate.try_send(&sink, ServerCommand::Pong).await;
        gate.activate(&sink).await.unwrap();

        let sent = sink.sent.lock().unwrap();
        assert_eq!(sent.len(), 3);
        // FIFO: buffered order is preserved after activation.
        assert!(matches!(sent[0], ServerCommand::ChangeHost(false)));
        assert!(matches!(sent[1], ServerCommand::Chat(Ok(()))));
        assert!(matches!(sent[2], ServerCommand::Pong));
    }

    #[tokio::test]
    async fn outbound_gate_forwarding_passes_through_after_activation() {
        let gate = test_gate();
        let sink = TestSink::default();
        gate.try_send(&sink, ServerCommand::Pong).await;
        gate.activate(&sink).await.unwrap();
        assert!(gate
            .send(&sink, ServerCommand::LockRoom(Ok(())))
            .await
            .is_ok());
        assert_eq!(sink.sent.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    #[should_panic(expected = "send_and_flush before outbound activation")]
    async fn send_and_flush_before_activation_panics() {
        let gate = test_gate();
        let sink = TestSink::default();
        let _ = gate.send_and_flush(&sink, ServerCommand::Pong).await;
    }

    /// PMP44 P0-G: 缓冲超过数量上限时，控制事件丢弃最旧（保留最新），
    /// 高频遥测直接 coalesce。
    #[tokio::test]
    async fn outbound_gate_bounded_control_keeps_latest_telemetry_dropped() {
        // 事件上限 3：控制事件满后继续入队应丢弃最旧。
        let gate = SessionOutboundGate::new(3, 64 * 1024, Duration::from_millis(8000));
        let sink = TestSink::default();
        let dropped_before = ProtocolTrace::get().gate_dropped.load(Ordering::Relaxed);

        assert!(gate.try_send(&sink, ServerCommand::ChangeHost(true)).await);
        assert!(gate.try_send(&sink, ServerCommand::Chat(Ok(()))).await);
        assert!(gate.try_send(&sink, ServerCommand::ChangeHost(false)).await);
        // 第四条控制事件触发 drop-oldest：丢弃最旧的 ChangeHost(true)。
        assert!(gate.try_send(&sink, ServerCommand::LockRoom(Ok(()))).await);
        gate.activate(&sink).await.unwrap();

        {
            // 作用域限定的锁：guard 必须在后续 `.await` 之前释放（await_holding_lock）。
            let sent = sink.sent.lock().unwrap();
            assert_eq!(sent.len(), 3, "oldest control event must be dropped");
            assert!(matches!(sent[0], ServerCommand::Chat(Ok(()))));
            assert!(matches!(sent[1], ServerCommand::ChangeHost(false)));
            assert!(matches!(sent[2], ServerCommand::LockRoom(Ok(()))));
        }

        // 遥测：缓冲满后新 Pong 直接丢弃（coalesce），不触发 drop-oldest。
        let gate2 = SessionOutboundGate::new(2, 64 * 1024, Duration::from_millis(8000));
        let sink2 = TestSink::default();
        assert!(gate2.try_send(&sink2, ServerCommand::Pong).await);
        assert!(gate2.try_send(&sink2, ServerCommand::Pong).await);
        assert!(gate2.try_send(&sink2, ServerCommand::Pong).await); // coalesced
        gate2.activate(&sink2).await.unwrap();
        assert_eq!(sink2.sent.lock().unwrap().len(), 2);

        // 至少发生了 1 次 drop（drop-oldest 或 telemetry coalesce）。
        let dropped_after = ProtocolTrace::get().gate_dropped.load(Ordering::Relaxed);
        assert!(
            dropped_after > dropped_before,
            "gate_dropped must have incremented"
        );
    }

    /// PMP44 P0-G: 排空期间发送失败 → activate 返回 Err，屏障保持未激活，
    /// 剩余缓冲被清空（fail-closed）。
    #[tokio::test]
    async fn outbound_gate_activation_failure_fail_closed() {
        let gate = test_gate();
        let sink = FailingSink;
        assert!(gate.try_send(&sink, ServerCommand::Pong).await);
        assert!(gate.try_send(&sink, ServerCommand::ChangeHost(true)).await);
        let result = gate.activate(&sink).await;
        assert!(result.is_err());
        assert!(
            !gate.is_activated(),
            "failed activation must leave the gate closed"
        );
        // 失败后缓冲已清空：新的未激活 send 仍会进入缓冲（不会直通 sink）。
        let sink2 = TestSink::default();
        assert!(gate.try_send(&sink2, ServerCommand::Pong).await);
        assert!(sink2.sent.lock().unwrap().is_empty());
    }

    /// PMP44 P0-H / PMP45 P0-G: 快照切换屏障——`seq <= cutover` 的
    /// **SnapshotCovered** 缓冲事件（快照已包含）在激活时被剔除；`seq >
    /// cutover`（快照构建期间到达）的事件正常发送。Chat 是 `NonSnapshot`，
    /// 无论序号一律发送（修复 §11 的认证窗口聊天丢失）。
    #[tokio::test]
    async fn outbound_gate_cutover_drops_snapshot_events_sends_delta() {
        let gate = test_gate();
        let sink = TestSink::default();
        // 快照构建前已缓冲的事件（已被快照反映）。
        assert!(gate.try_send(&sink, ServerCommand::ChangeHost(true)).await);
        assert!(gate.try_send(&sink, ServerCommand::Chat(Ok(()))).await);
        assert!(gate
            .try_send(&sink, ServerCommand::Message(Message::GameStart { user: 7 }))
            .await);
        // cutover 值为最后已入队事件的序号；在 build_client_room_state 之前调用。
        let _cutover = gate.begin_snapshot_cutover().await;
        // 快照构建期间到达的事件（快照未包含，必须发送）。
        assert!(gate.try_send(&sink, ServerCommand::Pong).await);
        assert!(gate.try_send(&sink, ServerCommand::ChangeHost(false)).await);
        gate.activate(&sink).await.unwrap();

        let sent = sink.sent.lock().unwrap();
        // ChangeHost(true) 是 SnapshotCovered 且 seq <= cutover → 剔除；
        // Chat(Ok) 与 Message::GameStart 是 NonSnapshot → 必须发送（P0-G）；
        // Pong 是 Telemetry → 发送；ChangeHost(false) 是 cutover 后增量 → 发送。
        assert_eq!(sent.len(), 4, "only snapshot-covered events are cut over");
        assert!(matches!(sent[0], ServerCommand::Chat(Ok(()))));
        assert!(matches!(sent[1], ServerCommand::Message(Message::GameStart { .. })));
        assert!(matches!(sent[2], ServerCommand::Pong));
        assert!(matches!(sent[3], ServerCommand::ChangeHost(false)));
    }

    /// PMP45 P0-G: `classify_command` 分类——快照覆盖/非快照/遥测。
    #[test]
    fn gate_classify_command_splits_snapshot_vs_semantic_vs_telemetry() {
        assert_eq!(
            classify_command(&ServerCommand::ChangeState(
                phira_mp_common::RoomState::WaitingForReady
            )),
            GateEventClass::SnapshotCovered
        );
        assert_eq!(
            classify_command(&ServerCommand::Message(Message::LockRoom { lock: true })),
            GateEventClass::SnapshotCovered
        );
        assert_eq!(
            classify_command(&ServerCommand::OnJoinRoom(phira_mp_common::UserInfo {
                id: 1,
                name: "p".into(),
                monitor: false,
            })),
            GateEventClass::SnapshotCovered
        );
        // Chat 与语义消息：cutover 绝不丢弃。
        assert_eq!(
            classify_command(&ServerCommand::Message(Message::Chat {
                user: 1,
                content: "hi".into(),
            })),
            GateEventClass::NonSnapshot
        );
        assert_eq!(
            classify_command(&ServerCommand::Message(Message::Ready { user: 1 })),
            GateEventClass::NonSnapshot
        );
        assert_eq!(
            classify_command(&ServerCommand::Message(Message::Played {
                user: 1, score: 0, accuracy: 0.0, full_combo: false,
                perfect: 0, good: 0, bad: 0, miss: 0, max_combo: 0,
            })),
            GateEventClass::NonSnapshot
        );
        assert_eq!(classify_command(&ServerCommand::Chat(Ok(()))), GateEventClass::NonSnapshot);
        // 遥测。
        assert_eq!(classify_command(&ServerCommand::Pong), GateEventClass::Telemetry);
        assert_eq!(
            classify_command(&ServerCommand::Touches {
                player: 1,
                frames: std::sync::Arc::new(vec![]),
            }),
            GateEventClass::Telemetry
        );
    }

    /// PMP45 P0-H: 控制事件溢出 → fail-closed。单条非遥测事件单独超过字节
    /// 预算时，`push_bounded` 置 `overflowed`，`activate` 返回 `Err`。
    #[tokio::test]
    async fn outbound_gate_control_overflow_fail_closed() {
        // 字节预算极小（4 字节）；一条 100 字符的 Chat（NonSnapshot）必然
        // 放不下。遥测不会触发 overflow（coalesce），因此用 Chat 验证。
        let gate = SessionOutboundGate::new(8, 4, Duration::from_millis(8000));
        let sink = TestSink::default();
        let dropped_before = ProtocolTrace::get().gate_control_overflow.load(Ordering::Relaxed);

        // 缓冲为空时入队超预算的语义事件：单事件即超预算 → overflowed。
        assert!(gate
            .try_send(
                &sink,
                ServerCommand::Message(Message::Chat {
                    user: 1,
                    content: "x".repeat(100),
                }),
            )
            .await);
        let dropped_after = ProtocolTrace::get().gate_control_overflow.load(Ordering::Relaxed);
        assert!(
            dropped_after > dropped_before,
            "gate_control_overflow must increment"
        );
        // 缓冲被清空，未入队。
        assert!(gate.pending.try_lock().unwrap().events.is_empty());

        // activate 检测到溢出 → fail-closed Err，绝不激活不完整连接。
        let result = gate.activate(&sink).await;
        assert!(result.is_err());
        assert!(!gate.is_activated());
    }

    /// PMP44 P0-G: 认证屏障超时未激活 → auth_duration_exceeded。
    #[tokio::test]
    async fn outbound_gate_auth_duration_exceeded_reports() {
        // 零时长屏障：短暂等待后必然超时（避免 Instant 分辨率导致的抖动）。
        let gate = SessionOutboundGate::new(8, 64 * 1024, Duration::from_millis(0));
        time::sleep(Duration::from_millis(1)).await;
        assert!(gate.auth_duration_exceeded());
        // 长持续时间在刚创建时未超时。
        let fresh = SessionOutboundGate::new(8, 64 * 1024, Duration::from_secs(60));
        assert!(!fresh.auth_duration_exceeded());
    }

    /// PMP44 P0-E: 认证阶段按
    /// Authenticating → DurableAccepted → ResponseFlushed → Active 单向推进；
    /// 任一步失败（WAL 失败 / flush 失败）都必须回滚，只有进入 Active 后
    /// 才不再回滚。
    #[test]
    fn phase_progresses_correctly() {
        // 阶段推进顺序（声明顺序即判别值顺序）。
        assert!(AuthPhase::Authenticating < AuthPhase::DurableAccepted);
        assert!(AuthPhase::DurableAccepted < AuthPhase::ResponseFlushed);
        assert!(AuthPhase::ResponseFlushed < AuthPhase::Active);

        // Authenticating 阶段 WAL 失败 → 必须回滚。
        assert!(should_rollback_auth(AuthPhase::Authenticating, false, true));
        // DurableAccepted 阶段 flush 失败 → 必须回滚。
        assert!(should_rollback_auth(
            AuthPhase::DurableAccepted,
            true,
            false
        ));
        // ResponseFlushed 阶段 flush 失败（防御性）→ 必须回滚。
        assert!(should_rollback_auth(
            AuthPhase::ResponseFlushed,
            true,
            false
        ));

        // 成功路径全部不回滚。
        assert!(!should_rollback_auth(AuthPhase::Authenticating, true, true));
        assert!(!should_rollback_auth(
            AuthPhase::DurableAccepted,
            true,
            true
        ));
        assert!(!should_rollback_auth(
            AuthPhase::ResponseFlushed,
            true,
            true
        ));
        // 已激活（Active）后，即使下游异常也不回滚。
        assert!(!should_rollback_auth(AuthPhase::Active, false, false));
    }
}

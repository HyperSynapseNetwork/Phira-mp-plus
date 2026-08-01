//! Session actor 每连接独立邮箱（迁移中）。
//!
//! 每个 Session 创建时初始化独立 mailbox，命令通过该 Session 的邮箱路由。
//! 所有有序业务命令必须经过该邮箱。
//!
//! PMP44 P0-K：路由结果区分「确定未入队」与「入队后不确定」。确定未入队
//! （邮箱缺失、关闭、入队超时）不关闭连接——调用方在原 deadline 内先发送
//! 官方错误响应，客户端据此可重试（audit §17.1）；入队后回复丢失（通道
//! 关闭/超时）才关闭 origin 传输（结果不确定，actor 可能已提交），并返回
//! best-effort 错误响应。绝不静默丢弃请求（PMP42 P0-A）。
//!
//! PMP42 P0-C：每条命令携带绝对 deadline（`CommandMeta::deadline`），
//! mailbox 发送与 reply 共享该预算；Actor 在执行前检查 deadline，
//! 过期命令不提交状态，直接返回对应错误。
//!
//! 迁移状态：WriteRouted（Ping、Authenticate、Touches/Judges、
//! QueryRoomInfo 属于协议快路径，不进入业务命令邮箱）。

use crate::session::{CommandOrigin, Session, SessionCategory, User};
use phira_mp_common::{RoomId, ServerCommand};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

/// Channel capacity for each per-session mailbox.
const SESSION_MAILBOX_CAPACITY: usize = 64;

/// Create a per-session mailbox for the given session and spawn the worker.
/// Returns the sender for the mailbox.
pub(crate) fn init_session_mailbox(session: &Arc<Session>) -> mpsc::Sender<SessionActorCmd> {
    let (tx, mut rx) = mpsc::channel::<SessionActorCmd>(SESSION_MAILBOX_CAPACITY);
    let weak_session = Arc::downgrade(session);
    let session_id = session.id;
    crate::supervisor_actor::spawn_named(format!("session-mailbox-{}", session.id), async move {
        while let Some(cmd) = rx.recv().await {
            // If session is gone, stop processing.
            if weak_session.upgrade().is_none() {
                break;
            }
            // P0-A: a command whose origin session is no longer the user's
            // current session (a reconnect bumped the generation) must never
            // execute — its response and compensations are bound to the old
            // origin. Stop the worker; the origin transport is being torn down.
            if !worker_should_run(cmd.origin()).await {
                tracing::debug!(session = %session_id, "origin superseded; stopping mailbox worker");
                break;
            }
            match cmd {
                SessionActorCmd::Chat {
                    meta,
                    user,
                    category,
                    msg,
                    reply,
                } => {
                    tracing::trace!(cmd_id = meta.command_id, "session actor: chat");
                    run_or_deadline(
                        meta.deadline,
                        reply,
                        Some(ServerCommand::Chat(Err(
                            "session command timed out".to_string(),
                        ))),
                        handle_chat(
                            user,
                            category,
                            msg,
                            meta.deadline,
                            meta.origin.clone(),
                        ),
                    )
                    .await;
                }
                SessionActorCmd::Lock { meta, user, lock, reply } => {
                    tracing::trace!(cmd_id = meta.command_id, "session actor: lock");
                    run_or_deadline(
                        meta.deadline,
                        reply,
                        Some(ServerCommand::LockRoom(Err(
                            "session command timed out".to_string(),
                        ))),
                        handle_lock(user, lock, meta.deadline, meta.origin.clone()),
                    )
                    .await;
                }
                SessionActorCmd::Cycle { meta, user, cycle, reply } => {
                    tracing::trace!(cmd_id = meta.command_id, "session actor: cycle");
                    run_or_deadline(
                        meta.deadline,
                        reply,
                        Some(ServerCommand::CycleRoom(Err(
                            "session command timed out".to_string(),
                        ))),
                        handle_cycle(user, cycle, meta.deadline, meta.origin.clone()),
                    )
                    .await;
                }
                SessionActorCmd::Leave { meta, user, category, reply } => {
                    tracing::trace!(cmd_id = meta.command_id, "session actor: leave");
                    run_or_deadline(
                        meta.deadline,
                        reply,
                        Some(ServerCommand::LeaveRoom(Err(
                            "session command timed out".to_string(),
                        ))),
                        handle_leave(user, category, meta.deadline, meta.origin.clone()),
                    )
                    .await;
                }
                SessionActorCmd::Create { meta, user, id, reply } => {
                    tracing::trace!(cmd_id = meta.command_id, "session actor: create");
                    run_or_deadline(
                        meta.deadline,
                        reply,
                        Some(ServerCommand::CreateRoom(Err(
                            "session command timed out".to_string(),
                        ))),
                        handle_create(user, id, meta.deadline, meta.origin.clone()),
                    )
                    .await;
                }
                SessionActorCmd::Join { meta, user, category, id, monitor, received_at, reply } => {
                    tracing::trace!(cmd_id = meta.command_id, "session actor: join");
                    run_or_deadline(
                        meta.deadline,
                        reply,
                        Some(ServerCommand::JoinRoom(Err(
                            "session command timed out".to_string(),
                        ))),
                        handle_join(
                            user,
                            category,
                            id,
                            monitor,
                            meta.deadline,
                            received_at,
                            meta.origin.clone(),
                        ),
                    )
                    .await;
                }
                SessionActorCmd::SelectChart { meta, user, id, reply } => {
                    tracing::trace!(cmd_id = meta.command_id, "session actor: select_chart");
                    run_or_deadline(
                        meta.deadline,
                        reply,
                        Some(ServerCommand::SelectChart(Err(
                            "session command timed out".to_string(),
                        ))),
                        handle_select_chart(user, id, meta.deadline, meta.origin.clone()),
                    )
                    .await;
                }
                SessionActorCmd::RequestStart { meta, user, reply } => {
                    tracing::trace!(cmd_id = meta.command_id, "session actor: request_start");
                    run_or_deadline(
                        meta.deadline,
                        reply,
                        Some(ServerCommand::RequestStart(Err(
                            "session command timed out".to_string(),
                        ))),
                        handle_request_start(user, meta.deadline, meta.origin.clone()),
                    )
                    .await;
                }
                SessionActorCmd::Ready { meta, user, reply } => {
                    tracing::trace!(cmd_id = meta.command_id, "session actor: ready");
                    run_or_deadline(
                        meta.deadline,
                        reply,
                        Some(ServerCommand::Ready(Err(
                            "session command timed out".to_string(),
                        ))),
                        handle_ready(user, meta.deadline, meta.origin.clone()),
                    )
                    .await;
                }
                SessionActorCmd::CancelReady { meta, user, reply } => {
                    tracing::trace!(cmd_id = meta.command_id, "session actor: cancel_ready");
                    run_or_deadline(
                        meta.deadline,
                        reply,
                        Some(ServerCommand::CancelReady(Err(
                            "session command timed out".to_string(),
                        ))),
                        handle_cancel_ready(user, meta.deadline, meta.origin.clone()),
                    )
                    .await;
                }
                SessionActorCmd::Played { meta, user, id, reply } => {
                    tracing::trace!(cmd_id = meta.command_id, "session actor: played");
                    run_or_deadline(
                        meta.deadline,
                        reply,
                        Some(ServerCommand::Played(Err(
                            "session command timed out".to_string(),
                        ))),
                        handle_played(user, id, meta.deadline, meta.origin.clone()),
                    )
                    .await;
                }
                SessionActorCmd::Abort { meta, user, reply } => {
                    tracing::trace!(cmd_id = meta.command_id, "session actor: abort");
                    run_or_deadline(
                        meta.deadline,
                        reply,
                        Some(ServerCommand::Abort(Err(
                            "session command timed out".to_string(),
                        ))),
                        handle_abort(user, meta.deadline, meta.origin.clone()),
                    )
                    .await;
                }
            }
        }
    });
    tx
}

// ── Command envelope ──────────────────────────────────────────────

/// A global atomic counter for session command tracing.
static NEXT_COMMAND_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Command envelope metadata.
pub(crate) struct CommandMeta {
    pub command_id: u64,
    /// Retained for future diagnostics/metrics integration.
    #[allow(dead_code)]
    pub created_at_ms: u64,
    /// Absolute deadline for the whole send→execute→reply pipeline. The actor
    /// checks it before executing (and MUST NOT commit after it passes).
    pub deadline: std::time::Instant,
    /// The Session that initiated this command. Every response, error, close
    /// and post-response compensation is bound to this origin, never to the
    /// user's current session (P0-A).
    pub origin: CommandOrigin,
}

impl CommandMeta {
    fn new(deadline: std::time::Instant, origin: CommandOrigin) -> Self {
        Self {
            command_id: NEXT_COMMAND_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            created_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            deadline,
            origin,
        }
    }
}

/// Decision point for the mailbox worker: may this queued command still run?
/// A stale origin means the user reconnected while the command was queued, so
/// it must be refused — its response and compensations are bound to the old
/// session (P0-A). Refusing also stops the worker, because the origin session
/// is being torn down.
pub(crate) async fn worker_should_run(origin: &CommandOrigin) -> bool {
    let current = origin.is_current().await;
    if !current {
        // PMP44 P1 §33: origin session 已非该用户当前绑定（重连抬升代际），
        // 命令被拒绝执行（P0-A）——跨会话命令观测计数 +1。
        crate::official_client_compat::protocol_trace::ProtocolTrace::get()
            .cross_session_command
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    current
}

pub(crate) enum SessionActorCmd {
    Chat {
        meta: CommandMeta,
        user: Arc<User>,
        category: SessionCategory,
        msg: String,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    Lock {
        meta: CommandMeta,
        user: Arc<User>,
        lock: bool,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    Cycle {
        meta: CommandMeta,
        user: Arc<User>,
        cycle: bool,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    Leave {
        meta: CommandMeta,
        user: Arc<User>,
        category: SessionCategory,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    Create {
        meta: CommandMeta,
        user: Arc<User>,
        id: RoomId,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    Join {
        meta: CommandMeta,
        user: Arc<User>,
        category: SessionCategory,
        id: RoomId,
        monitor: bool,
        /// Command receipt time recorded at the dispatch boundary (P0-F). Used
        /// to enforce the minimum response latency for the internally-delivered
        /// JoinRoom(Ok) response.
        received_at: std::time::Instant,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    SelectChart {
        meta: CommandMeta,
        user: Arc<User>,
        id: i32,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    RequestStart {
        meta: CommandMeta,
        user: Arc<User>,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    Ready {
        meta: CommandMeta,
        user: Arc<User>,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    CancelReady {
        meta: CommandMeta,
        user: Arc<User>,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    Played {
        meta: CommandMeta,
        user: Arc<User>,
        id: i32,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    Abort {
        meta: CommandMeta,
        user: Arc<User>,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
}

impl SessionActorCmd {
    /// The originating Session every response/compensation for this command is
    /// bound to (P0-A).
    pub(crate) fn origin(&self) -> &CommandOrigin {
        match self {
            SessionActorCmd::Chat { meta, .. }
            | SessionActorCmd::Lock { meta, .. }
            | SessionActorCmd::Cycle { meta, .. }
            | SessionActorCmd::Leave { meta, .. }
            | SessionActorCmd::Create { meta, .. }
            | SessionActorCmd::Join { meta, .. }
            | SessionActorCmd::SelectChart { meta, .. }
            | SessionActorCmd::RequestStart { meta, .. }
            | SessionActorCmd::Ready { meta, .. }
            | SessionActorCmd::CancelReady { meta, .. }
            | SessionActorCmd::Played { meta, .. }
            | SessionActorCmd::Abort { meta, .. } => &meta.origin,
        }
    }
}

// ── Generic route helper ──────────────────────────────────────────

async fn close_uncertain_session(origin: &CommandOrigin, reason: &'static str) {
    tracing::warn!(reason, "session command outcome is uncertain; closing origin transport");
    origin.close_uncertain().await;
}

/// Execute a command handler unless the absolute actor deadline has already
/// passed. A late command MUST NOT mutate authoritative state — reply with the
/// matching error response and count it as a blocked late commit (P0-C).
///
/// PMP44 P0-J: the handler is wrapped in the REMAINING budget. A handler that
/// outlives its absolute deadline (e.g. an external Phira fetch, persistence
/// admission or plugin callback) is aborted and the error response is returned
/// instead of committing after the client already timed out.
async fn run_or_deadline(
    deadline: std::time::Instant,
    reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    error_response: Option<ServerCommand>,
    handler: impl std::future::Future<Output = Option<ServerCommand>>,
) {
    if crate::official_client_compat::timing::deadline_expired(deadline) {
        crate::official_client_compat::protocol_trace::ProtocolTrace::get()
            .late_commit
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tracing::warn!(
            ?deadline,
            "session command arrived after deadline; refusing to commit"
        );
        let _ = reply.send(error_response);
        return;
    }
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    match tokio::time::timeout(remaining, handler).await {
        Ok(result) => {
            let _ = reply.send(result);
        }
        Err(_) => {
            // The handler outlived its absolute deadline — abort it and refuse.
            crate::official_client_compat::protocol_trace::ProtocolTrace::get()
                .late_commit
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            tracing::warn!(
                ?deadline,
                "session command handler exceeded deadline; aborting"
            );
            let _ = reply.send(error_response);
        }
    }
}

/// PMP44 P0-K: 路由结果——区分确定未入队与入队后不确定。
///
/// `route_via_mailbox` 不再对“确定未入队”的情况关闭 origin 传输（audit §17.1：
/// 关闭会先于错误响应杀掉连接，客户端只看到断连而收不到官方 Err）。
enum MailboxRouteResult {
    /// 命令确定未入队（mailbox 缺失/关闭/入队超时），未修改任何权威状态。
    /// 调用方应在原 Session deadline 内先发送对应官方 Err，再按策略关闭。
    NotEnqueued,
    /// 命令已入队并返回了结果。
    Completed(Option<ServerCommand>),
    /// 命令已入队但结果不确定（reply 关闭/超时）——origin 已被关闭，
    /// 不要再假装能发送业务 Err。
    Uncertain,
}

/// PMP44 P0-K: 纯决策——把邮箱阶段的失败归类为「确定未入队」还是「入队后
/// 不确定」。仅用于失败分支；`enqueued=true, reply_ok=true` 是成功路径，调用方
/// 持有真实 handler 结果直接构造 `Completed(result)`，不经过本分类器（此时返回
/// 的 `Completed(None)` 只是占位，绝不会被使用）。
fn classify_mailbox_failure(enqueued: bool, reply_ok: bool) -> MailboxRouteResult {
    if !enqueued {
        MailboxRouteResult::NotEnqueued
    } else if reply_ok {
        MailboxRouteResult::Completed(None)
    } else {
        MailboxRouteResult::Uncertain
    }
}

/// PMP44 P0-K: 把 `route_via_mailbox` 的路由结果翻译为 dispatch 层最终响应的
/// `Option<ServerCommand>`（保持各 `route_xxx` 外签名不变）。
///
/// - `Completed` 原样透传 handler 结果。
/// - `NotEnqueued`：命令确定未入队——传输未被关闭，先发官方 Err 让客户端知道
///   失败原因（可重试）；若 origin 已被 reconnect 取代（旧会话正在拆毁），直接
///   丢弃响应（OriginStale：不发送 Err 也不关闭）。
/// - `Uncertain`：origin 已被 `route_via_mailbox` 关闭，返回的 Err 只是
///   best-effort（发送失败会自然进入 lost-connection 路径，符合 audit
///   “不确定结果关闭 origin 合理，但不应再假装能够发送业务 Err”）。
async fn translate_route_result(
    origin: &CommandOrigin,
    result: MailboxRouteResult,
    error_response: impl FnOnce(String) -> Option<ServerCommand>,
) -> Option<ServerCommand> {
    match result {
        MailboxRouteResult::Completed(result) => result,
        MailboxRouteResult::NotEnqueued => {
            if !origin.is_current().await {
                tracing::debug!(
                    generation = origin.generation,
                    "route: origin superseded; dropping not-enqueued error response"
                );
                None
            } else {
                error_response("session command could not be enqueued".to_string())
            }
        }
        MailboxRouteResult::Uncertain => {
            error_response("session command outcome uncertain".to_string())
        }
    }
}

/// Send a command through the per-session mailbox.
///
/// There is deliberately no direct fallback. The result distinguishes whether
/// the command was enqueued so the caller can decide how to answer (PMP44 P0-K):
///
/// - Deterministic not-enqueued failures (mailbox missing, channel closed before
///   enqueue, enqueue timeout) do NOT close the transport — the caller must send
///   the official error response within the origin deadline so the client learns
///   the failure instead of only seeing a disconnect (§17.1).
/// - After a successful enqueue, a lost reply (channel closed or timeout) is
///   genuinely uncertain — the actor may have committed — so the origin transport
///   is closed and the caller's error response is best-effort.
///
/// Both the mailbox enqueue and the reply wait share the single absolute
/// `deadline`; each stage only uses the remaining budget.
async fn route_via_mailbox<Build>(
    origin: CommandOrigin,
    user: Arc<User>,
    deadline: std::time::Instant,
    build: Build,
) -> MailboxRouteResult
where
    Build: FnOnce(
            CommandOrigin,
            Arc<User>,
            tokio::sync::oneshot::Sender<Option<ServerCommand>>,
        ) -> SessionActorCmd,
{
    // P0-A: the origin is captured at the network boundary (session.rs), NOT by
    // re-reading the user's current binding here. The user's *current* session
    // may be replaced by a reconnect at any time; every response, error, close
    // and compensation for this command stays bound to the origin captured at
    // receive time, never to the new session.
    //
    // Route through the ORIGIN session's mailbox — not `user.binding`, which may
    // already point at a newer session after a reconnect.
    let tx = origin
        .session
        .upgrade()
        .and_then(|session| session.actor_tx.get().cloned());
    let Some(tx) = tx else {
        // PMP44 P0-K: 邮箱缺失——命令确定未入队。不在此关闭传输：origin session
        // 可能仍活着，关闭会先于错误响应杀掉连接（§17.1）。若 origin 已被取代，
        // translate_route_result 会检测到并直接丢弃响应（OriginStale）。
        return classify_mailbox_failure(false, false);
    };

    let (reply, rx) = tokio::sync::oneshot::channel();
    let cmd = build(origin.clone(), Arc::clone(&user), reply);
    let send_budget = deadline.saturating_duration_since(std::time::Instant::now());
    match tokio::time::timeout(send_budget, tx.send(cmd)).await {
        Ok(Ok(())) => {
            let reply_budget = deadline.saturating_duration_since(std::time::Instant::now());
            match tokio::time::timeout(reply_budget, rx).await {
                Ok(Ok(result)) => MailboxRouteResult::Completed(result),
                Ok(Err(_)) => {
                    // 入队后 reply 通道关闭——actor 可能已提交，结果不确定。
                    close_uncertain_session(&origin, "reply channel closed after enqueue").await;
                    classify_mailbox_failure(true, false)
                }
                Err(_) => {
                    // 入队后 reply 超时——actor 可能已提交，结果不确定。
                    close_uncertain_session(&origin, "reply timed out after enqueue").await;
                    classify_mailbox_failure(true, false)
                }
            }
        }
        // 邮箱已关闭 / 入队超时：Tokio 取消语义保证消息未被入队——确定未入队，
        // 不关闭传输，调用方先发送官方 Err。
        Ok(Err(_)) | Err(_) => classify_mailbox_failure(false, false),
    }
}

// ── Chat ──────────────────────────────────────────────────────────

async fn handle_chat(
    user: Arc<User>,
    _category: SessionCategory,
    content: String,
    deadline: std::time::Instant,
    _origin: CommandOrigin,
) -> Option<ServerCommand> {
    use anyhow::Result;
    if !user.server.live_config.read().await.chat_enabled {
        return Some(ServerCommand::Chat(Err(crate::tl!("chat-disabled"))));
    }
    // PMP44 P0-J: 绝对预算已耗尽时拒绝提交（persistence enqueue + room
    // broadcast 都不得在客户端已超时之后执行）。
    if crate::official_client_compat::timing::deadline_expired(deadline) {
        crate::official_client_compat::protocol_trace::ProtocolTrace::get()
            .late_commit
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tracing::warn!(?deadline, "chat command arrived after deadline; refusing to commit");
        return Some(ServerCommand::Chat(Err("chat timed out".to_string())));
    }
    let res: Result<()> = async {
        let room = user.room.read().await.as_ref().map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("{}", crate::tl!("no-room")))?;
        // PersistenceWorker (exclusive — no direct DB write)
        if let Err(e) = user.server.persistence_worker.enqueue(
            crate::persistence::message::PersistenceEvent::ServerEvent {
                kind: "chat.message".to_string(),
                payload: Arc::new(serde_json::json!({"room_id": room.id.to_string(), "user_id": user.id, "user_name": user.name.clone(), "message": content.clone()})),
            },
        ).await {
            tracing::warn!(user = user.id, kind = %e.kind(), "chat persistence enqueue failed");
        }
        room.send_as(&user, content).await;
        user.server.publish_runtime_event(crate::event_bus::MpEvent::ChatMessage {
            room_id: Some(room.id.clone()), user_id: user.id,
        });
        Ok(())
    }.await;
    Some(ServerCommand::Chat(res.map_err(|e| e.to_string())))
}

pub(crate) async fn route_chat(
    user: Arc<User>,
    category: SessionCategory,
    msg: String,
    origin: CommandOrigin,
    deadline: std::time::Instant,
) -> Option<ServerCommand> {
    let result = route_via_mailbox(
        origin.clone(),
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::Chat {
            meta: CommandMeta::new(deadline, origin),
            user,
            category,
            msg,
            reply,
        },
    )
    .await;
    translate_route_result(
        &origin,
        result,
        |err| Some(ServerCommand::Chat(Err(err))),
    )
    .await
}

// ── Lock / Cycle ──────────────────────────────────────────────────

async fn handle_lock(
    user: Arc<User>,
    lock: bool,
    deadline: Instant,
    origin: CommandOrigin,
) -> Option<ServerCommand> {
    Some(ServerCommand::LockRoom(
        crate::session_room::lock_room(user, lock, deadline, &origin)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_lock(
    user: Arc<User>,
    lock: bool,
    origin: CommandOrigin,
    deadline: std::time::Instant,
) -> Option<ServerCommand> {
    let result = route_via_mailbox(
        origin.clone(),
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::Lock {
            meta: CommandMeta::new(deadline, origin),
            user,
            lock,
            reply,
        },
    )
    .await;
    translate_route_result(
        &origin,
        result,
        |err| Some(ServerCommand::LockRoom(Err(err))),
    )
    .await
}

async fn handle_cycle(
    user: Arc<User>,
    cycle: bool,
    deadline: Instant,
    origin: CommandOrigin,
) -> Option<ServerCommand> {
    Some(ServerCommand::CycleRoom(
        crate::session_room::cycle_room(user, cycle, deadline, &origin)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_cycle(
    user: Arc<User>,
    cycle: bool,
    origin: CommandOrigin,
    deadline: std::time::Instant,
) -> Option<ServerCommand> {
    let result = route_via_mailbox(
        origin.clone(),
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::Cycle {
            meta: CommandMeta::new(deadline, origin),
            user,
            cycle,
            reply,
        },
    )
    .await;
    translate_route_result(
        &origin,
        result,
        |err| Some(ServerCommand::CycleRoom(Err(err))),
    )
    .await
}

// ── Leave ─────────────────────────────────────────────────────────

async fn handle_leave(
    user: Arc<User>,
    category: SessionCategory,
    deadline: Instant,
    origin: CommandOrigin,
) -> Option<ServerCommand> {
    Some(ServerCommand::LeaveRoom(
        crate::session_room::leave_room(user, category, deadline, &origin)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_leave(
    user: Arc<User>,
    category: SessionCategory,
    origin: CommandOrigin,
    deadline: std::time::Instant,
) -> Option<ServerCommand> {
    let result = route_via_mailbox(
        origin.clone(),
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::Leave {
            meta: CommandMeta::new(deadline, origin),
            user,
            category,
            reply,
        },
    )
    .await;
    translate_route_result(
        &origin,
        result,
        |err| Some(ServerCommand::LeaveRoom(Err(err))),
    )
    .await
}

// ── Create / Join ─────────────────────────────────────────────────

async fn handle_create(
    user: Arc<User>,
    id: RoomId,
    deadline: std::time::Instant,
    origin: CommandOrigin,
) -> Option<ServerCommand> {
    Some(ServerCommand::CreateRoom(
        crate::session_room::create_room(user, id, deadline, &origin)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_create(
    user: Arc<User>,
    id: RoomId,
    origin: CommandOrigin,
    deadline: std::time::Instant,
) -> Option<ServerCommand> {
    let result = route_via_mailbox(
        origin.clone(),
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::Create {
            meta: CommandMeta::new(deadline, origin),
            user,
            id,
            reply,
        },
    )
    .await;
    translate_route_result(
        &origin,
        result,
        |err| Some(ServerCommand::CreateRoom(Err(err))),
    )
    .await
}

async fn handle_join(
    user: Arc<User>,
    category: SessionCategory,
    id: RoomId,
    monitor: bool,
    deadline: std::time::Instant,
    received_at: std::time::Instant,
    origin: CommandOrigin,
) -> Option<ServerCommand> {
    match crate::session_room::join_room(user, category, id, monitor, deadline, received_at, &origin)
        .await
    {
        Ok(()) => {
            // join_room already sent JoinRoom(Ok) + chat history directly
            None
        }
        Err(e) => Some(ServerCommand::JoinRoom(Err(e.to_string()))),
    }
}

pub(crate) async fn route_join(
    user: Arc<User>,
    category: SessionCategory,
    id: RoomId,
    monitor: bool,
    received_at: std::time::Instant,
    origin: CommandOrigin,
    deadline: std::time::Instant,
) -> Option<ServerCommand> {
    let result = route_via_mailbox(
        origin.clone(),
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::Join {
            meta: CommandMeta::new(deadline, origin),
            user,
            category,
            id,
            monitor,
            received_at,
            reply,
        },
    )
    .await;
    translate_route_result(
        &origin,
        result,
        |err| Some(ServerCommand::JoinRoom(Err(err))),
    )
    .await
}

// ── SelectChart ───────────────────────────────────────────────────

async fn handle_select_chart(
    user: Arc<User>,
    id: i32,
    deadline: Instant,
    origin: CommandOrigin,
) -> Option<ServerCommand> {
    Some(ServerCommand::SelectChart(
        crate::session_room::select_chart(user, id, deadline, &origin)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_select_chart(
    user: Arc<User>,
    id: i32,
    origin: CommandOrigin,
    deadline: std::time::Instant,
) -> Option<ServerCommand> {
    let result = route_via_mailbox(
        origin.clone(),
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::SelectChart {
            meta: CommandMeta::new(deadline, origin),
            user,
            id,
            reply,
        },
    )
    .await;
    translate_route_result(
        &origin,
        result,
        |err| Some(ServerCommand::SelectChart(Err(err))),
    )
    .await
}

// ── RequestStart ──────────────────────────────────────────────────

async fn handle_request_start(
    user: Arc<User>,
    deadline: std::time::Instant,
    origin: CommandOrigin,
) -> Option<ServerCommand> {
    Some(ServerCommand::RequestStart(
        crate::session_room::request_start(user, deadline, &origin)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_request_start(
    user: Arc<User>,
    origin: CommandOrigin,
    deadline: std::time::Instant,
) -> Option<ServerCommand> {
    let result = route_via_mailbox(
        origin.clone(),
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::RequestStart {
            meta: CommandMeta::new(deadline, origin),
            user,
            reply,
        },
    )
    .await;
    translate_route_result(
        &origin,
        result,
        |err| Some(ServerCommand::RequestStart(Err(err))),
    )
    .await
}

// ── Ready / CancelReady ───────────────────────────────────────────

async fn handle_ready(
    user: Arc<User>,
    deadline: std::time::Instant,
    origin: CommandOrigin,
) -> Option<ServerCommand> {
    Some(ServerCommand::Ready(
        crate::session_room::ready(user, deadline, &origin)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_ready(
    user: Arc<User>,
    origin: CommandOrigin,
    deadline: std::time::Instant,
) -> Option<ServerCommand> {
    let result = route_via_mailbox(
        origin.clone(),
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::Ready {
            meta: CommandMeta::new(deadline, origin),
            user,
            reply,
        },
    )
    .await;
    translate_route_result(
        &origin,
        result,
        |err| Some(ServerCommand::Ready(Err(err))),
    )
    .await
}

async fn handle_cancel_ready(
    user: Arc<User>,
    deadline: std::time::Instant,
    origin: CommandOrigin,
) -> Option<ServerCommand> {
    Some(ServerCommand::CancelReady(
        crate::session_room::cancel_ready(user, deadline, &origin)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_cancel_ready(
    user: Arc<User>,
    origin: CommandOrigin,
    deadline: std::time::Instant,
) -> Option<ServerCommand> {
    let result = route_via_mailbox(
        origin.clone(),
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::CancelReady {
            meta: CommandMeta::new(deadline, origin),
            user,
            reply,
        },
    )
    .await;
    translate_route_result(
        &origin,
        result,
        |err| Some(ServerCommand::CancelReady(Err(err))),
    )
    .await
}

// ── Played / Abort ────────────────────────────────────────────────

async fn handle_played(
    user: Arc<User>,
    id: i32,
    deadline: Instant,
    origin: CommandOrigin,
) -> Option<ServerCommand> {
    Some(ServerCommand::Played(
        crate::session_room::played(user, id, deadline, &origin)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_played(
    user: Arc<User>,
    id: i32,
    origin: CommandOrigin,
    deadline: std::time::Instant,
) -> Option<ServerCommand> {
    let result = route_via_mailbox(
        origin.clone(),
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::Played {
            meta: CommandMeta::new(deadline, origin),
            user,
            id,
            reply,
        },
    )
    .await;
    translate_route_result(
        &origin,
        result,
        |err| Some(ServerCommand::Played(Err(err))),
    )
    .await
}

async fn handle_abort(
    user: Arc<User>,
    deadline: Instant,
    origin: CommandOrigin,
) -> Option<ServerCommand> {
    Some(ServerCommand::Abort(
        crate::session_room::abort(user, deadline, &origin)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_abort(
    user: Arc<User>,
    origin: CommandOrigin,
    deadline: std::time::Instant,
) -> Option<ServerCommand> {
    let result = route_via_mailbox(
        origin.clone(),
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::Abort {
            meta: CommandMeta::new(deadline, origin),
            user,
            reply,
        },
    )
    .await;
    translate_route_result(
        &origin,
        result,
        |err| Some(ServerCommand::Abort(Err(err))),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{OnceLock, Weak};
    use std::time::Duration;

    #[test]
    fn once_lock_pattern_works() {
        let lock = OnceLock::<u8>::new();
        assert!(lock.get().is_none());
    }

    #[test]
    fn command_meta_carries_origin() {
        let origin = CommandOrigin {
            session: Weak::new(),
            generation: 7,
        };
        let meta = CommandMeta::new(
            std::time::Instant::now() + Duration::from_secs(1),
            origin.clone(),
        );
        assert_eq!(meta.origin.generation, 7);
        assert_eq!(meta.origin.session.as_ptr(), origin.session.as_ptr());
    }

    #[tokio::test]
    async fn stale_origin_stops_worker_execution() {
        // A command whose origin session is already dropped must never run —
        // the worker's P0-A check refuses it before any handler executes.
        let origin = CommandOrigin {
            session: Weak::new(),
            generation: 3,
        };
        assert!(!worker_should_run(&origin).await);
    }

    /// PMP44 P0-K: 邮箱失败分类——确定未入队（NotEnqueued）与入队后不确定
    /// （Uncertain）必须严格区分，前者不得关闭传输。
    #[test]
    fn classify_mailbox_failure_pins_p0k_semantics() {
        // 未入队（enqueued=false）：确定未入队——调用方必须能先发送官方 Err。
        assert!(matches!(
            classify_mailbox_failure(false, false),
            MailboxRouteResult::NotEnqueued
        ));
        // 入队后回复丢失（enqueued=true, reply_ok=false）：结果不确定。
        assert!(matches!(
            classify_mailbox_failure(true, false),
            MailboxRouteResult::Uncertain
        ));
        // 成功路径（enqueued=true, reply_ok=true）：调用方持有真实结果，
        // 构造 Completed(result)；分类器占位 Completed(None) 不会被使用。
        assert!(matches!(
            classify_mailbox_failure(true, true),
            MailboxRouteResult::Completed(_)
        ));
    }
}

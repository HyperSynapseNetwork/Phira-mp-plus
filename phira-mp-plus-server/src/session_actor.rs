//! Session actor 每连接独立邮箱（迁移中）。
//!
//! 每个 Session 创建时初始化独立 mailbox，命令通过该 Session 的邮箱路由。
//! 所有有序业务命令必须经过该邮箱。邮箱缺失、关闭、拥塞超时或
//! 入队后的回复丢失都会关闭当前连接（禁止退回旧处理器改变执行模型），
//! 并返回对应命令的官方错误响应，绝不静默丢弃请求（PMP42 P0-A）。
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
                        handle_chat(user, category, msg),
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
                        handle_lock(user, lock, meta.deadline),
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
                        handle_cycle(user, cycle, meta.deadline),
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
                        handle_leave(user, category, meta.deadline),
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
                        handle_create(user, id, meta.origin.clone()),
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
                        handle_select_chart(user, id, meta.deadline),
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
                        handle_request_start(user, meta.deadline),
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
                        handle_ready(user, meta.deadline),
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
                        handle_cancel_ready(user, meta.deadline),
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
                        handle_played(user, id, meta.deadline),
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
                        handle_abort(user, meta.deadline),
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
    origin.is_current().await
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

/// Total budget for one ordinary client command, shared across the mailbox
/// send and reply stages (PMP42 P0-C). Must stay well below the official
/// client's ~7s deadline; defaults to 4500ms.
fn command_deadline(user: &User) -> std::time::Instant {
    let budget_ms = user.server.config.compatibility.session_command_deadline_ms;
    std::time::Instant::now() + std::time::Duration::from_millis(budget_ms)
}

async fn close_uncertain_session(origin: &CommandOrigin, reason: &'static str) {
    tracing::warn!(reason, "session command outcome is uncertain; closing origin transport");
    origin.close_uncertain().await;
}

/// Execute a command handler unless the absolute actor deadline has already
/// passed. A late command MUST NOT mutate authoritative state — reply with the
/// matching error response and count it as a blocked late commit (P0-C).
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
    } else {
        let _ = reply.send(handler.await);
    }
}

/// Send a command through the per-session mailbox.
///
/// There is deliberately no direct fallback. Missing, closed or timed-out
/// mailboxes close the transport (so non-idempotent room transitions cannot be
/// replayed through a second execution model) AND return the matching official
/// error response — a request-type command is never silently dropped (P0-A).
///
/// Both the mailbox enqueue and the reply wait share the single absolute
/// `deadline`; each stage only uses the remaining budget.
async fn route_via_mailbox<Build, ErrResp>(
    user: Arc<User>,
    deadline: std::time::Instant,
    build: Build,
    error_response: ErrResp,
) -> Option<ServerCommand>
where
    Build: FnOnce(
            CommandOrigin,
            Arc<User>,
            tokio::sync::oneshot::Sender<Option<ServerCommand>>,
        ) -> SessionActorCmd,
    ErrResp: FnOnce(String) -> Option<ServerCommand>,
{
    // P0-A: capture the originating Session BEFORE touching the mailbox. The
    // user's *current* session may be replaced by a reconnect at any time; every
    // response, error, close and compensation for this command stays bound to
    // the origin captured here, never to the new session.
    let origin = match user.current_origin().await {
        Some(origin) => origin,
        None => return error_response("session mailbox missing".to_string()),
    };

    // Route through the ORIGIN session's mailbox — not `user.session`, which
    // may already point at a newer session after a reconnect.
    let tx = origin
        .session
        .upgrade()
        .and_then(|session| session.actor_tx.get().cloned());
    let Some(tx) = tx else {
        close_uncertain_session(&origin, "session mailbox missing").await;
        return error_response("session mailbox missing".to_string());
    };

    let (reply, rx) = tokio::sync::oneshot::channel();
    let cmd = build(origin.clone(), Arc::clone(&user), reply);
    let send_budget = deadline.saturating_duration_since(std::time::Instant::now());
    match tokio::time::timeout(send_budget, tx.send(cmd)).await {
        Ok(Ok(())) => {
            let reply_budget = deadline.saturating_duration_since(std::time::Instant::now());
            match tokio::time::timeout(reply_budget, rx).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => {
                    close_uncertain_session(&origin, "reply channel closed after enqueue").await;
                    error_response("session command reply channel closed".to_string())
                }
                Err(_) => {
                    close_uncertain_session(&origin, "reply timed out after enqueue").await;
                    error_response("session command reply timed out".to_string())
                }
            }
        }
        Ok(Err(_)) => {
            close_uncertain_session(&origin, "session mailbox closed before enqueue").await;
            error_response("session mailbox closed".to_string())
        }
        Err(_) => {
            close_uncertain_session(&origin, "session mailbox enqueue timed out").await;
            error_response("session mailbox enqueue timed out".to_string())
        }
    }
}

// ── Chat ──────────────────────────────────────────────────────────

async fn handle_chat(
    user: Arc<User>,
    _category: SessionCategory,
    content: String,
) -> Option<ServerCommand> {
    use anyhow::Result;
    if !user.server.live_config.read().await.chat_enabled {
        return Some(ServerCommand::Chat(Err(crate::tl!("chat-disabled"))));
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
) -> Option<ServerCommand> {
    let deadline = command_deadline(&user);
    route_via_mailbox(
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::Chat {
            meta: CommandMeta::new(deadline, origin),
            user,
            category,
            msg,
            reply,
        },
        |err| Some(ServerCommand::Chat(Err(err))),
    )
    .await
}

// ── Lock / Cycle ──────────────────────────────────────────────────

async fn handle_lock(user: Arc<User>, lock: bool, deadline: Instant) -> Option<ServerCommand> {
    Some(ServerCommand::LockRoom(
        crate::session_room::lock_room(user, lock, deadline)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_lock(user: Arc<User>, lock: bool) -> Option<ServerCommand> {
    let deadline = command_deadline(&user);
    route_via_mailbox(
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::Lock {
            meta: CommandMeta::new(deadline, origin),
            user,
            lock,
            reply,
        },
        |err| Some(ServerCommand::LockRoom(Err(err))),
    )
    .await
}

async fn handle_cycle(user: Arc<User>, cycle: bool, deadline: Instant) -> Option<ServerCommand> {
    Some(ServerCommand::CycleRoom(
        crate::session_room::cycle_room(user, cycle, deadline)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_cycle(user: Arc<User>, cycle: bool) -> Option<ServerCommand> {
    let deadline = command_deadline(&user);
    route_via_mailbox(
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::Cycle {
            meta: CommandMeta::new(deadline, origin),
            user,
            cycle,
            reply,
        },
        |err| Some(ServerCommand::CycleRoom(Err(err))),
    )
    .await
}

// ── Leave ─────────────────────────────────────────────────────────

async fn handle_leave(
    user: Arc<User>,
    category: SessionCategory,
    deadline: Instant,
) -> Option<ServerCommand> {
    Some(ServerCommand::LeaveRoom(
        crate::session_room::leave_room(user, category, deadline)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_leave(
    user: Arc<User>,
    category: SessionCategory,
) -> Option<ServerCommand> {
    let deadline = command_deadline(&user);
    route_via_mailbox(
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::Leave {
            meta: CommandMeta::new(deadline, origin),
            user,
            category,
            reply,
        },
        |err| Some(ServerCommand::LeaveRoom(Err(err))),
    )
    .await
}

// ── Create / Join ─────────────────────────────────────────────────

async fn handle_create(user: Arc<User>, id: RoomId, origin: CommandOrigin) -> Option<ServerCommand> {
    Some(ServerCommand::CreateRoom(
        crate::session_room::create_room(user, id, &origin)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_create(user: Arc<User>, id: RoomId) -> Option<ServerCommand> {
    let deadline = command_deadline(&user);
    route_via_mailbox(
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::Create {
            meta: CommandMeta::new(deadline, origin),
            user,
            id,
            reply,
        },
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
) -> Option<ServerCommand> {
    let deadline = command_deadline(&user);
    route_via_mailbox(
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
        |err| Some(ServerCommand::JoinRoom(Err(err))),
    )
    .await
}

// ── SelectChart ───────────────────────────────────────────────────

async fn handle_select_chart(user: Arc<User>, id: i32, deadline: Instant) -> Option<ServerCommand> {
    Some(ServerCommand::SelectChart(
        crate::session_room::select_chart(user, id, deadline)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_select_chart(user: Arc<User>, id: i32) -> Option<ServerCommand> {
    let deadline = command_deadline(&user);
    route_via_mailbox(
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::SelectChart {
            meta: CommandMeta::new(deadline, origin),
            user,
            id,
            reply,
        },
        |err| Some(ServerCommand::SelectChart(Err(err))),
    )
    .await
}

// ── RequestStart ──────────────────────────────────────────────────

async fn handle_request_start(
    user: Arc<User>,
    deadline: std::time::Instant,
) -> Option<ServerCommand> {
    Some(ServerCommand::RequestStart(
        crate::session_room::request_start(user, deadline)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_request_start(user: Arc<User>) -> Option<ServerCommand> {
    let deadline = command_deadline(&user);
    route_via_mailbox(
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::RequestStart {
            meta: CommandMeta::new(deadline, origin),
            user,
            reply,
        },
        |err| Some(ServerCommand::RequestStart(Err(err))),
    )
    .await
}

// ── Ready / CancelReady ───────────────────────────────────────────

async fn handle_ready(user: Arc<User>, deadline: std::time::Instant) -> Option<ServerCommand> {
    Some(ServerCommand::Ready(
        crate::session_room::ready(user, deadline)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_ready(user: Arc<User>) -> Option<ServerCommand> {
    let deadline = command_deadline(&user);
    route_via_mailbox(
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::Ready {
            meta: CommandMeta::new(deadline, origin),
            user,
            reply,
        },
        |err| Some(ServerCommand::Ready(Err(err))),
    )
    .await
}

async fn handle_cancel_ready(
    user: Arc<User>,
    deadline: std::time::Instant,
) -> Option<ServerCommand> {
    Some(ServerCommand::CancelReady(
        crate::session_room::cancel_ready(user, deadline)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_cancel_ready(user: Arc<User>) -> Option<ServerCommand> {
    let deadline = command_deadline(&user);
    route_via_mailbox(
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::CancelReady {
            meta: CommandMeta::new(deadline, origin),
            user,
            reply,
        },
        |err| Some(ServerCommand::CancelReady(Err(err))),
    )
    .await
}

// ── Played / Abort ────────────────────────────────────────────────

async fn handle_played(user: Arc<User>, id: i32, deadline: Instant) -> Option<ServerCommand> {
    Some(ServerCommand::Played(
        crate::session_room::played(user, id, deadline)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_played(user: Arc<User>, id: i32) -> Option<ServerCommand> {
    let deadline = command_deadline(&user);
    route_via_mailbox(
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::Played {
            meta: CommandMeta::new(deadline, origin),
            user,
            id,
            reply,
        },
        |err| Some(ServerCommand::Played(Err(err))),
    )
    .await
}

async fn handle_abort(user: Arc<User>, deadline: Instant) -> Option<ServerCommand> {
    Some(ServerCommand::Abort(
        crate::session_room::abort(user, deadline)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_abort(user: Arc<User>) -> Option<ServerCommand> {
    let deadline = command_deadline(&user);
    route_via_mailbox(
        user,
        deadline,
        |origin, user, reply| SessionActorCmd::Abort {
            meta: CommandMeta::new(deadline, origin),
            user,
            reply,
        },
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
}

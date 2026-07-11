//! Session actor 每连接独立邮箱（迁移中）。
//!
//! 每个 Session 创建时初始化独立 mailbox，命令通过该 Session 的邮箱路由。
//! 所有有序业务命令必须经过该邮箱。邮箱缺失、关闭、拥塞超时或
//! 入队后的回复丢失都会关闭当前连接，禁止退回旧处理器改变执行模型。
//!
//! 迁移状态：WriteRouted（Ping、Authenticate、Touches/Judges、
//! QueryRoomInfo 属于协议快路径，不进入业务命令邮箱）。

use crate::session::{Session, SessionCategory, User};
use phira_mp_common::{RoomId, ServerCommand};
use std::sync::{Arc, Weak};
use tokio::sync::mpsc;

/// Channel capacity for each per-session mailbox.
const SESSION_MAILBOX_CAPACITY: usize = 64;

/// Create a per-session mailbox for the given session and spawn the worker.
/// Returns the sender for the mailbox.
pub(crate) fn init_session_mailbox(session: &Arc<Session>) -> mpsc::Sender<SessionActorCmd> {
    let (tx, mut rx) = mpsc::channel::<SessionActorCmd>(SESSION_MAILBOX_CAPACITY);
    let weak_session = Arc::downgrade(session);
    crate::supervisor_actor::spawn_named(format!("session-mailbox-{}", session.id), async move {
        while let Some(cmd) = rx.recv().await {
            // If session is gone, stop processing.
            if weak_session.upgrade().is_none() {
                break;
            }
            match cmd {
                SessionActorCmd::Chat {
                    user,
                    category,
                    msg,
                    reply,
                } => {
                    let _ = reply.send(handle_chat(user, category, msg).await);
                }
                SessionActorCmd::Lock { user, lock, reply } => {
                    let _ = reply.send(handle_lock(user, lock).await);
                }
                SessionActorCmd::Cycle { user, cycle, reply } => {
                    let _ = reply.send(handle_cycle(user, cycle).await);
                }
                SessionActorCmd::Leave {
                    user,
                    category,
                    reply,
                } => {
                    let _ = reply.send(handle_leave(user, category).await);
                }
                SessionActorCmd::Create { user, id, reply } => {
                    let _ = reply.send(handle_create(user, id).await);
                }
                SessionActorCmd::Join {
                    user,
                    category,
                    id,
                    monitor,
                    reply,
                } => {
                    let _ = reply.send(handle_join(user, category, id, monitor).await);
                }
                SessionActorCmd::SelectChart { user, id, reply } => {
                    let _ = reply.send(handle_select_chart(user, id).await);
                }
                SessionActorCmd::RequestStart { user, reply } => {
                    let _ = reply.send(handle_request_start(user).await);
                }
                SessionActorCmd::Ready { user, reply } => {
                    let _ = reply.send(handle_ready(user).await);
                }
                SessionActorCmd::CancelReady { user, reply } => {
                    let _ = reply.send(handle_cancel_ready(user).await);
                }
                SessionActorCmd::Played { user, id, reply } => {
                    let _ = reply.send(handle_played(user, id).await);
                }
                SessionActorCmd::Abort { user, reply } => {
                    let _ = reply.send(handle_abort(user).await);
                }
            }
        }
    });
    tx
}

// ── Command envelope ──────────────────────────────────────────────

pub(crate) enum SessionActorCmd {
    Chat {
        user: Arc<User>,
        category: SessionCategory,
        msg: String,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    Lock {
        user: Arc<User>,
        lock: bool,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    Cycle {
        user: Arc<User>,
        cycle: bool,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    Leave {
        user: Arc<User>,
        category: SessionCategory,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    Create {
        user: Arc<User>,
        id: RoomId,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    Join {
        user: Arc<User>,
        category: SessionCategory,
        id: RoomId,
        monitor: bool,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    SelectChart {
        user: Arc<User>,
        id: i32,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    RequestStart {
        user: Arc<User>,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    Ready {
        user: Arc<User>,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    CancelReady {
        user: Arc<User>,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    Played {
        user: Arc<User>,
        id: i32,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
    Abort {
        user: Arc<User>,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
}

// ── Generic route helper ──────────────────────────────────────────

/// Maximum time spent enqueueing or waiting for one ordered session command.
const SESSION_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

async fn close_uncertain_session(user: &User, reason: &'static str) {
    tracing::warn!(
        user = user.id,
        reason,
        "session command outcome is uncertain; closing transport"
    );
    let session = user.session.read().await.as_ref().and_then(Weak::upgrade);
    if let Some(session) = session {
        session.stream.close();
        let _ = user.server.lost_con_tx.try_send(session.id);
    }
}

/// Send a command through the per-session mailbox.
///
/// There is deliberately no direct fallback. Missing, closed or timed-out
/// mailboxes terminate the transport so non-idempotent room transitions cannot
/// be replayed through a second execution model.
async fn route_via_mailbox<Build>(user: Arc<User>, build: Build) -> Option<ServerCommand>
where
    Build:
        FnOnce(Arc<User>, tokio::sync::oneshot::Sender<Option<ServerCommand>>) -> SessionActorCmd,
{
    let tx = {
        let guard = user.session.read().await;
        guard
            .as_ref()
            .and_then(Weak::upgrade)
            .and_then(|session| session.actor_tx.get().cloned())
    };
    let Some(tx) = tx else {
        close_uncertain_session(&user, "session mailbox missing").await;
        return None;
    };

    let (reply, rx) = tokio::sync::oneshot::channel();
    let cmd = build(Arc::clone(&user), reply);
    match tokio::time::timeout(SESSION_COMMAND_TIMEOUT, tx.send(cmd)).await {
        Ok(Ok(())) => match tokio::time::timeout(SESSION_COMMAND_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                close_uncertain_session(&user, "reply channel closed after enqueue").await;
                None
            }
            Err(_) => {
                close_uncertain_session(&user, "reply timed out after enqueue").await;
                None
            }
        },
        Ok(Err(_)) => {
            close_uncertain_session(&user, "session mailbox closed before enqueue").await;
            None
        }
        Err(_) => {
            close_uncertain_session(&user, "session mailbox enqueue timed out").await;
            None
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
        return Some(ServerCommand::Chat(Err("chat is disabled".to_string())));
    }
    let res: Result<()> = async {
        let room = user.room.read().await.as_ref().map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("{}", crate::tl!("no-room")))?;
        // PersistenceWorker (exclusive — no direct DB write)
        let _ = user.server.persistence_worker.enqueue(
            crate::persistence::message::PersistenceEvent::ServerEvent {
                kind: "chat.message".to_string(),
                payload: Arc::new(serde_json::json!({"room_id": room.id.to_string(), "user_id": user.id, "user_name": user.name.clone(), "message": content.clone()})),
                simulation: false,
            },
        ).await;
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
    route_via_mailbox(user, |user, reply| SessionActorCmd::Chat {
        user,
        category,
        msg,
        reply,
    })
    .await
}

// ── Lock / Cycle ──────────────────────────────────────────────────

async fn handle_lock(user: Arc<User>, lock: bool) -> Option<ServerCommand> {
    Some(ServerCommand::LockRoom(
        crate::session_room::lock_room(user, lock)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_lock(user: Arc<User>, lock: bool) -> Option<ServerCommand> {
    route_via_mailbox(user, |user, reply| SessionActorCmd::Lock {
        user,
        lock,
        reply,
    })
    .await
}

async fn handle_cycle(user: Arc<User>, cycle: bool) -> Option<ServerCommand> {
    Some(ServerCommand::CycleRoom(
        crate::session_room::cycle_room(user, cycle)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_cycle(user: Arc<User>, cycle: bool) -> Option<ServerCommand> {
    route_via_mailbox(user, |user, reply| SessionActorCmd::Cycle {
        user,
        cycle,
        reply,
    })
    .await
}

// ── Leave ─────────────────────────────────────────────────────────

async fn handle_leave(user: Arc<User>, category: SessionCategory) -> Option<ServerCommand> {
    Some(ServerCommand::LeaveRoom(
        crate::session_room::leave_room(user, category)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_leave(
    user: Arc<User>,
    category: SessionCategory,
) -> Option<ServerCommand> {
    route_via_mailbox(user, |user, reply| SessionActorCmd::Leave {
        user,
        category,
        reply,
    })
    .await
}

// ── Create / Join ─────────────────────────────────────────────────

async fn handle_create(user: Arc<User>, id: RoomId) -> Option<ServerCommand> {
    Some(ServerCommand::CreateRoom(
        crate::session_room::create_room(user, id)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_create(user: Arc<User>, id: RoomId) -> Option<ServerCommand> {
    route_via_mailbox(user, |user, reply| SessionActorCmd::Create {
        user,
        id,
        reply,
    })
    .await
}

async fn handle_join(
    user: Arc<User>,
    category: SessionCategory,
    id: RoomId,
    monitor: bool,
) -> Option<ServerCommand> {
    Some(ServerCommand::JoinRoom(
        crate::session_room::join_room(user, category, id, monitor)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_join(
    user: Arc<User>,
    category: SessionCategory,
    id: RoomId,
    monitor: bool,
) -> Option<ServerCommand> {
    route_via_mailbox(user, |user, reply| SessionActorCmd::Join {
        user,
        category,
        id,
        monitor,
        reply,
    })
    .await
}

// ── SelectChart ───────────────────────────────────────────────────

async fn handle_select_chart(user: Arc<User>, id: i32) -> Option<ServerCommand> {
    Some(ServerCommand::SelectChart(
        crate::session_room::select_chart(user, id)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_select_chart(user: Arc<User>, id: i32) -> Option<ServerCommand> {
    route_via_mailbox(user, |user, reply| SessionActorCmd::SelectChart {
        user,
        id,
        reply,
    })
    .await
}

// ── RequestStart ──────────────────────────────────────────────────

async fn handle_request_start(user: Arc<User>) -> Option<ServerCommand> {
    Some(ServerCommand::RequestStart(
        crate::session_room::request_start(user)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_request_start(user: Arc<User>) -> Option<ServerCommand> {
    route_via_mailbox(user, |user, reply| SessionActorCmd::RequestStart {
        user,
        reply,
    })
    .await
}

// ── Ready / CancelReady ───────────────────────────────────────────

async fn handle_ready(user: Arc<User>) -> Option<ServerCommand> {
    Some(ServerCommand::Ready(
        crate::session_room::ready(user)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_ready(user: Arc<User>) -> Option<ServerCommand> {
    route_via_mailbox(user, |user, reply| SessionActorCmd::Ready { user, reply }).await
}

async fn handle_cancel_ready(user: Arc<User>) -> Option<ServerCommand> {
    Some(ServerCommand::CancelReady(
        crate::session_room::cancel_ready(user)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_cancel_ready(user: Arc<User>) -> Option<ServerCommand> {
    route_via_mailbox(user, |user, reply| SessionActorCmd::CancelReady {
        user,
        reply,
    })
    .await
}

// ── Played / Abort ────────────────────────────────────────────────

async fn handle_played(user: Arc<User>, id: i32) -> Option<ServerCommand> {
    Some(ServerCommand::Played(
        crate::session_room::played(user, id)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_played(user: Arc<User>, id: i32) -> Option<ServerCommand> {
    route_via_mailbox(user, |user, reply| SessionActorCmd::Played {
        user,
        id,
        reply,
    })
    .await
}

async fn handle_abort(user: Arc<User>) -> Option<ServerCommand> {
    Some(ServerCommand::Abort(
        crate::session_room::abort(user)
            .await
            .map_err(|e| e.to_string()),
    ))
}

pub(crate) async fn route_abort(user: Arc<User>) -> Option<ServerCommand> {
    route_via_mailbox(user, |user, reply| SessionActorCmd::Abort { user, reply }).await
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    #[test]
    fn once_lock_pattern_works() {
        let lock = OnceLock::<u8>::new();
        assert!(lock.get().is_none());
    }
}

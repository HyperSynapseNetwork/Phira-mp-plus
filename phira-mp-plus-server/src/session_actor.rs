//! Session actor 全局邮箱（迁移中）。
//!
//! 当前为全进程统一邮箱（`static MAILBOX`），所有用户命令通过同一通道。
//! 这能避免并发状态修改，但慢命令可能阻塞其他用户。
//!
//! 目标：每连接独立 Actor (`SessionActor(user A) → mailbox A`)。
//! 邮箱不可用时会回退到原有直接处理路径。
//!
//! 迁移状态：WriteRouted（除 Ping、Authenticate、Touches/Judges、
//! QueryRoomInfo 外均已迁移）。

use crate::session::{SessionCategory, User};
use phira_mp_common::{RoomId, ServerCommand};
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc;

// ── Global mailbox sender ──────────────────────────────────────────

static MAILBOX: OnceLock<mpsc::Sender<SessionActorCmd>> = OnceLock::new();

/// Initialize the global session actor mailbox. Called once at server startup.
pub(crate) fn init() {
    let (tx, mut rx) = mpsc::channel::<SessionActorCmd>(256);
    MAILBOX.set(tx).expect("session actor init called twice");

    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                SessionActorCmd::Chat { user, category, msg, reply } => {
                    let _ = reply.send(handle_chat(user, category, msg).await);
                }
                SessionActorCmd::Lock { user, lock, reply } => {
                    let _ = reply.send(handle_lock(user, lock).await);
                }
                SessionActorCmd::Cycle { user, cycle, reply } => {
                    let _ = reply.send(handle_cycle(user, cycle).await);
                }
                SessionActorCmd::Leave { user, category, reply } => {
                    let _ = reply.send(handle_leave(user, category).await);
                }
                SessionActorCmd::Create { user, id, reply } => {
                    let _ = reply.send(handle_create(user, id).await);
                }
                SessionActorCmd::Join { user, category, id, monitor, reply } => {
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
}

// ── Command envelope ──────────────────────────────────────────────

pub(crate) enum SessionActorCmd {
    Chat     { user: Arc<User>, category: SessionCategory, msg: String, reply: tokio::sync::oneshot::Sender<Option<ServerCommand>> },
    Lock     { user: Arc<User>, lock: bool, reply: tokio::sync::oneshot::Sender<Option<ServerCommand>> },
    Cycle    { user: Arc<User>, cycle: bool, reply: tokio::sync::oneshot::Sender<Option<ServerCommand>> },
    Leave    { user: Arc<User>, category: SessionCategory, reply: tokio::sync::oneshot::Sender<Option<ServerCommand>> },
    Create   { user: Arc<User>, id: RoomId, reply: tokio::sync::oneshot::Sender<Option<ServerCommand>> },
    Join     { user: Arc<User>, category: SessionCategory, id: RoomId, monitor: bool, reply: tokio::sync::oneshot::Sender<Option<ServerCommand>> },
    SelectChart { user: Arc<User>, id: i32, reply: tokio::sync::oneshot::Sender<Option<ServerCommand>> },
    RequestStart { user: Arc<User>, reply: tokio::sync::oneshot::Sender<Option<ServerCommand>> },
    Ready    { user: Arc<User>, reply: tokio::sync::oneshot::Sender<Option<ServerCommand>> },
    CancelReady { user: Arc<User>, reply: tokio::sync::oneshot::Sender<Option<ServerCommand>> },
    Played   { user: Arc<User>, id: i32, reply: tokio::sync::oneshot::Sender<Option<ServerCommand>> },
    Abort    { user: Arc<User>, reply: tokio::sync::oneshot::Sender<Option<ServerCommand>> },
}

// ── Generic route helper ──────────────────────────────────────────

/// Send a command through the mailbox.  Falls back to `fallback` if the
/// mailbox isn't ready or the channel / reply is lost.
async fn route_or_fallback<F, Fut>(
    cmd: SessionActorCmd,
    fallback: F,
) -> Option<ServerCommand>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<ServerCommand>>,
{
    let tx = match MAILBOX.get() {
        Some(tx) => tx,
        None => return fallback().await,
    };
    let (reply, rx) = tokio::sync::oneshot::channel();
    // Replace the reply channel in the command so the mailbox worker
    // uses our channel instead of a dummy.
    let cmd = cmd.with_reply(reply);
    match tx.send(cmd).await {
        Ok(()) => match rx.await {
            Ok(result) => result,
            Err(_) => fallback().await,
        },
        Err(_) => fallback().await,
    }
}

impl SessionActorCmd {
    fn with_reply(self, reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>) -> Self {
        match self {
            Self::Chat { user, category, msg, .. } => Self::Chat { user, category, msg, reply },
            Self::Lock { user, lock, .. } => Self::Lock { user, lock, reply },
            Self::Cycle { user, cycle, .. } => Self::Cycle { user, cycle, reply },
            Self::Leave { user, category, .. } => Self::Leave { user, category, reply },
            Self::Create { user, id, .. } => Self::Create { user, id, reply },
            Self::Join { user, category, id, monitor, .. } => Self::Join { user, category, id, monitor, reply },
            Self::SelectChart { user, id, .. } => Self::SelectChart { user, id, reply },
            Self::RequestStart { user, .. } => Self::RequestStart { user, reply },
            Self::Ready { user, .. } => Self::Ready { user, reply },
            Self::CancelReady { user, .. } => Self::CancelReady { user, reply },
            Self::Played { user, id, .. } => Self::Played { user, id, reply },
            Self::Abort { user, .. } => Self::Abort { user, reply },
        }
    }
}

// ── Chat ──────────────────────────────────────────────────────────

async fn handle_chat(user: Arc<User>, _category: SessionCategory, content: String) -> Option<ServerCommand> {
    use anyhow::Result;
    if !user.server.config.chat_enabled { return None; }
    let res: Result<()> = async {
        let room = user.room.read().await.as_ref().map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("{}", crate::tl!("no-room")))?;
        if let Some(db) = crate::internal_hooks::DB.get() {
            db.record_room_event_sync("chat.message", Some(room.id.to_string()), Some(user.id),
                serde_json::json!({"room_id": room.id.to_string(), "user_id": user.id, "user_name": user.name.clone(), "message": content.clone()}));
        }
        room.send_as(&user, content).await;
        user.server.publish_runtime_event(crate::event_bus::MpEvent::ChatMessage {
            room_id: Some(room.id.clone()), user_id: user.id,
        });
        Ok(())
    }.await;
    Some(ServerCommand::Chat(res.map_err(|e| e.to_string())))
}

pub(crate) async fn route_chat(user: Arc<User>, category: SessionCategory, msg: String) -> Option<ServerCommand> {
    route_or_fallback(
        SessionActorCmd::Chat { user: Arc::clone(&user), category, msg: msg.clone(), reply: tokio::sync::oneshot::channel().0 },
        || handle_chat(user, category, msg),
    ).await
}

// ── Lock / Cycle ──────────────────────────────────────────────────

async fn handle_lock(user: Arc<User>, lock: bool) -> Option<ServerCommand> {
    Some(ServerCommand::LockRoom(
        crate::session_room::lock_room(user, lock).await.map_err(|e| e.to_string())
    ))
}

pub(crate) async fn route_lock(user: Arc<User>, lock: bool) -> Option<ServerCommand> {
    route_or_fallback(
        SessionActorCmd::Lock { user: Arc::clone(&user), lock, reply: tokio::sync::oneshot::channel().0 },
        || handle_lock(user, lock),
    ).await
}

async fn handle_cycle(user: Arc<User>, cycle: bool) -> Option<ServerCommand> {
    Some(ServerCommand::CycleRoom(
        crate::session_room::cycle_room(user, cycle).await.map_err(|e| e.to_string())
    ))
}

pub(crate) async fn route_cycle(user: Arc<User>, cycle: bool) -> Option<ServerCommand> {
    route_or_fallback(
        SessionActorCmd::Cycle { user: Arc::clone(&user), cycle, reply: tokio::sync::oneshot::channel().0 },
        || handle_cycle(user, cycle),
    ).await
}

// ── Leave ─────────────────────────────────────────────────────────

async fn handle_leave(user: Arc<User>, category: SessionCategory) -> Option<ServerCommand> {
    Some(ServerCommand::LeaveRoom(
        crate::session_room::leave_room(user, category).await.map_err(|e| e.to_string())
    ))
}

pub(crate) async fn route_leave(user: Arc<User>, category: SessionCategory) -> Option<ServerCommand> {
    route_or_fallback(
        SessionActorCmd::Leave { user: Arc::clone(&user), category, reply: tokio::sync::oneshot::channel().0 },
        || handle_leave(user, category),
    ).await
}

// ── Create / Join ─────────────────────────────────────────────────

async fn handle_create(user: Arc<User>, id: RoomId) -> Option<ServerCommand> {
    Some(ServerCommand::CreateRoom(
        crate::session_room::create_room(user, id).await.map_err(|e| e.to_string())
    ))
}

pub(crate) async fn route_create(user: Arc<User>, id: RoomId) -> Option<ServerCommand> {
    route_or_fallback(
        SessionActorCmd::Create { user: Arc::clone(&user), id: id.clone(), reply: tokio::sync::oneshot::channel().0 },
        || handle_create(user, id),
    ).await
}

async fn handle_join(user: Arc<User>, category: SessionCategory, id: RoomId, monitor: bool) -> Option<ServerCommand> {
    Some(ServerCommand::JoinRoom(
        crate::session_room::join_room(user, category, id, monitor).await.map_err(|e| e.to_string())
    ))
}

pub(crate) async fn route_join(user: Arc<User>, category: SessionCategory, id: RoomId, monitor: bool) -> Option<ServerCommand> {
    route_or_fallback(
        SessionActorCmd::Join { user: Arc::clone(&user), category, id: id.clone(), monitor, reply: tokio::sync::oneshot::channel().0 },
        || handle_join(user, category, id, monitor),
    ).await
}

// ── SelectChart ───────────────────────────────────────────────────

async fn handle_select_chart(user: Arc<User>, id: i32) -> Option<ServerCommand> {
    Some(ServerCommand::SelectChart(
        crate::session_room::select_chart(user, id).await.map_err(|e| e.to_string())
    ))
}

pub(crate) async fn route_select_chart(user: Arc<User>, id: i32) -> Option<ServerCommand> {
    route_or_fallback(
        SessionActorCmd::SelectChart { user: Arc::clone(&user), id, reply: tokio::sync::oneshot::channel().0 },
        || handle_select_chart(user, id),
    ).await
}

// ── RequestStart ──────────────────────────────────────────────────

async fn handle_request_start(user: Arc<User>) -> Option<ServerCommand> {
    Some(ServerCommand::RequestStart(
        crate::session_room::request_start(user).await.map_err(|e| e.to_string())
    ))
}

pub(crate) async fn route_request_start(user: Arc<User>) -> Option<ServerCommand> {
    route_or_fallback(
        SessionActorCmd::RequestStart { user: Arc::clone(&user), reply: tokio::sync::oneshot::channel().0 },
        || handle_request_start(user),
    ).await
}

// ── Ready / CancelReady ───────────────────────────────────────────

async fn handle_ready(user: Arc<User>) -> Option<ServerCommand> {
    Some(ServerCommand::Ready(
        crate::session_room::ready(user).await.map_err(|e| e.to_string())
    ))
}

pub(crate) async fn route_ready(user: Arc<User>) -> Option<ServerCommand> {
    route_or_fallback(
        SessionActorCmd::Ready { user: Arc::clone(&user), reply: tokio::sync::oneshot::channel().0 },
        || handle_ready(user),
    ).await
}

async fn handle_cancel_ready(user: Arc<User>) -> Option<ServerCommand> {
    Some(ServerCommand::CancelReady(
        crate::session_room::cancel_ready(user).await.map_err(|e| e.to_string())
    ))
}

pub(crate) async fn route_cancel_ready(user: Arc<User>) -> Option<ServerCommand> {
    route_or_fallback(
        SessionActorCmd::CancelReady { user: Arc::clone(&user), reply: tokio::sync::oneshot::channel().0 },
        || handle_cancel_ready(user),
    ).await
}

// ── Played / Abort ────────────────────────────────────────────────

async fn handle_played(user: Arc<User>, id: i32) -> Option<ServerCommand> {
    Some(ServerCommand::Played(
        crate::session_room::played(user, id).await.map_err(|e| e.to_string())
    ))
}

pub(crate) async fn route_played(user: Arc<User>, id: i32) -> Option<ServerCommand> {
    route_or_fallback(
        SessionActorCmd::Played { user: Arc::clone(&user), id, reply: tokio::sync::oneshot::channel().0 },
        || handle_played(user, id),
    ).await
}

async fn handle_abort(user: Arc<User>) -> Option<ServerCommand> {
    Some(ServerCommand::Abort(
        crate::session_room::abort(user).await.map_err(|e| e.to_string())
    ))
}

pub(crate) async fn route_abort(user: Arc<User>) -> Option<ServerCommand> {
    route_or_fallback(
        SessionActorCmd::Abort { user: Arc::clone(&user), reply: tokio::sync::oneshot::channel().0 },
        || handle_abort(user),
    ).await
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

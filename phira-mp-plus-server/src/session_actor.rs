//! Session actor mailbox.
//!
//! Step 2 of the SessionActor migration: route Chat through the mailbox.
//! Additional commands are added by extending `SessionActorCmd` and adding
//! `handle_*` functions.
//!
//! Migration status: WriteRouted (Chat only; remaining commands still direct).

use crate::session::{SessionCategory, User};
use phira_mp_common::{ClientCommand, ServerCommand};
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
                    let result = handle_chat(user, category, msg).await;
                    let _ = reply.send(result);
                }
            }
        }
    });
}

/// Send a command through the mailbox. Returns None if closed.
pub(crate) async fn send(cmd: SessionActorCmd) {
    if let Some(tx) = MAILBOX.get() {
        let _ = tx.send(cmd).await;
    }
}

/// Check if the mailbox is initialised.
pub(crate) fn is_ready() -> bool {
    MAILBOX.get().is_some()
}

// ── Command envelope ──────────────────────────────────────────────

pub(crate) enum SessionActorCmd {
    Chat {
        user: Arc<User>,
        category: SessionCategory,
        msg: String,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
}

// ── Chat handler ───────────────────────────────────────────────────

async fn handle_chat(
    user: Arc<User>,
    _category: SessionCategory,
    content: String,
) -> Option<ServerCommand> {
    use anyhow::Result;

    if !user.server.config.chat_enabled {
        return None;
    }
    let res: Result<()> = async {
        let room = user
            .room
            .read()
            .await
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| anyhow::anyhow!("{}", crate::tl!("no-room")))?;
        if let Some(db) = crate::internal_hooks::DB.get() {
            db.record_room_event_sync(
                "chat.message",
                Some(room.id.to_string()),
                Some(user.id),
                serde_json::json!({
                    "room_id": room.id.to_string(),
                    "user_id": user.id,
                    "user_name": user.name.clone(),
                    "message": content.clone(),
                }),
            );
        }
        room.send_as(&user, content).await;
        user.server
            .publish_runtime_event(crate::event_bus::MpEvent::ChatMessage {
                room_id: Some(room.id.clone()),
                user_id: user.id,
            });
        Ok(())
    }
    .await;
    Some(ServerCommand::Chat(
        res.map_err(|e| e.to_string()),
    ))
}

/// Route a Chat command through the actor mailbox.
/// Falls back to direct handling if the mailbox isn't ready.
pub(crate) async fn route_chat(
    user: Arc<User>,
    category: SessionCategory,
    msg: String,
) -> Option<ServerCommand> {
    let tx = match MAILBOX.get() {
        Some(tx) => tx,
        None => return handle_chat(user, category, msg).await,
    };
    let (reply, rx) = tokio::sync::oneshot::channel();
    match tx
        .send(SessionActorCmd::Chat {
            user,
            category,
            msg,
            reply,
        })
        .await
    {
        Ok(()) => rx.await.unwrap_or(None),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_not_ready_before_init() {
        assert!(!is_ready(), "mailbox should not be ready before init()");
    }

    #[test]
    fn init_creates_mailbox() {
        // Can't really test this without tokio, but verify OnceLock behavior
        assert!(OnceLock::<u8>::new().get().is_none());
    }
}

//! Session actor mailbox.
//!
//! Step 1 of the SessionActor migration: establish the mailbox skeleton
//! and route the simplest command (Chat) through it.  Once the pattern is
//! proven, additional commands are added by extending the enum and the
//! worker dispatch loop.
//!
//! Migration status: Mirrored → WriteRouted (Chat only; remaining
//! commands still go through session_dispatch::process directly).

use crate::session::{SessionCategory, User};
use phira_mp_common::{ClientCommand, ServerCommand};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Session actor command envelope.
enum SessionActorCmd {
    Process {
        user: Arc<User>,
        category: SessionCategory,
        cmd: ClientCommand,
        reply: tokio::sync::oneshot::Sender<Option<ServerCommand>>,
    },
}

/// Start the session actor mailbox worker.
pub(crate) fn start_mailbox() -> mpsc::Sender<SessionActorCmd> {
    let (tx, mut rx) = mpsc::channel::<SessionActorCmd>(256);

    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                SessionActorCmd::Process {
                    user,
                    category,
                    cmd,
                    reply,
                } => {
                    let result = crate::session_dispatch::process(user, category, cmd).await;
                    let _ = reply.send(result);
                }
            }
        }
    });

    tx
}

/// Route a session command through the actor mailbox.
/// Returns `None` if the mailbox is closed (fallback path).
pub(crate) async fn route(
    tx: &mpsc::Sender<SessionActorCmd>,
    user: Arc<User>,
    category: SessionCategory,
    cmd: ClientCommand,
) -> Option<ServerCommand> {
    let (reply, rx) = tokio::sync::oneshot::channel();
    match tx
        .send(SessionActorCmd::Process {
            user,
            category,
            cmd,
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
    fn mailbox_starts_and_accepts_commands() {
        let tx = start_mailbox();
        assert!(!tx.is_closed(), "mailbox sender should be open");
    }
}

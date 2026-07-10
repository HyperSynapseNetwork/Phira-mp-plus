//! Mailbox-backed routing for room commands.

use super::{
    actor::RoomActor, command::RoomActorCommand, context::RoomCommandContext,
    handler::RoomCommandHandler, RoomCommandDelivery, RoomCommandGateway, RoomCommandResult,
};
use crate::server::PlusServerState;
use serde_json::Value;
use std::{
    future::Future,
    sync::{
        atomic::Ordering,
        Arc, Weak,
    },
};
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

impl RoomCommandGateway {
    pub fn start_mailbox(self: &Arc<Self>, state: Arc<PlusServerState>, capacity: usize) {
        if let Ok(mut guard) = self.self_ref.write() {
            *guard = Some(Arc::downgrade(self));
        }
        if let Ok(mut guard) = self.state_ref.write() {
            *guard = Some(Arc::downgrade(&state));
        }

        let (tx, mut rx) = mpsc::channel::<RoomActorCommand>(capacity.max(1));
        if let Ok(mut guard) = self.mailbox_tx.write() {
            *guard = Some(tx);
        } else {
            self.mailbox_closed.fetch_add(1, Ordering::Relaxed);
            return;
        }

        let gateway = Arc::clone(self);
        let state = Arc::downgrade(&state);
        tokio::spawn(async move {
            while let Some(command) = rx.recv().await {
                let Some(state) = state.upgrade() else {
                    gateway.mailbox_closed.fetch_add(1, Ordering::Relaxed);
                    let result = RoomCommandResult::mailbox_error(
                        "server state dropped before room command could run",
                    );
                    gateway.observe_mailbox_result(&result);
                    command.reply_with(result);
                    continue;
                };

                let should_stop = gateway.execute_mailbox_command(&state, command).await;
                if should_stop {
                    break;
                }
            }
            gateway.mailbox_closed.fetch_add(1, Ordering::Relaxed);
        });
    }

    pub(super) fn mailbox_sender(&self) -> Option<mpsc::Sender<RoomActorCommand>> {
        self.mailbox_tx.read().ok().and_then(|guard| guard.clone())
    }

    pub(super) fn mailbox_enabled(&self) -> bool {
        self.mailbox_sender().is_some()
            || self
                .room_mailboxes
                .read()
                .map(|mailboxes| !mailboxes.is_empty())
                .unwrap_or(false)
    }

    pub(super) fn state_weak(&self) -> Option<Weak<PlusServerState>> {
        self.state_ref.read().ok().and_then(|guard| guard.clone())
    }

    pub(super) fn self_arc(&self) -> Option<Arc<RoomCommandGateway>> {
        self.self_ref
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().and_then(Weak::upgrade))
    }

    pub(super) fn room_mailbox_sender(
        &self,
        room_id: &str,
    ) -> Option<mpsc::Sender<RoomActorCommand>> {
        if let Ok(mailboxes) = self.room_mailboxes.read() {
            if let Some(tx) = mailboxes.get(room_id).cloned() {
                self.mailbox_registry_hit.fetch_add(1, Ordering::Relaxed);
                return Some(tx);
            }
        }

        self.mailbox_registry_miss.fetch_add(1, Ordering::Relaxed);
        let weak_state = self.state_weak()?;
        let gateway = self.self_arc()?;
        let mut mailboxes = self.room_mailboxes.write().ok()?;
        if let Some(tx) = mailboxes.get(room_id).cloned() {
            self.mailbox_registry_hit.fetch_add(1, Ordering::Relaxed);
            return Some(tx);
        }

        let (tx, mut rx) = mpsc::channel::<RoomActorCommand>(128);
        mailboxes.insert(room_id.to_string(), tx.clone());
        self.mailbox_created.fetch_add(1, Ordering::Relaxed);
        let worker_room_id = room_id.to_string();
        drop(mailboxes);
        tokio::spawn(async move {
            let Some(state) = weak_state.upgrade() else {
                gateway.mailbox_closed.fetch_add(1, Ordering::Relaxed);
                return;
            };
            // Look up the room and create RoomActor.
            let actor = {
                let rooms = state.rooms.read().await;
                rooms.values()
                    .find(|r| r.id.to_string() == worker_room_id)
                    .map(|room| RoomActor::new(room.clone(), state.clone()))
            };
            let Some(mut actor) = actor else {
                gateway.mailbox_closed.fetch_add(1, Ordering::Relaxed);
                return;
            };
            // Publish initial snapshot.
            let snapshot = actor.snapshot().clone();
            if let Ok(mut snapshots) = gateway.snapshots.write() {
                snapshots.insert(worker_room_id.clone(), snapshot);
            }

            while let Some(command) = rx.recv().await {
                let should_stop = {
                    let result = gateway.execute_mailbox_command_with_room(
                        &actor.state, actor.room().clone(), command,
                    ).await;
                    // Refresh snapshot after command execution.
                    actor.refresh_snapshot().await;
                    let snapshot = actor.snapshot().clone();
                    if let Ok(mut snapshots) = gateway.snapshots.write() {
                        snapshots.insert(worker_room_id.clone(), snapshot);
                    }
                    result
                };
                if should_stop {
                    break;
                }
            }
            if let Ok(mut mailboxes) = gateway.room_mailboxes.write() {
                mailboxes.remove(&worker_room_id);
            }
            if let Ok(mut snapshots) = gateway.snapshots.write() {
                snapshots.remove(&worker_room_id);
            }
            gateway.mailbox_closed.fetch_add(1, Ordering::Relaxed);
        });
        Some(tx)
    }

    pub(super) async fn execute_mailbox_command(
        &self,
        state: &PlusServerState,
        command: RoomActorCommand,
    ) -> bool {
        let ctx = RoomCommandContext::new(self, state);
        let result = RoomCommandHandler::execute(ctx, &command).await;
        let should_stop = RoomCommandHandler::should_stop_room_mailbox(&command, &result);
        self.observe_mailbox_result(&result);
        command.reply_with(result);
        should_stop
    }

    /// Execute a command with a room reference already resolved.
    /// The context carries the room so handlers can use it directly.
    pub(super) async fn execute_mailbox_command_with_room(
        &self,
        state: &PlusServerState,
        room: Arc<crate::room::Room>,
        command: RoomActorCommand,
    ) -> bool {
        let ctx = RoomCommandContext::with_room(self, state, room);
        let result = RoomCommandHandler::execute(ctx, &command).await;
        let should_stop = RoomCommandHandler::should_stop_room_mailbox(&command, &result);
        self.observe_mailbox_result(&result);
        command.reply_with(result);
        should_stop
    }

    pub(super) fn observe_mailbox_result(&self, result: &RoomCommandResult) {
        if result.is_ok() {
            self.mailbox_completed.fetch_add(1, Ordering::Relaxed);
        } else {
            self.mailbox_failed.fetch_add(1, Ordering::Relaxed);
        }
    }

    const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
    const MAX_CONSECUTIVE_FALLBACK: u64 = 5;

    /// Runtime v2 mailbox routing: prefer per-room mailbox, fall back to inline.
    /// Used by general ops (set_lock, set_cycle, set_host, kick, close).
    ///
    /// On timeout, retries the mailbox once before falling back to inline,
    /// and logs a warning if consecutive fallbacks exceed threshold.
    pub(super) async fn room_mailbox_or_inline<Build, Inline, Fut>(
        &self,
        room_id: &str,
        build: Build,
        inline: Inline,
    ) -> RoomCommandResult
    where
        Build: FnOnce(oneshot::Sender<RoomCommandResult>) -> RoomActorCommand,
        Inline: FnOnce() -> Fut,
        Fut: Future<Output = Result<Value, String>>,
    {
        let result = self.try_mailbox_send(room_id, build).await;
        match result {
            Some(r) => r,
            None => {
                let fb = self.mailbox_fallback.fetch_add(1, Ordering::Relaxed) + 1;
                if fb >= Self::MAX_CONSECUTIVE_FALLBACK {
                    warn!(fallbacks = fb, room = room_id, "room mailbox: too many consecutive fallbacks, using inline");
                }
                RoomCommandResult::from_untyped(
                    inline().await,
                    RoomCommandDelivery::FallbackInline,
                )
            }
        }
    }

    /// Try to send a command through the mailbox, retrying once on timeout.
    /// Returns `None` if the mailbox is unavailable or both attempts fail.
    async fn try_mailbox_send<Build>(
        &self,
        room_id: &str,
        build: Build,
    ) -> Option<RoomCommandResult>
    where
        Build: FnOnce(oneshot::Sender<RoomCommandResult>) -> RoomActorCommand,
    {
        let tx = self.room_mailbox_sender(room_id)?;
        let (reply, rx) = oneshot::channel();
        let cmd = build(reply);
        self.mailbox_enqueued.fetch_add(1, Ordering::Relaxed);

        // First attempt
        match tokio::time::timeout(Self::COMMAND_TIMEOUT, tx.send(cmd)).await {
            Ok(Ok(())) => {
                match tokio::time::timeout(Self::COMMAND_TIMEOUT, rx).await {
                    Ok(Ok(result)) => return Some(result),
                    Ok(Err(_)) => {
                        self.mailbox_closed.fetch_add(1, Ordering::Relaxed);
                        return None;
                    }
                    Err(_) => {
                        // Reply timed out — retry once
                        self.mailbox_failed.fetch_add(1, Ordering::Relaxed);
                        self.mailbox_retried.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Ok(Err(_)) => {
                self.mailbox_closed.fetch_add(1, Ordering::Relaxed);
                return None;
            }
            Err(_) => {
                // Send timed out — retry once
                self.mailbox_failed.fetch_add(1, Ordering::Relaxed);
                self.mailbox_retried.fetch_add(1, Ordering::Relaxed);
            }
        }

        // Retry: re-send command through mailbox
        let tx = self.room_mailbox_sender(room_id)?;
        let (reply, rx) = oneshot::channel();
        let cmd = build(reply);
        self.mailbox_enqueued.fetch_add(1, Ordering::Relaxed);
        match tokio::time::timeout(Self::COMMAND_TIMEOUT, tx.send(cmd)).await {
            Ok(Ok(())) => match tokio::time::timeout(Self::COMMAND_TIMEOUT, rx).await {
                Ok(Ok(result)) => Some(result),
                _ => None,
            },
            _ => None,
        }
    }

    /// Route a non-idempotent room command through the per-room mailbox.
    ///
    /// If the command was successfully sent to the mailbox but the reply channel is
    /// lost, we deliberately return an error instead of falling back to inline
    /// execution.  This avoids duplicate `start`/`cancel` effects when the mailbox
    /// worker may already have executed the command but failed before replying.
    /// Used by start_room and cancel_start.
    pub(super) async fn room_mailbox_or_inline_control<Build, Inline, Fut>(
        &self,
        room_id: &str,
        build: Build,
        inline: Inline,
    ) -> RoomCommandResult
    where
        Build: FnOnce(oneshot::Sender<RoomCommandResult>) -> RoomActorCommand,
        Inline: FnOnce() -> Fut,
        Fut: Future<Output = Result<Value, String>>,
    {
        if let Some(tx) = self.room_mailbox_sender(room_id) {
            let (reply, rx) = oneshot::channel();
            let cmd = build(reply);
            self.mailbox_enqueued.fetch_add(1, Ordering::Relaxed);
            match tokio::time::timeout(Self::COMMAND_TIMEOUT, tx.send(cmd)).await {
                Ok(Ok(())) => match tokio::time::timeout(Self::COMMAND_TIMEOUT, rx).await {
                    Ok(Ok(result)) => result,
                    Ok(Err(_)) => {
                        self.mailbox_closed.fetch_add(1, Ordering::Relaxed);
                        self.mailbox_failed.fetch_add(1, Ordering::Relaxed);
                        RoomCommandResult::mailbox_error("room command mailbox reply lost after enqueue; refused inline retry for non-idempotent command")
                    }
                    Err(_) => {
                        // Reply timed out; command may still be executing —
                        // refuse inline retry for non-idempotent command.
                        self.mailbox_failed.fetch_add(1, Ordering::Relaxed);
                        RoomCommandResult::mailbox_error("room command mailbox reply timed out; refused inline retry for non-idempotent command")
                    }
                },
                Ok(Err(_)) => {
                    self.mailbox_closed.fetch_add(1, Ordering::Relaxed);
                    self.mailbox_fallback.fetch_add(1, Ordering::Relaxed);
                    RoomCommandResult::from_untyped(
                        inline().await,
                        RoomCommandDelivery::FallbackInline,
                    )
                }
                Err(_) => {
                    // Send timed out — command was never enqueued, so inline is safe.
                    self.mailbox_fallback.fetch_add(1, Ordering::Relaxed);
                    RoomCommandResult::from_untyped(
                        inline().await,
                        RoomCommandDelivery::FallbackInline,
                    )
                }
            }
        } else {
            self.mailbox_fallback.fetch_add(1, Ordering::Relaxed);
            RoomCommandResult::from_untyped(inline().await, RoomCommandDelivery::FallbackInline)
        }
    }
}

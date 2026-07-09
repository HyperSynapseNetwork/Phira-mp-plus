//! Mailbox-backed routing for room commands.

use super::{
    command::{RoomActorCommand, RoomCommandKind}, context::RoomCommandContext, handler::RoomCommandHandler,
    RoomCommandDelivery, RoomCommandGateway, RoomCommandResult,
};
use crate::server::PlusServerState;
use serde_json::Value;
use std::{
    future::Future,
    sync::{atomic::Ordering, Arc, Weak},
};
use tokio::sync::{mpsc, oneshot};

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
            while let Some(command) = rx.recv().await {
                let Some(state) = weak_state.upgrade() else {
                    gateway.mailbox_closed.fetch_add(1, Ordering::Relaxed);
                    let result = RoomCommandResult::mailbox_error(
                        "server state dropped before room command could run",
                    );
                    gateway.observe_mailbox_result(&result);
                    command.reply_with(result);
                    continue;
                };
                // Look up the room once and pass it through context so
                // handlers don't need to call find_room() again.
                let room = {
                    let rooms = state.rooms.read().await;
                    rooms.values().find(|r| r.id.to_string() == command.room_id()).map(Arc::clone)
                };
                // Track owned state after command execution
                let cmd_room_id = command.room_id().to_string();
                let cmd_kind = command.kind();
                let is_set_lock = matches!(cmd_kind, RoomCommandKind::SetLock);
                let is_set_cycle = matches!(cmd_kind, RoomCommandKind::SetCycle);
                let is_set_host = matches!(cmd_kind, RoomCommandKind::SetHost);
                let should_stop = if let Some(room) = room {
                    let result = gateway.execute_mailbox_command_with_room(&state, room.clone(), command).await;
                    // Mirror lock state after SetLock
                    if is_set_lock {
                        if let Ok(mut locks) = gateway.owned_locks.write() {
                            locks.insert(cmd_room_id.clone(), room.locked.load(Ordering::SeqCst));
                        }
                    }
                    // Mirror cycle state after SetCycle
                    if is_set_cycle {
                        if let Ok(mut cycles) = gateway.owned_cycles.write() {
                            cycles.insert(cmd_room_id.clone(), room.cycle.load(Ordering::SeqCst));
                        }
                    }
                    // Mirror host state after SetHost
                    if is_set_host {
                        let host_weak = room.host.read().await.clone();
                        let host_id = host_weak.upgrade().map(|u| u.id);
                        if let Ok(mut hosts) = gateway.owned_hosts.write() {
                            hosts.insert(cmd_room_id, host_id);
                        }
                    }
                    result
                } else {
                    let result = RoomCommandResult::mailbox_error(
                        "room not found in per-room mailbox",
                    );
                    gateway.observe_mailbox_result(&result);
                    command.reply_with(result);
                    continue;
                };
                if should_stop {
                    break;
                }
            }
            if let Ok(mut mailboxes) = gateway.room_mailboxes.write() {
                mailboxes.remove(&worker_room_id);
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

    /// Runtime v2 mailbox routing: prefer per-room mailbox, fall back to inline.
    /// Used by general ops (set_lock, set_cycle, set_host, kick, close) where
    /// inline fallback is safe.
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
        if let Some(tx) = self.room_mailbox_sender(room_id) {
            let (reply, rx) = oneshot::channel();
            let cmd = build(reply);
            self.mailbox_enqueued.fetch_add(1, Ordering::Relaxed);
            match tokio::time::timeout(Self::COMMAND_TIMEOUT, tx.send(cmd)).await {
                Ok(Ok(())) => match tokio::time::timeout(Self::COMMAND_TIMEOUT, rx).await {
                    Ok(Ok(result)) => result,
                    Ok(Err(_)) => {
                        self.mailbox_closed.fetch_add(1, Ordering::Relaxed);
                        self.mailbox_fallback.fetch_add(1, Ordering::Relaxed);
                        RoomCommandResult::from_untyped(
                            inline().await,
                            RoomCommandDelivery::FallbackInline,
                        )
                    }
                    Err(_) => {
                        // Reply timed out — command may still be executing.
                        // Fall back to inline to avoid hanging caller.
                        self.mailbox_failed.fetch_add(1, Ordering::Relaxed);
                        RoomCommandResult::from_untyped(
                            inline().await,
                            RoomCommandDelivery::FallbackInline,
                        )
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
                    // Send timed out — mailbox channel full or worker stuck.
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

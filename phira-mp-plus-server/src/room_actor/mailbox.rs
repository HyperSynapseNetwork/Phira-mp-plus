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

enum MailboxAttempt {
    Completed(RoomCommandResult),
    NotEnqueued,
    Uncertain(&'static str),
}

impl RoomCommandGateway {
    pub fn start_mailbox(self: &Arc<Self>, state: Arc<PlusServerState>, capacity: usize) {
        if let Ok(mut guard) = self.self_ref.write() {
            *guard = Some(Arc::downgrade(self));
        }
        if let Ok(mut guard) = self.state_ref.write() {
            *guard = Some(Arc::downgrade(&state));
        }
        self.mailbox_capacity.store(capacity.max(16), Ordering::Release);
        self.mailbox_started.store(true, Ordering::Release);
    }

    pub(super) fn mailbox_enabled(&self) -> bool {
        self.mailbox_started.load(Ordering::Acquire)
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

    pub(super) async fn room_mailbox_sender(
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
        let state = self.state_weak()?.upgrade()?;
        let gateway = self.self_arc()?;
        let rid: phira_mp_common::RoomId = room_id.to_string().try_into().ok()?;
        let room = {
            let rooms = state.rooms.read().await;
            rooms.get(&rid).map(Arc::clone)
        }?;

        let mut mailboxes = self.room_mailboxes.write().ok()?;
        if let Some(tx) = mailboxes.get(room_id).cloned() {
            self.mailbox_registry_hit.fetch_add(1, Ordering::Relaxed);
            return Some(tx);
        }

        let capacity = self.mailbox_capacity.load(Ordering::Acquire).max(16);
        let (tx, mut rx) = mpsc::channel::<RoomActorCommand>(capacity);
        mailboxes.insert(room_id.to_string(), tx.clone());
        self.mailbox_created.fetch_add(1, Ordering::Relaxed);
        let worker_room_id = room_id.to_string();
        drop(mailboxes);
        crate::supervisor_actor::spawn_named(format!("room-mailbox-{worker_room_id}"), async move {
            let mut actor = RoomActor::new(room, state.clone()).await;
            let snapshot = actor.snapshot().clone();
            if let Ok(mut snapshots) = gateway.snapshots.write() {
                snapshots.insert(worker_room_id.clone(), snapshot);
            }

            while let Some(command) = rx.recv().await {
                let should_stop = gateway
                    .execute_mailbox_command_with_room(
                        &actor.state,
                        actor.room().clone(),
                        command,
                    )
                    .await;
                actor.refresh_snapshot().await;
                let snapshot = actor.snapshot().clone();
                if let Ok(mut snapshots) = gateway.snapshots.write() {
                    snapshots.insert(worker_room_id.clone(), snapshot);
                }
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

    /// Prefer the per-room mailbox. Inline fallback is permitted only when the
    /// command is known not to have been enqueued. Once enqueue succeeds, a
    /// missing/timed-out reply is an uncertain outcome and is never retried
    /// inline, preventing duplicate side effects.
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
        match self.try_mailbox_send(room_id, build).await {
            MailboxAttempt::Completed(result) => result,
            MailboxAttempt::NotEnqueued => {
                self.mailbox_fallback.fetch_add(1, Ordering::Relaxed);
                RoomCommandResult::from_untyped(
                    inline().await,
                    RoomCommandDelivery::FallbackInline,
                )
            }
            MailboxAttempt::Uncertain(message) => {
                self.mailbox_failed.fetch_add(1, Ordering::Relaxed);
                RoomCommandResult::mailbox_error(message)
            }
        }
    }

    async fn try_mailbox_send<Build>(
        &self,
        room_id: &str,
        build: Build,
    ) -> MailboxAttempt
    where
        Build: FnOnce(oneshot::Sender<RoomCommandResult>) -> RoomActorCommand,
    {
        let Some(tx) = self.room_mailbox_sender(room_id).await else {
            return MailboxAttempt::NotEnqueued;
        };
        let (reply, rx) = oneshot::channel();
        let command = build(reply);

        match tokio::time::timeout(Self::COMMAND_TIMEOUT, tx.send(command)).await {
            Ok(Ok(())) => {
                self.mailbox_enqueued.fetch_add(1, Ordering::Relaxed);
                match tokio::time::timeout(Self::COMMAND_TIMEOUT, rx).await {
                    Ok(Ok(result)) => MailboxAttempt::Completed(result),
                    Ok(Err(_)) => {
                        self.mailbox_closed.fetch_add(1, Ordering::Relaxed);
                        MailboxAttempt::Uncertain(
                            "room command reply channel closed after enqueue; inline retry refused",
                        )
                    }
                    Err(_) => MailboxAttempt::Uncertain(
                        "room command reply timed out after enqueue; inline retry refused",
                    ),
                }
            }
            Ok(Err(_)) => {
                self.mailbox_closed.fetch_add(1, Ordering::Relaxed);
                MailboxAttempt::NotEnqueued
            }
            Err(_) => MailboxAttempt::NotEnqueued,
        }
    }

    /// Non-idempotent controls use the same uncertainty-safe routing policy.
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
        self.room_mailbox_or_inline(room_id, build, inline).await
    }

}

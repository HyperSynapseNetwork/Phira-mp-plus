//! Mailbox-backed routing for room commands.

use super::{
    actor::RoomActor, command::RoomActorCommand, context::RoomCommandContext,
    handler::RoomCommandHandler, RoomCommandGateway, RoomCommandResult,
};
use crate::server::PlusServerState;
use std::sync::{atomic::Ordering, Arc, Weak};
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
        self.mailbox_capacity
            .store(capacity.max(16), Ordering::Release);
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
        let state = self.state_weak()?.upgrade()?;
        let gateway = self.self_arc()?;
        let rid: phira_mp_common::RoomId = room_id.to_string().try_into().ok()?;
        let room = {
            let rooms = state.rooms.read().await;
            rooms.get(&rid).map(Arc::clone)
        }?;
        let room_uuid = room.uuid.clone();

        if let Ok(mailboxes) = self.room_mailboxes.read() {
            if let Some(entry) = mailboxes.get(room_id) {
                if entry.room_uuid == room_uuid && !entry.tx.is_closed() {
                    self.mailbox_registry_hit.fetch_add(1, Ordering::Relaxed);
                    return Some(entry.tx.clone());
                }
            }
        }

        self.mailbox_registry_miss.fetch_add(1, Ordering::Relaxed);
        // 作用域限制 StdRwLockWriteGuard 在 .await 之前释放
        let (tx, mut rx, capacity) = {
            let mut mailboxes = self.room_mailboxes.write().ok()?;
            if let Some(entry) = mailboxes.get(room_id) {
                if entry.room_uuid == room_uuid && !entry.tx.is_closed() {
                    self.mailbox_registry_hit.fetch_add(1, Ordering::Relaxed);
                    return Some(entry.tx.clone());
                }
            }
            let cap = self.mailbox_capacity.load(Ordering::Acquire).max(16);
            let (tx, rx) = mpsc::channel::<RoomActorCommand>(cap);
            mailboxes.insert(
                room_id.to_string(),
                super::RoomMailboxEntry {
                    room_uuid: room_uuid.clone(),
                    tx: tx.clone(),
                },
            );
            (tx, rx, cap)
        };
        self.mailbox_created.fetch_add(1, Ordering::Relaxed);
        let worker_room_id = room_id.to_string();
        let worker_room_uuid = room_uuid.clone();
        let worker_rid = rid.clone();
        // 作用域结束，mailboxes 已释放

        // The room registry may have changed while the mailbox registry was
        // being updated. Refuse this command rather than attaching a fresh
        // sender to a room generation that is no longer authoritative.
        let still_current = {
            let rooms = state.rooms.read().await;
            rooms
                .get(&rid)
                .map(|current| current.uuid == room_uuid)
                .unwrap_or(false)
        };
        if !still_current {
            self.remove_mailbox_if_current(room_id, room_uuid);
            return None;
        }

        crate::supervisor_actor::spawn_named(
            format!("room-mailbox-{worker_room_id}"),
            async move {
                let mut actor = RoomActor::new(room, state.clone()).await;
                gateway.store_snapshot_if_current(
                    &worker_room_id,
                    worker_room_uuid.clone(),
                    actor.snapshot().clone(),
                );

                let mut lifecycle_tick = tokio::time::interval(std::time::Duration::from_secs(1));
                lifecycle_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tokio::select! {
                        command = rx.recv() => {
                            let Some(command) = command else {
                                break;
                            };
                            let should_stop = gateway
                                .execute_mailbox_command_with_room(
                                    &actor.state,
                                    actor.room().clone(),
                                    command,
                                )
                                .await;
                            actor.refresh_snapshot().await;
                            gateway.store_snapshot_if_current(
                                &worker_room_id,
                                worker_room_uuid.clone(),
                                actor.snapshot().clone(),
                            );
                            if should_stop {
                                break;
                            }
                        }
                        _ = lifecycle_tick.tick() => {
                            let generation_is_current = {
                                let rooms = actor.state.rooms.read().await;
                                rooms
                                    .get(&worker_rid)
                                    .map(|current| current.uuid == worker_room_uuid)
                                    .unwrap_or(false)
                            };
                            if !generation_is_current {
                                break;
                            }
                        }
                    }
                }
                gateway.remove_mailbox_if_current(&worker_room_id, worker_room_uuid.clone());
                gateway.remove_snapshot_if_current(&worker_room_id, worker_room_uuid);
                gateway.mailbox_closed.fetch_add(1, Ordering::Relaxed);
            },
        );
        Some(tx)
    }

    fn store_snapshot_if_current(
        &self,
        room_id: &str,
        room_uuid: uuid::Uuid,
        snapshot: super::actor::RoomSnapshot,
    ) {
        // Keep the mailbox read guard until the snapshot update commits. This
        // prevents an old actor from passing an identity check, being replaced,
        // and then overwriting the new room generation's snapshot.
        let Ok(mailboxes) = self.room_mailboxes.read() else {
            return;
        };
        let current = mailboxes
            .get(room_id)
            .map(|entry| entry.room_uuid == room_uuid && !entry.tx.is_closed())
            .unwrap_or(false);
        if !current {
            return;
        }
        if let Ok(mut snapshots) = self.snapshots.write() {
            snapshots.insert(
                room_id.to_string(),
                super::RoomSnapshotEntry {
                    room_uuid,
                    snapshot,
                },
            );
        }
    }

    fn remove_mailbox_if_current(&self, room_id: &str, room_uuid: uuid::Uuid) {
        if let Ok(mut mailboxes) = self.room_mailboxes.write() {
            let matches = mailboxes
                .get(room_id)
                .map(|entry| entry.room_uuid == room_uuid)
                .unwrap_or(false);
            if matches {
                mailboxes.remove(room_id);
            }
        }
    }

    fn remove_snapshot_if_current(&self, room_id: &str, room_uuid: uuid::Uuid) {
        if let Ok(mut snapshots) = self.snapshots.write() {
            let matches = snapshots
                .get(room_id)
                .map(|entry| entry.room_uuid == room_uuid)
                .unwrap_or(false);
            if matches {
                snapshots.remove(room_id);
            }
        }
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

    /// Route through the per-room mailbox only. Missing, closed, congested or
    /// uncertain mailboxes fail explicitly so the room control plane has one
    /// execution model and one lock ordering.
    pub(super) async fn room_mailbox<Build>(&self, room_id: &str, build: Build) -> RoomCommandResult
    where
        Build: FnOnce(oneshot::Sender<RoomCommandResult>) -> RoomActorCommand,
    {
        match self.try_mailbox_send(room_id, build).await {
            MailboxAttempt::Completed(result) => result,
            MailboxAttempt::NotEnqueued => {
                self.mailbox_failed.fetch_add(1, Ordering::Relaxed);
                RoomCommandResult::mailbox_error(
                    "room mailbox unavailable before enqueue; inline execution is disabled",
                )
            }
            MailboxAttempt::Uncertain(message) => {
                self.mailbox_failed.fetch_add(1, Ordering::Relaxed);
                RoomCommandResult::mailbox_error(message)
            }
        }
    }

    async fn try_mailbox_send<Build>(&self, room_id: &str, build: Build) -> MailboxAttempt
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
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_actor_cleanup_cannot_remove_new_mailbox_generation() {
        let gateway = RoomCommandGateway::new();
        let old_uuid = uuid::Uuid::new_v4();
        let new_uuid = uuid::Uuid::new_v4();
        let (tx, _rx) = mpsc::channel(1);

        gateway
            .room_mailboxes
            .write()
            .expect("mailbox registry lock")
            .insert(
                "same-name".to_string(),
                super::super::RoomMailboxEntry {
                    room_uuid: new_uuid,
                    tx,
                },
            );

        gateway.remove_mailbox_if_current("same-name", old_uuid);

        let guard = gateway
            .room_mailboxes
            .read()
            .expect("mailbox registry lock");
        assert_eq!(
            guard.get("same-name").map(|entry| entry.room_uuid),
            Some(new_uuid)
        );
    }

    #[test]
    fn stale_actor_cleanup_cannot_remove_new_snapshot_generation() {
        let gateway = RoomCommandGateway::new();
        let old_uuid = uuid::Uuid::new_v4();
        let new_uuid = uuid::Uuid::new_v4();
        let snapshot = super::super::actor::RoomSnapshot {
            room_id: "same-name".to_string(),
            room_uuid: new_uuid.to_string(),
            locked: false,
            cycle: false,
            host: None,
            hidden: false,
            live: false,
            created_at: 0,
        };

        gateway
            .snapshots
            .write()
            .expect("snapshot registry lock")
            .insert(
                "same-name".to_string(),
                super::super::RoomSnapshotEntry {
                    room_uuid: new_uuid,
                    snapshot,
                },
            );

        gateway.remove_snapshot_if_current("same-name", old_uuid);

        let guard = gateway.snapshots.read().expect("snapshot registry lock");
        assert_eq!(
            guard.get("same-name").map(|entry| entry.room_uuid),
            Some(new_uuid)
        );
    }
}

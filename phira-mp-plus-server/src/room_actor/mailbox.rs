//! Mailbox-backed routing for room commands.

use super::{
    actor::RoomActor, command::RoomActorCommand,
    RoomCommandGateway, RoomCommandResult,
};
use crate::room::InternalRoomState;
use crate::server::PlusServerState;
use phira_mp_common::ServerCommand;
use std::sync::{atomic::Ordering, Arc, Weak};
use std::time::Instant;
use tokio::sync::{broadcast, mpsc, oneshot};

/// PMP44 P1 §26: lifecycle 维护周期（代际检查 / stale 清理 / Ready 倒计时 /
/// Playing 超时）。`biased` select 下 control 分支始终优先，可能长期饿死
/// `lifecycle_tick` 分支；control 分支内用同一常量做 deadline 检查兜底，
/// 保证维护至少按此周期运行。
const LIFECYCLE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

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
        // 审计 P1-A: bounded broadcast channel for monitor telemetry.
        // Capacity 16 — small and bounded. try_send drops oldest when full,
        // so slow monitors never block the game hot path.
        const MONITOR_TELEMETRY_CAPACITY: usize = 16;
        let (tx, mut rx, mut telemetry_rx, _capacity, monitor_rx) = {
            let mut mailboxes = self.room_mailboxes.write().ok()?;
            if let Some(entry) = mailboxes.get(room_id) {
                if entry.room_uuid == room_uuid && !entry.tx.is_closed() {
                    self.mailbox_registry_hit.fetch_add(1, Ordering::Relaxed);
                    return Some(entry.tx.clone());
                }
            }
            let cap = self.mailbox_capacity.load(Ordering::Acquire).max(16);
            let (tx, rx) = mpsc::channel::<RoomActorCommand>(cap);
            // 审计 P0: 独立 telemetry channel，容量 2× control 以应对高频 Touch/Judge。
            let telemetry_cap = cap * 2;
            let (telemetry_tx, telemetry_rx) = mpsc::channel::<RoomActorCommand>(telemetry_cap);
            let (monitor_tx, monitor_rx) = broadcast::channel::<ServerCommand>(MONITOR_TELEMETRY_CAPACITY);
            mailboxes.insert(
                room_id.to_string(),
                super::RoomMailboxEntry {
                    room_uuid: room_uuid.clone(),
                    tx: tx.clone(),
                    telemetry_tx,
                    monitor_telemetry_tx: monitor_tx,
                },
            );
            (tx, rx, telemetry_rx, cap, monitor_rx)
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
                let mut actor = RoomActor::new(room, state.clone());
                gateway.store_snapshot_if_current(
                    &worker_room_id,
                    worker_room_uuid.clone(),
                    actor.snapshot().clone(),
                );

                // 审计 P1-A: 独立 monitor telemetry relay task。
                // Reads from the bounded broadcast channel and forwards to actual
                // monitor connections. If the broadcast channel is full, old
                // telemetry frames are dropped (coalesce/skip old telemetry).
                // This relay runs alongside the main mailbox loop so monitor
                // broadcasts never block the game hot path.
                {
                    let relay_room = Arc::clone(actor.room());
                    crate::supervisor_actor::spawn_named(
                        format!("room-monitor-relay-{worker_room_id}"),
                        async move {
                            let mut rx = monitor_rx;
                            loop {
                                match rx.recv().await {
                                    Ok(cmd) => relay_room.broadcast_monitors(cmd).await,
                                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                                        tracing::trace!(
                                            room = %relay_room.id,
                                            skipped,
                                            "monitor relay lagged, dropping old telemetry"
                                        );
                                    }
                                    Err(broadcast::error::RecvError::Closed) => break,
                                }
                            }
                        },
                    );
                }

                let mut lifecycle_tick = tokio::time::interval(LIFECYCLE_INTERVAL);
                lifecycle_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                // PMP44 P1 §26: 记录上次 lifecycle 维护的时间。`biased` select
                // 下 control 分支始终优先，若仅靠 `lifecycle_tick.tick()` 触发，
                // 持续的控制命令压力会饿死该分支（Ready 倒计时 / Playing 超时 /
                // stale 清理永不运行）。control 分支内按 `elapsed` 做 deadline
                // 兜底，保证维护至少按 `LIFECYCLE_INTERVAL` 周期运行。
                let mut last_tick = tokio::time::Instant::now();
                loop {
                    tokio::select! {
                        biased;
                        // 审计 P0: control 命令优先处理。
                        command = rx.recv() => {
                            let Some(command) = command else {
                                break;
                            };
                            let should_stop = actor.execute_command(command).await;
                            if should_stop {
                                break;
                            }
                            // PMP44 P1 §26: control 分支内检查 lifecycle tick 是否到期——
                            // 若已超期立即让出执行维护，避免 biased select 饿死维护。
                            // 注：`tick()` 只在周期边界触发，此处按 elapsed 判断可能
                            // 略早（一个周期内）运行维护；这是可接受的——维护至少按
                            // 周期频率运行，deadline 检查正是为了防止饥饿。
                            if last_tick.elapsed() >= LIFECYCLE_INTERVAL {
                                if run_lifecycle_maintenance(
                                    &mut actor,
                                    &worker_rid,
                                    &worker_room_uuid,
                                ).await {
                                    break;
                                }
                                last_tick = tokio::time::Instant::now();
                            }
                        }
                        // 审计 P0: telemetry 命令通过独立 channel 非阻塞接收。
                        telemetry = telemetry_rx.recv() => {
                            let Some(telemetry) = telemetry else {
                                break;
                            };
                            actor.execute_telemetry(telemetry).await;
                        }
                        _ = lifecycle_tick.tick() => {
                            if run_lifecycle_maintenance(
                                &mut actor,
                                &worker_rid,
                                &worker_room_uuid,
                            ).await {
                                break;
                            }
                            last_tick = tokio::time::Instant::now();
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

    pub(super) fn store_snapshot_if_current(
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
    ///
    /// P0-C/P0-G: `deadline` is the absolute actor deadline carried by the
    /// invoking client command. When `Some(d)`, both the enqueue and the reply
    /// wait use `d.saturating_duration_since(now)` as their budget so the actor
    /// never keeps a client's response channel open past its deadline. When
    /// `None` (non-session paths: CLI/admin/force-move), the mailbox falls back
    /// to the internal `COMMAND_TIMEOUT` (30s) so non-session callers are not
    /// accidentally killed by a missing deadline.
    pub(super) async fn room_mailbox<Build>(
        &self,
        room_id: &str,
        deadline: Option<Instant>,
        build: Build,
    ) -> RoomCommandResult
    where
        Build: FnOnce(oneshot::Sender<RoomCommandResult>) -> RoomActorCommand,
    {
        match self.try_mailbox_send(room_id, deadline, build).await {
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

    /// Remaining mailbox budget for an optional absolute actor deadline.
    /// `Some(d)` clamps to the time left before `d`; `None` falls back to the
    /// internal 30s command timeout.
    fn mailbox_remaining(deadline: Option<Instant>) -> std::time::Duration {
        deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
            .unwrap_or(Self::COMMAND_TIMEOUT)
    }

    async fn try_mailbox_send<Build>(
        &self,
        room_id: &str,
        deadline: Option<Instant>,
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
        let budget = Self::mailbox_remaining(deadline);

        match tokio::time::timeout(budget, tx.send(command)).await {
            Ok(Ok(())) => {
                self.mailbox_enqueued.fetch_add(1, Ordering::Relaxed);
                let reply_budget = Self::mailbox_remaining(deadline);
                match tokio::time::timeout(reply_budget, rx).await {
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

/// PMP44 P1 §26: 周期性 lifecycle 维护。从 select 循环中抽出以便 control
/// 分支和 `lifecycle_tick.tick()` 分支共用，避免 `biased` select 下该维护被
/// 持续控制命令饿死。内容包括：房间代际检查（过期则返回 `true` 停止 worker）、
/// stale player_data / display_names 清理、Ready 倒计时超时强制开赛、
/// Playing 超时强制结束。
async fn run_lifecycle_maintenance(
    actor: &mut RoomActor,
    worker_rid: &phira_mp_common::RoomId,
    worker_room_uuid: &uuid::Uuid,
) -> bool {
    let generation_is_current = {
        let rooms = actor.state.rooms.read().await;
        rooms
            .get(worker_rid)
            .map(|current| current.uuid == *worker_room_uuid)
            .unwrap_or(false)
    };
    if !generation_is_current {
        return true;
    }
    // Prune stale player_data and display_names entries for users no
    // longer in this room. Users may leave through direct paths (dangle/
    // leave_room) that bypass the actor mailbox, so periodic cleanup is
    // necessary to prevent unbounded HashMap growth over time.
    let as_ = &mut actor.actor_state;
    // Collect current member IDs from actor state (authoritative).
    let current_ids: std::collections::HashSet<i32> = {
        let members = &as_.state.members;
        members.users.iter().chain(members.monitors.iter()).copied().collect()
    };
    as_.player_data.retain(|&k, _| current_ids.contains(&k));
    as_.display_names.retain(|&k, _| current_ids.contains(&k));

    // 准备倒计时：检查是否超时
    if let InternalRoomState::WaitForReady { .. } = &as_.state.lifecycle {
        if let Some(started_at) = as_.state.ready_countdown_started_at {
            let elapsed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0) - started_at;
            let timeout_ms = (actor.state.config.ready_countdown_secs.max(10) * 1000) as i64;
            if elapsed >= timeout_ms {
                // 超时 —— 强制开赛
                let room = Arc::clone(&actor.room);
                let lc = crate::room_actor::lifecycle::DefaultRoomLifecycle::new(
                    room,
                    Arc::clone(&actor.state),
                );
                crate::room_actor::handler::force_start_playing(
                    &lc, &mut as_.state,
                    std::time::Instant::now() + RoomCommandGateway::COMMAND_TIMEOUT,
                ).await;
            }
        }
    }

    // 对局超时：检查 Playing 状态下是否超过截止时间
    if let InternalRoomState::Playing { .. } = &as_.state.lifecycle {
        if let Some(deadline) = as_.state.playing_timeout_deadline {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            if now >= deadline {
                as_.state.playing_timeout_deadline = None;
                let room = Arc::clone(&actor.room);
                let lc = crate::room_actor::lifecycle::DefaultRoomLifecycle::new(
                    room,
                    Arc::clone(&actor.state),
                );
                crate::room_actor::handler::force_end_playing(
                    &lc, &mut as_.state,
                ).await;
            }
        }
    }
    // Ready 倒计时强开（force_start_playing）和 Playing 超时强结
    // （force_end_playing）都直接改 actor_state，绕过 execute_command——
    // 若不在此刷新 snapshot 缓存，外部读（current_room_in_select_chart /
    // request_start / room info）会看到过期的 lifecycle（如 Playing 或
    // WaitForReady），导致房主游玩结束后请求开赛被拒为 invalid room state
    //（PMP28 回归）。
    actor.refresh_snapshot_from_state();
    actor.state.room_commands.store_snapshot_if_current(
        &worker_rid.to_string(),
        worker_room_uuid.clone(),
        actor.snapshot().clone(),
    );
    false
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
        let (telem_tx, _telem_rx) = mpsc::channel(1);
        let (monitor_tx, _monitor_rx) = broadcast::channel::<ServerCommand>(16);

        gateway
            .room_mailboxes
            .write()
            .expect("mailbox registry lock")
            .insert(
                "same-name".to_string(),
                super::super::RoomMailboxEntry {
                    room_uuid: new_uuid,
                    tx,
                    telemetry_tx: telem_tx,
                    monitor_telemetry_tx: monitor_tx,
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
            system_host: false,
            hidden: false,
            live: false,
            created_at: 0,
            persistent_empty: false,
            chart: None,
            chart_name: None,
            stripped: phira_mp_common::StrippedRoomState::SelectingChart,
            round_id: None,
            ready_set: None,
            members: super::super::actor::RoomMembers { users: Vec::new(), monitors: Vec::new() },
            results_keys: Vec::new(),
            aborted_users: Vec::new(),
            playing_users: Vec::new(),
            degraded: false,
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

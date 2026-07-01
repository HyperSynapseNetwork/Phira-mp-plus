//! Runtime v2 room-command gateway.
//!
//! This is the first production-facing seam for the future `room-actor`.
//! It deliberately does **not** own room state yet.  Instead, CLI/admin/WIT-like
//! room write commands route through one facade while the existing `Room` state
//! machine continues to own behavior.  The migration path is:
//!
//! 1. route duplicate room write commands through this gateway;
//! 2. add metrics, tests, and simulation coverage around the gateway;
//! 3. route low-risk commands through a mailbox-backed path;
//! 4. replace the remaining inline implementation with per-room actors;
//! 5. remove the old direct calls from `cli.rs`, `server.rs`, and `session.rs`.

use crate::plugin::PluginEvent;
use crate::server::PlusServerState;
use phira_mp_common::{Message, PartialRoomData, RoomEvent, RoomId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    sync::atomic::{AtomicU64, Ordering},
    sync::{Arc, RwLock as StdRwLock},
};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomCommandGatewayStats {
    pub routed: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub mailbox_enabled: bool,
    pub mailbox_enqueued: u64,
    pub mailbox_completed: u64,
    pub mailbox_failed: u64,
    pub mailbox_fallback: u64,
    pub mailbox_closed: u64,
    pub phase: String,
    pub note: String,
}

pub struct RoomCommandGateway {
    routed: AtomicU64,
    succeeded: AtomicU64,
    failed: AtomicU64,
    mailbox_tx: StdRwLock<Option<mpsc::Sender<RoomActorCommand>>>,
    mailbox_enqueued: AtomicU64,
    mailbox_completed: AtomicU64,
    mailbox_failed: AtomicU64,
    mailbox_fallback: AtomicU64,
    mailbox_closed: AtomicU64,
}

enum RoomActorCommand {
    SetLock {
        room_id: String,
        locked: bool,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    SetCycle {
        room_id: String,
        cycle: bool,
        reply: oneshot::Sender<Result<Value, String>>,
    },
}

impl RoomCommandGateway {
    pub fn new() -> Self {
        Self {
            routed: AtomicU64::new(0),
            succeeded: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            mailbox_tx: StdRwLock::new(None),
            mailbox_enqueued: AtomicU64::new(0),
            mailbox_completed: AtomicU64::new(0),
            mailbox_failed: AtomicU64::new(0),
            mailbox_fallback: AtomicU64::new(0),
            mailbox_closed: AtomicU64::new(0),
        }
    }

    /// Start the first mailbox-backed path for low-risk room commands.
    ///
    /// Runtime v2 Step 14 intentionally keeps this as a gateway-level worker,
    /// not yet a true per-room actor.  The important migration property is that
    /// selected writes now cross an async command boundary before touching the
    /// existing `Room` state machine.  Once this proves stable, the worker can
    /// be split into per-room mailboxes without changing CLI/admin callers.
    pub fn start_mailbox(self: &Arc<Self>, state: Arc<PlusServerState>, capacity: usize) {
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
                    match command {
                        RoomActorCommand::SetLock { reply, .. } => {
                            let result = Err("server state dropped before room command could run".to_string());
                            gateway.observe_mailbox_result(&result);
                            let _ = reply.send(result);
                        }
                        RoomActorCommand::SetCycle { reply, .. } => {
                            let result = Err("server state dropped before room command could run".to_string());
                            gateway.observe_mailbox_result(&result);
                            let _ = reply.send(result);
                        }
                    }
                    continue;
                };

                match command {
                    RoomActorCommand::SetLock { room_id, locked, reply } => {
                        let result = gateway.set_lock_inline(&state, &room_id, locked).await;
                        gateway.observe_mailbox_result(&result);
                        let _ = reply.send(result);
                    }
                    RoomActorCommand::SetCycle { room_id, cycle, reply } => {
                        let result = gateway.set_cycle_inline(&state, &room_id, cycle).await;
                        gateway.observe_mailbox_result(&result);
                        let _ = reply.send(result);
                    }
                }
            }
            gateway.mailbox_closed.fetch_add(1, Ordering::Relaxed);
        });
    }

    fn mailbox_sender(&self) -> Option<mpsc::Sender<RoomActorCommand>> {
        self.mailbox_tx.read().ok().and_then(|guard| guard.clone())
    }

    fn mailbox_enabled(&self) -> bool {
        self.mailbox_sender().is_some()
    }

    fn observe_mailbox_result<T>(&self, result: &Result<T, String>) {
        match result {
            Ok(_) => {
                self.mailbox_completed.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                self.mailbox_failed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub fn stats(&self) -> RoomCommandGatewayStats {
        let mailbox_enabled = self.mailbox_enabled();
        RoomCommandGatewayStats {
            routed: self.routed.load(Ordering::Relaxed),
            succeeded: self.succeeded.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            mailbox_enabled,
            mailbox_enqueued: self.mailbox_enqueued.load(Ordering::Relaxed),
            mailbox_completed: self.mailbox_completed.load(Ordering::Relaxed),
            mailbox_failed: self.mailbox_failed.load(Ordering::Relaxed),
            mailbox_fallback: self.mailbox_fallback.load(Ordering::Relaxed),
            mailbox_closed: self.mailbox_closed.load(Ordering::Relaxed),
            phase: if mailbox_enabled { "mailbox_partial" } else { "inline_facade" }.to_string(),
            note: "set_lock/set_cycle now cross a mailbox-backed command boundary; remaining room writes still use the inline gateway facade".to_string(),
        }
    }

    fn observe<T>(&self, result: Result<T, String>) -> Result<T, String> {
        self.routed.fetch_add(1, Ordering::Relaxed);
        match &result {
            Ok(_) => { self.succeeded.fetch_add(1, Ordering::Relaxed); }
            Err(_) => { self.failed.fetch_add(1, Ordering::Relaxed); }
        }
        result
    }

    async fn find_room(
        &self,
        state: &PlusServerState,
        room_id: &str,
    ) -> Result<(RoomId, Arc<crate::room::Room>), String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let room = {
            let rooms = state.rooms.read().await;
            rooms.get(&rid).map(Arc::clone)
        }
        .ok_or_else(|| "room not found".to_string())?;
        Ok((rid, room))
    }

    /// Kick a user/monitor from a room.
    pub async fn kick_user(
        &self,
        state: &PlusServerState,
        room_id: &str,
        target_id: i32,
    ) -> Result<Value, String> {
        let result = async {
            let (rid, room) = self.find_room(state, room_id).await?;
            let users = room.users().await;
            let monitors = room.monitors().await;
            let user = users
                .into_iter()
                .chain(monitors.into_iter())
                .find(|u| u.id == target_id)
                .ok_or_else(|| "user not in room".to_string())?;

            room.send(Message::Chat {
                user: 0,
                content: format!("用户 {} 已被管理员踢出房间", user.name),
            })
            .await;
            let was_monitor = user.monitor.load(Ordering::SeqCst);
            let should_drop = room.on_user_leave(&user).await;
            if should_drop {
                state.rooms.write().await.remove(&rid);
            }
            if !was_monitor {
                state
                    .publish_room_event(RoomEvent::LeaveRoom {
                        room: rid.clone(),
                        user: target_id,
                    })
                    .await;
            }
            state
                .plugin_manager
                .trigger(&PluginEvent::RoomModify {
                    user_id: target_id,
                    room_id: room_id.to_string(),
                    data: serde_json::json!({"action":"kicked"}).to_string(),
                })
                .await;
            Ok(serde_json::json!({
                "ok": true,
                "room_id": room_id,
                "user_id": target_id,
                "user_name": user.name.clone(),
            }))
        }
        .await;
        self.observe(result)
    }

    /// Close and remove a room.
    pub async fn close_room(&self, state: &PlusServerState, room_id: &str) -> Result<Value, String> {
        let result = async {
            let (rid, room) = self.find_room(state, room_id).await?;
            let room_id_str = room.id.to_string();
            room.send(Message::Chat {
                user: 0,
                content: "房间已被管理员关闭".to_string(),
            })
            .await;
            for user in room.users().await {
                *user.room.write().await = None;
                state
                    .publish_room_event(RoomEvent::LeaveRoom {
                        room: rid.clone(),
                        user: user.id,
                    })
                    .await;
            }
            for monitor in room.monitors().await {
                *monitor.room.write().await = None;
            }
            state.rooms.write().await.remove(&rid);
            state
                .plugin_manager
                .trigger(&PluginEvent::RoomModify {
                    user_id: 0,
                    room_id: room_id_str.clone(),
                    data: serde_json::json!({"action":"closed"}).to_string(),
                })
                .await;
            Ok(serde_json::json!({"ok": true, "room_id": room_id_str}))
        }
        .await;
        self.observe(result)
    }

    /// Start a room through the existing admin-start path.
    pub async fn start_room(&self, state: &PlusServerState, room_id: &str) -> Result<Value, String> {
        let result = async {
            let (_rid, room) = self.find_room(state, room_id).await?;
            room.begin_admin_start().await.map_err(|e| e.to_string())?;
            if let Some(pm) = &room.plugin_manager {
                pm.trigger(&PluginEvent::GameStart {
                    user_id: 0,
                    room_id: room_id.to_string(),
                })
                .await;
            }
            Ok(serde_json::json!({"ok": true, "room_id": room_id, "action": "start"}))
        }
        .await;
        self.observe(result)
    }

    /// Cancel a pending admin-start wait state.
    pub async fn cancel_start(&self, state: &PlusServerState, room_id: &str) -> Result<Value, String> {
        let result = async {
            let (_rid, room) = self.find_room(state, room_id).await?;
            let canceled = {
                let mut room_state = room.state.write().await;
                if matches!(&*room_state, crate::room::InternalRoomState::WaitForReady { .. }) {
                    room.send(Message::CancelGame { user: 0 }).await;
                    *room_state = crate::room::InternalRoomState::SelectChart;
                    true
                } else {
                    false
                }
            };
            if canceled {
                room.finish_admin_start().await;
                room.on_state_change().await;
            }
            Ok(serde_json::json!({
                "ok": true,
                "room_id": room_id,
                "canceled": canceled,
            }))
        }
        .await;
        self.observe(result)
    }

    /// Set the room host. `None` means the system `?` host.
    pub async fn set_host(
        &self,
        state: &PlusServerState,
        room_id: &str,
        target_id: Option<i32>,
    ) -> Result<Value, String> {
        let result = async {
            let (_rid, room) = self.find_room(state, room_id).await?;
            match target_id {
                Some(user_id) => {
                    let mut user_name = user_id.to_string();
                    for user in room.users().await {
                        if user.id == user_id {
                            user_name = room.display_name(&user).await;
                            break;
                        }
                    }
                    room.send(Message::Chat {
                        user: 0,
                        content: format!("房主已转移给 {}", user_name),
                    })
                    .await;
                    room.set_host(Some(user_id), true).await.map_err(|e| e.to_string())?;
                    Ok(serde_json::json!({
                        "ok": true,
                        "room_id": room_id,
                        "host": user_id,
                        "host_name": user_name,
                        "host_is_system": false,
                    }))
                }
                None => {
                    room.send(Message::Chat {
                        user: 0,
                        content: "房主已设为系统 ?".to_string(),
                    })
                    .await;
                    room.set_host(None, true).await.map_err(|e| e.to_string())?;
                    Ok(serde_json::json!({
                        "ok": true,
                        "room_id": room_id,
                        "host": Value::Null,
                        "host_name": "?",
                        "host_is_system": true,
                    }))
                }
            }
        }
        .await;
        self.observe(result)
    }

    pub async fn set_lock(
        &self,
        state: &PlusServerState,
        room_id: &str,
        locked: bool,
    ) -> Result<Value, String> {
        let result = if let Some(tx) = self.mailbox_sender() {
            let (reply, rx) = oneshot::channel();
            self.mailbox_enqueued.fetch_add(1, Ordering::Relaxed);
            match tx
                .send(RoomActorCommand::SetLock {
                    room_id: room_id.to_string(),
                    locked,
                    reply,
                })
                .await
            {
                Ok(()) => match rx.await {
                    Ok(result) => result,
                    Err(_) => {
                        self.mailbox_closed.fetch_add(1, Ordering::Relaxed);
                        self.mailbox_fallback.fetch_add(1, Ordering::Relaxed);
                        self.set_lock_inline(state, room_id, locked).await
                    }
                },
                Err(_) => {
                    self.mailbox_closed.fetch_add(1, Ordering::Relaxed);
                    self.mailbox_fallback.fetch_add(1, Ordering::Relaxed);
                    self.set_lock_inline(state, room_id, locked).await
                }
            }
        } else {
            self.mailbox_fallback.fetch_add(1, Ordering::Relaxed);
            self.set_lock_inline(state, room_id, locked).await
        };
        self.observe(result)
    }

    async fn set_lock_inline(
        &self,
        state: &PlusServerState,
        room_id: &str,
        locked: bool,
    ) -> Result<Value, String> {
        let (_rid, room) = self.find_room(state, room_id).await?;
        room.locked.store(locked, Ordering::SeqCst);
        room.send(Message::LockRoom { lock: locked }).await;
        room.publish_update(PartialRoomData {
            lock: Some(locked),
            ..Default::default()
        })
        .await;
        state
            .plugin_manager
            .trigger(&PluginEvent::RoomModify {
                user_id: 0,
                room_id: room_id.to_string(),
                data: serde_json::json!({"action":"lock","value":locked}).to_string(),
            })
            .await;
        Ok(serde_json::json!({"ok": true, "room_id": room_id, "locked": locked}))
    }

    pub async fn set_cycle(
        &self,
        state: &PlusServerState,
        room_id: &str,
        cycle: bool,
    ) -> Result<Value, String> {
        let result = if let Some(tx) = self.mailbox_sender() {
            let (reply, rx) = oneshot::channel();
            self.mailbox_enqueued.fetch_add(1, Ordering::Relaxed);
            match tx
                .send(RoomActorCommand::SetCycle {
                    room_id: room_id.to_string(),
                    cycle,
                    reply,
                })
                .await
            {
                Ok(()) => match rx.await {
                    Ok(result) => result,
                    Err(_) => {
                        self.mailbox_closed.fetch_add(1, Ordering::Relaxed);
                        self.mailbox_fallback.fetch_add(1, Ordering::Relaxed);
                        self.set_cycle_inline(state, room_id, cycle).await
                    }
                },
                Err(_) => {
                    self.mailbox_closed.fetch_add(1, Ordering::Relaxed);
                    self.mailbox_fallback.fetch_add(1, Ordering::Relaxed);
                    self.set_cycle_inline(state, room_id, cycle).await
                }
            }
        } else {
            self.mailbox_fallback.fetch_add(1, Ordering::Relaxed);
            self.set_cycle_inline(state, room_id, cycle).await
        };
        self.observe(result)
    }

    async fn set_cycle_inline(
        &self,
        state: &PlusServerState,
        room_id: &str,
        cycle: bool,
    ) -> Result<Value, String> {
        let (_rid, room) = self.find_room(state, room_id).await?;
        room.cycle.store(cycle, Ordering::SeqCst);
        room.send(Message::CycleRoom { cycle }).await;
        room.publish_update(PartialRoomData {
            cycle: Some(cycle),
            ..Default::default()
        })
        .await;
        state
            .plugin_manager
            .trigger(&PluginEvent::RoomModify {
                user_id: 0,
                room_id: room_id.to_string(),
                data: serde_json::json!({"action":"cycle","value":cycle}).to_string(),
            })
            .await;
        Ok(serde_json::json!({"ok": true, "room_id": room_id, "cycle": cycle}))
    }
}

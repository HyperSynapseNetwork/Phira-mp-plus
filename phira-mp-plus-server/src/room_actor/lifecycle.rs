//! Room lifecycle trait — abstraction over Room broadcast and Server event
//! dispatch.
//!
//! Replaces the pattern of passing separate `(&PlusServerState, &Arc<Room>)`
//! to handler functions.  Handler functions now take `&dyn RoomLifecycle`,
//! which provides a unified interface for broadcasts, plugin events, runtime
//! events, room updates, and user access.
//!
//! ## Usage
//!
//! ```ignore
//! use super::lifecycle::{RoomLifecycle, DefaultRoomLifecycle};
//!
//! let state: &PlusServerState = &*server_arc;
//! let room = Arc::clone(&self.room);
//! let lc = DefaultRoomLifecycle::new(room, state);
//! handler::do_something(&lc, &mut as_).await;
//! ```

use async_trait::async_trait;
use crate::event_bus::MpEvent;
use crate::plugin::PluginEvent;
use crate::room::Room;
use crate::session::User;
use crate::server::state::PlusServerState;
use phira_mp_common::{Message, PartialRoomData, RoomEvent, RoomId, ServerCommand};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// Lifecycle abstraction for room command handlers.
///
/// All methods take `&self` so the lifecycle can be held behind a shared
/// reference while the actor state is mutated separately.
#[async_trait]
pub trait RoomLifecycle: Send + Sync {
    /// Get a reference to the underlying Room broadcast bus.
    fn room(&self) -> &Arc<Room>;

    /// Get a reference to the server state.
    fn server_state(&self) -> &PlusServerState;

    /// PMP44 P0-M: 获取 `PlusServerState` 的 owned `Arc`，用于把插件事件
    /// 派发拆到 Actor reply 之后的后台任务（response-after）。
    fn server_state_arc(&self) -> Arc<PlusServerState>;

    // ── Broadcast / send ──────────────────────────────────────────

    /// Broadcast a `ServerCommand` to all users and monitors.
    async fn broadcast(&self, cmd: ServerCommand);

    /// Send a `Message` to all users and monitors.
    async fn send_msg(&self, msg: Message);

    // ── Room updates ─────────────────────────────────────────────

    /// Publish a `PartialRoomData` update to the monitoring infrastructure.
    async fn publish_update(&self, data: PartialRoomData);

    // ── Plugin events ────────────────────────────────────────────

    /// Dispatch a plugin event.
    async fn dispatch_plugin_event(&self, event: PluginEvent);

    // ── Server events ────────────────────────────────────────────

    /// Publish a runtime event.
    fn publish_runtime_event(&self, event: MpEvent) -> usize;

    /// Publish a room event.
    async fn publish_room_event(&self, event: RoomEvent);

    // ── User access ─────────────────────────────────────────────

    /// Get all connected users in the room.
    async fn users(&self) -> Vec<Arc<User>>;

    /// Get all connected monitors in the room.
    async fn monitors(&self) -> Vec<Arc<User>>;

    // ── User lifecycle ──────────────────────────────────────────

    /// Handle a user leaving the room. Returns `true` if the room should be
    /// dropped.
    async fn on_user_leave(&self, user: &User) -> bool;

    /// Remove the room from the server's room registry.
    async fn remove_room(&self, room_id: &RoomId);

    /// Reset in-game timers for all users.
    async fn reset_game_time(&self);
}

/// Default implementation of [`RoomLifecycle`] that wraps a `Room` broadcast
/// bus and a `PlusServerState` Arc (PMP44 P0-M: 持有 owned `Arc`，这样插件
/// 事件可以克隆出独立引用放到 Actor reply 之后的后台任务里执行)。
pub struct DefaultRoomLifecycle {
    room: Arc<Room>,
    state: Arc<PlusServerState>,
}

impl DefaultRoomLifecycle {
    pub fn new(room: Arc<Room>, state: Arc<PlusServerState>) -> Self {
        Self { room, state }
    }
}

#[async_trait]
impl RoomLifecycle for DefaultRoomLifecycle {
    fn room(&self) -> &Arc<Room> {
        &self.room
    }

    fn server_state(&self) -> &PlusServerState {
        &*self.state
    }

    fn server_state_arc(&self) -> Arc<PlusServerState> {
        Arc::clone(&self.state)
    }

    async fn broadcast(&self, cmd: ServerCommand) {
        self.room.broadcast(cmd).await;
    }

    async fn send_msg(&self, msg: Message) {
        self.room.send(msg).await;
    }

    async fn publish_update(&self, data: PartialRoomData) {
        self.room.publish_update(data).await;
    }

    async fn dispatch_plugin_event(&self, event: PluginEvent) {
        self.state.dispatch_plugin_event(event).await;
    }

    fn publish_runtime_event(&self, event: MpEvent) -> usize {
        self.state.publish_runtime_event(event)
    }

    async fn publish_room_event(&self, event: RoomEvent) {
        self.state.publish_room_event(event).await;
    }

    async fn users(&self) -> Vec<Arc<User>> {
        self.room.users().await
    }

    async fn monitors(&self) -> Vec<Arc<User>> {
        self.room.monitors().await
    }

    async fn on_user_leave(&self, user: &User) -> bool {
        self.room.on_user_leave(user).await
    }

    async fn remove_room(&self, room_id: &RoomId) {
        self.state.rooms.write().await.remove(room_id);
    }

    async fn reset_game_time(&self) {
        for user in self.users().await {
            user.game_time
                .store(f32::NEG_INFINITY.to_bits(), Ordering::Relaxed);
        }
    }
}

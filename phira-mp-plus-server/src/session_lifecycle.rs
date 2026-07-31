use crate::l10n::Language;
use crate::server::PlusServerState;
use crate::session::Session;
use anyhow::{anyhow, Result};
use fluent::FluentArgs;
use phira_mp_common::{RoomEvent, ServerCommand, UserInfo};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tokio::time;
use tracing::{debug, warn};
use uuid::Uuid;

pub struct User {
    pub id: i32,
    pub name: String,
    pub lang: Language,

    pub server: Arc<PlusServerState>,
    pub auth_token: RwLock<Option<String>>,
    pub session: RwLock<Option<Weak<Session>>>,
    pub room: RwLock<Option<Arc<super::room::Room>>>,

    pub monitor: AtomicBool,
    pub game_time: AtomicU32,

    pub dangle_mark: Mutex<Option<Arc<()>>>,
    pub admin_cli_pending: Mutex<Option<String>>,
    /// 用户确认加入进行中游戏的房间 ID（第一次请求时设置，第二次直接加入）。
    pub join_pending_game: RwLock<Option<String>>,
}

impl User {
    pub fn new(
        id: i32,
        name: String,
        lang: Language,
        server: Arc<PlusServerState>,
        auth_token: Option<String>,
    ) -> Self {
        Self {
            id,
            name,
            lang,

            server,
            auth_token: RwLock::new(auth_token),
            session: RwLock::default(),
            room: RwLock::default(),

            monitor: AtomicBool::default(),
            game_time: AtomicU32::default(),

            dangle_mark: Mutex::default(),
            admin_cli_pending: Mutex::default(),
            join_pending_game: RwLock::default(),
        }
    }

    /// Current connection session id, if the session reference is still alive.
    /// Returns an empty string when the session has already been dropped —
    /// callers treat that as "match any session for this instance" (fallback).
    pub async fn current_session_id(&self) -> String {
        self.session
            .read()
            .await
            .as_ref()
            .and_then(|weak| weak.upgrade())
            .map(|s| s.id.to_string())
            .unwrap_or_default()
    }

    pub fn to_info(&self) -> UserInfo {
        UserInfo {
            id: self.id,
            name: self.name.clone(),
            monitor: self.monitor.load(Ordering::Relaxed),
        }
    }

    /// Send a localized system message (Message::Chat { user: 0 }) to this user.
    /// Translates `key` with `args` into the user's language.
    pub async fn send_system_msg(&self, key: &str, args: &FluentArgs<'_>) {
        let content = crate::l10n::translate_system(&self.lang, key, args);
        self.try_send(ServerCommand::Message(phira_mp_common::Message::Chat {
            user: 0,
            content,
        }))
        .await;
    }

    /// Send a localized system message with no args.
    pub async fn send_system_msg_simple(&self, key: &str) {
        let args = FluentArgs::new();
        self.send_system_msg(key, &args).await;
    }

    pub async fn set_session(&self, session: Weak<Session>) {
        *self.session.write().await = Some(session);
        *self.dangle_mark.lock().await = None;
    }

    pub async fn set_auth_token(&self, token: Option<String>) {
        *self.auth_token.write().await = token;
    }

    pub async fn auth_token(&self) -> Option<String> {
        self.auth_token.read().await.clone()
    }

    pub fn auth_token_sync(&self) -> Option<String> {
        self.auth_token
            .try_read()
            .ok()
            .and_then(|token| token.clone())
    }

    pub async fn try_send(&self, cmd: ServerCommand) {
        if let Some(session) = self.session.read().await.as_ref().and_then(Weak::upgrade) {
            session.try_send(cmd).await;
        } else {
            warn!("sending {:?} to dangling user {}", cmd, self.id);
        }
    }

    /// Send a command to this user's session, waiting for capacity (async).
    /// Returns an error if there is no session or the send queue is closed.
    pub async fn send(&self, cmd: ServerCommand) -> Result<()> {
        match self.session.read().await.as_ref().and_then(Weak::upgrade) {
            Some(session) => session.send(cmd).await,
            None => Err(anyhow!("no session for user {}", self.id)),
        }
    }

    pub async fn dangle(self: Arc<Self>, disconnected_session_id: Uuid) {
        warn!(user = self.id, session = %disconnected_session_id, "user dangling");

        // Normal-user registration and disconnect finalization share one gate.
        // This prevents a reconnect from racing an offline transition.
        let registration_guard = if self.id >= 0 {
            Some(self.server.user_registration_gate.lock().await)
        } else {
            None
        };
        let is_current_session = self
            .session
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|session| session.id == disconnected_session_id);
        if !is_current_session {
            debug!(
                user = self.id,
                session = %disconnected_session_id,
                "ignoring stale disconnect after transport replacement"
            );
            return;
        }

        let room = self.room.read().await.as_ref().map(Arc::clone);

        // Monitor sessions are transient and never enter the player lifecycle.
        if self.id < 0 {
            if let Some(room) = room {
                if room.on_user_leave(&self).await {
                    self.server.rooms.write().await.remove(&room.id);
                }
            }
            let mut users = self.server.users.write().await;
            if users
                .get(&self.id)
                .is_some_and(|current| Arc::ptr_eq(current, &self))
            {
                users.remove(&self.id);
            }
            drop(users);
            let mut monitors = self.server.game_monitors.write().await;
            if monitors
                .get(&self.id)
                .and_then(Weak::upgrade)
                .is_some_and(|session| Arc::ptr_eq(&session.user, &self))
            {
                monitors.remove(&self.id);
            }
            return;
        }

        if let Some(room) = room.as_ref() {
            let is_playing = room.server.upgrade()
                .and_then(|s| s.room_snapshot(&room.id.to_string()))
                .map(|snap| matches!(snap.stripped, phira_mp_common::StrippedRoomState::Playing))
                .unwrap_or(false);
            if is_playing {
                let grace_secs = self.server.config.idle.playing_reconnect_grace_secs;
                if grace_secs > 0 {
                    warn!(
                        user = self.id,
                        grace_secs,
                        "lost connection while playing; reconnect grace started"
                    );
                    // Playing reconnect grace: keep room membership, use playing-specific timer.
                    let dangle_mark = Arc::new(());
                    *self.dangle_mark.lock().await = Some(Arc::clone(&dangle_mark));
                    drop(registration_guard);

                    self.server
                        .publish_user_disconnected(self.id, self.name.clone())
                        .await;

                    let weak_self = Arc::downgrade(&self);
                    crate::supervisor_actor::spawn_named(
                        format!("playing-grace-{}", self.id),
                        async move {
                            time::sleep(Duration::from_secs(grace_secs)).await;
                            let Some(self_) = weak_self.upgrade() else { return };
                            let registration_guard = self_.server.user_registration_gate.lock().await;
                            let expired = {
                                let mut current = self_.dangle_mark.lock().await;
                                if current.as_ref().is_some_and(|mark| Arc::ptr_eq(mark, &dangle_mark)) {
                                    current.take();
                                    true
                                } else { false }
                            };
                            if !expired { return; }

                            // Grace expired — abort game, remove from room.
                            let room = self_.room.read().await.as_ref().map(Arc::clone);
                            if let Some(room) = room {
                                let room_id = room.id.clone();
                                // Abort the player's game if room still exists
                                if let Some(server) = room.server.upgrade() {
                                    let _ = server.room_commands.abort_round(
                                        &server, &room_id.to_string(), self_.id,
                                    ).await;
                                }
                                let _ = self_.server.room_commands.remove_user(
                                    &self_.server,
                                    &room_id.to_string(),
                                    self_.id,
                                ).await;
                            }
                            let mut users = self_.server.users.write().await;
                            if users.get(&self_.id).is_some_and(|current| Arc::ptr_eq(current, &self_)) {
                                users.remove(&self_.id);
                            }
                            drop(users);
                            drop(registration_guard);
                            // Use the fixed session id captured at disconnect
                            // entry — re-reading the weak ref could return a NEW
                            // session's id after a reconnect (P0-C).
                            let sid = disconnected_session_id.to_string();
                            let _ = self_.server.persistence_worker.enqueue(
                                crate::persistence::message::PersistenceEvent::UserDisconnect {
                                    user_id: self_.id,
                                    user_name: self_.name.clone(),
                                    server_instance_id: crate::server_instance::current().to_string(),
                                    session_id: sid.clone(),
                                },
                            ).await;
                            let _ = self_.server.persistence_worker.enqueue(
                                crate::persistence::message::PersistenceEvent::UserOffline {
                                    user_id: self_.id,
                                    server_instance_id: crate::server_instance::current().to_string(),
                                    session_id: sid,
                                },
                            ).await;
                        },
                    );
                    return;
                } else {
                    warn!(
                        user = self.id,
                        "lost connection while playing; removing immediately (grace disabled)"
                    );
                    let room_id = room.id.clone();
                    let was_monitor = self.monitor.load(Ordering::Relaxed);
                    if room.on_user_leave(&self).await {
                        self.server.rooms.write().await.remove(&room_id);
                    }
                    let mut users = self.server.users.write().await;
                    if users
                        .get(&self.id)
                        .is_some_and(|current| Arc::ptr_eq(current, &self))
                    {
                        users.remove(&self.id);
                    }
                    drop(users);
                    drop(registration_guard);

                    if !was_monitor {
                        self.server
                            .publish_room_event(RoomEvent::LeaveRoom {
                                room: room_id,
                                user: self.id,
                            })
                            .await;
                    }
                    self.server
                        .publish_user_disconnected(self.id, self.name.clone())
                        .await;
                    let sid = disconnected_session_id.to_string();
                    if let Err(e) = self
                        .server
                        .persistence_worker
                        .enqueue(
                            crate::persistence::message::PersistenceEvent::UserDisconnect {
                                user_id: self.id,
                                user_name: self.name.clone(),
                                server_instance_id: crate::server_instance::current().to_string(),
                                session_id: sid.clone(),
                            },
                        )
                        .await
                    {
                        warn!(user = self.id, kind = %e.kind(), "UserDisconnect enqueue failed");
                    }
                    if let Err(e) = self
                        .server
                        .persistence_worker
                        .enqueue(crate::persistence::message::PersistenceEvent::UserOffline {
                            user_id: self.id,
                            server_instance_id: crate::server_instance::current().to_string(),
                            session_id: sid,
                        })
                        .await
                    {
                        warn!(user = self.id, kind = %e.kind(), "UserOffline enqueue failed");
                    }
                    return;
                }
            }
        }

        let dangle_mark = Arc::new(());
        *self.dangle_mark.lock().await = Some(Arc::clone(&dangle_mark));
        drop(registration_guard);

        self.server
            .publish_user_disconnected(self.id, self.name.clone())
            .await;
        if let Err(e) = self
            .server
            .persistence_worker
            .enqueue(
                crate::persistence::message::PersistenceEvent::UserDisconnect {
                    user_id: self.id,
                    user_name: self.name.clone(),
                    server_instance_id: crate::server_instance::current().to_string(),
                    session_id: disconnected_session_id.to_string(),
                },
            )
            .await
        {
            warn!(user = self.id, kind = %e.kind(), "UserDisconnect enqueue failed after dangle");
        }

        // The grace closure uses the fixed `disconnected_session_id` param
        // captured at disconnect entry, so a stale offline event can never
        // match a NEWER session after a reconnect (P0-C).
        let weak_self = Arc::downgrade(&self);
        let grace_secs = self.server.config.idle.dangle_grace_secs.max(5);
        crate::supervisor_actor::spawn_named(format!("dangle-grace-{}", self.id), async move {
            time::sleep(Duration::from_secs(grace_secs)).await;
            let Some(self_) = weak_self.upgrade() else {
                return;
            };
            let registration_guard = self_.server.user_registration_gate.lock().await;
            let expired = {
                let mut current = self_.dangle_mark.lock().await;
                if current
                    .as_ref()
                    .is_some_and(|mark| Arc::ptr_eq(mark, &dangle_mark))
                {
                    current.take();
                    true
                } else {
                    false
                }
            };
            if !expired {
                return;
            }

            let room = self_.room.read().await.as_ref().map(Arc::clone);
            let mut room_leave_event = None;
            if let Some(room) = room {
                let room_id = room.id.clone();
                let was_monitor = self_.monitor.load(Ordering::Relaxed);
                let _ = self_.server.room_commands.remove_user(
                    &self_.server,
                    &room_id.to_string(),
                    self_.id,
                ).await;
                if !was_monitor {
                    room_leave_event = Some(RoomEvent::LeaveRoom {
                        room: room_id,
                        user: self_.id,
                    });
                }
            }

            let mut users = self_.server.users.write().await;
            if users
                .get(&self_.id)
                .is_some_and(|current| Arc::ptr_eq(current, &self_))
            {
                users.remove(&self_.id);
            }
            drop(users);
            drop(registration_guard);

            if let Some(event) = room_leave_event {
                self_.server.publish_room_event(event).await;
            }
            // Use the session id captured at disconnect entry (fixed), not a
            // re-read of the (possibly-dead) weak ref.
            if let Err(e) = self_
                .server
                .persistence_worker
                .enqueue(crate::persistence::message::PersistenceEvent::UserOffline {
                    user_id: self_.id,
                    server_instance_id: crate::server_instance::current().to_string(),
                    session_id: disconnected_session_id.to_string(),
                })
                .await
            {
                warn!(user = self_.id, kind = %e.kind(), "UserOffline enqueue failed in dangle grace");
            }
        });
    }
}

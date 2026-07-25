use crate::l10n::Language;
use crate::server::PlusServerState;
use crate::session::Session;
use phira_mp_common::{RoomEvent, ServerCommand, UserInfo};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};
use tokio::time;
use tracing::{debug, error, info, warn};
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

    pub fn to_info(&self) -> UserInfo {
        UserInfo {
            id: self.id,
            name: self.name.clone(),
            monitor: self.monitor.load(Ordering::Relaxed),
        }
    }

    pub async fn can_monitor(&self) -> bool {
        self.server
            .live_config
            .read()
            .await
            .monitors
            .contains(&self.id)
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
            let playing = room.server.upgrade()
                .and_then(|s| s.room_snapshot(&room.id.to_string()))
                .map(|snap| matches!(snap.stripped, phira_mp_common::StrippedRoomState::Playing))
                .unwrap_or(false);
            if playing {
                warn!(
                    user = self.id,
                    "lost connection while playing; removing immediately"
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
                crate::internal_hooks::playtime_disconnect(self.id);
                let _ = self
                    .server
                    .persistence_worker
                    .enqueue(
                        crate::persistence::message::PersistenceEvent::UserDisconnect {
                            user_id: self.id,
                            user_name: self.name.clone(),
                        },
                    )
                    .await;
                let _ = self
                    .server
                    .persistence_worker
                    .enqueue(crate::persistence::message::PersistenceEvent::UserOffline {
                        user_id: self.id,
                    })
                    .await;
                return;
            }
        }

        let dangle_mark = Arc::new(());
        *self.dangle_mark.lock().await = Some(Arc::clone(&dangle_mark));
        drop(registration_guard);

        self.server
            .publish_user_disconnected(self.id, self.name.clone())
            .await;
        let _ = self
            .server
            .persistence_worker
            .enqueue(
                crate::persistence::message::PersistenceEvent::UserDisconnect {
                    user_id: self.id,
                    user_name: self.name.clone(),
                },
            )
            .await;

        let weak_self = Arc::downgrade(&self);
        crate::supervisor_actor::spawn_named(format!("dangle-grace-{}", self.id), async move {
            time::sleep(Duration::from_secs(10)).await;
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
                if room.on_user_leave(&self_).await {
                    self_.server.rooms.write().await.remove(&room_id);
                }
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
            crate::internal_hooks::playtime_disconnect(self_.id);
            let _ = self_
                .server
                .persistence_worker
                .enqueue(crate::persistence::message::PersistenceEvent::UserOffline {
                    user_id: self_.id,
                })
                .await;
        });
    }
}

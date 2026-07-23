//! User disconnection and kick methods.
//!
//! Extracted from orig.rs.

use phira_mp_common::{RoomEvent, ServerCommand};
use serde_json::Value;
use std::sync::Arc;
use tracing::{info, warn};

use super::state::PlusServerState;

impl PlusServerState {
    /// If the banned user is currently online, deliver the localized reason before
    /// closing the session. Returning `true` means an active session was found.
    pub async fn disconnect_banned_user(&self, user_id: i32, reason: &str) -> bool {
        let target = {
            let sessions = self.sessions.read().await;
            sessions
                .iter()
                .find(|(_, session)| session.user.id == user_id)
                .map(|(session_id, session)| (*session_id, Arc::clone(session)))
        };
        let Some((session_id, session)) = target else {
            return false;
        };

        let language = session.user.lang.0.to_string();
        let message = crate::session_auth::ban_rejection_message(&language, reason);
        if let Err(err) = session
            .stream
            .send_and_flush(ServerCommand::Authenticate(Err(message)))
            .await
        {
            warn!(
                user = user_id,
                ?err,
                "failed to deliver ban reason before disconnect"
            );
        }

        session.stream.close();
        if let Err(err) = self.lost_con_tx.send(session_id).await {
            warn!(user = user_id, ?err, "failed to disconnect banned user");
        }
        true
    }
}

/// 从房间踢出用户。
pub(crate) async fn run_admin_kick_user(
    state: &PlusServerState,
    target_id: i32,
    reason: &str,
) -> Result<Value, String> {
    // Serialize against authentication/reconnect finalization so the old
    // transport cannot race with a replacement and reinsert stale presence.
    let registration_guard = state.user_registration_gate.lock().await;
    let user = state
        .users
        .read()
        .await
        .get(&target_id)
        .map(Arc::clone)
        .ok_or("user not found")?;

    if let Some(room) = user.room.read().await.as_ref().map(Arc::clone) {
        let room_id = room.id.to_string();
        let room_key = room.id.clone();
        let was_monitor = user.monitor.load(std::sync::atomic::Ordering::SeqCst);
        if room.on_user_leave(&user).await {
            state.rooms.write().await.remove(&room_key);
        }
        if !was_monitor {
            state
                .publish_room_event(RoomEvent::LeaveRoom {
                    room: room_key,
                    user: target_id,
                })
                .await;
        }
        state
            .dispatch_plugin_event(crate::plugin::PluginEvent::RoomLeave {
                user_id: target_id,
                room_id,
            })
            .await;
    }

    let target_session = {
        let mut sessions = state.sessions.write().await;
        let session_id = sessions
            .iter()
            .find(|(_, session)| session.user.id == target_id)
            .map(|(id, _)| *id);
        session_id.and_then(|id| sessions.remove(&id))
    };

    // Make the eventual transport-lost notification stale before closing.
    *user.session.write().await = None;
    let mut users = state.users.write().await;
    if users
        .get(&target_id)
        .is_some_and(|current| Arc::ptr_eq(current, &user))
    {
        users.remove(&target_id);
    }
    drop(users);
    drop(registration_guard);

    if let Some(session) = target_session {
        let message = ServerCommand::Message(phira_mp_common::Message::Chat {
            user: 0,
            content: format!("你已被管理员踢出服务器: {reason}"),
        });
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            session.stream.send_and_flush(message),
        )
        .await;
        session.stream.close();
    }

    info!(user = target_id, reason = %reason, "kicked from server by admin");
    state
        .publish_user_disconnected(target_id, user.name.clone())
        .await;
    crate::internal_hooks::playtime_disconnect(target_id);
    let _ = state
        .persistence_worker
        .enqueue(
            crate::persistence::message::PersistenceEvent::UserDisconnect {
                user_id: target_id,
                user_name: user.name.clone(),
            },
        )
        .await;
    let _ = state
        .persistence_worker
        .enqueue(crate::persistence::message::PersistenceEvent::UserOffline {
            user_id: target_id,
        })
        .await;

    Ok(serde_json::json!({"ok": true, "reason": reason}))
}

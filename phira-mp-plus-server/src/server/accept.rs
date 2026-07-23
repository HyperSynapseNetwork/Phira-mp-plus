//! PlusServer::accept() — TCP listener accept loop.
//!
//! Extracted from orig.rs.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::{trace, warn};
use uuid::Uuid;

use super::state::PlusServer;

impl PlusServer {
    /// Accept a TCP connection and hand authentication to a bounded task.
    ///
    /// The listener path intentionally performs no protocol reads: one slow or
    /// malicious unauthenticated client must not block subsequent accepts.
    pub async fn accept(&self) -> std::result::Result<(), anyhow::Error> {
        if self.state.shutting_down.load(Ordering::Acquire) {
            return Ok(());
        }
        let (stream, addr) = self.listener.accept().await?;
        if self.state.shutting_down.load(Ordering::Acquire) {
            return Ok(());
        }
        let ip = addr.ip().to_string();

        if !self.state.connection_limiter.check(&ip).await {
            return Ok(());
        }

        let session_permit = match Arc::clone(&self.state.session_gate).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                warn!(%ip, "connection rejected: session capacity reached");
                return Ok(());
            }
        };

        let permit = match Arc::clone(&self.state.pre_auth_gate).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                warn!(%ip, "connection rejected: pre-authentication capacity reached");
                return Ok(());
            }
        };

        self.state.idle_monitor.mark_activity();
        let id = Uuid::new_v4();
        let auth_timeout = self.state.config.idle.auth_timeout_secs.max(5);
        let state = Arc::clone(&self.state);
        crate::supervisor_actor::spawn_named(format!("pre-auth-{id}"), async move {
            let _permit = permit;
            let session = match tokio::time::timeout(
                std::time::Duration::from_secs(auth_timeout),
                crate::session::Session::new(id, addr, stream, Arc::clone(&state), session_permit),
            )
            .await
            {
                Ok(Ok(session)) => session,
                Ok(Err(err)) => {
                    warn!(%ip, ?err, "failed to create session");
                    return;
                }
                Err(_) => {
                    warn!(%ip, "session creation timed out");
                    return;
                }
            };

            // Authentication may complete while the main task has already begun
            // shutdown. Never publish a late session into the authoritative map.
            if state.shutting_down.load(Ordering::Acquire) {
                *session.user.session.write().await = None;
                session.stream.close();
                if session.user.id >= 0 {
                    let mut users = state.users.write().await;
                    if users
                        .get(&session.user.id)
                        .is_some_and(|current| Arc::ptr_eq(current, &session.user))
                    {
                        users.remove(&session.user.id);
                    }
                    drop(users);
                    state
                        .publish_user_disconnected(session.user.id, session.user.name.clone())
                        .await;
                    let _ = state
                        .persistence_worker
                        .enqueue(
                            crate::persistence::message::PersistenceEvent::UserDisconnect {
                                user_id: session.user.id,
                                user_name: session.user.name.clone(),
                            },
                        )
                        .await;
                    let _ = state
                        .persistence_worker
                        .enqueue(crate::persistence::message::PersistenceEvent::UserOffline {
                            user_id: session.user.id,
                        })
                        .await;
                    crate::internal_hooks::playtime_disconnect(session.user.id);
                }
                return;
            }

            // The session-capacity permit was reserved before authentication
            // and is now owned by Session, so insertion cannot overrun the limit.
            state.sessions.write().await.insert(id, session);
            trace!(%ip, %id, "connection accepted");
        });

        Ok(())
    }
}

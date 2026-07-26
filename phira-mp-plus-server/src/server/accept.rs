//! PlusServer::accept() — TCP listener accept loop.

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
        let ip = addr.ip();
        let ip_str = ip.to_string();
        if !self.state.connection_limiter.check(&ip_str).await {
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

        let id = Uuid::new_v4();
        let auth_timeout = self.state.config.idle.auth_timeout_secs.max(5);
        let state = Arc::clone(&self.state);

        // Check whether the connection originates from a CIDR range that
        // is configured for PROXY protocol.
        let proxy_enabled = state
            .config
            .proxy_allow_cidr
            .as_ref()
            .map_or(false, |cidr| {
                crate::server::proxy_protocol::ip_matches_any_cidr(&addr.ip(), cidr)
            });

        crate::supervisor_actor::spawn_named(format!("pre-auth-{id}"), async move {
            let _permit = permit;

            // ── PROXY protocol header parsing ─────────────────────
            // If the peer is from a trusted PROXY source, attempt to
            // read the PROXY header and substitute the real client
            // address.  Non-PROXY connections from the same CIDR are
            // rejected (the admin has explicitly opted in).
            let (stream, addr) = if proxy_enabled {
                let (s, proxy_addr) =
                    crate::server::proxy_protocol::maybe_read_proxy_header(stream).await;
                match proxy_addr {
                    Some(forwarded) => {
                        trace!(
                            peer = %ip,
                            forwarded = %forwarded.ip(),
                            "PROXY protocol forwarded connection"
                        );
                        (s, forwarded)
                    }
                    None => {
                        warn!(
                            %ip,
                            "PROXY source without valid PROXY header; closing connection"
                        );
                        return;
                    }
                }
            } else {
                (stream, addr)
            };

            // ── IP ban check (uses real client IP from PROXY if applicable) ──
            if state.ban_manager.is_ip_banned(&addr.ip()).await {
                trace!(ip = %addr.ip(), "connection rejected: IP banned");
                return;
            }

            let session = match tokio::time::timeout(
                std::time::Duration::from_secs(auth_timeout),
                crate::session::Session::new(
                    id,
                    addr,
                    stream,
                    Arc::clone(&state),
                    session_permit,
                ),
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

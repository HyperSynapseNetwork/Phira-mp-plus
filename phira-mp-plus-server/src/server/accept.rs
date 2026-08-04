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
        // Use dedicated proxy rate limiter for trusted proxy peers to avoid
        // throttling HAProxy/LB traffic before the forwarded client IP is known.
        let is_trusted_proxy = self.state.config.proxy_allow_cidr.as_ref().map_or(false, |cidr| {
            crate::server::proxy_protocol::ip_matches_any_cidr(&ip, cidr)
        });
        let limiter = if is_trusted_proxy {
            &self.state.proxy_connection_limiter
        } else {
            &self.state.connection_limiter
        };
        if !limiter.check(&ip_str).await {
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

        crate::supervisor_actor::spawn_named(format!("pre-auth-{id}"), async move {
            let _permit = permit;

            // ── PROXY protocol header parsing ─────────────────────
            // `maybe_read_proxy_header` handles the CIDR check internally.
            // If the peer is in `proxy_allow_cidr` it attempts PROXY header
            // parsing via a non-consuming peek; untrusted peers are returned
            // immediately with the stream untouched.
            //
            // Dual rate‑limiting: the trusted proxy peer IP was checked above
            // against the dedicated proxy_connection_limiter (higher limit);
            // the forwarded client IP is checked here against the normal
            // connection_limiter so both are independently rate‑limited.
            const PROXY_HDR_TIMEOUT: std::time::Duration =
                std::time::Duration::from_secs(3);
            const PROXY_MAX_HDR: usize = 16384;

            let cidr = state.config.proxy_allow_cidr.as_deref();
            let (stream, addr) = match crate::server::proxy_protocol::maybe_read_proxy_header(
                stream,
                cidr,
                PROXY_HDR_TIMEOUT,
                PROXY_MAX_HDR,
            )
            .await
            {
                Ok((s, Some(forwarded))) => {
                    // Rate‑limit the forwarded client IP (PROXY protocol
                    // semantics).  This is the IP that subsequent rate‑limit
                    // and ban checks should use.
                    let fwd_ip = forwarded.ip();
                    if !state
                        .connection_limiter
                        .check(&fwd_ip.to_string())
                        .await
                    {
                        trace!(
                            peer = %ip,
                            forwarded = %fwd_ip,
                            "connection rejected: forwarded IP rate‑limited"
                        );
                        return;
                    }

                    trace!(
                        peer = %ip,
                        forwarded = %fwd_ip,
                        "PROXY protocol forwarded connection"
                    );
                    (s, forwarded)
                }
                Ok((s, None)) => {
                    // Either the peer is not in the trusted CIDR, or the
                    // peeked data did not match v1/v2 and the stream is
                    // untouched — proceed with the original peer address.
                    (s, addr)
                }
                Err(e) => {
                    warn!(%ip, "PROXY protocol error: {e}; closing connection");
                    return;
                }
            };

            // ── IP ban check moved to the auth path ───────────────────────
            // The accept layer no longer silently drops IP-banned connections:
            // closing the TCP stream without a response makes the official
            // client wait until timeout with no explanation. The check now runs
            // in the session `Authenticate` path, which sends
            // `Authenticate(Err(reason))` so the client can display the ban
            // reason. The real client IP (PROXY-resolved above) is preserved on
            // `addr`, which is passed into `Session::new` for that check.

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
                session.user.clear_session().await;
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
                    state.note_user_offline().await;
                    state
                        .publish_user_disconnected(session.user.id, session.user.name.clone())
                        .await;
                    if let Err(e) = state
                        .persistence_worker
                        .enqueue(
                            crate::persistence::message::PersistenceEvent::UserDisconnect {
                                user_id: session.user.id,
                                user_name: session.user.name.clone(),
                                server_instance_id: crate::server_instance::current().to_string(),
                                session_id: session.id.to_string(),
                                occurred_at: crate::db::now_ms(),
                            },
                        )
                        .await
                    {
                        warn!(user = session.user.id, kind = %e.kind(), "UserDisconnect enqueue failed during accept cleanup");
                    }
                    if let Err(e) = state
                        .persistence_worker
                        .enqueue(crate::persistence::message::PersistenceEvent::UserOffline {
                            user_id: session.user.id,
                            server_instance_id: crate::server_instance::current().to_string(),
                            session_id: session.id.to_string(),
                            occurred_at: crate::db::now_ms(),
                        })
                        .await
                    {
                        warn!(user = session.user.id, kind = %e.kind(), "UserOffline enqueue failed during accept cleanup");
                    }
                }
                return;
            }

            // ── Active 发布屏障（PMP45 P0-D）──────────────────────────
            // AuthenticationOutcome::Accepted 在 WAL/flush/gate 之前就已发出，
            // 所以 `Session::new` 返回时认证可能仍在推进。绝不把半认证 Session
            // 发布进全局 sessions 表：等待认证回调 `mark_active()`（Authenticate
            // (Ok) 已 flush 且 gate 已激活），超时则放弃发布并关闭传输。
            //
            // 等待上限取认证绝对预算（auth_deadline_ms）加 2s 余量：认证回调
            // 必须在 auth_deadline 内达到 Active 或失败，因此该上限既覆盖整个
            // 认证窗口，又避免以整个 auth_timeout 挂住容量 permit。
            let active_wait_timeout = std::time::Duration::from_millis(
                state.config.compatibility.auth_deadline_ms + 2000,
            );
            if session.active.get().is_none() {
                // notified() 在检查之后创建也是安全的：`mark_active` 用
                // `notify_one`（无等待者时存储一个 permit），不会丢失唤醒。
                // future 作用域被限制在本块内，使 `session` 的借用先于后续
                // `insert(id, session)` 结束。
                let active_wait = session.active_notify.notified();
                match tokio::time::timeout(active_wait_timeout, active_wait).await {
                    Ok(()) => {}
                    Err(_) => {
                        // 认证未能在预算内达到 Active——不发布；传输已由认证
                        // 回滚路径或此处关闭，Session 的 Drop 会释放容量 permit。
                        crate::official_client_compat::protocol_trace::ProtocolTrace::get()
                            .provisional_sessions
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        warn!(%ip, %id, "session never reached Active; not publishing");
                        session.user.clear_session_if_matches(
                            session.id,
                            session.bound_generation.get().copied().unwrap_or(0),
                        )
                        .await;
                        session.stream.close();
                        let _ = state.lost_con_tx.try_send(id);
                        return;
                    }
                }
            }

            // The session-capacity permit was reserved before authentication
            // and is now owned by Session, so insertion cannot overrun the limit.
            state.sessions.write().await.insert(id, session);
            trace!(%ip, %id, "connection accepted");
        });

        Ok(())
    }
}

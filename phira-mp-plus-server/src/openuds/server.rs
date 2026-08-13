//! UDS listener and connection management for OpenUDS.
//!
//! - Listens on a Unix Domain Socket path
//! - Accepts connections, spawns per-connection session tasks
//! - Manages connected session registry
//! - Cleans up the socket file on shutdown

use crate::openuds::auth::{build_auth_error_response, build_authenticated_response};
use crate::openuds::protocol;
use crate::openuds::session::{self as openuds_session, Session};
use crate::server::config::OpenUdsConfig;
use crate::server::PlusServerState;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Start the OpenUDS server. Spawns the accept loop and returns immediately.
///
/// Called from `PlusServer::new()` when `config.openuds.enabled` is true.
pub async fn start(state: Arc<PlusServerState>, config: &OpenUdsConfig) {
    let socket_path = &config.socket_path;
    let max_connections = config.max_connections.max(1) as usize;
    let send_buffer_size = config.event_buffer_size.max(64) as usize;
    let heartbeat_interval = std::time::Duration::from_secs(
        config.heartbeat_interval_secs.max(5),
    );
    let use_token_mode = !config.auth_token.is_empty();
    let auth_token = config.auth_token.clone();
    let sock_path = socket_path.clone();

    // Remove old socket file if it exists
    let _ = tokio::fs::remove_file(&sock_path).await;

    // Create the UDS listener
    let listener = match UnixListener::bind(&sock_path) {
        Ok(l) => {
            tracing::info!("OpenUDS listening on {}", sock_path);
            l
        }
        Err(e) => {
            tracing::error!("OpenUDS: failed to bind {}: {}", sock_path, e);
            return;
        }
    };

    // Set permissions to 660 (owner+group read/write)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o660)).await;
    }

    // Shared session registry
    let sessions: Arc<RwLock<HashMap<Uuid, Arc<Session>>>> =
        Arc::new(RwLock::new(HashMap::new()));

    // Auth state
    let auth_state = Arc::new(crate::openuds::auth::AuthState::new(auth_token));

    // Start event dispatcher
    let event_dispatcher = crate::openuds::events::EventDispatcher::new(
        &state.event_bus,
        Arc::clone(&sessions),
    );
    crate::supervisor_actor::spawn_named("openuds-events", async move {
        event_dispatcher.run().await;
    });

    // Start stream manager (for touch/judge/log streams).  Held alive by the
    // log broker task below (and by any future delivery call sites).
    let stream_manager = Arc::new(crate::openuds::streams::StreamManager::new(
        Arc::clone(&sessions),
    ));
    // 注册全局引用，供生产 touches/judges 投递（session_telemetry 热路径）。
    crate::openuds::set_stream_manager(Arc::clone(&stream_manager));

    // Log-stream broker: forwards formatted server log lines from the tracing
    // layer to sessions subscribed to "logs".
    if let Some(log_rx) = crate::logging::take_openuds_log_rx() {
        let log_stream_manager = Arc::clone(&stream_manager);
        crate::supervisor_actor::spawn_named("openuds-logs", async move {
            let mut rx = log_rx;
            while let Some(line) = rx.recv().await {
                log_stream_manager.deliver_logs(line).await;
            }
        });
    }

    // Start heartbeat
    let heartbeat_sessions = Arc::clone(&sessions);
    let heartbeat_state = Arc::clone(&state);
    crate::supervisor_actor::spawn_named("openuds-heartbeat", async move {
        loop {
            tokio::time::sleep(heartbeat_interval).await;
            deliver_heartbeat(&heartbeat_sessions, &heartbeat_state).await;
        }
    });

    // Accept loop
    crate::supervisor_actor::spawn_named("openuds-accept", async move {
        loop {
            // Check if we've reached the max connection limit
            {
                let session_count = sessions.read().await.len();
                if session_count >= max_connections {
                    match listener.accept().await {
                        Ok((mut stream, _)) => {
                            let reject = build_auth_error_response(
                                "max connections reached",
                            );
                            let _ = protocol::write_frame_async(&mut stream, &reject).await;
                        }
                        Err(e) => {
                            tracing::warn!("OpenUDS accept error (full): {e}");
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            }

            match listener.accept().await {
                Ok((stream, _addr)) => {
                    let (reader, writer) = stream.into_split();
                    let session_state = Arc::clone(&sessions);
                    let state_clone = Arc::clone(&state);
                    let auth_state_clone = Arc::clone(&auth_state);

                    crate::supervisor_actor::spawn_named(
                        "openuds-session",
                        handle_session(
                            reader,
                            writer,
                            session_state,
                            state_clone,
                            auth_state_clone,
                            send_buffer_size,
                            use_token_mode,
                        ),
                    );
                }
                Err(e) => {
                    tracing::warn!("OpenUDS accept error: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    });
}

/// Handle a single UDS client connection.
async fn handle_session(
    mut read_half: tokio::net::unix::OwnedReadHalf,
    write_half: tokio::net::unix::OwnedWriteHalf,
    sessions: Arc<RwLock<HashMap<Uuid, Arc<Session>>>>,
    state: Arc<PlusServerState>,
    auth_state: Arc<crate::openuds::auth::AuthState>,
    send_buffer_size: usize,
    use_token_mode: bool,
) {
    // Create session with write half
    let (session, rx) = Session::new(send_buffer_size);
    let session = Arc::new(session);
    let session_id = session.id;

    // Register session
    sessions.write().await.insert(session_id, Arc::clone(&session));

    // Spawn writer task using the write half
    crate::supervisor_actor::spawn_named(
        format!("openuds-writer-{session_id}"),
        openuds_session::session_writer(write_half, rx),
    );

    let read_result: Result<(), String> = async {
        loop {
            let frame = protocol::read_frame_async(&mut read_half)
                .await
                .map_err(|e| format!("read error: {e}"))?;

            // Check authentication
            if !session.is_authenticated() {
                handle_auth_frame(
                    &session,
                    &frame,
                    &auth_state,
                    use_token_mode,
                )
                .await?;
                continue;
            }

            // Handle authenticated commands
            handle_command_frame(
                &session,
                &frame,
                &state,
            )
            .await;
        }
    }
    .await;

    // Session ended — cleanup
    if let Err(e) = &read_result {
        tracing::debug!("OpenUDS session {session_id} ended: {e}");
    }
    sessions.write().await.remove(&session_id);
}

/// Handle an authentication frame from a pending session.
async fn handle_auth_frame(
    session: &Session,
    frame: &Value,
    auth_state: &Arc<crate::openuds::auth::AuthState>,
    use_token_mode: bool,
) -> Result<(), String> {
    let msg_type = frame
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing type field".to_string())?;

    if msg_type != "authenticate" {
        let err = build_auth_error_response("authentication required");
        let _ = session.send(err).await;
        return Err("authentication required".to_string());
    }

    if use_token_mode {
        // Token mode
        let token = frame
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing token field".to_string())?;

        if !auth_state.validate_token(token) {
            let err = build_auth_error_response("invalid token");
            let _ = session.send(err).await;
            return Err("invalid token".to_string());
        }

        // Authenticated!
        session.set_authenticated();
        let session_id = session.id.to_string();
        let resp = build_authenticated_response(&session_id, env!("CARGO_PKG_VERSION"));
        let _ = session.send(resp).await;
        Ok(())
    } else {
        // No token configured: the Unix socket's filesystem permissions
        // (mode 660) already isolate access, so authenticate directly.
        session.set_authenticated();
        let session_id = session.id.to_string();
        let resp = build_authenticated_response(&session_id, env!("CARGO_PKG_VERSION"));
        let _ = session.send(resp).await;
        Ok(())
    }
}

/// Handle an authenticated command frame.
async fn handle_command_frame(
    session: &Session,
    frame: &Value,
    state: &Arc<PlusServerState>,
) {
    let msg_type = frame
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("");

    match msg_type {
        "command" | "request" | "" => {
            let command = frame
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("");
            let params = frame.get("params").unwrap_or(&Value::Null);
            let req_id = frame.get("id").and_then(Value::as_str);

            if command.is_empty() {
                let err = Session::error_response(req_id, "MISSING_COMMAND", "command field required");
                let _ = session.send(err).await;
                return;
            }

            let response = crate::openuds::dispatch::dispatch_command(
                session,
                command,
                params,
                req_id,
                state,
            )
            .await;

            let _ = session.send(response).await;
        }
        "subscribe" => {
            let event_types: Vec<String> = frame
                .get("event_types")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            session.add_subscriptions(&event_types);

            let resp = serde_json::json!({
                "type": "subscribed",
                "active": event_types,
            });
            let _ = session.send(resp).await;
        }
        "unsubscribe" => {
            let event_types: Vec<String> = frame
                .get("event_types")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            session.remove_subscriptions(&event_types);

            let resp = serde_json::json!({
                "type": "unsubscribed",
                "removed": event_types,
            });
            let _ = session.send(resp).await;
        }
        "subscribe_stream" => {
            let stream = frame
                .get("stream")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();

            session.add_stream_subscriptions(std::slice::from_ref(&stream));

            let resp = serde_json::json!({
                "type": "stream_subscribed",
                "stream": stream,
            });
            let _ = session.send(resp).await;
        }
        "ping" => {
            let resp = serde_json::json!({"type": "pong"});
            let _ = session.send(resp).await;
        }
        _ => {
            let err = Session::error_response(None, "UNKNOWN_TYPE", &format!("unknown message type: {msg_type}"));
            let _ = session.send(err).await;
        }
    }
}

/// Deliver a heartbeat event to all authenticated sessions.
async fn deliver_heartbeat(
    sessions: &Arc<RwLock<HashMap<Uuid, Arc<Session>>>>,
    state: &PlusServerState,
) {
    let user_count = state.users.read().await.values().filter(|u| u.id > 0).count();
    let room_count = state.rooms.read().await.len();
    let session_count = state.sessions.read().await.len();

    let data = serde_json::json!({
        "users": user_count,
        "rooms": room_count,
        "sessions": session_count,
    });

    let event = Session::event_response("server.heartbeat", data);
    let sessions_guard = sessions.read().await;
    for (_id, session) in sessions_guard.iter() {
        if session.is_authenticated() {
            let _ = session.send(event.clone()).await;
        }
    }
}

/// Clean up the UDS socket file on shutdown.
pub async fn cleanup(socket_path: &str) {
    let _ = tokio::fs::remove_file(socket_path).await;
}

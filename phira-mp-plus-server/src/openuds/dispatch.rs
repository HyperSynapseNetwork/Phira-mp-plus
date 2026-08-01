//! Command dispatch for the OpenUDS API.
//!
//! Receives JSON commands from authenticated sessions and routes them to
//! existing PMP handlers. All commands return a standardized response:
//!
//! ```json
//! {"type":"response","id":"req-uuid","ok":true,"data":{...}}
//! {"type":"response","id":"req-uuid","ok":false,"error":{"code":"ERR_CODE","message":"..."}}
//! ```

use crate::openuds::session::Session;
use crate::server::PlusServerState;
use serde_json::Value;
use std::sync::Arc;
use phira_mp_common::RoomId;

/// Dispatch a parsed command to the appropriate handler.
///
/// `session` is the authenticated session making the request.
/// `command` is the command name (e.g., "room.create", "player.ban").
/// `params` is the parsed parameters object.
/// `req_id` is the optional request ID for correlating responses.
/// `state` is the PMP server state.
pub async fn dispatch_command(
    _session: &Session,
    command: &str,
    params: &Value,
    req_id: Option<&str>,
    state: &Arc<PlusServerState>,
) -> Value {
    let result = match command {
        // ── Room commands ──────────────────────────────────────────
        "room.create" => cmd_room_create(state, params).await,
        "room.close" => cmd_room_close(state, params).await,
        "room.start" => cmd_room_start(state, params).await,
        "room.cancel_start" => cmd_room_cancel_start(state, params).await,
        "room.ready" => cmd_room_ready(state, params).await,
        "room.lock" => cmd_room_lock(state, params).await,
        "room.cycle" => cmd_room_cycle(state, params).await,
        "room.set_host" => cmd_room_set_host(state, params).await,
        "room.kick" => cmd_room_kick(state, params).await,
        "room.force_move" => cmd_room_force_move(state, params).await,
        "room.info" => cmd_room_info(state, params).await,
        "room.list" => cmd_room_list(state, params).await,

        // ── Player commands ────────────────────────────────────────
        "player.ban" => cmd_player_ban(state, params).await,
        "player.unban" => cmd_player_unban(state, params).await,
        "player.banlist" => cmd_player_banlist(state).await,
        "player.ban_ip" => cmd_player_ban_ip(state, params).await,
        "player.unban_ip" => cmd_player_unban_ip(state, params).await,
        "player.ip_history" => cmd_player_ip_history(state, params).await,
        "player.info" => cmd_player_info(state, params).await,
        "player.kick" => cmd_player_kick(state, params).await,

        // ── Server commands ────────────────────────────────────────
        "server.stats" => cmd_server_stats(state).await,
        "server.status" => cmd_server_status(state).await,
        "server.config_reload" => cmd_server_config_reload(state).await,
        "server.shutdown" => cmd_server_shutdown(state).await,
        "server.roomcreation" => cmd_server_roomcreation(state, params).await,

        // ── Broadcast commands ─────────────────────────────────────
        "broadcast.all" => cmd_broadcast_all(state, params).await,
        "broadcast.room" => cmd_broadcast_room(state, params).await,
        "broadcast.user" => cmd_broadcast_user(state, params).await,

        // ── Plugin commands ────────────────────────────────────────
        "plugin.list" => cmd_plugin_list(state).await,
        "plugin.enable" => cmd_plugin_enable(state, params).await,
        "plugin.disable" => cmd_plugin_disable(state, params).await,
        "plugin.reload" => cmd_plugin_reload(state).await,
        "plugin.info" => cmd_plugin_info(state, params).await,
        "plugin.remove" => cmd_plugin_remove(state, params).await,
        "plugin.call" => cmd_plugin_call(state, params).await,

        // ── Runtime commands ───────────────────────────────────────
        "runtime.status" => cmd_runtime_status(state).await,
        "runtime.actors" => cmd_runtime_actors(state).await,
        "runtime.persistence" => cmd_runtime_persistence(state).await,
        "runtime.phira" => cmd_runtime_phira(state).await,

        // ── Subscription commands (handled by session) ────────────
        "subscribe" | "unsubscribe" | "subscribe_stream" => {
            // These are handled in the session reader loop, not here.
            Ok(Value::Null)
        }

        _ => {
            return Session::error_response(
                req_id,
                "UNKNOWN_COMMAND",
                &format!("unknown command: {command}"),
            );
        }
    };

    match result {
        Ok(data) => Session::success_response(req_id, data),
        Err(e) => Session::error_response(req_id, "COMMAND_ERROR", &e),
    }
}

// ════════════════════════════════════════════════════════════════════
// Room Commands
// ════════════════════════════════════════════════════════════════════

async fn cmd_room_create(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params
        .get("room_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "room_id required".to_string())?;
    let endpoint = params.get("endpoint").and_then(Value::as_str);
    let persistent_empty = params
        .get("persistent_empty")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    state
        .create_empty_room(room_id, endpoint.map(String::from), persistent_empty)
        .await
}

async fn cmd_room_close(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params
        .get("room_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "room_id required".to_string())?;
    state.room_commands.close_room(state, room_id).await
}

async fn cmd_room_start(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params
        .get("room_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "room_id required".to_string())?;
    state.room_commands.start_room(state, room_id).await
}

async fn cmd_room_cancel_start(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params
        .get("room_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "room_id required".to_string())?;
    state.room_commands.cancel_start(state, room_id).await
}

async fn cmd_room_ready(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params
        .get("room_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "room_id required".to_string())?;
    let user_id = params
        .get("user_id")
        .and_then(Value::as_i64)
        .map(|v| v as i32)
        .ok_or_else(|| "user_id required".to_string())?;
    let admin_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    state.room_commands.set_ready(state, room_id, user_id, admin_deadline, None).await
}

async fn cmd_room_lock(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params
        .get("room_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "room_id required".to_string())?;
    let locked = params
        .get("locked")
        .and_then(Value::as_bool)
        .ok_or_else(|| "locked (bool) required".to_string())?;
    state.room_commands.set_lock(state, room_id, locked).await
}

async fn cmd_room_cycle(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params
        .get("room_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "room_id required".to_string())?;
    let cycle = params
        .get("cycle")
        .and_then(Value::as_bool)
        .ok_or_else(|| "cycle (bool) required".to_string())?;
    state.room_commands.set_cycle(state, room_id, cycle).await
}

async fn cmd_room_set_host(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params
        .get("room_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "room_id required".to_string())?;
    let host_id = params.get("host_id").and_then(Value::as_i64).map(|v| v as i32);
    state.room_commands.set_host(state, room_id, host_id).await
}

async fn cmd_room_kick(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params
        .get("room_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "room_id required".to_string())?;
    let user_id = params
        .get("user_id")
        .and_then(Value::as_i64)
        .map(|v| v as i32)
        .ok_or_else(|| "user_id required".to_string())?;
    state.room_commands.kick_user(state, room_id, user_id).await
}

async fn cmd_room_force_move(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params
        .get("room_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "room_id required".to_string())?;
    let user_id = params
        .get("user_id")
        .and_then(Value::as_i64)
        .map(|v| v as i32)
        .ok_or_else(|| "user_id required".to_string())?;
    let monitor = params.get("monitor").and_then(Value::as_bool).unwrap_or(false);
    state
        .force_move_user_to_room(room_id, user_id, monitor)
        .await
}

async fn cmd_room_info(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params
        .get("room_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "room_id required".to_string())?;
    let rid: RoomId = room_id
        .to_string()
        .try_into()
        .map_err(|_| "invalid room_id".to_string())?;

    let room = {
        let rooms = state.rooms.read().await;
        rooms
            .get(&rid)
            .map(Arc::clone)
            .ok_or_else(|| "room not found".to_string())?
    };

    let control = room.control_snapshot();
    let actor_snap = state.room_snapshot(&rid.to_string());
    let (users_data, monitors_data) = {
        let users: Vec<_> = room
            .users()
            .await
            .into_iter()
            .map(|u| {
                serde_json::json!({
                    "id": u.id,
                    "name": u.name,
                    "online": true,
                })
            })
            .collect();
        let monitors: Vec<_> = room
            .monitors()
            .await
            .into_iter()
            .map(|u| {
                serde_json::json!({
                    "id": u.id,
                    "name": u.name,
                })
            })
            .collect();
        (users, monitors)
    };

    let chart_info = actor_snap.as_ref().and_then(|s| s.chart);
    let state_str = actor_snap
        .as_ref()
        .map(|s| format!("{:?}", s.stripped))
        .unwrap_or_else(|| "unknown".to_string());

    Ok(serde_json::json!({
        "room_id": rid.to_string(),
        "uuid": room.uuid.to_string(),
        "created_at": room.created_at,
        "locked": control.locked,
        "cycle": control.cycle,
        "hidden": control.hidden,
        "live": room.is_live(),
        "persistent_empty": control.persistent_empty,
        "host_id": control.host_id,
        "system_host": control.system_host,
        "max_users": control.max_users,
        "chart_id": chart_info,
        "state": state_str,
        "phira_api_endpoint_override": control.phira_api_endpoint,
        "users": users_data,
        "monitors": monitors_data,
        "player_count": users_data.len(),
        "monitor_count": monitors_data.len(),
    }))
}

async fn cmd_room_list(
    state: &Arc<PlusServerState>,
    _params: &Value,
) -> Result<Value, String> {
    // Collect room references first, then query each outside the lock
    let rooms: Vec<Arc<crate::room::Room>> = {
        let rooms_guard = state.rooms.read().await;
        rooms_guard.values().map(Arc::clone).collect()
    };

    let mut list = Vec::with_capacity(rooms.len());
    for room in &rooms {
        let control = room.control_snapshot();
        let user_count = room.users().await.len();
        let monitor_count = room.monitors().await.len();
        list.push(serde_json::json!({
            "room_id": room.id.to_string(),
            "uuid": room.uuid.to_string(),
            "locked": control.locked,
            "cycle": control.cycle,
            "hidden": control.hidden,
            "player_count": user_count,
            "monitor_count": monitor_count,
            "persistent_empty": control.persistent_empty,
            "live": room.is_live(),
        }));
    }
    Ok(serde_json::json!({"rooms": list, "total": list.len()}))
}

// ════════════════════════════════════════════════════════════════════
// Player Commands
// ════════════════════════════════════════════════════════════════════

async fn cmd_player_ban(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let user_id = params
        .get("user_id")
        .and_then(Value::as_i64)
        .map(|v| v as i32)
        .ok_or_else(|| "user_id required".to_string())?;
    let reason = params
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("banned via OpenUDS");
    let reason_str = state.ban_manager.ban_user(user_id, reason).await?;
    let _ = state.disconnect_banned_user(user_id, &reason_str).await;
    Ok(serde_json::json!({"user_id": user_id, "reason": reason_str}))
}

async fn cmd_player_unban(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let user_id = params
        .get("user_id")
        .and_then(Value::as_i64)
        .map(|v| v as i32)
        .ok_or_else(|| "user_id required".to_string())?;
    state.ban_manager.unban_user(user_id).await?;
    Ok(serde_json::json!({"user_id": user_id}))
}

async fn cmd_player_banlist(
    state: &Arc<PlusServerState>,
) -> Result<Value, String> {
    let bans = state.ban_manager.list_banned().await;
    Ok(serde_json::json!({"bans": bans}))
}

async fn cmd_player_ban_ip(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let target = params
        .get("target")
        .and_then(Value::as_str)
        .ok_or_else(|| "target (user_id or IP) required".to_string())?;
    let reason = params
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("banned via OpenUDS");

    // Try parsing as IP address first
    if let Ok(ip) = target.parse::<std::net::IpAddr>() {
        state.ban_manager.ban_ip(ip, reason).await?;
        Ok(serde_json::json!({"ip": target, "reason": reason}))
    } else if let Ok(user_id) = target.parse::<i32>() {
        // Treat as user_id, ban all their IPs
        let db = &state.db_manager;
        let crate::db::DbManager::Pg(pool) = db;
        let count = state.ban_manager.ban_user_ips(user_id, reason, pool).await;
        Ok(serde_json::json!({"user_id": user_id, "banned_ips": count, "reason": reason}))
    } else {
        Err("target must be a valid IP address or user ID".to_string())
    }
}

async fn cmd_player_unban_ip(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let ip_str = params
        .get("ip")
        .and_then(Value::as_str)
        .ok_or_else(|| "ip required".to_string())?;
    let ip: std::net::IpAddr = ip_str
        .parse()
        .map_err(|_| format!("invalid IP address: {ip_str}"))?;
    state.ban_manager.unban_ip(ip).await?;
    Ok(serde_json::json!({"ip": ip_str}))
}

async fn cmd_player_ip_history(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let user_id = params
        .get("user_id")
        .and_then(Value::as_i64)
        .map(|v| v as i32)
        .ok_or_else(|| "user_id required".to_string())?;
    let db = &state.db_manager;
    let crate::db::DbManager::Pg(pool) = db;
    let history = state.ban_manager.user_ip_history(user_id, pool).await;
    Ok(serde_json::json!({"user_id": user_id, "ip_history": history}))
}

async fn cmd_player_info(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let user_id = params
        .get("user_id")
        .and_then(Value::as_i64)
        .map(|v| v as i32)
        .ok_or_else(|| "user_id required".to_string())?;

    let users = state.users.read().await;
    let user = users.get(&user_id).map(Arc::clone);
    drop(users);

    match user {
        Some(user) => {
            let room_id = user.room.read().await.as_ref().map(|r| r.id.to_string());
            let is_banned = state.ban_manager.is_banned(user_id).await;
            Ok(serde_json::json!({
                "user_id": user_id,
                "name": user.name,
                "online": true,
                "room_id": room_id,
                "banned": is_banned,
            }))
        }
        None => {
            let is_banned = state.ban_manager.is_banned(user_id).await;
            Ok(serde_json::json!({
                "user_id": user_id,
                "online": false,
                "banned": is_banned,
            }))
        }
    }
}

async fn cmd_player_kick(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let user_id = params
        .get("user_id")
        .and_then(Value::as_i64)
        .map(|v| v as i32)
        .ok_or_else(|| "user_id required".to_string())?;
    let reason = params
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or("kicked via OpenUDS");
    crate::server::run_admin_kick_user(state, user_id, reason).await
}

// ════════════════════════════════════════════════════════════════════
// Server Commands
// ════════════════════════════════════════════════════════════════════

async fn cmd_server_stats(
    state: &Arc<PlusServerState>,
) -> Result<Value, String> {
    let user_count = state
        .users
        .read()
        .await
        .values()
        .filter(|user| user.id > 0)
        .count();
    let room_count = state.rooms.read().await.len();
    let session_count = state.sessions.read().await.len();
    let plugin_count = state.plugin_manager.list_plugins().await.len();

    Ok(serde_json::json!({
        "users_online": user_count,
        "active_rooms": room_count,
        "active_sessions": session_count,
        "loaded_plugins": plugin_count,
        "port": state.config.port,
        "http_port": state.config.http_port,
        "uptime_secs": 0, // TODO: track server start time
        "server_name": state.config.server_name,
    }))
}

async fn cmd_server_status(
    state: &Arc<PlusServerState>,
) -> Result<Value, String> {
    Ok(serde_json::json!({
        "server": "Phira-mp+",
        "version": env!("CARGO_PKG_VERSION"),
        "running": !state.shutting_down.load(std::sync::atomic::Ordering::Acquire),
        "port": state.config.port,
        "http_port": state.config.http_port,
        "server_name": state.config.server_name,
    }))
}

async fn cmd_server_config_reload(
    state: &Arc<PlusServerState>,
) -> Result<Value, String> {
    // Reload YAML config from the original config path (same logic as CLI `config reload`)
    let path = std::path::Path::new(&state.config.config_path);
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("读取配置文件失败: {e}"))?;
    let mut config: crate::server::PlusConfig = serde_yaml::from_str(&content)
        .map_err(|e| format!("解析配置文件失败: {e}"))?;
    config.config_path = state.config.config_path.clone();
    if let Some(monitors) = state.config.cli_monitors_override.clone() {
        config.monitors = monitors.clone();
        config.cli_monitors_override = Some(monitors);
    }
    config.normalize().map_err(|e| format!("配置规范化失败: {e}"))?;
    config.validate().map_err(|e| format!("配置校验失败: {e}"))?;

    let admin_update = if !config.admin_phira_ids.is_empty() {
        Some(
            config
                .admin_phira_ids
                .iter()
                .copied()
                .filter(|id| *id > 0)
                .collect::<std::collections::HashSet<_>>(),
        )
    } else {
        None
    };

    let live = crate::server::LiveConfig::from_full(&config);
    *state.live_config.write().await = live;
    if let Some(ids) = admin_update {
        *state.admin_ids.write().await = ids;
    }

    Ok(serde_json::json!({"ok": true, "message": "config reloaded"}))
}

async fn cmd_server_shutdown(
    state: &Arc<PlusServerState>,
) -> Result<Value, String> {
    state.shutting_down.store(true, std::sync::atomic::Ordering::Release);
    state.shutdown.notify_one();
    Ok(serde_json::json!({"ok": true, "message": "shutdown signal sent"}))
}

async fn cmd_server_roomcreation(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let enabled = params
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| "enabled (bool) required".to_string())?;
    let mut live = state.live_config.write().await;
    live.room_creation_enabled = enabled;
    Ok(serde_json::json!({"room_creation_enabled": enabled}))
}

// ════════════════════════════════════════════════════════════════════
// Broadcast Commands
// ════════════════════════════════════════════════════════════════════

async fn cmd_broadcast_all(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let message = params
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| "message required".to_string())?;
    let sent = state.broadcast_system_message(message).await;
    Ok(serde_json::json!({"sent": sent}))
}

async fn cmd_broadcast_room(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params
        .get("room_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "room_id required".to_string())?;
    let message = params
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| "message required".to_string())?;
    let rid: RoomId = room_id
        .to_string()
        .try_into()
        .map_err(|_| "invalid room_id".to_string())?;

    let room = {
        let rooms = state.rooms.read().await;
        rooms
            .get(&rid)
            .map(Arc::clone)
            .ok_or_else(|| "room not found".to_string())?
    };

    room.send(phira_mp_common::Message::Chat {
        user: 0,
        content: message.to_string(),
    })
    .await;

    Ok(serde_json::json!({"ok": true, "room_id": room_id}))
}

async fn cmd_broadcast_user(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let user_id = params
        .get("user_id")
        .and_then(Value::as_i64)
        .map(|v| v as i32)
        .ok_or_else(|| "user_id required".to_string())?;
    let message = params
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| "message required".to_string())?;

    let user = {
        let users = state.users.read().await;
        users.get(&user_id).map(Arc::clone)
    };

    match user {
        Some(user) => {
            user.try_send(phira_mp_common::ServerCommand::Message(
                phira_mp_common::Message::Chat {
                    user: 0,
                    content: message.to_string(),
                },
            ))
            .await;
            Ok(serde_json::json!({"ok": true, "user_id": user_id}))
        }
        None => Err("user not found".to_string()),
    }
}

// ════════════════════════════════════════════════════════════════════
// Plugin Commands
// ════════════════════════════════════════════════════════════════════

async fn cmd_plugin_list(
    state: &Arc<PlusServerState>,
) -> Result<Value, String> {
    let plugins = state.plugin_manager.list_plugins().await;
    let list: Vec<Value> = plugins
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.info.name,
                "version": p.info.version,
                "author": p.info.author,
                "description": p.info.description,
                "enabled": p.enabled,
                "state": format!("{:?}", p.state),
            })
        })
        .collect();
    Ok(serde_json::json!({"plugins": list, "total": list.len()}))
}

async fn cmd_plugin_enable(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "name required".to_string())?;
    state.plugin_manager.enable_plugin(name).await?;
    Ok(serde_json::json!({"name": name}))
}

async fn cmd_plugin_disable(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "name required".to_string())?;
    state.plugin_manager.disable_plugin(name).await?;
    Ok(serde_json::json!({"name": name}))
}

async fn cmd_plugin_reload(
    state: &Arc<PlusServerState>,
) -> Result<Value, String> {
    let count = state.plugin_manager.reload_plugins().await?;
    Ok(serde_json::json!({"loaded": count}))
}

async fn cmd_plugin_info(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "name required".to_string())?;
    let plugins = state.plugin_manager.list_plugins().await;
    let plugin = plugins
        .iter()
        .find(|p| p.info.name == name)
        .ok_or_else(|| format!("plugin '{name}' not found"))?;
    Ok(serde_json::json!({
        "name": plugin.info.name,
        "version": plugin.info.version,
        "author": plugin.info.author,
        "description": plugin.info.description,
        "enabled": plugin.enabled,
        "state": format!("{:?}", plugin.state),
        "path": plugin.path,
    }))
}

async fn cmd_plugin_remove(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "name required".to_string())?;
    state.plugin_manager.remove_plugin(name).await?;
    Ok(serde_json::json!({"name": name}))
}

async fn cmd_plugin_call(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "name required".to_string())?;
    let method = params
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| "method required".to_string())?;
    let args = params
        .get("args")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    state.plugin_manager.call_plugin_api(name, method, args).await
}

// ════════════════════════════════════════════════════════════════════
// Runtime Commands
// ════════════════════════════════════════════════════════════════════

async fn cmd_runtime_status(
    state: &Arc<PlusServerState>,
) -> Result<Value, String> {
    let persistence = state.persistence_worker.stats().await;
    let events = state
        .event_bus
        .stats(crate::runtime_diagnostics::BENCHMARK_REPORT_RECENT_DEFAULT);
    let commands = state.command_registry.iter().count();
    let room_commands = state.room_commands.stats();
    let phira_http = state.phira_client.stats();
    Ok(serde_json::json!({
        "persistence_worker": persistence,
        "event_bus": events,
        "registered_commands": commands,
        "room_command_gateway": room_commands,
        "phira_http": phira_http,
    }))
}

async fn cmd_runtime_actors(
    state: &Arc<PlusServerState>,
) -> Result<Value, String> {
    let stats = state.room_commands.stats();
    Ok(serde_json::json!({
        "room_command_gateway": stats,
    }))
}

async fn cmd_runtime_persistence(
    state: &Arc<PlusServerState>,
) -> Result<Value, String> {
    let stats = state.persistence_worker.stats().await;
    Ok(serde_json::to_value(stats).unwrap_or_default())
}

async fn cmd_runtime_phira(
    state: &Arc<PlusServerState>,
) -> Result<Value, String> {
    let stats = state.phira_client.stats();
    Ok(serde_json::to_value(stats).unwrap_or_default())
}

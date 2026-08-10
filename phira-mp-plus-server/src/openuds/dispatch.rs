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
    // 记录 OpenUDS 输入到历史（供面板 `logs.input` 查询；只记方法名，避免泄露参数）。
    crate::history::record_input("openuds", command);
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
        "room.set_tournament" => cmd_room_set_tournament(state, params).await,
        "room.set_live" => cmd_room_set_live(state, params).await,
        "room.set_chart" => cmd_room_set_chart(state, params).await,
        "room.kick" => cmd_room_kick(state, params).await,
        "room.force_move" => cmd_room_force_move(state, params).await,
        "room.info" => cmd_room_info(state, params).await,
        "room.list" => cmd_room_list(state, params).await,
        "room.history" => cmd_room_history(state, params).await,
        "room.chat_history" => cmd_room_chat_history(state, params).await,
        "room.uuid" => cmd_room_uuid(state, params).await,
        "room.rounds" => cmd_room_rounds(state, params).await,
        "room.round" => cmd_room_round(state, params).await,
        "room.set_hidden" => cmd_room_set_hidden(state, params).await,
        "room.set_persistent" => cmd_room_set_persistent(state, params).await,
        "room.set_degraded" => cmd_room_set_degraded(state, params).await,
        "room.set_api_endpoint" => cmd_room_set_api_endpoint(state, params).await,
        "room.ban" => cmd_room_ban(state, params).await,
        "room.unban" => cmd_room_unban(state, params).await,
        "room.banlist" => cmd_room_banlist(state, params).await,
        "room.whitelist" => cmd_room_whitelist(state, params).await,
        "room.whitelist_add" => cmd_room_whitelist_add(state, params).await,
        "room.whitelist_remove" => cmd_room_whitelist_remove(state, params).await,

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
        "cli.execute" => cmd_cli_execute(state, params).await,

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

        // ── 历史查询 ──────────────────────────────────────────────
        "logs.history" => cmd_logs_history(state, params).await,
        "logs.input" => cmd_logs_input(state, params).await,

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

async fn cmd_room_set_tournament(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params
        .get("room_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "room_id required".to_string())?;
    let tournament = params
        .get("tournament")
        .and_then(Value::as_bool)
        .ok_or_else(|| "tournament (bool) required".to_string())?;
    state
        .room_commands
        .set_tournament(state, room_id, tournament)
        .await
}

async fn cmd_room_set_live(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params
        .get("room_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "room_id required".to_string())?;
    let live = params
        .get("live")
        .and_then(Value::as_bool)
        .ok_or_else(|| "live (bool) required".to_string())?;
    state.room_commands.set_live(state, room_id, live).await
}

async fn cmd_room_set_chart(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params
        .get("room_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "room_id required".to_string())?;
    let chart_id = params
        .get("chart_id")
        .and_then(Value::as_i64)
        .map(|v| v as i32)
        .ok_or_else(|| "chart_id (int) required".to_string())?;
    // chart_name 可选；缺省时从 Phira API 拉取（与 CLI room set chart-id 一致）。
    let chart_name = match params.get("chart_name").and_then(Value::as_str) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => {
            let fetched = state
                .phira_client
                .get_json::<crate::server::Chart>(
                    &state.config.phira_api_endpoint,
                    None,
                    &format!("/chart/{chart_id}"),
                    None,
                    crate::phira_client::PhiraRetryNoticeTarget::Silent,
                    None,
                )
                .await;
            match fetched {
                Ok(chart) => chart.name,
                Err(_) => format!("chart_{chart_id}"),
            }
        }
    };
    state
        .room_commands
        .set_chart(state, room_id, chart_id, &chart_name, 0, None, None)
        .await
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
        "tournament": control.tournament,
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

async fn cmd_room_history(
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

    Ok(serde_json::json!({
        "room_id": rid.to_string(),
        "rounds": crate::server::snapshot::room_history_json(&room),
    }))
}

/// `room.chat_history` — 读取房间聊天历史缓存（最近 `chat_history_limit` 条 Chat 消息）。
async fn cmd_room_chat_history(
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

    let history = room.chat_history.read().await;
    let messages: Vec<Value> = history
        .iter()
        .filter_map(|msg| match msg {
            phira_mp_common::Message::Chat { user, content } => Some(serde_json::json!({
                "user": user,
                "content": content,
            })),
            _ => None,
        })
        .collect();

    Ok(serde_json::json!({
        "room_id": rid.to_string(),
        "messages": messages,
        "count": messages.len(),
    }))
}

/// 解析 `room_id` 并克隆房间 Arc。
async fn resolve_room(state: &Arc<PlusServerState>, room_id: &str) -> Result<Arc<crate::room::Room>, String> {
    let rid: RoomId = room_id.to_string().try_into().map_err(|_| "invalid room_id".to_string())?;
    let rooms = state.rooms.read().await;
    rooms.get(&rid).map(Arc::clone).ok_or_else(|| "room not found".to_string())
}

/// `room.uuid` — 房间 UUID。
async fn cmd_room_uuid(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params.get("room_id").and_then(Value::as_str).ok_or_else(|| "room_id required".to_string())?;
    let room = resolve_room(state, room_id).await?;
    Ok(serde_json::json!({"room_id": room.id.to_string(), "uuid": room.uuid.to_string()}))
}

/// `room.rounds` — 房间轮次列表（含轮次 UUID）。
async fn cmd_room_rounds(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params.get("room_id").and_then(Value::as_str).ok_or_else(|| "room_id required".to_string())?;
    let room = resolve_room(state, room_id).await?;
    let rounds: Vec<Value> = room.play_history.recent_sync().iter().map(|r| serde_json::json!({
        "round_id": r.round_id.to_string(),
        "chart_id": r.chart_id,
        "chart_name": r.chart_name,
    })).collect();
    Ok(serde_json::json!({"room_id": room.id.to_string(), "rounds": rounds}))
}

/// `room.round` — 按轮次 UUID 查单轮详情。
async fn cmd_room_round(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params.get("room_id").and_then(Value::as_str).ok_or_else(|| "room_id required".to_string())?;
    let round_id = params.get("round_id").and_then(Value::as_str).ok_or_else(|| "round_id required".to_string())?;
    let room = resolve_room(state, room_id).await?;
    let round = room
        .play_history
        .recent_sync()
        .into_iter()
        .find(|r| r.round_id.to_string() == round_id)
        .ok_or_else(|| format!("round {round_id} not found"))?;
    let results: Vec<Value> = round.results.iter().map(|res| serde_json::json!({
        "player": res.user_id,
        "user_name": res.user_name,
        "score": res.score,
        "accuracy": res.accuracy,
        "perfect": res.perfect, "good": res.good, "bad": res.bad, "miss": res.miss,
        "max_combo": res.max_combo,
        "full_combo": res.full_combo,
        "aborted": res.aborted,
    })).collect();
    Ok(serde_json::json!({
        "room_id": room.id.to_string(),
        "round_id": round.round_id.to_string(),
        "chart_id": round.chart_id,
        "chart_name": round.chart_name,
        "results": results,
    }))
}

/// `room.set_hidden` — 隐藏/公开房间。
async fn cmd_room_set_hidden(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params.get("room_id").and_then(Value::as_str).ok_or_else(|| "room_id required".to_string())?;
    let hidden = params.get("hidden").and_then(Value::as_bool).ok_or_else(|| "hidden (bool) required".to_string())?;
    state.room_commands.set_hidden(state, room_id, hidden).await
}

/// `room.set_persistent` — 持久空房间开关。
async fn cmd_room_set_persistent(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params.get("room_id").and_then(Value::as_str).ok_or_else(|| "room_id required".to_string())?;
    let persistent = params.get("persistent").and_then(Value::as_bool).ok_or_else(|| "persistent (bool) required".to_string())?;
    state.room_commands.set_persistent_empty(state, room_id, persistent).await
}

/// `room.set_degraded` — 清除房间持久化降级标志。
async fn cmd_room_set_degraded(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params.get("room_id").and_then(Value::as_str).ok_or_else(|| "room_id required".to_string())?;
    let degraded = params.get("degraded").and_then(Value::as_bool).ok_or_else(|| "degraded (bool) required".to_string())?;
    state.room_commands.set_degraded(state, room_id, degraded).await
}

/// `room.set_api_endpoint` — 设置房间 Phira API 端点。
async fn cmd_room_set_api_endpoint(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params.get("room_id").and_then(Value::as_str).ok_or_else(|| "room_id required".to_string())?;
    let endpoint = params.get("endpoint").and_then(Value::as_str).map(|s| s.to_string());
    state.room_commands.set_phira_api_endpoint(state, room_id, endpoint).await
}

/// `room.ban` — 房间黑名单封禁（按房间名解析 UUID）。
async fn cmd_room_ban(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params.get("room_id").and_then(Value::as_str).ok_or_else(|| "room_id required".to_string())?;
    let user_id = params.get("user_id").and_then(Value::as_i64).map(|v| v as i32).ok_or_else(|| "user_id required".to_string())?;
    let reason = params.get("reason").and_then(Value::as_str).unwrap_or("").to_string();
    let room = resolve_room(state, room_id).await?;
    state.ban_manager.room_ban_user(&room.uuid.to_string(), user_id, &reason).await?;
    Ok(serde_json::json!({"ok": true, "room_id": room_id, "user_id": user_id}))
}

/// `room.unban` — 房间黑名单解封。
async fn cmd_room_unban(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params.get("room_id").and_then(Value::as_str).ok_or_else(|| "room_id required".to_string())?;
    let user_id = params.get("user_id").and_then(Value::as_i64).map(|v| v as i32).ok_or_else(|| "user_id required".to_string())?;
    let room = resolve_room(state, room_id).await?;
    state.ban_manager.room_unban_user(&room.uuid.to_string(), user_id).await?;
    Ok(serde_json::json!({"ok": true, "room_id": room_id, "user_id": user_id}))
}

/// `room.banlist` — 房间黑名单列表。
async fn cmd_room_banlist(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params.get("room_id").and_then(Value::as_str).ok_or_else(|| "room_id required".to_string())?;
    let room = resolve_room(state, room_id).await?;
    let bans: Vec<Value> = state.ban_manager.list_room_bans(&room.uuid.to_string()).await.iter().map(|b| serde_json::json!({
        "user_id": b.user_id, "reason": b.reason, "banned_at": b.banned_at,
    })).collect();
    Ok(serde_json::json!({"room_id": room_id, "bans": bans}))
}

/// `room.whitelist` — 房间白名单列表。
async fn cmd_room_whitelist(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params.get("room_id").and_then(Value::as_str).ok_or_else(|| "room_id required".to_string())?;
    let room = resolve_room(state, room_id).await?;
    let list = state.ban_manager.room_whitelist(&room.uuid.to_string()).await;
    Ok(serde_json::json!({"room_id": room_id, "whitelist": list}))
}

/// `room.whitelist_add` — 白名单添加用户。
async fn cmd_room_whitelist_add(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params.get("room_id").and_then(Value::as_str).ok_or_else(|| "room_id required".to_string())?;
    let user_id = params.get("user_id").and_then(Value::as_i64).map(|v| v as i32).ok_or_else(|| "user_id required".to_string())?;
    let room = resolve_room(state, room_id).await?;
    state.ban_manager.whitelist_add_user(&room.uuid.to_string(), user_id).await?;
    Ok(serde_json::json!({"ok": true, "room_id": room_id, "user_id": user_id}))
}

/// `room.whitelist_remove` — 白名单移除用户。
async fn cmd_room_whitelist_remove(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let room_id = params.get("room_id").and_then(Value::as_str).ok_or_else(|| "room_id required".to_string())?;
    let user_id = params.get("user_id").and_then(Value::as_i64).map(|v| v as i32).ok_or_else(|| "user_id required".to_string())?;
    let room = resolve_room(state, room_id).await?;
    state.ban_manager.whitelist_remove_user(&room.uuid.to_string(), user_id).await?;
    Ok(serde_json::json!({"ok": true, "room_id": room_id, "user_id": user_id}))
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

/// `cli.execute` — 执行任意管理 CLI 命令（复用管理控制台命令路径）。
/// 供 PPB / 外部工具程序化跑 CLI（如 `rooms`、`room set ...`、`config reload`）。
async fn cmd_cli_execute(
    state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let command = params
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| "command (string) required".to_string())?;
    let lines = crate::cli::execute_cli_once(Arc::clone(state), command.to_string()).await;
    Ok(serde_json::json!({ "output": lines }))
}

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
    state
        .room_creation_enabled
        .store(enabled, std::sync::atomic::Ordering::Release);
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
            user.try_send(
                phira_mp_common::ServerCommand::Message(phira_mp_common::Message::Chat {
                    user: 0,
                    content: message.to_string(),
                }),
                // 管理消息非房间状态事件，cutover 不适用。
                None,
            )
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

/// `logs.history` — 本进程运行以来的最近日志行（默认 100 条）。
async fn cmd_logs_history(
    _state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(100)
        .clamp(1, 2000);
    let lines = crate::history::recent_logs(limit);
    Ok(serde_json::json!({ "lines": lines, "count": lines.len() }))
}

/// `logs.input` — 本进程运行以来的最近管理输入（CLI/OpenUDS/管理员）。
async fn cmd_logs_input(
    _state: &Arc<PlusServerState>,
    params: &Value,
) -> Result<Value, String> {
    let limit = params
        .get("limit")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(100)
        .clamp(1, 1000);
    let entries = crate::history::recent_inputs(limit);
    Ok(serde_json::json!({ "inputs": entries, "count": entries.len() }))
}

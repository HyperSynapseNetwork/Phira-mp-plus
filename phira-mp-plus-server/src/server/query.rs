//! Server state query dispatch — the sync query engine for CLI, WIT, and Web API.
//!
//! Extracted from the original `server.rs`. These functions are synchronous —
//! they use `spawn_on_runtime` for async operations and `read_lock!` for sync reads.

use crate::server::snapshot::build_snapshot;
use crate::server::PlusServerState;
use serde_json::Value;
use std::sync::Arc;

pub(crate) fn runtime_state_query_timeout() -> std::time::Duration {
    crate::runtime_diagnostics::RUNTIME_STATE_QUERY_TIMEOUT
}

pub(crate) fn spawn_on_runtime<F>(f: F) -> Option<tokio::task::JoinHandle<()>>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        Some(handle.spawn(f))
    } else {
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build temp runtime");
            rt.block_on(f);
        });
        None
    }
}

fn parse_sse_stream_registration(args: &[Value]) -> Result<(String, String, Vec<String>), String> {
    if let Some(config) = args.first().and_then(Value::as_object) {
        let path = config
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "path required".to_string())?;
        let plugin = config
            .get("plugin")
            .and_then(Value::as_str)
            .ok_or_else(|| "plugin name required".to_string())?;
        let event_types = config
            .get("event_types")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        return Ok((path.to_string(), plugin.to_string(), event_types));
    }
    Err("sse.register_stream requires json object with path/plugin/event_types".to_string())
}

fn parse_http_route_registration(args: &[Value]) -> Result<(String, String), String> {
    if let Some(config) = args.first().and_then(Value::as_object) {
        let path = config
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| "path required".to_string())?;
        let plugin = config
            .get("plugin")
            .and_then(Value::as_str)
            .ok_or_else(|| "plugin name required".to_string())?;
        return Ok((path.to_string(), plugin.to_string()));
    }

    let path = args
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| "path required".to_string())?;
    let plugin = args
        .get(1)
        .and_then(Value::as_str)
        .ok_or_else(|| "plugin name required".to_string())?;
    Ok((path.to_string(), plugin.to_string()))
}

pub(crate) fn server_state_query_inner(
    state: &Arc<PlusServerState>,
    method: &str,
    args: &[Value],
) -> Result<Value, String> {
    match method {
        "runtime.status" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let simulation = s.simulation.status().await;
                let persistence = s.persistence_worker.stats().await;
                let events = s
                    .event_bus
                    .stats(crate::runtime_diagnostics::BENCHMARK_REPORT_RECENT_DEFAULT);
                let commands = s.command_registry.iter().count();
                let room_commands = s.room_commands.stats();
                let phira_http = s.phira_client.stats();
                let benchmark_reports = s
                    .benchmark_reports
                    .snapshot(crate::runtime_diagnostics::BENCHMARK_REPORT_RECENT_DEFAULT);
                let _ = tx.send(Ok(serde_json::json!({
                    "runtime": true,
                    "note": "Runtime hardening is active: Session and Room management commands are mailbox-only, Room control state has coherent snapshots, and failed database writes are preserved in a local dead-letter journal. Full Room state ownership and enqueue-before crash-consistent WAL remain migration items.",
                    "commands": {"registered": commands},
                    "event_bus": events, "simulation": simulation,
                    "persistence_worker": persistence, "room_command_gateway": room_commands,
                    "phira_http": phira_http, "benchmark_reports": benchmark_reports,
                })));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("runtime.status timeout".to_string()))
        }
        "simulation.status" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let status = s.simulation.status().await;
                let _ = tx.send(Ok(serde_json::to_value(status).unwrap_or_default()));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("simulation.status timeout".to_string()))
        }
        "simulation.start" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let config = serde_json::from_value(args.first().cloned().unwrap_or_default())
                .map_err(|e| format!("invalid simulation config: {e}"))?;
            spawn_on_runtime(async move {
                let status = s.simulation.start(config).await;
                let _ = tx.send(Ok(serde_json::to_value(status).unwrap_or_default()));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("simulation.start timeout".to_string()))
        }
        "simulation.stop" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let reason = args
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or("stopped via state query")
                .to_string();
            spawn_on_runtime(async move {
                let status = s.simulation.stop(&reason).await;
                let _ = tx.send(Ok(serde_json::to_value(status).unwrap_or_default()));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("simulation.stop timeout".to_string()))
        }
        "simulation.cleanup" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                s.simulation.cleanup().await;
                let _ = tx.send(Ok(serde_json::json!({"ok": true})));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("simulation.cleanup timeout".to_string()))
        }
        "benchmark.reports" => {
            let count = args
                .first()
                .and_then(|v| v.as_u64())
                .unwrap_or(crate::runtime_diagnostics::BENCHMARK_REPORT_RECENT_DEFAULT as u64)
                as usize;
            let reports = state.benchmark_reports.snapshot(count);
            serde_json::to_value(&reports).map_err(|e| format!("serialize benchmark reports: {e}"))
        }
        "benchmark.latest" => {
            let latest = state
                .benchmark_reports
                .snapshot(1)
                .recent
                .into_iter()
                .next();
            serde_json::to_value(latest)
                .map_err(|e| format!("serialize latest benchmark report: {e}"))
        }
        "benchmark.history" => {
            let max = args
                .first()
                .and_then(|v| v.as_u64())
                .map(|v| v as usize)
                .unwrap_or(usize::MAX);
            let reports = state.benchmark_reports.snapshot(max);
            serde_json::to_value(&reports).map_err(|e| format!("serialize benchmark reports: {e}"))
        }
        "rooms.history" => {
            let users = crate::read_lock!(state.rooms);
            let rooms_snapshot: Vec<Value> = users.iter().map(|(_, room)| {
                let rounds: Vec<Value> = room.play_history.recent_sync().iter().map(|r| {
                    let results: Vec<Value> = r.results.iter().map(|res| serde_json::json!({
                        "player": res.user_id, "user_name": res.user_name.clone(),
                        "score": res.score, "accuracy": res.accuracy,
                        "perfect": res.perfect, "good": res.good,
                        "bad": res.bad, "miss": res.miss,
                    })).collect();
                    serde_json::json!({"round_id": r.round_id.to_string(), "chart_id": r.chart_id, "chart_name": r.chart_name, "results": results})
                }).collect();
                serde_json::json!({"room_id": room.id.to_string(), "rounds": rounds})
            }).collect();
            Ok(serde_json::json!(rooms_snapshot))
        }
        "persist.events" => {
            let since = args.first().and_then(Value::as_i64).unwrap_or(0);
            let limit = args.get(1).and_then(Value::as_i64).unwrap_or(100);
            let kind = args.get(2).and_then(Value::as_str).map(str::to_string);
            let room_id = args.get(3).and_then(Value::as_str).map(str::to_string);
            let user_id = args
                .get(4)
                .and_then(Value::as_i64)
                .map(|value| value as i32);
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let rows = s
                    .db_manager
                    .query_events(since, limit, kind, room_id, user_id)
                    .await;
                let _ = tx.send(Ok(serde_json::json!(rows)));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("persist.events timeout".to_string()))
        }
        "persist.rooms" => {
            let since = args.first().and_then(Value::as_i64).unwrap_or(0);
            let limit = args.get(1).and_then(Value::as_i64).unwrap_or(100);
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let rows = s.db_manager.query_room_snapshots(since, limit).await;
                let _ = tx.send(Ok(serde_json::json!(rows)));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("persist.rooms timeout".to_string()))
        }
        "persist.touches" => {
            let since = args.first().and_then(Value::as_i64).unwrap_or(0);
            let limit = args.get(1).and_then(Value::as_i64).unwrap_or(100);
            let round = args.get(2).and_then(Value::as_str).map(str::to_string);
            let player = args
                .get(3)
                .and_then(Value::as_i64)
                .map(|value| value as i32);
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let rows = s
                    .db_manager
                    .query_touch_batches(since, limit, round, player)
                    .await;
                let _ = tx.send(Ok(serde_json::json!(rows)));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("persist.touches timeout".to_string()))
        }
        "persist.judges" => {
            let since = args.first().and_then(Value::as_i64).unwrap_or(0);
            let limit = args.get(1).and_then(Value::as_i64).unwrap_or(100);
            let round = args.get(2).and_then(Value::as_str).map(str::to_string);
            let player = args
                .get(3)
                .and_then(Value::as_i64)
                .map(|value| value as i32);
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let rows = s
                    .db_manager
                    .query_judge_batches(since, limit, round, player)
                    .await;
                let _ = tx.send(Ok(serde_json::json!(rows)));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("persist.judges timeout".to_string()))
        }
        "persist.playtime" => {
            let user_id = args
                .first()
                .and_then(Value::as_i64)
                .map(|value| value as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let row = s.db_manager.get_playtime(user_id).await;
                let value = row
                    .map(|row| {
                        serde_json::json!({
                            "user_id": user_id,
                            "total_secs": row.total_secs,
                            "session_start": row.session_start,
                        })
                    })
                    .unwrap_or(Value::Null);
                let _ = tx.send(Ok(value));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("persist.playtime timeout".to_string()))
        }
        "persist.top_playtime" => {
            let limit = args
                .first()
                .and_then(Value::as_i64)
                .unwrap_or(100)
                .clamp(1, 1000);
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let rows = s.db_manager.top_playtime(limit).await;
                let _ = tx.send(Ok(serde_json::json!(rows)));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("persist.top_playtime timeout".to_string()))
        }
        "player.touches" | "player.judges" => {
            Err("player.touches/judges requires async context".to_string())
        }
        _ => server_state_query_dispatch(state, method, args),
    }
}

/// Public wrapper for WIT host feature — includes capability enforcement.
pub fn server_state_query_for_host(
    state: &Arc<PlusServerState>,
    method: &str,
    args: &[Value],
) -> Result<Value, String> {
    if let Some(required) = crate::wasm_host_helpers::required_capability(method) {
        let defaults = crate::wasm_host_helpers::default_capabilities();
        if !defaults.contains(required) {
            return Err(format!("method '{method}' requires capability '{required}', which is not in default capabilities"));
        }
    }
    server_state_query(state, method, args)
}

/// Web API state query dispatch stub.
fn server_state_query(
    state: &Arc<PlusServerState>,
    method: &str,
    args: &[Value],
) -> Result<Value, String> {
    server_state_query_dispatch(state, method, args)
}

fn server_state_query_dispatch(
    state: &Arc<PlusServerState>,
    method: &str,
    args: &[Value],
) -> Result<Value, String> {
    match method {
        "rooms.list" => {
            let rooms = crate::read_lock!(state.rooms);
            let list: Vec<Value> = rooms
                .iter()
                .filter(|(_, room)| !room.is_hidden())
                .map(|(rid, room)| {
                    let ss = build_snapshot(state, &rid.to_string(), room);
                    serde_json::to_value(ss).unwrap_or_default()
                })
                .collect();
            Ok(serde_json::json!(list))
        }
        "auth.visited_count" => {
            let users = crate::read_lock!(state.users);
            Ok(serde_json::json!(users.len()))
        }
        "users.list" => {
            let users = crate::read_lock!(state.users);
            let list = users
                .iter()
                .filter_map(|(user_id, user)| {
                    let session = user.session.try_read().ok()?;
                    let session = session.as_ref()?.upgrade()?;
                    Some(serde_json::json!({
                        "id": user_id,
                        "name": session.name(),
                        "online": true,
                    }))
                })
                .collect::<Vec<_>>();
            Ok(serde_json::json!(list))
        }
        "user.is_online" => {
            let user_id = args
                .first()
                .and_then(Value::as_i64)
                .map(|value| value as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let users = crate::read_lock!(state.users);
            let online = users.get(&user_id).is_some_and(|user| {
                user.session
                    .try_read()
                    .ok()
                    .and_then(|session| session.as_ref().and_then(|weak| weak.upgrade()))
                    .is_some()
            });
            Ok(serde_json::json!(online))
        }
        "user_name" => {
            let user_id = args
                .first()
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let users = crate::read_lock!(state.users);
            let user = users.get(&user_id);
            match user {
                Some(user) => {
                    let name = user
                        .session
                        .try_read()
                        .ok()
                        .and_then(|s| s.as_ref().and_then(|w| w.upgrade()))
                        .map(|s| s.name().to_string())
                        .unwrap_or_default();
                    Ok(serde_json::json!({"id": user_id, "name": name}))
                }
                None => Err(format!("user {user_id} not found")),
            }
        }
        "playtime.leaderboard" => {
            let (tx, rx) = std::sync::mpsc::channel();
            spawn_on_runtime(async move {
                let data = if let Some(db) = crate::internal_hooks::DB.get() {
                    db.top_playtime(1000).await
                } else {
                    Vec::new()
                };
                let total = data.len();
                let _ = tx.send(Ok(serde_json::json!({
                    "success": true,
                    "data": data,
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                    "total_users": total,
                })));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("playtime.leaderboard timeout".to_string()))
        }
        "send_room_chat" => {
            let room_id = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "room_id required".to_string())?;
            let message = args
                .get(1)
                .and_then(|v| v.as_str())
                .ok_or_else(|| "message required".to_string())?;
            let rooms = crate::read_lock!(state.rooms);
            let room = rooms
                .values()
                .find(|r| r.id.to_string() == room_id)
                .ok_or_else(|| format!("room {room_id} not found"))?
                .clone();
            drop(rooms);
            let (tx, rx) = std::sync::mpsc::channel();
            let msg = message.to_string();
            spawn_on_runtime(async move {
                room.send(phira_mp_common::Message::Chat {
                    user: 0,
                    content: msg,
                })
                .await;
                let _ = tx.send(());
            });
            rx.recv_timeout(std::time::Duration::from_secs(5)).ok();
            Ok(serde_json::json!({"ok": true}))
        }
        "rooms.by_name" => {
            let room_id = args
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| "room_id required".to_string())?;
            let rooms = crate::read_lock!(state.rooms);
            let room = rooms
                .values()
                .find(|room| room.id.to_string() == room_id)
                .ok_or_else(|| format!("room {room_id} not found"))?;
            let snapshot = build_snapshot(state, room_id, room);
            serde_json::to_value(snapshot)
                .map_err(|error| format!("serialize room snapshot: {error}"))
        }
        "rooms.by_user" => {
            let user_id = args
                .first()
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let users = crate::read_lock!(state.users);
            let user = users.get(&user_id);
            match user {
                Some(user) => {
                    let room_id = crate::read_lock!(user.room)
                        .as_ref()
                        .map(|r| r.id.to_string());
                    Ok(serde_json::json!({"user_id": user_id, "room_id": room_id}))
                }
                None => Err(format!("user {user_id} not found")),
            }
        }
        "http.register_route" => {
            let (path, plugin_name) = parse_http_route_registration(args)?;
            let s = Arc::clone(state);
            let (tx, rx) = std::sync::mpsc::channel();
            spawn_on_runtime(async move {
                let http_handle = s.plugin_manager.http_handle();
                let result = match http_handle {
                    Some(handle) => {
                        let pm = Arc::clone(&s.plugin_manager);
                        let pn = plugin_name.clone();
                        let route_path = path.clone();
                        let route_path_for_closure = route_path.clone();
                        let handler: phira_mp_plus_server_api::HttpHandler =
                            std::sync::Arc::new(move |body, params| {
                                let mut args: Vec<serde_json::Value> =
                                    params.into_iter().map(serde_json::Value::String).collect();
                                if let Some(json_body) = body {
                                    args.push(json_body);
                                }
                                match futures::executor::block_on(pm.call_plugin_api(
                                    &pn,
                                    &route_path_for_closure,
                                    args,
                                )) {
                                    Ok(val) => Ok(val),
                                    Err(e) => Err((500, e)),
                                }
                            });
                        handle.register_route(&route_path, handler);
                        Ok(serde_json::json!({"ok": true, "path": &path}))
                    }
                    None => Err("http not enabled".to_string()),
                };
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("http.register_route timeout".to_string()))
        }
        "sse.register_stream" => {
            let (path, plugin_name, event_types) = parse_sse_stream_registration(args)?;
            let s = Arc::clone(state);
            let (tx, rx) = std::sync::mpsc::channel();
            spawn_on_runtime(async move {
                let http_handle = s.plugin_manager.http_handle();
                let result = match http_handle {
                    Some(handle) => {
                        handle.register_sse_stream(&path, &plugin_name, &event_types);
                        Ok(serde_json::json!({"ok": true, "path": path}))
                    }
                    None => Err("http not enabled".to_string()),
                };
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("sse.register_stream timeout".to_string()))
        }
        "user.kick" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let reason = args
                .get(1)
                .and_then(|v| v.as_str())
                .unwrap_or("kicked by admin")
                .to_string();
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let result = crate::server::run_admin_kick_user(&s, uid, &reason).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("user.kick timeout".to_string()))
        }
        "room.kick" => {
            let room_id = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "room_id required".to_string())?;
            let target_id = args
                .get(1)
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let rid = room_id.to_string();
            spawn_on_runtime(async move {
                let result = s.room_commands.kick_user(&*s, &rid, target_id).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("room.kick timeout".to_string()))
        }
        "room.set_host" => {
            let room_id = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "room_id required".to_string())?;
            let target_id = args.get(1).and_then(|v| v.as_i64()).map(|v| v as i32);
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let rid = room_id.to_string();
            spawn_on_runtime(async move {
                let result = s.room_commands.set_host(&*s, &rid, target_id).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("room.set_host timeout".to_string()))
        }
        "room.set_lock" => {
            let room_id = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "room_id required".to_string())?;
            let locked = args
                .get(1)
                .and_then(|v| v.as_bool())
                .ok_or_else(|| "locked (bool) required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let rid = room_id.to_string();
            spawn_on_runtime(async move {
                let result = s.room_commands.set_lock(&*s, &rid, locked).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("room.set_lock timeout".to_string()))
        }
        "room.set_cycle" => {
            let room_id = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "room_id required".to_string())?;
            let cycle = args
                .get(1)
                .and_then(|v| v.as_bool())
                .ok_or_else(|| "cycle (bool) required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let rid = room_id.to_string();
            spawn_on_runtime(async move {
                let result = s.room_commands.set_cycle(&*s, &rid, cycle).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("room.set_cycle timeout".to_string()))
        }
        "room.close" => {
            let room_id = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "room_id required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let rid = room_id.to_string();
            spawn_on_runtime(async move {
                let result = s.room_commands.close_room(&*s, &rid).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("room.close timeout".to_string()))
        }
        "room.create_empty" => {
            let room_id = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "room_id required".to_string())?;
            let endpoint = args.get(1).and_then(|v| v.as_str());
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let rid = room_id.to_string();
            let ep = endpoint.map(|e| e.to_string());
            spawn_on_runtime(async move {
                let result = s.create_empty_room(&rid, ep.clone(), false).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("room.create_empty timeout".to_string()))
        }
        "admin.kick_user" => {
            let target_id = args
                .get(0)
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let result =
                    crate::server::run_admin_kick_user(&*s, target_id, "kicked via state query")
                        .await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("admin.kick_user timeout".to_string()))
        }
        "room.set_hidden" => {
            let room_id = args
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| "room_id required".to_string())?;
            let hidden = args
                .get(1)
                .and_then(Value::as_bool)
                .ok_or_else(|| "hidden (bool) required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let room_id = room_id.to_string();
            spawn_on_runtime(async move {
                let _ = tx.send(s.room_commands.set_hidden(&s, &room_id, hidden).await);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("room.set_hidden timeout".to_string()))
        }
        "room.get_phira_api_endpoint" => {
            let room_id = args
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| "room_id required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let room_id = room_id.to_string();
            spawn_on_runtime(async move {
                let _ = tx.send(s.get_room_phira_api_endpoint(&room_id).await);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("room.get_phira_api_endpoint timeout".to_string()))
        }
        "room.set_phira_api_endpoint" | "room.clear_phira_api_endpoint" => {
            let room_id = args
                .first()
                .and_then(Value::as_str)
                .ok_or_else(|| "room_id required".to_string())?;
            let endpoint = if method == "room.clear_phira_api_endpoint" {
                None
            } else {
                args.get(1).and_then(Value::as_str).map(str::to_string)
            };
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let room_id = room_id.to_string();
            spawn_on_runtime(async move {
                let _ = tx.send(
                    s.room_commands
                        .set_phira_api_endpoint(&s, &room_id, endpoint)
                        .await,
                );
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("room.set_phira_api_endpoint timeout".to_string()))
        }
        "admin.list" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let _ = tx.send(Ok(serde_json::json!(s.admin_id_list().await)));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("admin.list timeout".to_string()))
        }
        "admin.check" => {
            let user_id = args
                .first()
                .and_then(Value::as_i64)
                .map(|value| value as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let _ = tx.send(Ok(serde_json::json!(s.is_admin_id(user_id).await)));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("admin.check timeout".to_string()))
        }
        "admin.add" | "admin.remove" => {
            let user_id = args
                .first()
                .and_then(Value::as_i64)
                .map(|value| value as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let add = method == "admin.add";
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let ids = if add {
                    s.add_admin_id(user_id).await
                } else {
                    s.remove_admin_id(user_id).await
                };
                let _ = tx.send(Ok(serde_json::json!(ids)));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("admin update timeout".to_string()))
        }
        "admin.set" => {
            let ids = args
                .first()
                .and_then(Value::as_array)
                .ok_or_else(|| "admin id array required".to_string())?
                .iter()
                .filter_map(Value::as_i64)
                .map(|value| value as i32)
                .collect::<Vec<_>>();
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let _ = tx.send(Ok(serde_json::json!(s.set_admin_ids(ids).await)));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("admin.set timeout".to_string()))
        }
        "ban.list" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let manager = Arc::clone(&state.ban_manager);
            spawn_on_runtime(async move {
                let _ = tx.send(Ok(serde_json::json!(manager.list_banned().await)));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("ban.list timeout".to_string()))
        }
        "ban.check" => {
            let user_id = args
                .first()
                .and_then(Value::as_i64)
                .map(|value| value as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let manager = Arc::clone(&state.ban_manager);
            spawn_on_runtime(async move {
                let _ = tx.send(Ok(serde_json::json!(manager.is_banned(user_id).await)));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("ban.check timeout".to_string()))
        }
        "ban.add" => {
            let user_id = args
                .first()
                .and_then(Value::as_i64)
                .map(|value| value as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let reason = args
                .get(1)
                .and_then(Value::as_str)
                .unwrap_or("banned by plugin")
                .to_string();
            let (tx, rx) = std::sync::mpsc::channel();
            let manager = Arc::clone(&state.ban_manager);
            spawn_on_runtime(async move {
                let result = manager.ban_user(user_id, &reason).await.map(
                    |reason| serde_json::json!({"ok": true, "user_id": user_id, "reason": reason}),
                );
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("ban.add timeout".to_string()))
        }
        "ban.remove" => {
            let user_id = args
                .first()
                .and_then(Value::as_i64)
                .map(|value| value as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let manager = Arc::clone(&state.ban_manager);
            spawn_on_runtime(async move {
                let result = manager
                    .unban_user(user_id)
                    .await
                    .map(|()| serde_json::json!({"ok": true, "user_id": user_id}));
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("ban.remove timeout".to_string()))
        }
        _ => Err(format!("unknown method: {method}")),
    }
}

#[cfg(test)]
mod registration_tests {
    use super::parse_http_route_registration;
    use serde_json::json;

    #[test]
    fn parses_documented_object_form() {
        let args = vec![json!({"path": "/api/hello", "plugin": "my-plugin"})];
        assert_eq!(
            parse_http_route_registration(&args).unwrap(),
            ("/api/hello".to_string(), "my-plugin".to_string())
        );
    }

    #[test]
    fn keeps_positional_form_compatible() {
        let args = vec![json!("/api/hello"), json!("my-plugin")];
        assert_eq!(
            parse_http_route_registration(&args).unwrap(),
            ("/api/hello".to_string(), "my-plugin".to_string())
        );
    }
}

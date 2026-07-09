//! Server state query dispatch — the sync query engine for CLI, WIT, and Web API.
//!
//! Extracted from the original `server.rs`. These functions are synchronous —
//! they use `spawn_on_runtime` for async operations and `read_lock!` for sync reads.

use crate::benchmark_report::BenchmarkMode;
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

fn parse_benchmark_mode_arg(value: &str) -> Option<BenchmarkMode> {
    match value {
        "simulation" | "sim" => Some(BenchmarkMode::Simulation),
        "hybrid" => Some(BenchmarkMode::Hybrid),
        "real" => Some(BenchmarkMode::Real),
        _ => None,
    }
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
                let events = s.event_bus.stats(crate::runtime_diagnostics::BENCHMARK_REPORT_RECENT_DEFAULT);
                let commands = s.command_registry.iter().count();
                let room_commands = s.room_commands.stats();
                let phira_http = s.phira_client.stats();
                let benchmark_reports = s.benchmark_reports.snapshot(crate::runtime_diagnostics::BENCHMARK_REPORT_RECENT_DEFAULT);
                let _ = tx.send(Ok(serde_json::json!({
                    "runtime_v2": true,
                    "note": "Runtime v2 is partially installed; real Room/Session runtime is still the current production path.",
                    "commands": {"registered": commands},
                    "event_bus": events, "simulation": simulation,
                    "persistence_worker": persistence, "room_command_gateway": room_commands,
                    "phira_http": phira_http, "benchmark_reports": benchmark_reports,
                })));
            });
            rx.recv_timeout(runtime_state_query_timeout()).unwrap_or(Err("runtime.status timeout".to_string()))
        }
        "simulation.status" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let status = s.simulation.status().await;
                let _ = tx.send(Ok(serde_json::to_value(status).unwrap_or_default()));
            });
            rx.recv_timeout(runtime_state_query_timeout()).unwrap_or(Err("simulation.status timeout".to_string()))
        }
        "simulation.start" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let config = serde_json::from_value(args.first().cloned().unwrap_or_default()).map_err(|e| format!("invalid simulation config: {e}"))?;
            spawn_on_runtime(async move {
                let status = s.simulation.start(config).await;
                let _ = tx.send(Ok(serde_json::to_value(status).unwrap_or_default()));
            });
            rx.recv_timeout(runtime_state_query_timeout()).unwrap_or(Err("simulation.start timeout".to_string()))
        }
        "simulation.stop" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let reason = args.first().and_then(|v| v.as_str()).unwrap_or("stopped via state query").to_string();
            spawn_on_runtime(async move {
                let status = s.simulation.stop(&reason).await;
                let _ = tx.send(Ok(serde_json::to_value(status).unwrap_or_default()));
            });
            rx.recv_timeout(runtime_state_query_timeout()).unwrap_or(Err("simulation.stop timeout".to_string()))
        }
        "simulation.cleanup" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                s.simulation.cleanup().await;
                let _ = tx.send(Ok(serde_json::json!({"ok": true})));
            });
            rx.recv_timeout(runtime_state_query_timeout()).unwrap_or(Err("simulation.cleanup timeout".to_string()))
        }
        "benchmark.reports" => {
            let count = args.first().and_then(|v| v.as_u64()).unwrap_or(crate::runtime_diagnostics::BENCHMARK_REPORT_RECENT_DEFAULT as u64) as usize;
            let reports = state.benchmark_reports.snapshot(count);
            serde_json::to_value(&reports).map_err(|e| format!("serialize benchmark reports: {e}"))
        }
        "benchmark.latest" => {
            let reports = state.benchmark_reports.snapshot(1);
            Ok(serde_json::json!({"latest": serde_json::Value::Null}))
        }
        "benchmark.history" => {
            let max = args.first().and_then(|v| v.as_u64()).map(|v| v as usize).unwrap_or(usize::MAX);
            let reports = state.benchmark_reports.snapshot(max);
            serde_json::to_value(&reports).map_err(|e| format!("serialize benchmark reports: {e}"))
        }
        "rooms.history" => {
            let users = crate::read_lock!(state.rooms);
            let rooms_snapshot: Vec<Value> = users.iter().map(|(_, room)| {
                let hist = crate::read_lock!(room.play_history);
                let rounds: Vec<Value> = hist.iter().map(|r| {
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
        "player.touches" | "player.judges" => {
            Err("player.touches/judges requires async context".to_string())
        }
        _ => Err(format!("unknown method: {method}"))
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
            let list: Vec<Value> = rooms.iter()
                .filter(|(_, room)| !room.is_hidden())
                .map(|(rid, room)| {
                    let ss = build_snapshot(state, &rid.to_string(), room);
                    serde_json::to_value(ss).unwrap_or_default()
                })
                .collect();
            Ok(serde_json::json!(list))
        }
        "user_name" => {
            let user_id = args.first().and_then(|v| v.as_i64()).map(|v| v as i32).ok_or_else(|| "user_id required".to_string())?;
            let users = crate::read_lock!(state.users);
            let user = users.get(&user_id);
            match user {
                Some(user) => {
                    let name = user.session.try_read().ok()
                        .and_then(|s| s.as_ref().and_then(|w| w.upgrade()))
                        .map(|s| s.name.clone())
                        .unwrap_or_default();
                    Ok(serde_json::json!({"id": user_id, "name": name}))
                }
                None => Err(format!("user {user_id} not found")),
            }
        }
        "send_room_chat" => {
            let room_id = args.first().and_then(|v| v.as_str()).ok_or_else(|| "room_id required".to_string())?;
            let message = args.get(1).and_then(|v| v.as_str()).ok_or_else(|| "message required".to_string())?;
            let rooms = crate::read_lock!(state.rooms);
            let room = rooms.values().find(|r| r.id.to_string() == room_id).ok_or_else(|| format!("room {room_id} not found"))?.clone();
            drop(rooms);
            let (tx, rx) = std::sync::mpsc::channel();
            let msg = message.to_string();
            spawn_on_runtime(async move {
                room.send(phira_mp_common::Message::Chat { user: 0, content: msg }).await;
                let _ = tx.send(());
            });
            rx.recv_timeout(std::time::Duration::from_secs(5)).ok();
            Ok(serde_json::json!({"ok": true}))
        }
        "rooms.by_user" => {
            let user_id = args.first().and_then(|v| v.as_i64()).map(|v| v as i32).ok_or_else(|| "user_id required".to_string())?;
            let users = crate::read_lock!(state.users);
            let user = users.get(&user_id);
            match user {
                Some(user) => {
                    let room_id = crate::read_lock!(user.room).as_ref().map(|r| r.id.to_string());
                    Ok(serde_json::json!({"user_id": user_id, "room_id": room_id}))
                }
                None => Err(format!("user {user_id} not found")),
            }
        }
        "http.register_route" => {
            let path = args.first().and_then(|v| v.as_str()).ok_or_else(|| "path required".to_string())?;
            let plugin = args.get(1).and_then(|v| v.as_str()).ok_or_else(|| "plugin name required".to_string())?;
            let s = Arc::clone(state);
            let path = path.to_string();
            let plugin = plugin.to_string();
            let (tx, rx) = std::sync::mpsc::channel();
            spawn_on_runtime(async move {
                let http_handle = s.plugin_manager.http_handle();
                let result = match http_handle {
                    Some(handle) => {
                        let handler: phira_mp_plus_server_api::HttpHandler = std::sync::Arc::new(move |_, _| {
                            Ok(serde_json::json!({"ok": true, "path": &path}))
                        });
                        handle.register_route(&path, handler);
                        Ok(serde_json::json!({"ok": true, "path": &path}))
                    }
                    None => Err("http not enabled".to_string()),
                };
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout()).unwrap_or(Err("http.register_route timeout".to_string()))
        }
        "sse.register_stream" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let event_bus_rx = s.event_bus.subscribe();
                let (sse_tx, _) = tokio::sync::mpsc::channel(64);
                let stream_id = s.events.register_stream(event_bus_rx, sse_tx).await;
                let _ = tx.send(Ok(serde_json::json!({"ok": true, "stream_id": stream_id})));
            });
            rx.recv_timeout(runtime_state_query_timeout()).unwrap_or(Err("sse.register_stream timeout".to_string()))
        }
        "user.kick" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let reason = args.get(1).and_then(|v| v.as_str()).unwrap_or("kicked by admin").to_string();
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                use phira_mp_common::{Message, RoomEvent};
                use crate::plugin::PluginEvent;
                let result = async {
                    let user = s.users.read().await.get(&uid).map(std::sync::Arc::clone)
                        .ok_or("user not found".to_string())?;
                    if let Some(room) = user.room.read().await.as_ref().map(std::sync::Arc::clone) {
                        let room_id = room.id.to_string();
                        let room_key = room.id.clone();
                        let was_monitor = user.monitor.load(std::sync::atomic::Ordering::SeqCst);
                        if room.on_user_leave(&user).await { s.rooms.write().await.remove(&room_key); }
                        if !was_monitor { s.publish_room_event(RoomEvent::LeaveRoom { room: room_key, user: uid }).await; }
                        s.event_bus.publish(crate::event_bus::MpEvent::PluginEventDispatched(
                            std::sync::Arc::new(PluginEvent::RoomLeave { user_id: uid, room_id }),
                        ));
                        room.send(Message::Chat { user: 0, content: format!("用户已被踢出 (reason: {reason})") }).await;
                    }
                    user.session.write().await.take().and_then(|w| w.upgrade()).map(|session| { let _ = session; });
                    serde_json::json!({"ok": true, "user_id": uid})
                }.await;
                let _ = tx.send(Ok(result));
            });
            rx.recv_timeout(runtime_state_query_timeout()).unwrap_or(Err("user.kick timeout".to_string()))
        }
        "room.kick" => {
            let room_id = args.first().and_then(|v| v.as_str()).ok_or_else(|| "room_id required".to_string())?;
            let target_id = args.get(1).and_then(|v| v.as_i64()).map(|v| v as i32).ok_or_else(|| "user_id required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let rid = room_id.to_string();
            spawn_on_runtime(async move {
                let result = s.room_commands.kick_user(&*s, &rid, target_id).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout()).unwrap_or(Err("room.kick timeout".to_string()))
        }
        "room.set_host" => {
            let room_id = args.first().and_then(|v| v.as_str()).ok_or_else(|| "room_id required".to_string())?;
            let target_id = args.get(1).and_then(|v| v.as_i64()).map(|v| v as i32);
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let rid = room_id.to_string();
            spawn_on_runtime(async move {
                let result = s.room_commands.set_host(&*s, &rid, target_id).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout()).unwrap_or(Err("room.set_host timeout".to_string()))
        }
        "room.set_lock" => {
            let room_id = args.first().and_then(|v| v.as_str()).ok_or_else(|| "room_id required".to_string())?;
            let locked = args.get(1).and_then(|v| v.as_bool()).ok_or_else(|| "locked (bool) required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let rid = room_id.to_string();
            spawn_on_runtime(async move {
                let result = s.room_commands.set_lock(&*s, &rid, locked).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout()).unwrap_or(Err("room.set_lock timeout".to_string()))
        }
        "room.set_cycle" => {
            let room_id = args.first().and_then(|v| v.as_str()).ok_or_else(|| "room_id required".to_string())?;
            let cycle = args.get(1).and_then(|v| v.as_bool()).ok_or_else(|| "cycle (bool) required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let rid = room_id.to_string();
            spawn_on_runtime(async move {
                let result = s.room_commands.set_cycle(&*s, &rid, cycle).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout()).unwrap_or(Err("room.set_cycle timeout".to_string()))
        }
        "room.close" => {
            let room_id = args.first().and_then(|v| v.as_str()).ok_or_else(|| "room_id required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let rid = room_id.to_string();
            spawn_on_runtime(async move {
                let result = s.room_commands.close_room(&*s, &rid).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout()).unwrap_or(Err("room.close timeout".to_string()))
        }
        "room.create_empty" => {
            let room_id = args.first().and_then(|v| v.as_str()).ok_or_else(|| "room_id required".to_string())?;
            let endpoint = args.get(1).and_then(|v| v.as_str());
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let rid = room_id.to_string();
            let ep = endpoint.map(|e| e.to_string());
            spawn_on_runtime(async move {
                let result = s.create_empty_room(&rid, ep.as_deref()).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout()).unwrap_or(Err("room.create_empty timeout".to_string()))
        }
        "admin.kick_user" => {
            let target_id = args.get(0).and_then(|v| v.as_i64()).map(|v| v as i32).ok_or_else(|| "user_id required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let result = crate::server::run_admin_kick_user(&*s, target_id, "kicked via state query").await;
                let _ = tx.send(Ok(result));
            });
            rx.recv_timeout(runtime_state_query_timeout()).unwrap_or(Err("admin.kick_user timeout".to_string()))
        }
        "admin.list" | "admin.check" | "admin.add" | "admin.remove" | "admin.set"
        | "ban.list" | "ban.add" | "ban.remove" | "ban.check" => {
            server_state_query_inner(state, method, args)
        }
        _ => server_state_query_inner(state, method, args)
    }
}

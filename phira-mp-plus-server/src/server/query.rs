//! Server state query dispatch — the sync query engine for CLI, WIT, and Web API.
//!
//! Extracted from the original `server.rs`.

use crate::benchmark_report::BenchmarkMode;
use crate::benchmark_snapshot::BenchmarkReportStore;
use crate::command_registry::CommandRegistry;
use crate::plugin::PluginEvent;
use crate::server::snapshot::build_snapshot;
use phira_mp_plus_server_api as api;
use super::PlusServerState;
use serde_json::Value;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tracing::warn;

fn runtime_state_query_timeout() -> std::time::Duration {
    crate::runtime_diagnostics::RUNTIME_STATE_QUERY_TIMEOUT
}

/// Spawn an async task on a Tokio runtime.
fn spawn_on_runtime<F>(f: F) -> Option<tokio::task::JoinHandle<()>>
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

fn server_state_query_inner(
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
                    "runtime_v2": true,
                    "note": "Runtime v2 is partially installed; real Room/Session runtime is still the current production path.",
                    "commands": {"registered": commands},
                    "event_bus": events,
                    "simulation": simulation,
                    "persistence_worker": persistence,
                    "room_command_gateway": room_commands,
                    "phira_http": phira_http,
                    "benchmark_reports": benchmark_reports,
                })));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("runtime.status timeout".to_string()))
        }
        "simulation.status" => {
            let s = Arc::clone(state);
            let (tx, rx) = std::sync::mpsc::channel();
            spawn_on_runtime(async move {
                let status = s.simulation.status().await;
                let _ = tx.send(Ok(serde_json::to_value(status).unwrap_or_default()));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("simulation.status timeout".to_string()))
        }
        "simulation.start" => {
            let s = Arc::clone(state);
            let (tx, rx) = std::sync::mpsc::channel();
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
            let s = Arc::clone(state);
            let (tx, rx) = std::sync::mpsc::channel();
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
            let s = Arc::clone(state);
            let (tx, rx) = std::sync::mpsc::channel();
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
            let reports = state
                .benchmark_reports
                .snapshot(count);
            serde_json::to_value(&reports)
                .map_err(|e| format!("serialize benchmark reports: {e}"))
        }
        "benchmark.latest" => {
            let reports = state.benchmark_reports.snapshot(1);
            Ok(serde_json::json!(reports.first()))
        }
        "benchmark.history" => {
            let max = args
                .first()
                .and_then(|v| v.as_u64())
                .map(|v| v as usize)
                .unwrap_or(usize::MAX);
            let reports = state.benchmark_reports.all_up_to(max);
            serde_json::to_value(&reports)
                .map_err(|e| format!("serialize benchmark reports: {e}"))
        }
        "rooms.history" => {
            let users = read_lock!(state.rooms);
            let rooms_snapshot: Vec<Value> = users
                .iter()
                .map(|(_, room)| {
                    let hist = read_lock!(room.play_history);
                    let rounds: Vec<Value> = hist
                        .iter()
                        .map(|r| {
                            let results: Vec<Value> = r
                                .results
                                .iter()
                                .map(|res| {
                                    serde_json::json!({
                                        "player": res.user_id,
                                        "user_name": res.user_name.clone(),
                                        "score": res.score,
                                        "accuracy": res.accuracy,
                                        "perfect": res.perfect,
                                        "good": res.good,
                                        "bad": res.bad,
                                        "miss": res.miss,
                                    })
                                })
                                .collect();
                            serde_json::json!({
                                "round_id": r.round_id.to_string(),
                                "chart_id": r.chart_id,
                                "chart_name": r.chart_name,
                                "results": results,
                            })
                        })
                        .collect();
                    serde_json::json!({
                        "room_id": room.id,
                        "rounds": rounds,
                    })
                })
                .collect();
            Ok(serde_json::json!(rooms_snapshot))
        }
        "player.touches" | "player.judges" => {
            let room_id = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "room_id required".to_string())?;
            let user_id: i32 = args
                .get(1)
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let limit: usize = args
                .get(2)
                .and_then(|v| v.as_u64())
                .map(|v| v as usize)
                .unwrap_or(100);
            let rooms = state.rooms.read().await;
            let room = rooms
                .values()
                .find(|r| r.id.to_string() == room_id || r.uuid.to_string() == room_id)
                .ok_or_else(|| format!("room {room_id} not found"))?
                .clone();
            drop(rooms);
            let data = if method == "player.touches" {
                room.player_touches(user_id, limit).await
            } else {
                room.player_judges(user_id, limit).await
            };
            serde_json::to_value(data)
                .map_err(|e| format!("serialize {method}: {e}"))
        }
        "player.current_round" => {
            let room_id = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "room_id required".to_string())?;
            let rooms = state.rooms.read().await;
            let room = rooms
                .values()
                .find(|r| r.id.to_string() == room_id)
                .ok_or_else(|| format!("room {room_id} not found"))?
                .clone();
            drop(rooms);
            let user_count = room.users().await.len();
            Ok(serde_json::json!({
                "room_id": room.id,
                "current_round": read_lock!(room.current_round_id).map(|id| id.to_string()),
                "state": room.state_name().await,
                "user_count": user_count,
                "chart": read_lock!(room.chart).as_ref().map(|c| c.id),
            }))
        }
        "game_time.range" | "game_time.touches" | "game_time.judges" => {
            let room_id = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "room_id required".to_string())?;
            let user_id: i32 = args
                .get(1)
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let rooms = state.rooms.read().await;
            let room = rooms
                .values()
                .find(|r| r.id.to_string() == room_id)
                .ok_or_else(|| format!("room {room_id} not found"))?
                .clone();
            drop(rooms);
            let data = if method == "game_time.touches" {
                serde_json::to_value(room.player_touch_times(user_id).await)
            } else if method == "game_time.judges" {
                serde_json::to_value(room.player_judge_times(user_id).await)
            } else {
                Ok(serde_json::json!({
                    "touch_times": room.player_touch_times(user_id).await,
                    "judge_times": room.player_judge_times(user_id).await,
                }))
            };
            data.map_err(|e| format!("serialize {method}: {e}"))
        }
        "test.config" => {
            let mut config = serde_json::Map::new();
            config.insert("idle_after_secs".into(), serde_json::json!(state.config.idle.idle_after_secs));
            config.insert("check_interval_secs".into(), serde_json::json!(state.config.idle.check_interval_secs));
            config.insert("minimal".into(), serde_json::json!(state.config.idle.minimal));
            config.insert("plugins_enabled".into(), serde_json::json!(state.config.idle.plugins_enabled));
            config.insert("lazy_services".into(), serde_json::json!(state.config.idle.lazy_services));
            config.insert("telemetry_cutover".into(), serde_json::json!(state.config.runtime_v2.telemetry_cutover_mode.as_str()));
            config.insert("telemetry_batcher".into(), serde_json::json!(state.config.runtime_v2.telemetry_batcher.enabled));
            Ok(serde_json::Value::Object(config))
        }
        "test.snapshot_latency" => {
            let rooms = state.rooms.read().await;
            let latencies: Vec<Value> = rooms
                .iter()
                .map(|(_, room)| {
                    serde_json::json!({
                        "room_id": room.id,
                        "user_count": room.users_snapshot_len(),
                        "snapshot_latency_us": room.snapshot_latency(),
                    })
                })
                .collect();
            Ok(serde_json::json!(latencies))
        }
        "test.run_benchmark" => {
            let args_obj = args.first().ok_or_else(|| "benchmark config required".to_string())?;
            let mode = args_obj
                .get("mode")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "mode field required".to_string())?;
            let duration_secs = args_obj
                .get("duration")
                .and_then(|v| v.as_u64())
                .unwrap_or(30);
            let target = args_obj
                .get("target_rooms")
                .and_then(|v| v.as_u64())
                .unwrap_or(1) as usize;
            let (result_tx, result_rx) = std::sync::mpsc::channel();
            let bench_req = match parse_benchmark_mode_arg(mode) {
                Some(BenchmarkMode::Hybrid) => {
                    let config: crate::server::HybridBenchmarkConfig =
                        serde_json::from_value(args_obj.clone())
                            .map_err(|e| format!("invalid hybrid config: {e}"))?;
                    crate::server::BenchRequest::hybrid(config, result_tx)
                }
                _ => crate::server::BenchRequest::real(duration_secs, target, result_tx),
            };
            let _ = state.bench_tx.try_send(bench_req).map_err(|e| format!("benchmark queue full: {e}"))?;
            result_rx
                .recv_timeout(std::time::Duration::from_secs(duration_secs + 180))
                .unwrap_or(Err("benchmark timed out".to_string()))
        }
        "test.bind_phira_tokens" => {
            let raw = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "tokens string required".to_string())?;
            let s = Arc::clone(state);
            let (tx, rx) = std::sync::mpsc::channel();
            spawn_on_runtime(async move {
                use crate::server::benchmark::sanitize_benchmark_tokens;
                let tokens = sanitize_benchmark_tokens(
                    raw.split(|ch: char| ch == ',' || ch == ';' || ch.is_whitespace())
                        .map(|s| s.to_string()),
                );
                let count = tokens.len();
                *s.bench_tokens.write().await = tokens;
                let _ = tx.send(Ok(serde_json::json!({"ok": true, "count": count, "path": crate::server::benchmark::BENCH_AUTH_FILE})));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("test.bind_phira_tokens timeout".to_string()))
        }
        "test.cleanup" => {
            let rooms = state.rooms.read().await;
            let room_count = rooms.len();
            let user_count = state.users.read().await.len();
            drop(rooms);
            Ok(serde_json::json!({
                "rooms": room_count,
                "users": user_count,
                "note": "query only — use runtime management commands for cleanup",
            }))
        }
        "rooms.by_uuid" => {
            let uuid = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "uuid required".to_string())?;
            let rooms = state.rooms.read().await;
            let room = rooms
                .values()
                .find(|r| r.uuid.to_string() == uuid)
                .ok_or_else(|| format!("room {uuid} not found"))?
                .clone();
            drop(rooms);
            let ss = build_snapshot(state, &room.id.to_string(), &room);
            serde_json::to_value(ss).map_err(|e| format!("serialize room: {e}"))
        }
        "rooms.all_with_data" => {
            let rooms = state.rooms.read().await;
            let list: Vec<Value> = rooms
                .iter()
                .map(|(rid, room)| {
                    let ss = build_snapshot(state, &rid.to_string(), room);
                    serde_json::to_value(ss).unwrap_or_default()
                })
                .collect();
            Ok(serde_json::json!(list))
        }
        "rooms.by_name" => {
            let name = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "name required".to_string())?;
            let rooms = state.rooms.read().await;
            let room = rooms
                .values()
                .find(|r| r.id.to_string() == name)
                .ok_or_else(|| format!("room {name} not found"))?
                .clone();
            drop(rooms);
            let ss = build_snapshot(state, name, &room);
            serde_json::to_value(ss).map_err(|e| format!("serialize room: {e}"))
        }
        "rooms._test_room" => {
            let name = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "name required".to_string())?;
            let rooms = state.rooms.read().await;
            let room = rooms
                .values()
                .find(|r| r.id.to_string() == name)
                .ok_or_else(|| format!("room {name} not found"))?
                .clone();
            drop(rooms);
            let ss = build_snapshot(state, name, &room);
            serde_json::to_value(ss).map_err(|e| format!("serialize room: {e}"))
        }
        "room.create_empty" => {
            let room_id = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "room_id required".to_string())?;
            let endpoint = args.get(1).and_then(|v| v.as_str());
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let room_id = room_id.to_string();
            let endpoint = endpoint.map(|e| e.to_string());
            spawn_on_runtime(async move {
                let result = s.create_empty_room(&room_id, endpoint.as_deref()).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("room.create_empty timeout".to_string()))
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
                .ok_or_else(|| "target user_id required".to_string())?;
            {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let room_id = room_id.to_string();
            spawn_on_runtime(async move {
                let result = s.room_commands.kick_user(&s, &room_id, target_id).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("room.kick timeout".to_string()))
        }
        }
        "room.set_host" => {
            let room_id = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "room_id required".to_string())?;
            let target_id = args
                .get(1)
                .and_then(|v| v.as_i64())
                .map(|v| v as i32);
            {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let room_id = room_id.to_string();
            spawn_on_runtime(async move {
                let result = s.room_commands.set_host(&s, &room_id, target_id).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("room.set_host timeout".to_string()))
        }
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
            {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let room_id = room_id.to_string();
            spawn_on_runtime(async move {
                let result = s.room_commands.set_lock(&s, &room_id, locked).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("room.set_lock timeout".to_string()))
        }
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
            {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let room_id = room_id.to_string();
            spawn_on_runtime(async move {
                let result = s.room_commands.set_cycle(&s, &room_id, cycle).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("room.set_cycle timeout".to_string()))
        }
        }
        "room.force_move" => {
            let room_id = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "room_id required".to_string())?;
            let user_id = args
                .get(1)
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let target_room_id = args
                .get(2)
                .and_then(|v| v.as_str())
                .ok_or_else(|| "target room_id required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let room_id = room_id.to_string();
            let target_room_id = target_room_id.to_string();
            spawn_on_runtime(async move {
                let result = s.force_move_user_to_room(&room_id, user_id, &target_room_id).await;
                let _ = tx.send(result.map(|_| serde_json::json!({"ok": true})));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("room.force_move timeout".to_string()))
        }
        "room.set_hidden" => {
            let room_id = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "room_id required".to_string())?;
            let hidden = args
                .get(1)
                .and_then(|v| v.as_bool())
                .ok_or_else(|| "hidden (bool) required".to_string())?;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let room_id = room_id.to_string();
            spawn_on_runtime(async move {
                s.set_room_hidden(&room_id, hidden).await;
                let _ = tx.send(Ok(serde_json::json!({"ok": true})));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("room.set_hidden timeout".to_string()))
        }
        "room.close" => {
            let room_id = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "room_id required".to_string())?;
            {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let room_id = room_id.to_string();
            spawn_on_runtime(async move {
                let result = s.room_commands.close_room(&s, &room_id).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("room.close timeout".to_string()))
        }
        }
        "admin.kick_user" => {
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
            let room_id = room_id.to_string();
            spawn_on_runtime(async move {
                let result = crate::server::run_admin_kick_user(&s, &room_id, target_id).await;
                let _ = tx.send(Ok(result));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("admin.kick_user timeout".to_string()))
        }
        "admin.ban_list" | "ban.list" => {
            let bans = state.ban_manager.list().await;
            serde_json::to_value(&bans)
                .map_err(|e| format!("serialize bans: {e}"))
        }
        "admin.ban_add" | "ban.add" => {
            let ip = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "ip required".to_string())?;
            let reason = args.get(1).and_then(|v| v.as_str()).unwrap_or("");
            state.ban_manager.add(ip, reason).await;
            Ok(serde_json::json!({"ok": true, "ip": ip}))
        }
        "admin.ban_remove" | "ban.remove" => {
            let ip = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "ip required".to_string())?;
            state.ban_manager.remove(ip).await;
            Ok(serde_json::json!({"ok": true, "ip": ip}))
        }
        "admin.ban_check" | "ban.check" => {
            let ip = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "ip required".to_string())?;
            let banned = state.ban_manager.is_banned(ip).await;
            Ok(serde_json::json!({"ip": ip, "banned": banned}))
        }
        "admin.list" => {
            let ids = state.admin_id_list().await;
            Ok(serde_json::json!(ids))
        }
        "admin.check" => {
            let user_id = args
                .first()
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let is_admin = state.is_admin_id(user_id).await;
            Ok(serde_json::json!({"user_id": user_id, "admin": is_admin}))
        }
        "admin.add" => {
            let user_id = args
                .first()
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let ids = state.add_admin_id(user_id).await;
            Ok(serde_json::json!({"ok": true, "admin_ids": ids}))
        }
        "admin.remove" => {
            let user_id = args
                .first()
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let ids = state.remove_admin_id(user_id).await;
            Ok(serde_json::json!({"ok": true, "admin_ids": ids}))
        }
        "admin.set" => {
            let ids: Vec<i32> = args
                .first()
                .ok_or_else(|| "ids array required".to_string())?
                .as_array()
                .ok_or_else(|| "ids must be an array".to_string())?
                .iter()
                .filter_map(|v| v.as_i64().map(|id| id as i32))
                .collect();
            let result = state.set_admin_ids(ids).await;
            Ok(serde_json::json!({"ok": true, "admin_ids": result}))
        }
        "persist.events" => {
            let since = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
            let limit = args.get(1).and_then(|v| v.as_i64()).unwrap_or(100).clamp(1, 1000);
            let kind = args.get(2).and_then(|v| v.as_str()).map(str::to_string);
            let room_id = args.get(3).and_then(|v| v.as_str()).map(str::to_string);
            let user_id = args.get(4).and_then(|v| v.as_i64()).and_then(|v| i32::try_from(v).ok());
            let (tx, rx) = std::sync::mpsc::channel::<Result<Value, String>>();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let rows = if let Some(db) = crate::internal_hooks::DB.get() {
                    db.query_events(since, limit, kind.clone(), room_id.clone(), user_id).await
                } else { Vec::new() };
                let total = rows.len();
                let _ = tx.send(Ok(serde_json::json!({"events": rows, "total": total})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("persist.events timeout".to_string()))
        }
        "persist.rooms" => {
            let since = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
            let limit = args.get(1).and_then(|v| v.as_i64()).unwrap_or(100).clamp(1, 1000);
            let (tx, rx) = std::sync::mpsc::channel::<Result<Value, String>>();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let rows = if let Some(db) = crate::internal_hooks::DB.get() {
                    db.query_room_snapshots(since, limit).await
                } else { Vec::new() };
                let _ = tx.send(Ok(serde_json::json!({"rooms": rows, "total": rows.len()})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("persist.rooms timeout".to_string()))
        }
        "persist.touches" => {
            let round_id = args.get(0).and_then(|v| v.as_str()).ok_or_else(|| "round_uuid required".to_string())?.to_string();
            let player_id = args.get(1).and_then(|v| v.as_i64()).map(|v| v as i32).ok_or_else(|| "player_id required".to_string())?;
            let limit = args.get(2).and_then(|v| v.as_i64()).unwrap_or(1000).clamp(1, 10000);
            let (tx, rx) = std::sync::mpsc::channel::<Result<Value, String>>();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let rows = if let Some(db) = crate::internal_hooks::DB.get() {
                    db.query_touch_batches(&round_id, player_id, limit).await
                } else { Vec::new() };
                let _ = tx.send(Ok(serde_json::json!({"touches": rows, "total": rows.len()})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("persist.touches timeout".to_string()))
        }
        "persist.judges" => {
            let round_id = args.get(0).and_then(|v| v.as_str()).ok_or_else(|| "round_uuid required".to_string())?.to_string();
            let player_id = args.get(1).and_then(|v| v.as_i64()).map(|v| v as i32).ok_or_else(|| "player_id required".to_string())?;
            let limit = args.get(2).and_then(|v| v.as_i64()).unwrap_or(1000).clamp(1, 10000);
            let (tx, rx) = std::sync::mpsc::channel::<Result<Value, String>>();
            let s = Arc::clone(state);
            spawn_on_runtime(async move {
                let rows = if let Some(db) = crate::internal_hooks::DB.get() {
                    db.query_judge_batches(&round_id, player_id, limit).await
                } else { Vec::new() };
                let _ = tx.send(Ok(serde_json::json!({"judges": rows, "total": rows.len()})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("persist.judges timeout".to_string()))
        }
        "chart.by_phira_id" => {
            let chart_id = args
                .first()
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .ok_or_else(|| "chart_id required".to_string())?;
            use crate::server::Chart;
            let chart = state.phira_client.get_chart_by_phira_id(chart_id).await;
            match chart {
                Some(c) => serde_json::to_value(&c)
                    .map_err(|e| format!("serialize chart: {e}")),
                None => Err(format!("chart {chart_id} not found")),
            }
        }
        "records.by_phira_id" => {
            let record_id = args
                .first()
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .ok_or_else(|| "record_id required".to_string())?;
            use crate::server::Record;
            let record = state.phira_client.get_record_by_phira_id(record_id).await;
            match record {
                Some(r) => serde_json::to_value(&r)
                    .map_err(|e| format!("serialize record: {e}")),
                None => Err(format!("record {record_id} not found")),
            }
        }
        "uuid.v4" => {
            let uuid = uuid::Uuid::new_v4();
            Ok(serde_json::json!({"uuid": uuid.to_string()}))
        }
        "time.now" => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            Ok(serde_json::json!({"unix_ms": now}))
        }
        "env.vars" => {
            let prefix = args.first().and_then(|v| v.as_str()).unwrap_or("");
            let vars: std::collections::BTreeMap<String, String> = std::env::vars()
                .filter(|(k, _)| k.starts_with(prefix))
                .collect();
            Ok(serde_json::to_value(vars).unwrap_or_default())
        }
        _ => {
            // Fall through to the webapi dispatch which handles
            // HTTP/WIT-specific methods
            Err(format!("unknown method: {method}"))
        }
    }
}

/// Public wrapper for WIT host feature — includes capability enforcement.
pub fn server_state_query_for_host(
    state: &Arc<PlusServerState>,
    method: &str,
    args: &[Value],
) -> Result<Value, String> {
    // Capability enforcement: check if the caller has the required capability.
    if let Some(required) = crate::wasm_host_helpers::required_capability(method) {
        let defaults = crate::wasm_host_helpers::default_capabilities();
        if !defaults.contains(required) {
            return Err(format!(
                "method '{method}' requires capability '{required}', which is not in default capabilities"
            ));
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

/// Web API 状态查询（内置）
///
/// 方法前缀：
/// - `rooms.*`  → 房间只读查询
/// - `room.*`   → 房间写操作
/// - `user.*`   → 用户查询
/// - `send.*`   → 消息发送
/// - `http.*`   → HTTP 动态路由
/// - `sse.*`    → SSE 流
/// - `ban.*`    → 封禁管理
/// - `runtime.*` → 运行时诊断
fn server_state_query_dispatch(
    state: &Arc<PlusServerState>,
    method: &str,
    args: &[Value],
) -> Result<Value, String> {
    match method {
        "rooms.list" => {
            let rooms = read_lock!(state.rooms);
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
        "user_name" => {
            let user_id = args
                .first()
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let users = read_lock!(state.users);
            let user = users.get(&user_id);
            match user {
                Some(user) => {
                    let name = {
                        let session_guard = user.session.try_read().ok();
                        let session = session_guard
                            .and_then(|s| s.as_ref().and_then(|w| w.upgrade()));
                        match session {
                            Some(session) => session.name.clone(),
                            None => String::new(),
                        }
                    };
                    Ok(serde_json::json!({
                        "id": user_id,
                        "name": name,
                        "in_room": user.room.try_read().ok().and_then(|r| r.clone()).is_some(),
                    }))
                }
                None => Err(format!("user {user_id} not found")),
            }
        }
        "send_chat" => {
            let message = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "message required".to_string())?;
            state
                .plugin_manager
                .trigger(&PluginEvent::Chat {
                    user_id: 0,
                    content: message.to_string(),
                })
                .await;
            Ok(serde_json::json!({"ok": true}))
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
            let rooms = read_lock!(state.rooms);
            let room = rooms
                .values()
                .find(|r| r.id.to_string() == room_id)
                .ok_or_else(|| format!("room {room_id} not found"))?
                .clone();
            drop(rooms);
            room.send(phira_mp_common::Message::Chat {
                user: 0,
                content: message.to_string(),
            })
            .await;
            Ok(serde_json::json!({"ok": true}))
        }
        "rooms.by_user" => {
            let user_id = args
                .first()
                .and_then(|v| v.as_i64())
                .map(|v| v as i32)
                .ok_or_else(|| "user_id required".to_string())?;
            let users = read_lock!(state.users);
            let user = users.get(&user_id);
            match user {
                Some(user) => {
                    let room_id = read_lock!(user.room).as_ref().map(|r| r.id.to_string());
                    Ok(serde_json::json!({"user_id": user_id, "room_id": room_id}))
                }
                None => Err(format!("user {user_id} not found")),
            }
        }
        "http.register_route" => {
            let path = args
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| "path required".to_string())?;
            let plugin = args
                .get(1)
                .and_then(|v| v.as_str())
                .ok_or_else(|| "plugin name required".to_string())?;
            let path = path.to_string();
            let plugin_captured = plugin.to_string();
            let path_captured = path.clone();
            let pm = Arc::clone(&state.plugin_manager);
            let path_cloned = path.clone();
            let http_handle = pm.http_handle().ok_or_else(|| "http not enabled".to_string())?;
            let handler: api::HttpHandler = std::sync::Arc::new(
                move |_body: Option<serde_json::Value>, _params: Vec<String>| {
                    let pm = Arc::clone(&pm);
                    let plugin = plugin_captured.clone();
                    let method = path_captured.clone();
                    // This runs on the HTTP server's thread pool, so we need
                    // a tokio runtime to call_plugin_api.
                    match tokio::runtime::Handle::try_current() {
                        Ok(handle) => {
                            handle.block_on(async {
                                pm.call_plugin_api(&plugin, &method, _params.into_iter().map(serde_json::Value::String).collect())
                                    .await
                            })
                        }
                        Err(_) => {
                            let rt = tokio::runtime::Builder::new_current_thread()
                                .enable_all().build().expect("build temp runtime");
                            rt.block_on(async {
                                pm.call_plugin_api(&plugin, &method, _params.into_iter().map(serde_json::Value::String).collect())
                                    .await
                            })
                        }
                    }
                    .map_err(|e| (500u16, e))
                },
            );
            http_handle.register_route(&path_cloned, handler);
            Ok(serde_json::json!({"ok": true, "path": path}))
        }
        "sse.register_stream" => {
            let stream_type = args
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or("default");
            let (tx, _rx) = tokio::sync::mpsc::channel(64);
            let event_bus_rx = state.event_bus.subscribe();
            let stream_id = state
                .events
                .register(event_bus_rx, tx)
                .await;
            Ok(serde_json::json!({
                "ok": true,
                "stream_id": stream_id,
                "stream_type": stream_type,
            }))
        }
        "user.kick" => {
            // Server-level kick: remove from room + notify + delete session.
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
                        if room.on_user_leave(&user).await {
                            s.rooms.write().await.remove(&room_key);
                        }
                        if !was_monitor {
                            s.publish_room_event(RoomEvent::LeaveRoom {
                                room: room_key,
                                user: uid,
                            }).await;
                        }
                        s.event_bus.publish(crate::event_bus::MpEvent::PluginEventDispatched(
                            std::sync::Arc::new(PluginEvent::RoomLeave {
                                user_id: uid,
                                room_id,
                            }),
                        ));
                        room.send(Message::Chat {
                            user: 0,
                            content: format!("用户已被踢出 (reason: {reason})"),
                        }).await;
                    }
                    user.session
                        .write()
                        .await
                        .take()
                        .and_then(|w| w.upgrade())
                        .map(|session| session.disconnect());
                    serde_json::json!({"ok": true, "user_id": uid})
                }.await;
                let _ = tx.send(Ok(result));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("user.kick timeout".to_string()))
        }
        "admin.list" | "admin.check" | "admin.add" | "admin.remove" | "admin.set"
        | "ban.list" | "ban.add" | "ban.remove" | "ban.check" => {
            // Delegate to inner router for shared admin/ban methods
            server_state_query_inner(state, method, args)
        }
        _ => {
            // Fall through to inner router
            server_state_query_inner(state, method, args)
        }
    }
}

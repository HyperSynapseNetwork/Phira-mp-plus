//! 内置功能（欢迎语、玩家追踪、游玩统计）
use crate::plugin::PluginManager;
use crate::plugin_http::PluginHttpServer;
use crate::server::PlusServerState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use tracing::info;

/// 全局数据库管理器（保留静态用于未迁移的模块）
pub static DB: OnceLock<super::db::DbManager> = OnceLock::new();

const PLAYER_TRACKER_MAX_ENTRIES: usize = 50_000;
const PLAYTIME_CACHE_MAX_ENTRIES: usize = 50_000;

pub async fn init_internal_hooks(
    state: &PlusServerState,
    http: &PluginHttpServer,
    pm: &PluginManager,
) {
    // Set the static reference from the state's db_manager
    let _ = DB.set(state.db_manager.clone());
    // Load admin IDs from database if configured
    if let Some(ids) = state.db_manager.get_admin_ids().await {
        let mut guard = state.admin_ids.write().await;
        for id in ids {
            guard.insert(id);
        }
    }
    state
        .persistence_worker
        .record_runtime_config_snapshot()
        .await;

    init_welcome(state, pm).await;
    init_player_tracker(state, http, pm).await;
    init_playtime_tracker(state, http, pm).await;
    init_round_results(state, pm).await;
    info!("internal hooks initialized");
}

// ════════════════════════════════════
//  欢迎语
// ════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WelcomeConfig {
    messages: Vec<String>,
    show_time: bool,
    time_format: String,
}

impl Default for WelcomeConfig {
    fn default() -> Self {
        Self {
            messages: vec![
                "欢迎 [user_name] 来到 HSN Phira-mp+！当前在线 [player-count] 人。以-开头的房间会被隐藏。可以前往 https://phira.htadiy.com/ 使用更多相关功能哦。也欢迎加入我们的QQ交流群1049578201！".into(),
                "您在本服务器上游玩了[playtime]".into(),
                "--------------------------------------------------".into(),
                "游玩时间排行榜：[top_playtime]".into(),
                "--------------------------------------------------".into(),
                "活跃房间：[active_rooms]".into(),
            ],
            show_time: true,
            time_format: "%Y-%m-%d %H:%M".into(),
        }
    }
}

static WELCOME: once_cell::sync::Lazy<Arc<Mutex<WelcomeConfig>>> =
    once_cell::sync::Lazy::new(|| {
        let cfg = std::fs::read_to_string("data/welcome-config.json")
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Arc::new(Mutex::new(cfg))
    });

pub fn send_welcome(user_id: i32, user_name: &str, online: usize, state: &PlusServerState) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let cfg = WELCOME.lock().unwrap();
    for tpl in &cfg.messages {
        let mut text = tpl
            .replace("[user_name]", user_name)
            .replace("[user_id]", &user_id.to_string())
            .replace("[player-count]", &online.to_string())
            .replace("[players]", &online.to_string());
        if cfg.show_time {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            text = text.replace("[time]", &ts.to_string());
        }
        // [playtime <id>] → 指定用户（先处理，避免被 [playtime] 替换干扰）
        if text.contains("[playtime ") {
            let pt = PLAYTIME_DATA.lock().unwrap();
            while let Some(start) = text.find("[playtime ") {
                if let Some(end) = text[start..].find(']') {
                    let arg = text[start + 10..start + end].trim();
                    if let Ok(uid) = arg.parse::<i32>() {
                        let s = pt
                            .get(&uid)
                            .map(|e| {
                                e.total_secs
                                    + e.session_start
                                        .map(|s| {
                                            SystemTime::now()
                                                .duration_since(UNIX_EPOCH)
                                                .map(|d| d.as_secs())
                                                .unwrap_or(0)
                                                .saturating_sub(s)
                                        })
                                        .unwrap_or(0)
                            })
                            .unwrap_or(0);
                        text = text.replace(
                            &format!("[playtime {}]", uid),
                            &format!("{:.1}h", s as f64 / 3600.0),
                        );
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
        }
        // [playtime] → 当前用户的游玩时间
        if text.contains("[playtime]") {
            let pt = PLAYTIME_DATA.lock().unwrap();
            let secs = pt
                .get(&user_id)
                .map(|e| {
                    e.total_secs
                        + e.session_start
                            .map(|s| {
                                SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0)
                                    .saturating_sub(s)
                            })
                            .unwrap_or(0)
                })
                .unwrap_or(0);
            text = text.replace("[playtime]", &format!("{:.1}h", secs as f64 / 3600.0));
        }
        // [active_rooms] → 活跃房间详情
        if text.contains("[active_rooms]") {
            let rooms_guard = state.rooms.try_read();
            let room_list: Vec<String> = match rooms_guard {
                Ok(ref rooms) => {
                    let visible_rooms: Vec<_> = rooms
                        .iter()
                        .filter(|(_, room)| !room.is_hidden())
                        .take(10)
                        .collect();
                    if visible_rooms.is_empty() {
                        vec!["暂无房间".into()]
                    } else {
                        visible_rooms
                            .into_iter()
                            .map(|(id, room)| {
                                let host_name = room
                                    .host
                                    .try_read()
                                    .ok()
                                    .and_then(|h| h.upgrade())
                                    .map(|u| room.display_name_sync(&u))
                                    .unwrap_or_default();
                                let players =
                                    room.users.try_read().ok().map(|u| u.len()).unwrap_or(0);
                                let max = room.max_users_count();
                                let chart = room
                                    .chart
                                    .try_read()
                                    .ok()
                                    .and_then(|c| c.as_ref().map(|c| c.name.clone()))
                                    .unwrap_or_default();
                                let locked = room.locked.load(std::sync::atomic::Ordering::Relaxed);
                                let cycling = room.cycle.load(std::sync::atomic::Ordering::Relaxed);
                                let state_desc = room
                                    .state
                                    .try_read()
                                    .ok()
                                    .map(|s| match &*s {
                                        crate::room::InternalRoomState::SelectChart => "选曲中",
                                        crate::room::InternalRoomState::WaitForReady { .. } => {
                                            "等待准备"
                                        }
                                        crate::room::InternalRoomState::Playing { .. } => "游戏中",
                                    })
                                    .unwrap_or("?");
                                let mut flags = Vec::new();
                                if locked {
                                    flags.push("锁定");
                                }
                                if cycling {
                                    flags.push("循环");
                                }
                                let flag_str = if flags.is_empty() {
                                    String::new()
                                } else {
                                    format!(" [{}]", flags.join(","))
                                };
                                format!(
                                    "房间:{}{} 房主:{} [{}/{}] {}{}",
                                    id,
                                    flag_str,
                                    host_name,
                                    players,
                                    max,
                                    state_desc,
                                    if chart.is_empty() {
                                        String::new()
                                    } else {
                                        format!(" | {}", chart)
                                    }
                                )
                            })
                            .collect()
                    }
                }
                _ => vec!["暂无房间".into()],
            };
            text = text.replace("[active_rooms]", &room_list.join(" | "));
        }
        // [top_playtime] → 游玩时间排行榜（取前 10）
        if text.contains("[top_playtime]") {
            use std::time::{SystemTime, UNIX_EPOCH};
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let pt = PLAYTIME_DATA.lock().unwrap();
            let users_guard = state.users.try_read();
            let mut ranking: Vec<(i32, u64)> = pt
                .iter()
                .map(|(&uid, entry)| {
                    let total = entry.total_secs
                        + entry
                            .session_start
                            .map(|s| now.saturating_sub(s))
                            .unwrap_or(0);
                    (uid, total)
                })
                .collect();
            ranking.sort_by(|a, b| b.1.cmp(&a.1));
            let top: Vec<String> = ranking
                .iter()
                .take(10)
                .enumerate()
                .map(|(i, (uid, secs))| {
                    let name = users_guard
                        .as_ref()
                        .ok()
                        .and_then(|u| u.get(uid))
                        .map(|u| u.name.clone())
                        .unwrap_or_else(|| uid.to_string());
                    format!("#{} {}: {:.1}h", i + 1, name, *secs as f64 / 3600.0)
                })
                .collect();
            text = text.replace("[top_playtime]", &top.join(" | "));
        }
        if let Ok(users) = state.users.try_read() {
            if let Some(user) = users.get(&user_id) {
                if let Ok(session) = user.session.try_read() {
                    if let Some(Some(session)) = session.as_ref().map(|w| w.upgrade()) {
                        use phira_mp_common::{Message, ServerCommand};
                        if let Ok(handle) = tokio::runtime::Handle::try_current() {
                            let cmd = ServerCommand::Message(Message::Chat {
                                user: 0,
                                content: text,
                            });
                            handle.spawn(async move {
                                let _ = session.stream.send(cmd).await;
                            });
                        }
                    }
                }
            }
        }
    }
}

async fn init_welcome(_state: &PlusServerState, pm: &PluginManager) {
    save_json("data/welcome-config.json", &*WELCOME.lock().unwrap());
    let _ = pm
        .register_cli_command(crate::plugin::CliCommand {
            name: "welcome-config".into(),
            description: "查看欢迎语配置与占位符说明".into(),
            usage: "welcome-config".into(),
            handler: Arc::new(|_| {
                let cfg = WELCOME.lock().unwrap();
                let mut out = vec![
                    format!("  ◆ 欢迎语配置"),
                    format!("  │ data/welcome-config.json"),
                ];
                for (i, msg) in cfg.messages.iter().enumerate() {
                    out.push(format!("  │ [{i}] {msg}"));
                }
                out.push(format!(""));
                out.push(format!("  ■ 占位符:"));
                out.push(format!("  │ [user_name]    用户名"));
                out.push(format!("  │ [user_id]      Phira ID"));
                out.push(format!("  │ [player-count] 当前在线数"));
                out.push(format!("  │ [playtime]     该用户游玩时间"));
                out.push(format!("  │ [playtime <id>]指定用户游玩时间"));
                out.push(format!("  │ [active_rooms] 活跃房间数"));
                out
            }),
        })
        .await;
}

// ════════════════════════════════════
//  玩家追踪
// ════════════════════════════════════

static PLAYERS: once_cell::sync::Lazy<Mutex<HashMap<i32, String>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

/// 内存玩家追踪缓存数量（达到上限后会裁剪旧条目）
pub fn player_count() -> usize {
    PLAYERS.lock().unwrap().len()
}

/// 获取内存玩家追踪缓存 (id → name)
pub fn all_players() -> Vec<(i32, String)> {
    let guard = PLAYERS.lock().unwrap();
    guard.iter().map(|(&id, name)| (id, name.clone())).collect()
}

pub fn track_player(user_id: i32, user_name: &str) {
    let mut players = PLAYERS.lock().unwrap();
    players
        .entry(user_id)
        .or_insert_with(|| user_name.to_string());
    while players.len() > PLAYER_TRACKER_MAX_ENTRIES {
        let Some(remove_id) = players.keys().copied().find(|id| *id != user_id) else {
            break;
        };
        players.remove(&remove_id);
    }
}

async fn init_player_tracker(
    _state: &PlusServerState,
    http: &PluginHttpServer,
    pm: &PluginManager,
) {
    http.register_route_sync(
        "/api/players/count",
        Arc::new(|_, _| Ok(serde_json::json!({"count": PLAYERS.lock().unwrap().len()}))),
    );
    let _ = pm
        .register_cli_command(crate::plugin::CliCommand {
            name: "player-count".into(),
            description: "游玩过的玩家总数".into(),
            usage: "player-count".into(),
            handler: Arc::new(|_| vec![format!("  ◆ 玩家总数: {}", PLAYERS.lock().unwrap().len())]),
        })
        .await;
}

// ════════════════════════════════════
//  游玩时间统计
// ════════════════════════════════════

static PLAYTIME_DATA: once_cell::sync::Lazy<Mutex<HashMap<i32, PlaytimeEntry>>> =
    once_cell::sync::Lazy::new(|| {
        std::fs::read_to_string("data/playtime-tracker.json")
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    });

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PlaytimeEntry {
    total_secs: u64,
    session_start: Option<u64>,
}

pub fn playtime_connect(user_id: i32) {
    // PostgreSQL 双写
    if let Some(db) = DB.get() {
        db.set_online_sync(user_id);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut data = PLAYTIME_DATA.lock().unwrap();
    data.entry(user_id).or_default().session_start = Some(now);
    prune_playtime_cache(&mut data, user_id);
}

pub fn playtime_disconnect(user_id: i32) {
    // PostgreSQL 双写
    if let Some(db) = DB.get() {
        db.set_offline_sync(user_id);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut data = PLAYTIME_DATA.lock().unwrap();
    let mut changed = false;
    if let Some(entry) = data.get_mut(&user_id) {
        if let Some(start) = entry.session_start {
            entry.total_secs += now.saturating_sub(start);
            entry.session_start = None;
            changed = true;
        }
    }
    if changed {
        save_json("data/playtime-tracker.json", &*data);
    }
    prune_playtime_cache(&mut data, user_id);
}

fn prune_playtime_cache(data: &mut HashMap<i32, PlaytimeEntry>, keep_user_id: i32) {
    if !DB.get().is_some_and(|db| db.is_active()) {
        return;
    }
    while data.len() > PLAYTIME_CACHE_MAX_ENTRIES {
        let Some(remove_id) = data
            .iter()
            .find(|(id, entry)| **id != keep_user_id && entry.session_start.is_none())
            .map(|(id, _)| *id)
        else {
            break;
        };
        data.remove(&remove_id);
    }
}

async fn init_playtime_tracker(
    _state: &PlusServerState,
    _http: &PluginHttpServer,
    pm: &PluginManager,
) {
    let _ = pm
        .register_cli_command(crate::plugin::CliCommand {
            name: "playtime".into(),
            description: "查询用户游玩时间: playtime <用户ID>".into(),
            usage: "playtime <用户ID>".into(),
            handler: Arc::new(|args| {
                let uid: i32 = args.first().and_then(|a| a.parse().ok()).unwrap_or(0);
                let data = PLAYTIME_DATA.lock().unwrap();
                let secs = data
                    .get(&uid)
                    .map(|e| {
                        e.total_secs
                            + e.session_start
                                .map(|s| {
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_secs())
                                        .unwrap_or(0)
                                        .saturating_sub(s)
                                })
                                .unwrap_or(0)
                    })
                    .unwrap_or(0);
                vec![format!("  ◆ 用户 {uid}: {:.1}h", secs as f64 / 3600.0)]
            }),
        })
        .await;
}

// ════════════════════════════════════
//  结算排行
// ════════════════════════════════════

async fn init_round_results(_state: &PlusServerState, pm: &PluginManager) {
    let _ = pm
        .register_cli_command(crate::plugin::CliCommand {
            name: "round-last".into(),
            description: "查看房间最近一轮结算 (查询 room history)".into(),
            usage: "round-last <房间ID>".into(),
            handler: Arc::new(|args| {
                let room_id = args.first().cloned().unwrap_or_default();
                vec![format!("  ◆ round-last: use 'room history {room_id}'")]
            }),
        })
        .await;
}

// ════════════════════════════════════
//  辅助
// ════════════════════════════════════

fn save_json<T: Serialize>(path: &str, data: &T) {
    if let Ok(json) = serde_json::to_string_pretty(data) {
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(path, json).ok();
    }
}

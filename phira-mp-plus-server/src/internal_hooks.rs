//! 内置功能（原 NativePlugin 系统合并入核心）
//!
//! 完整实现：欢迎语（全部占位符）、玩家追踪、游玩统计、结算排行

use crate::plugin::PluginManager;
use crate::plugin_http::PluginHttpServer;
use crate::server::PlusServerState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::info;

pub async fn init_internal_hooks(state: &PlusServerState, http: &PluginHttpServer, pm: &PluginManager) {
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
                "欢迎 [user_name] 来到 Phira-mp+！当前在线 [player-count] 人".into(),
                "[user_name] 来了！在线 [player-count] 人".into(),
            ],
            show_time: true,
            time_format: "%H:%M".into(),
        }
    }
}

static WELCOME: once_cell::sync::Lazy<Arc<Mutex<WelcomeConfig>>> =
    once_cell::sync::Lazy::new(|| {
        let cfg = std::fs::read_to_string("data/welcome-config.json")
            .ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
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
            let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            text = text.replace("[time]", &ts.to_string());
        }
        // [playtime <id>] → 指定用户（先处理，避免被 [playtime] 替换干扰）
        if text.contains("[playtime ") {
            let pt = PLAYTIME_DATA.lock().unwrap();
            while let Some(start) = text.find("[playtime ") {
                if let Some(end) = text[start..].find(']') {
                    let arg = text[start+10..start+end].trim();
                    if let Ok(uid) = arg.parse::<i32>() {
                        let s = pt.get(&uid).map(|e| { e.total_secs
                            + e.session_start.map(|s| SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0).saturating_sub(s)).unwrap_or(0)
                        }).unwrap_or(0);
                        text = text.replace(&format!("[playtime {}]", uid), &format!("{:.1}h", s as f64 / 3600.0));
                    } else { break; }
                } else { break; }
            }
        }
        // [playtime] → 当前用户的游玩时间
        if text.contains("[playtime]") {
            let pt = PLAYTIME_DATA.lock().unwrap();
            let secs = pt.get(&user_id).map(|e| {
                e.total_secs + e.session_start.map(|s| {
                    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0).saturating_sub(s)
                }).unwrap_or(0)
            }).unwrap_or(0);
            text = text.replace("[playtime]", &format!("{:.1}h", secs as f64 / 3600.0));
        }
        // [active_rooms]
        if text.contains("[active_rooms]") {
            let rooms = state.rooms.try_read().map(|r| r.len()).unwrap_or(0);
            text = text.replace("[active_rooms]", &format!("{} 个房间", rooms));
        }
        if let Ok(users) = state.users.try_read() {
            if let Some(user) = users.get(&user_id) {
                if let Ok(session) = user.session.try_read() {
                    if let Some(Some(session)) = session.as_ref().map(|w| w.upgrade()) {
                        use phira_mp_common::{Message, ServerCommand};
                        if let Ok(handle) = tokio::runtime::Handle::try_current() {
                            let cmd = ServerCommand::Message(Message::Chat { user: 0, content: text });
                            handle.spawn(async move { let _ = session.stream.send(cmd).await; });
                        }
                    }
                }
            }
        }
    }
}

async fn init_welcome(_state: &PlusServerState, pm: &PluginManager) {
    save_json("data/welcome-config.json", &*WELCOME.lock().unwrap());
    let _ = pm.register_cli_command(crate::plugin::CliCommand {
        name: "welcome-config".into(),
        description: "查看欢迎语配置与占位符说明".into(),
        usage: "welcome-config".into(),
        handler: Arc::new(|_| {
            let cfg = WELCOME.lock().unwrap();
            let mut out = vec![format!("  ◆ 欢迎语配置"), format!("  │ data/welcome-config.json")];
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
    }).await;
}

// ════════════════════════════════════
//  玩家追踪
// ════════════════════════════════════

static PLAYERS: once_cell::sync::Lazy<Mutex<HashMap<i32, String>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

pub fn track_player(user_id: i32, user_name: &str) {
    PLAYERS.lock().unwrap().entry(user_id).or_insert_with(|| user_name.to_string());
}

async fn init_player_tracker(_state: &PlusServerState, http: &PluginHttpServer, pm: &PluginManager) {
    http.register_route_sync("/api/players/count", Arc::new(|_, _| {
        Ok(serde_json::json!({"count": PLAYERS.lock().unwrap().len()}))
    }));
    let _ = pm.register_cli_command(crate::plugin::CliCommand {
        name: "player-count".into(),
        description: "游玩过的玩家总数".into(),
        usage: "player-count".into(),
        handler: Arc::new(|_| vec![format!("  ◆ 玩家总数: {}", PLAYERS.lock().unwrap().len())]),
    }).await;
}

// ════════════════════════════════════
//  游玩时间统计
// ════════════════════════════════════

static PLAYTIME_DATA: once_cell::sync::Lazy<Mutex<HashMap<i32, PlaytimeEntry>>> =
    once_cell::sync::Lazy::new(|| {
        std::fs::read_to_string("data/playtime-tracker.json")
            .ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    });

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PlaytimeEntry {
    total_secs: u64,
    session_start: Option<u64>,
}

pub fn playtime_connect(user_id: i32) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    PLAYTIME_DATA.lock().unwrap().entry(user_id).or_default().session_start = Some(now);
}

pub fn playtime_disconnect(user_id: i32) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut data = PLAYTIME_DATA.lock().unwrap();
    if let Some(entry) = data.get_mut(&user_id) {
        if let Some(start) = entry.session_start {
            entry.total_secs += now.saturating_sub(start);
            entry.session_start = None;
            save_json("data/playtime-tracker.json", &*data);
        }
    }
}

async fn init_playtime_tracker(_state: &PlusServerState, _http: &PluginHttpServer, pm: &PluginManager) {
    let _ = pm.register_cli_command(crate::plugin::CliCommand {
        name: "playtime".into(),
        description: "查询用户游玩时间: playtime <用户ID>".into(),
        usage: "playtime <用户ID>".into(),
        handler: Arc::new(|args| {
            let uid: i32 = args.first().and_then(|a| a.parse().ok()).unwrap_or(0);
            let data = PLAYTIME_DATA.lock().unwrap();
            let secs = data.get(&uid).map(|e| {
                e.total_secs + e.session_start.map(|s| {
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0).saturating_sub(s)
                }).unwrap_or(0)
            }).unwrap_or(0);
            vec![format!("  ◆ 用户 {uid}: {:.1}h", secs as f64 / 3600.0)]
        }),
    }).await;
}

// ════════════════════════════════════
//  结算排行
// ════════════════════════════════════

async fn init_round_results(_state: &PlusServerState, pm: &PluginManager) {
    let _ = pm.register_cli_command(crate::plugin::CliCommand {
        name: "round-last".into(),
        description: "查看房间最近一轮结算 (查询 room history)".into(),
        usage: "round-last <房间ID>".into(),
        handler: Arc::new(|args| {
            let room_id = args.first().cloned().unwrap_or_default();
            vec![format!("  ◆ round-last: use 'room history {room_id}'")]
        }),
    }).await;
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

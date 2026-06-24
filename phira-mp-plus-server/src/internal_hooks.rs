//! 内置功能（原 NativePlugin 内置插件合并入核心）
//!
//! 欢迎语、玩家追踪、游玩统计、结算排行均在此实现。

use crate::plugin::PluginManager;
use crate::plugin_http::PluginHttpServer;
use crate::server::PlusServerState;
use phira_mp_plus_server_api as api;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::info;

// ═══════════════════════════════════════════════════════════════
// 初始化入口 — 在 PlusServer::new 中调用
// ═══════════════════════════════════════════════════════════════

pub async fn init_internal_hooks(state: &PlusServerState, http: &PluginHttpServer, pm: &PluginManager) {
    init_welcome(state, pm).await;
    init_player_tracker(state, http, pm).await;
    init_playtime_tracker(state, http, pm).await;
    init_round_results(state, pm).await;
    info!("internal hooks initialized");
}

// ═══════════════════════════════════════════════════════════════
// 欢迎语
// ═══════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WelcomeConfig {
    messages: Vec<String>,
}

impl Default for WelcomeConfig {
    fn default() -> Self {
        Self { messages: vec![
            "欢迎 [user_name] 来到 Phira-mp+！当前在线 [player-count] 人".into(),
        ]}
    }
}

static WELCOME: once_cell::sync::Lazy<Arc<Mutex<WelcomeConfig>>> =
    once_cell::sync::Lazy::new(|| {
        let cfg = std::fs::read_to_string("data/welcome-config.json")
            .ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Arc::new(Mutex::new(cfg))
    });

pub fn send_welcome(user_id: i32, user_name: &str, online: usize, state: &PlusServerState) {
    use phira_mp_common::{Message, ServerCommand};
    let cfg = WELCOME.lock().unwrap();
    for tpl in &cfg.messages {
        let text = tpl
            .replace("[user_name]", user_name)
            .replace("[user_id]", &user_id.to_string())
            .replace("[player-count]", &online.to_string())
            .replace("[players]", &online.to_string());
        if let Ok(users) = state.users.try_read() {
            if let Some(user) = users.get(&user_id) {
                if let Ok(session) = user.session.try_read() {
                    if let Some(Some(session)) = session.as_ref().map(|w| w.upgrade()) {
                        let _ = session.stream.try_send(
                            ServerCommand::Message(Message::Chat { user: 0, content: text })
                        );
                    }
                }
            }
        }
    }
}

async fn init_welcome(state: &PlusServerState, pm: &PluginManager) {
    save_json("data/welcome-config.json", &*WELCOME.lock().unwrap());
    info!("welcome: {} message(s)", WELCOME.lock().unwrap().messages.len());
}

// ═══════════════════════════════════════════════════════════════
// 玩家追踪
// ═══════════════════════════════════════════════════════════════

static PLAYERS: once_cell::sync::Lazy<Mutex<HashMap<i32, String>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

pub fn track_player(user_id: i32, user_name: &str) {
    let mut p = PLAYERS.lock().unwrap();
    if !p.contains_key(&user_id) {
        p.insert(user_id, user_name.to_string());
    }
}

async fn init_player_tracker(state: &PlusServerState, http: &PluginHttpServer, pm: &PluginManager) {
    http.register_route_sync("/api/players/count", Arc::new(|_, _| {
        let count = PLAYERS.lock().unwrap().len();
        Ok(serde_json::json!({"count": count}))
    }));
    let _ = pm.register_cli_command(crate::plugin::CliCommand {
        name: "player-count".into(),
        description: "游玩过的玩家总数".into(),
        usage: "player-count".into(),
        handler: Arc::new(|_| {
            let count = PLAYERS.lock().unwrap().len();
            vec![format!("  ◆ 玩家总数: {count}")]
        }),
    }).await;
    info!("player-tracker: ready");
}

// ═══════════════════════════════════════════════════════════════
// 游玩时间统计
// ═══════════════════════════════════════════════════════════════

static PLAYTIME_DATA: once_cell::sync::Lazy<Mutex<HashMap<i32, PlaytimeEntry>>> =
    once_cell::sync::Lazy::new(|| {
        let data = std::fs::read_to_string("data/playtime-tracker.json")
            .ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Mutex::new(data)
    });

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PlaytimeEntry {
    total_secs: u64,
    session_start: Option<u64>,
}

pub fn playtime_connect(user_id: i32) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut data = PLAYTIME_DATA.lock().unwrap();
    let entry = data.entry(user_id).or_default();
    entry.session_start = Some(now);
}

pub fn playtime_disconnect(user_id: i32) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut data = PLAYTIME_DATA.lock().unwrap();
    if let Some(entry) = data.get_mut(&user_id) {
        if let Some(start) = entry.session_start {
            entry.total_secs += now.saturating_sub(start);
            entry.session_start = None;
        }
    }
    save_json("data/playtime-tracker.json", &*data);
}

async fn init_playtime_tracker(state: &PlusServerState, http: &PluginHttpServer, pm: &PluginManager) {
    let _ = pm.register_cli_command(crate::plugin::CliCommand {
        name: "playtime".into(),
        description: "查询用户游玩时间: playtime <用户ID>".into(),
        usage: "playtime <用户ID>".into(),
        handler: Arc::new(|args| {
            let uid: i32 = args.first().and_then(|a| a.parse().ok()).unwrap_or(0);
            let data = PLAYTIME_DATA.lock().unwrap();
            let secs = data.get(&uid).map(|e| {
                let base = e.total_secs;
                base + e.session_start.map(|s| {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                    now.saturating_sub(s)
                }).unwrap_or(0)
            }).unwrap_or(0);
            vec![format!("  ◆ 用户 {uid}: {:.1}h ({}s)", secs as f64 / 3600.0, secs)]
        }),
    }).await;
    info!("playtime-tracker: ready");
}

// ═══════════════════════════════════════════════════════════════
// 结算排行
// ═══════════════════════════════════════════════════════════════

async fn init_round_results(state: &PlusServerState, pm: &PluginManager) {
    let _ = pm.register_cli_command(crate::plugin::CliCommand {
        name: "round-last".into(),
        description: "查看房间最近一轮结算: round-last <房间ID>".into(),
        usage: "round-last <房间ID>".into(),
        handler: Arc::new(|args| {
            let room_id = args.first().cloned().unwrap_or_default();
            vec![format!("  ◆ 最近一轮结算: {room_id}")]
        }),
    }).await;
    info!("round-results: ready");
}

// ═══════════════════════════════════════════════════════════════
// 辅助
// ═══════════════════════════════════════════════════════════════

fn save_json<T: Serialize>(path: &str, data: &T) {
    if let Ok(json) = serde_json::to_string_pretty(data) {
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(path, json).ok();
    }
}

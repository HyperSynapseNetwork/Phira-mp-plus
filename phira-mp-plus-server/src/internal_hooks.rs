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

/// PostgreSQL-backed playtime cache (single source of truth).
/// Maps user_id → effective total seconds (includes current session contribution).
/// Loaded on startup from the `playtime` table.
static PLAYTIME_CACHE: OnceLock<Mutex<HashMap<i32, i64>>> = OnceLock::new();

fn ensure_playtime_cache() -> &'static Mutex<HashMap<i32, i64>> {
    PLAYTIME_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Load the playtime cache from PostgreSQL on startup.
async fn load_playtime_cache(state: &PlusServerState) {
    let rows = state.db_manager.top_playtime(100000).await;
    let mut cache = ensure_playtime_cache().lock().unwrap();
    for row in rows {
        if let (Some(user_id), Some(secs)) = (
            row.get("user_id").and_then(|v| v.as_i64()),
            row.get("total_playtime").and_then(|v| v.as_i64()),
        ) {
            cache.insert(user_id as i32, secs);
        }
    }
    info!("playtime cache loaded with {} entries", cache.len());
}

pub async fn init_internal_hooks(
    state: &PlusServerState,
    http: &Option<Arc<PluginHttpServer>>,
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

    // Load the playtime cache from the authoritative PostgreSQL table.
    load_playtime_cache(state).await;

    init_welcome(state, pm).await;
    init_player_tracker(state, http, pm).await;
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

/// 预编译的欢迎语片段，避免每次连接时做字符串 replace。
#[derive(Debug, Clone)]
enum WelcomeSegment {
    /// 纯静态文本
    Static(String),
    /// [user_name]
    UserName,
    /// [user_id]
    UserId,
    /// [player-count] 或 [players]
    PlayerCount,
    /// [time]（仅当 show_time = true）
    Time,
    /// [playtime] — 当前用户
    Playtime,
    /// [playtime <id>] — 指定用户
    PlaytimeId(i32),
    /// [active_rooms]
    ActiveRooms,
    /// [top_playtime]
    TopPlaytime,
}

static WELCOME: once_cell::sync::Lazy<Arc<Mutex<WelcomeConfig>>> =
    once_cell::sync::Lazy::new(|| {
        let cfg = std::fs::read_to_string("data/welcome-config.json")
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Arc::new(Mutex::new(cfg))
    });

/// 预编译的欢迎语模板（一次启动时编译，避免每次连接做字符串搜索和 replace）。
static WELCOME_SEGMENTS: once_cell::sync::Lazy<Arc<Mutex<Vec<Vec<WelcomeSegment>>>>> =
    once_cell::sync::Lazy::new(|| {
        let cfg = WELCOME.lock().unwrap();
        Arc::new(Mutex::new(
            cfg.messages
                .iter()
                .map(|msg| compile_template(msg, cfg.show_time))
                .collect(),
        ))
    });

/// 将单个模板字符串预编译为片段列表。
/// 运行时只需遍历片段拼接动态值，无需扫描 / replace。
fn compile_template(tpl: &str, show_time: bool) -> Vec<WelcomeSegment> {
    let mut segs: Vec<WelcomeSegment> = Vec::new();
    let mut pos = 0;
    while pos < tpl.len() {
        if tpl.as_bytes()[pos] != b'[' {
            // 快速扫描到下一个 '[' 或末尾
            let next = tpl[pos..]
                .find('[')
                .map(|p| pos + p)
                .unwrap_or(tpl.len());
            segs.push(WelcomeSegment::Static(tpl[pos..next].to_string()));
            pos = next;
            continue;
        }

        let rest = &tpl[pos..];

        // [playtime N] — 必须在 [playtime] 之前检查
        if rest.starts_with("[playtime ") {
            if let Some(end) = rest.find(']') {
                let arg = rest[10..end].trim();
                if let Ok(uid) = arg.parse::<i32>() {
                    segs.push(WelcomeSegment::PlaytimeId(uid));
                    pos += end + 1;
                    continue;
                }
                // 解析失败，视为静态文本
                segs.push(WelcomeSegment::Static(rest[..end + 1].to_string()));
                pos += end + 1;
                continue;
            }
        }

        // 按长度降序检查已知占位符（避免前缀误匹配）
        macro_rules! try_placeholder {
            ($pat:expr, $seg:expr) => {
                if rest.starts_with($pat) {
                    segs.push($seg);
                    pos += $pat.len();
                    continue;
                }
            };
        }

        try_placeholder!("[player-count]", WelcomeSegment::PlayerCount);
        try_placeholder!("[active_rooms]", WelcomeSegment::ActiveRooms);
        try_placeholder!("[top_playtime]", WelcomeSegment::TopPlaytime);
        try_placeholder!("[user_name]", WelcomeSegment::UserName);
        try_placeholder!("[user_id]", WelcomeSegment::UserId);
        try_placeholder!("[players]", WelcomeSegment::PlayerCount);
        try_placeholder!("[playtime]", WelcomeSegment::Playtime);
        if show_time {
            try_placeholder!("[time]", WelcomeSegment::Time);
        }

        // 不是已知占位符，将 '[' 视为普通字符
        segs.push(WelcomeSegment::Static("[".to_string()));
        pos += 1;
    }
    segs
}

pub fn send_welcome(user_id: i32, user_name: &str, online: usize, state: &PlusServerState) -> usize {
    use std::time::{SystemTime, UNIX_EPOCH};
    let compiled = WELCOME_SEGMENTS.lock().unwrap();
    let mut texts: Vec<String> = Vec::with_capacity(compiled.len());

    for segments in compiled.iter() {
        let mut text = String::new();
        for segment in segments {
            match segment {
                WelcomeSegment::Static(s) => text.push_str(s),
                WelcomeSegment::UserName => text.push_str(user_name),
                WelcomeSegment::UserId => text.push_str(&user_id.to_string()),
                WelcomeSegment::PlayerCount => text.push_str(&online.to_string()),
                WelcomeSegment::Time => {
                    let ts = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    text.push_str(&ts.to_string());
                }
                WelcomeSegment::Playtime => {
                    let pt = ensure_playtime_cache().lock().unwrap();
                    let secs = pt.get(&user_id).copied().unwrap_or(0);
                    text.push_str(&format!("{:.1}h", secs as f64 / 3600.0));
                }
                WelcomeSegment::PlaytimeId(uid) => {
                    let pt = ensure_playtime_cache().lock().unwrap();
                    let secs = pt.get(uid).copied().unwrap_or(0);
                    text.push_str(&format!("{:.1}h", secs as f64 / 3600.0));
                }
                WelcomeSegment::ActiveRooms => {
                    let rooms_guard = state.rooms.try_read();
                    let room_list: Vec<String> = match rooms_guard {
                        Ok(ref rooms) => {
                            let visible_rooms: Vec<_> = rooms
                                .iter()
                                .filter(|(_, room)| !room.control_snapshot().hidden)
                                .take(10)
                                .collect();
                            if visible_rooms.is_empty() {
                                vec!["暂无房间".into()]
                            } else {
                                let users_guard = state.users.try_read();
                                visible_rooms
                                    .into_iter()
                                    .map(|(id, room)| {
                                        let control = room.control_snapshot();
                                        let host_name: String = control
                                            .host_id
                                            .and_then(|hid| {
                                                users_guard
                                                    .as_ref()
                                                    .ok()
                                                    .and_then(|g| g.get(&hid))
                                                    .map(|u| u.name.clone())
                                            })
                                            .unwrap_or_default();
                                        let players =
                                            room.users.try_read().ok().map(|u| u.len()).unwrap_or(0);
                                        let max = control.max_users;
                                        let locked = control.locked;
                                        let cycling = control.cycle;
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
                                            "{}{}(房主:{} [{}/{}])",
                                            id, flag_str, host_name, players, max,
                                        )
                                    })
                                    .collect()
                            }
                        }
                        _ => vec!["暂无房间".into()],
                    };
                    text.push_str(&room_list.join("; "));
                }
                WelcomeSegment::TopPlaytime => {
                    let pt = ensure_playtime_cache().lock().unwrap();
                    let users_guard = state.users.try_read();
                    let mut ranking: Vec<(i32, i64)> = pt.iter().map(|(&uid, &secs)| (uid, secs)).collect();
                    ranking.sort_by(|a, b| b.1.cmp(&a.1));
                    let top: Vec<String> = ranking
                        .iter()
                        .take(10)
                        .enumerate()
                        .map(|(i, (uid, secs))| {
                            let name = users_guard
                                .as_ref()
                                .ok()
                                .and_then(|g| g.get(uid))
                                .map(|u| u.name.clone())
                                .or_else(|| {
                                    PLAYERS.lock().unwrap().get(uid).cloned()
                                    // Missing usernames are prefetched in background when
                                    // a user connects (track_player is called on auth).
                                })
                                .unwrap_or_default();
                            format!("#{} {}: {:.1}h", i + 1, name, *secs as f64 / 3600.0)
                        })
                        .collect();
                    text.push_str(&top.join(" | "));
                }
            }
        }
        texts.push(text);
    }

    let line_count = texts.len();

    // Send all welcome messages in order in a single spawned task.
    // Previously each message was spawned individually, which caused
    // non-deterministic send order when tasks ran concurrently.
    if let Ok(users) = state.users.try_read() {
        if let Some(user) = users.get(&user_id) {
            if let Ok(session) = user.session.try_read() {
                if let Some(Some(session)) = session.as_ref().map(|w| w.upgrade()) {
                    use phira_mp_common::{Message, ServerCommand};
                    if let Ok(handle) = tokio::runtime::Handle::try_current() {
                        handle.spawn(async move {
                            for content in texts {
                                let cmd =
                                    ServerCommand::Message(Message::Chat { user: 0, content });
                                let _ = session.stream.send(cmd).await;
                            }
                        });
                        // Background prefetch missing usernames for playtime leaderboard.
                        let client = Arc::clone(&state.phira_client);
                        let endpoint = state.config.phira_api_endpoint.clone();
                        handle.spawn(async move {
                            let uids: Vec<i32> = {
                                let pt = ensure_playtime_cache().lock().unwrap();
                                pt.keys().copied().take(50).collect()
                            };
                            for uid in uids {
                                {
                                    let guard = PLAYERS.lock().unwrap();
                                    if guard.contains_key(&uid) { continue; }
                                }
                                if let Some(name) = client.fetch_user_by_id(&endpoint, uid).await {
                                    track_player(uid, &name);
                                }
                            }
                        });
                    }
                }
            }
        }
    }
    line_count
}

async fn init_welcome(_state: &PlusServerState, pm: &PluginManager) {
    save_json("data/welcome-config.json", &*WELCOME.lock().unwrap());
    // 强制在启动时预编译 segments（而非等到首次连接时）
    drop(WELCOME_SEGMENTS.lock().unwrap());
    let _ = pm
        .register_cli_command(crate::plugin::CliCommand {
            name: "welcome-config".into(),
            description: "查看欢迎语配置与占位符说明".into(),
            usage: "welcome-config".into(),
            handler: Arc::new(|_| {
                let cfg = WELCOME.lock().unwrap();
                let mut out = vec![
                    "  ◆ 欢迎语配置".to_string(),
                    "  │ data/welcome-config.json".to_string(),
                ];
                for (i, msg) in cfg.messages.iter().enumerate() {
                    out.push(format!("  │ [{i}] {msg}"));
                }
                out.push(String::new());
                out.push("  ■ 占位符:".to_string());
                out.push("  │ [user_name]    用户名".to_string());
                out.push("  │ [user_id]      Phira ID".to_string());
                out.push("  │ [player-count] 当前在线数".to_string());
                out.push("  │ [playtime]     该用户游玩时间".to_string());
                out.push("  │ [playtime <id>]指定用户游玩时间".to_string());
                out.push("  │ [active_rooms] 活跃房间数".to_string());
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
    _http: &Option<Arc<PluginHttpServer>>,
    pm: &PluginManager,
) {
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
//  游玩时间统计 (removed — single source of truth is PostgreSQL via PersistenceWorker)
// ════════════════════════════════════

/// No-op: playtime is tracked purely through UserOnline/UserOffline persistence
/// events updating the PostgreSQL `playtime` table.
pub fn playtime_connect(_user_id: i32) {}

/// No-op: playtime is tracked purely through UserOnline/UserOffline persistence
/// events updating the PostgreSQL `playtime` table.
pub fn playtime_room_enter(_user_id: i32) {}

/// No-op: playtime is tracked purely through UserOnline/UserOffline persistence
/// events updating the PostgreSQL `playtime` table.
pub fn playtime_room_leave(_user_id: i32) {}

/// No-op: playtime is tracked purely through UserOnline/UserOffline persistence
/// events updating the PostgreSQL `playtime` table. The UserOffline persistence
/// event handles the actual playtime accumulation.
pub fn playtime_disconnect(_user_id: i32) {}

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

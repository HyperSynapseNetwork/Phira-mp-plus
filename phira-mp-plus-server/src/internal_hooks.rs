//! 内置功能（欢迎语、玩家追踪、游玩统计）
use crate::l10n::Language;
use crate::plugin::PluginManager;
use crate::plugin_http::PluginHttpServer;
use crate::server::PlusServerState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::{info, warn};

/// 全局数据库管理器（保留静态用于未迁移的模块）
pub static DB: OnceLock<super::db::DbManager> = OnceLock::new();

const PLAYER_TRACKER_MAX_ENTRIES: usize = 50_000;

/// PostgreSQL-backed playtime cache (single source of truth).
/// Maps user_id → effective total seconds (includes current session contribution).
/// Loaded on startup from the `playtime` table.
static PLAYTIME_CACHE: OnceLock<Mutex<HashMap<i32, i64>>> = OnceLock::new();
static PLAYTIME_CACHE_LAST_REFRESH: OnceLock<Mutex<Instant>> = OnceLock::new();

fn ensure_playtime_cache() -> &'static Mutex<HashMap<i32, i64>> {
    PLAYTIME_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn ensure_playtime_cache_last_refresh() -> &'static Mutex<Instant> {
    PLAYTIME_CACHE_LAST_REFRESH.get_or_init(|| Mutex::new(Instant::now()))
}

/// Load the playtime cache from PostgreSQL on startup.
async fn load_playtime_cache(state: &PlusServerState) {
    let rows = state.db_manager.top_playtime(100000).await;
    let hide = &state.config.playtime_leaderboard_hide;
    let mut cache = ensure_playtime_cache().lock().unwrap();
    for row in rows {
        if let (Some(user_id), Some(secs)) = (
            row.get("user_id").and_then(|v| v.as_i64()),
            row.get("total_playtime").and_then(|v| v.as_i64()),
        ) {
            let uid = user_id as i32;
            if hide.contains(&uid) {
                continue; // 排行榜过滤用户（测试站 Bot 等）
            }
            cache.insert(uid, secs);
        }
    }
    *ensure_playtime_cache_last_refresh().lock().unwrap() = Instant::now();
    info!("playtime cache loaded with {} entries", cache.len());
}

/// Refresh the playtime cache from PostgreSQL using the static DB handle.
/// Called periodically to keep the cache fresh. `hide` = 排行榜过滤用户。
async fn refresh_playtime_cache(hide: &[i32]) {
    let Some(db) = DB.get() else {
        warn!("playtime cache refresh: DB not initialized");
        return;
    };
    let rows = db.top_playtime(100000).await;
    let mut cache = ensure_playtime_cache().lock().unwrap();
    cache.clear();
    for row in rows {
        if let (Some(user_id), Some(secs)) = (
            row.get("user_id").and_then(|v| v.as_i64()),
            row.get("total_playtime").and_then(|v| v.as_i64()),
        ) {
            let uid = user_id as i32;
            if hide.contains(&uid) {
                continue;
            }
            cache.insert(uid, secs);
        }
    }
    *ensure_playtime_cache_last_refresh().lock().unwrap() = Instant::now();
    info!("playtime cache refreshed with {} entries", cache.len());
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

    // Spawn periodic playtime cache refresh every 60s.
    let hide = state.config.playtime_leaderboard_hide.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            refresh_playtime_cache(&hide).await;
        }
    });

    // Periodic login_count reconciliation against mp_user_visits (hourly).
    // Keeps the aggregate counter aligned with the idempotent visit ledger.
    let reconcile_db = state.db_manager.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            match reconcile_db.reconcile_login_counts().await {
                Ok(0) => {}
                Ok(n) => info!("reconciled {n} user login_count(s) against mp_user_visits"),
                Err(e) => warn!(error = %e, "login_count reconciliation failed"),
            }
        }
    });

    init_welcome(state, pm).await;
    init_player_tracker(state, http, pm).await;
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
            // 空 messages = 使用内置国际化（l10n）欢迎语模板；仅当 l10n 不可用时
            // 才回退到内置默认文本（见 builtin_welcome_messages）。
            messages: vec![],
            show_time: true,
            time_format: "%Y-%m-%d %H:%M".into(),
        }
    }
}

/// 内置默认欢迎语模板（最终回退，中文）。三语默认模板位于 locales/*.ftl 的
/// `welcome-message` 键。
const FALLBACK_WELCOME_MESSAGES: &[&str] = &[
    "欢迎 [user_name] 来到 HSN Phira-mp+！当前在线 [player-count] 人。以-开头的房间会被隐藏，可以进入游戏中的房间哦。可以前往 https://phira.htadiy.com/ 使用更多相关功能哦。也欢迎加入我们的QQ交流群1049578201！",
    "您在本服务器上游玩了[playtime]",
    "--------------------------------------------------",
    "游玩时间排行榜：[top_playtime]",
    "--------------------------------------------------",
    "活跃房间：[active_rooms]",
];

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

/// 解析欢迎语专用 .ftl 文件中的 `welcome-message` 键（单行值，`\n` 分隔多行）。
fn parse_welcome_ftl(content: &str) -> Option<Vec<String>> {
    for line in content.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("welcome-message") {
            let rest = rest.trim_start();
            if let Some(val) = rest.strip_prefix('=') {
                let val = val.trim();
                if !val.is_empty() {
                    return Some(val.split("\\n").map(|s| s.to_string()).collect());
                }
            }
        }
    }
    None
}

/// 按语言获取欢迎语模板消息列表。
///
/// 优先级：管理员 welcome-config.json 显式 messages > 可选 `welcome_ftl_dir`
/// 下的 `welcome-{lang}.ftl` > 内置 l10n `welcome-message` 键 > 内置默认文本。
fn welcome_messages_for(lang: &Language, state: &PlusServerState) -> Vec<String> {
    let cfg = WELCOME.lock().unwrap();
    if !cfg.messages.is_empty() {
        return cfg.messages.clone();
    }
    if let Some(dir) = &state.config.welcome_ftl_dir {
        let lang_tag = lang.0.to_string();
        let path = std::path::Path::new(dir).join(format!("welcome-{lang_tag}.ftl"));
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Some(msgs) = parse_welcome_ftl(&content) {
                return msgs;
            }
        }
    }
    let template = crate::l10n::translate_system(lang, "welcome-message", &fluent::FluentArgs::new());
    // l10n 缺失时 translate_system 返回键名本身。
    if !template.is_empty() && template != "welcome-message" {
        return template
            .split('\n')
            .map(|s| s.trim_end().to_string())
            .collect();
    }
    FALLBACK_WELCOME_MESSAGES.iter().map(|s| s.to_string()).collect()
}

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
    let lang = state
        .users
        .try_read()
        .ok()
        .and_then(|g| g.get(&user_id).map(|u| u.lang.clone()))
        .unwrap_or_default();
    let show_time = WELCOME.lock().unwrap().show_time;
    let messages = welcome_messages_for(&lang, state);
    let compiled: Vec<Vec<WelcomeSegment>> = messages
        .iter()
        .map(|msg| compile_template(msg, show_time))
        .collect();
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
                    let mut args = fluent::FluentArgs::new();
                    args.set("hours", format!("{:.1}", secs as f64 / 3600.0));
                    text.push_str(&crate::l10n::translate_system(
                        &lang,
                        "welcome-playtime-value",
                        &args,
                    ));
                }
                WelcomeSegment::PlaytimeId(uid) => {
                    let pt = ensure_playtime_cache().lock().unwrap();
                    let secs = pt.get(uid).copied().unwrap_or(0);
                    let mut args = fluent::FluentArgs::new();
                    args.set("hours", format!("{:.1}", secs as f64 / 3600.0));
                    text.push_str(&crate::l10n::translate_system(
                        &lang,
                        "welcome-playtime-value",
                        &args,
                    ));
                }
                WelcomeSegment::ActiveRooms => {
                    let no_rooms =
                        crate::l10n::translate_system(&lang, "welcome-no-rooms", &fluent::FluentArgs::new());
                    let rooms_guard = state.rooms.try_read();
                    let room_list: Vec<String> = match rooms_guard {
                        Ok(ref rooms) => {
                            let visible_rooms: Vec<_> = rooms
                                .iter()
                                .filter(|(_, room)| !room.control_snapshot().hidden)
                                .take(10)
                                .collect();
                            if visible_rooms.is_empty() {
                                vec![no_rooms]
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
                                            flags.push(crate::l10n::translate_system(
                                                &lang,
                                                "welcome-locked",
                                                &fluent::FluentArgs::new(),
                                            ));
                                        }
                                        if cycling {
                                            flags.push(crate::l10n::translate_system(
                                                &lang,
                                                "welcome-cycling",
                                                &fluent::FluentArgs::new(),
                                            ));
                                        }
                                        let flag_str = if flags.is_empty() {
                                            String::new()
                                        } else {
                                            format!(" [{}]", flags.join(","))
                                        };
                                        let mut args = fluent::FluentArgs::new();
                                        args.set("room", id.to_string());
                                        args.set("flags", &flag_str);
                                        args.set("host", &host_name);
                                        args.set("players", players as i64);
                                        args.set("max", max as i64);
                                        crate::l10n::translate_system(
                                            &lang,
                                            "welcome-room-line",
                                            &args,
                                        )
                                    })
                                    .collect()
                            }
                        }
                        _ => vec![no_rooms],
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
                            let mut args = fluent::FluentArgs::new();
                            args.set("rank", (i + 1) as i64);
                            args.set("name", &name);
                            args.set("hours", format!("{:.1}", *secs as f64 / 3600.0));
                            crate::l10n::translate_system(&lang, "welcome-rank-line", &args)
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
            if let Ok(binding) = user.binding.try_read() {
                if let Some(Some(session)) = binding.session.as_ref().map(|w| w.upgrade()) {
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
                            // 按时长排序取 top-10（对齐排行榜显示），确保 top 用户
                            // 名字被预取——原实现 HashMap 无序 take(50) 会漏掉 top-10。
                            let mut uids: Vec<(i32, i64)> = {
                                let pt = ensure_playtime_cache().lock().unwrap();
                                pt.iter().map(|(&uid, &secs)| (uid, secs)).collect()
                            };
                            uids.sort_by(|a, b| b.1.cmp(&a.1));
                            for (uid, _) in uids.iter().take(10) {
                                {
                                    let guard = PLAYERS.lock().unwrap();
                                    if guard.contains_key(uid) { continue; }
                                }
                                if let Some(name) = client.fetch_user_by_id(&endpoint, *uid).await {
                                    track_player(*uid, &name);
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
                if cfg.messages.is_empty() {
                    out.push("  │ （未配置 → 使用内置国际化默认欢迎语，按用户语言）".to_string());
                    out.push("  │ 自定义：编辑 welcome-config.json，或配置 welcome_ftl_dir".to_string());
                }
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
//  游玩时间统计（房间内时间）
// ════════════════════════════════════

/// 进房时间追踪：user_id → 进房时间戳（毫秒 epoch）。游玩时长按「在房间内」
/// 的时间统计（非连接时长）：进房记录，离开/断开时累加到 playtime 表。
static ROOM_ENTER_AT: once_cell::sync::Lazy<Mutex<HashMap<i32, i64>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

fn now_ms_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 连接建立：无需记录（游玩时间只算房间内）。
pub fn playtime_connect(_user_id: i32) {}

/// 进入房间：记录进房时间。
pub fn playtime_room_enter(user_id: i32, _state: &PlusServerState) {
    ROOM_ENTER_AT.lock().unwrap().insert(user_id, now_ms_epoch());
}

/// 离开房间：把在房时长累加到 playtime 表（异步落库，允许近似）。
/// 幂等：未在房（如服务器重启丢失记录）时直接跳过。
pub fn playtime_room_leave(user_id: i32, state: &PlusServerState) {
    let start = ROOM_ENTER_AT.lock().unwrap().remove(&user_id);
    if let Some(start) = start {
        let secs = ((now_ms_epoch() - start) / 1000).max(0);
        if secs > 0 {
            let db = state.db_manager.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    db.add_playtime_seconds(user_id, secs).await;
                });
            }
        }
    }
}

/// 断开连接：等同于离开房间（若仍在房）。
pub fn playtime_disconnect(user_id: i32, state: &PlusServerState) {
    playtime_room_leave(user_id, state);
}

// ════════════════════════════════════
//  结算排行
// ════════════════════════════════════


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

//! Phira-mp+ 欢迎语插件
//!
//! 用户连接服务器时发送可配置的欢迎语。
//! 支持占位符：[player-count], [players], [playtime <user_id>], [rank <user_id>]
//! 配置文件路径：data/welcome-config.json

use phira_mp_plus_server_api::{
    NativePlugin, PluginContext, PluginEvent, PluginInfo,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::path::Path;
use tracing::{info, warn};

/// 欢迎语配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WelcomeConfig {
    /// 用户连接时的欢迎消息列表（每次随机选一条发送）
    pub welcome_messages: Vec<String>,
    /// 是否启用时间显示
    pub show_time: bool,
    /// 时间格式（如 "%Y-%m-%d %H:%M"）
    pub time_format: String,
}

impl Default for WelcomeConfig {
    fn default() -> Self {
        Self {
            welcome_messages: vec![
                "欢迎 [user_name] 来到 Phira-mp+！当前在线 [player-count] 人".into(),
                "[user_name] 来了！在线 [player-count] 人".into(),
            ],
            show_time: true,
            time_format: "%Y-%m-%d %H:%M".into(),
        }
    }
}

/// 当前配置路径
const CONFIG_PATH: &str = "data/welcome-config.json";

pub struct WelcomePlugin {
    config: Arc<Mutex<WelcomeConfig>>,
}

impl WelcomePlugin {
    pub fn create() -> Box<dyn NativePlugin> {
        // 从文件加载配置，不存在则使用默认
        let config = load_config().unwrap_or_default();
        Box::new(WelcomePlugin { config: Arc::new(Mutex::new(config)) })
    }
}

fn load_config() -> Option<WelcomeConfig> {
    let path = Path::new(CONFIG_PATH);
    if path.exists() {
        std::fs::read_to_string(path).ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    } else {
        None
    }
}

fn save_config(config: &WelcomeConfig) {
    if let Ok(json) = serde_json::to_string_pretty(config) {
        let path = Path::new(CONFIG_PATH);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(path, json).ok();
    }
}

/// 替换占位符
/// 通过 state 查询将用户 ID 转为用户名
fn resolve_name(uid: i32, ctx: &PluginContext) -> String {
    if uid == 0 { return "系统".into(); }
    if let Some(st) = &ctx.state {
        if let Ok(v) = st.call("user_name", &[serde_json::json!(uid)]) {
            if let Some(n) = v.get("name").and_then(|n| n.as_str()) {
                return n.to_string();
            }
        }
    }
    format!("{}", uid)
}

fn replace_placeholders(template: &str, user_id: i32, user_name: &str, user_ip: &str, ctx: &PluginContext) -> String {
    let mut text = template
        .replace("[user_id]", &user_id.to_string())
        .replace("[user_name]", user_name)
        .replace("[user_ip]", user_ip);

    // [player-count] → 通过 api 查询 player-tracker
    if text.contains("[player-count]") || text.contains("[players]") {
        if let Some(api) = &ctx.api {
            let r = api.call("player-tracker", "count", &[]);
            let cnt = r.ok().and_then(|v| v.get("count").and_then(|c| c.as_u64())).unwrap_or(0);
            text = text.replace("[player-count]", &cnt.to_string());
            text = text.replace("[players]", &cnt.to_string());
        } else {
            text = text.replace("[player-count]", "?");
            text = text.replace("[players]", "?");
        }
    }

    // [top_playtime] → 通过 api 查询排行榜（锁定前 10）
    if text.contains("[top_playtime]") {
        if let Some(api) = &ctx.api {
            let r = api.call("playtime-tracker", "leaderboard", &[Value::Number(10.into())]);
            let board = r.ok().and_then(|v| v.get("data").cloned()).unwrap_or(Value::Array(vec![]));
            let lines: Vec<String> = if let Value::Array(items) = &board {
                items.iter().enumerate().map(|(i, item)| {
                    let uid = item.get("user_id").and_then(|v| v.as_i64()).unwrap_or(0);
                    let secs = item.get("total_playtime").and_then(|v| v.as_u64()).unwrap_or(0);
                    let name = resolve_name(uid as i32, ctx);
                    format!("#{} {} {:.1}h", i + 1, name, secs as f64 / 3600.0)
                }).collect()
            } else { vec![] };
            let replacement = if lines.is_empty() { "暂无数据".into() } else { lines.join("、") };
            text = text.replace("[top_playtime]", &replacement);
        }
    }

    // [active_rooms] → 通过 state 查询活跃房间
    if text.contains("[active_rooms]") {
        if let Some(state) = &ctx.state {
            let r = state.call("rooms.list", &[]);
            let rooms = r.ok().unwrap_or(Value::Array(vec![]));
            let lines: Vec<String> = if let Value::Array(items) = &rooms {
                items.iter().map(|room| {
                    let name = room.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let data = &room.get("data");
                    let host = data.and_then(|d| d.get("host")).and_then(|v| v.as_i64()).unwrap_or(0);
                    let host_name = resolve_name(host as i32, ctx);
                    let users = data.and_then(|d| d.get("users")).and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                    let state_str = data.and_then(|d| d.get("state")).and_then(|v| v.as_str()).unwrap_or("?");
                    let chart_name = data.and_then(|d| d.get("chart_name")).and_then(|v| v.as_str()).unwrap_or("");
                    let chart: String = if chart_name.is_empty() { "未选曲".into() } else { chart_name.into() };
                    format!("房间：{} 房主{} [{}] {} {}人", name, host_name, state_str, chart, users)
                }).collect()
            } else { vec![] };
            let replacement = if lines.is_empty() { "暂无活跃房间".into() } else { lines.join("；") };
            text = text.replace("[active_rooms]", &replacement);
        }
    }

    // [playtime] → 通过 api 查询 playtime-tracker（默认当前用户）
    if text.contains("[playtime") {
        if let Some(api) = &ctx.api {
            // 兼容旧语法 [playtime <id>]
            if let Some(start) = text.find("[playtime ") {
                if let Some(end) = text[start..].find(']') {
                    let arg = text[start+10..start+end].trim();
                    if let Ok(uid) = arg.parse::<i32>() {
                        let r = api.call("playtime-tracker", "user_playtime", &[Value::Number(uid.into())]);
                        let secs = r.ok().and_then(|v| v.get("total_seconds").and_then(|s| s.as_u64())).unwrap_or(0);
                        text = text.replace(&format!("[playtime {}]", uid), &format!("{:.1}h", secs as f64 / 3600.0));
                    }
                }
            }
            // 裸 [playtime] → 使用连接用户
            if text.contains("[playtime]") {
                let r = api.call("playtime-tracker", "user_playtime", &[Value::Number(user_id.into())]);
                let secs = r.ok().and_then(|v| v.get("total_seconds").and_then(|s| s.as_u64())).unwrap_or(0);
                text = text.replace("[playtime]", &format!("{:.1}h", secs as f64 / 3600.0));
            }
        }
    }

    text
}

impl NativePlugin for WelcomePlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "welcome-plugin".to_string(),
            version: "0.1.0".to_string(),
            author: "Phira-mp+".to_string(),
            description: "用户连接时发送可配置的欢迎语".to_string(),
        }
    }

    fn init(&mut self, ctx: &PluginContext) -> Result<(), String> {
        info!("WelcomePlugin initializing...");
        save_config(&self.config.lock().unwrap_or_else(|e| e.into_inner()));

        // CLI
        if let Some(cli) = &ctx.cli {
            let config = self.config.clone();
            let _ = cli.register(
                "welcome-config", "查看欢迎语配置", "welcome-config",
                Arc::new(move |_| {
                    let cfg = config.lock().unwrap_or_else(|e| e.into_inner());
                    let mut out = vec![
                        format!("  ◆ 欢迎语配置"),
                        format!("  │ 配置文件: {}", CONFIG_PATH),
                        format!("  │ 消息数量: {}", cfg.welcome_messages.len()),
                    ];
                    for (i, msg) in cfg.welcome_messages.iter().enumerate() {
                        out.push(format!("  │ [{i}] {msg}"));
                    }
                    out.push(format!("  │ 显示时间: {}", cfg.show_time));
                    out.push(format!("  │ 时间格式: {}", cfg.time_format));
                    out.push(format!(""));
                    out.push(format!("  ■ 可用占位符:"));
                    out.push(format!("  │ [user_id]           用户 Phira ID"));
                    out.push(format!("  │ [user_name]         用户名"));
                    out.push(format!("  │ [user_ip]           用户 IP 地址"));
                    out.push(format!("  │ [player-count]      当前在线玩家数"));
                    out.push(format!("  │ [players]           当前在线玩家数"));
                    out.push(format!("  │ [playtime]          该用户的游玩时间"));
                    out.push(format!("  │ [top_playtime]      游玩时间前 10 名排行"));
                    out.push(format!("  │ [active_rooms]      活跃房间列表及详情"));
                    out
                }),
            );
            info!("registered CLI: welcome-config");
        }

        Ok(())
    }

    fn on_event(&self, ctx: &PluginContext, event: &PluginEvent) -> Vec<String> {
        if let PluginEvent::UserConnect { user_id, user_name, user_ip } = event {
            let messages = self.config.lock().unwrap_or_else(|e| e.into_inner()).welcome_messages.clone();
            // 按顺序发送所有消息
            for msg_template in &messages {
                let text = replace_placeholders(msg_template, *user_id, user_name, user_ip, ctx);
                if let Some(send) = &ctx.send_chat {
                    send(*user_id, text);
                }
            }
            info!("Welcome sent {} msgs to {}", messages.len(), user_name);
        }
        vec![]
    }

    fn cleanup(&mut self) {
        info!("WelcomePlugin cleaned up");
    }
}

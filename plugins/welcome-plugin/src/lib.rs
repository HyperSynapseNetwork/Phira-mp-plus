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
    /// 用户连接时的欢迎消息（支持占位符）
    pub welcome_message: String,
    /// 是否启用时间显示
    pub show_time: bool,
    /// 时间格式（如 "%Y-%m-%d %H:%M"）
    pub time_format: String,
}

impl Default for WelcomeConfig {
    fn default() -> Self {
        Self {
            welcome_message: "欢迎 [user_name] 来到 Phira-mp+！当前在线 [player-count] 人".into(),
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
fn replace_placeholders(template: &str, user_id: i32, user_name: &str, user_ip: &str, ctx: &PluginContext) -> String {
    let mut text = template
        .replace("[user_id]", &user_id.to_string())
        .replace("[user_name]", user_name)
        .replace("[user_ip]", user_ip)
        // [user_location] removed

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

    // [playtime <id>] → 通过 api 查询 playtime-tracker
    if text.contains("[playtime") {
        if let Some(api) = &ctx.api {
            if let Some(start) = text.find("[playtime ") {
                if let Some(end) = text[start..].find(']') {
                    let arg = text[start+10..start+end].trim();
                    let uid: i32 = arg.parse().unwrap_or(user_id);
                    let r = api.call("playtime-tracker", "user_playtime", &[Value::Number(uid.into())]);
                    let secs = r.ok().and_then(|v| v.get("total_seconds").and_then(|s| s.as_u64())).unwrap_or(0);
                    let hours = secs as f64 / 3600.0;
                    text = text.replace(&format!("[playtime {}]", arg), &format!("{:.1}h", hours));
                }
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
                    vec![
                        format!("  ◆ 欢迎语配置"),
                        format!("  │ 配置文件: {}", CONFIG_PATH),
                        format!("  │ 消息: {}", cfg.welcome_message),
                        format!("  │ 显示时间: {}", cfg.show_time),
                        format!("  │ 时间格式: {}", cfg.time_format),
                        format!(""),
                        format!("  ■ 可用占位符:"),
                        format!("  │ [user_id]     用户 Phira ID"),
                        format!("  │ [user_name]   用户名"),
                        format!("  │ [user_ip]     用户 IP 地址"),
                        format!("  │ [player-count] 当前在线玩家数"),
                        format!("  │ [players]      当前在线玩家数（同 [player-count]）"),
                        format!("  │ [playtime <id>] 指定用户的游玩时间"),
                    ]
                }),
            );
            info!("registered CLI: welcome-config");
        }

        Ok(())
    }

    fn on_event(&self, ctx: &PluginContext, event: &PluginEvent) -> Vec<String> {
        if let PluginEvent::UserConnect { user_id, user_name, user_ip } = event {
            let cfg = self.config.lock().unwrap_or_else(|e| e.into_inner());
            let text = replace_placeholders(&cfg.welcome_message, *user_id, user_name, user_ip, ctx);
            drop(cfg);

            // 发送欢迎语（通过 ctx.send_chat）
            if let Some(send) = &ctx.send_chat {
                let text_clone = text.clone();
                send(*user_id, text_clone);
                info!("Welcome sent to {}: {}", user_name, text);
            } else {
                warn!("Welcome plugin: send_chat not available");
            }
        }
        vec![]
    }

    fn cleanup(&mut self) {
        info!("WelcomePlugin cleaned up");
    }
}

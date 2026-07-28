//! Core command specifications: help, exit, status, config reload, check-config, doctor.
//!
//! These are the day-to-day administrative commands.

use crate::command_registry::{CommandArgSpec, CommandSpec};
use std::sync::Arc;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("help", "core", "显示命令帮助。", "help [command]")
            .arg(CommandArgSpec::optional("command", "要查看详情的命令名"))
            .example("help")
            .example("help room list")
            .example("help group rooms")
            .example("help all")
            .example("help groups"),
        CommandSpec::new("exit", "core", "关闭服务器。", "exit").example("exit"),
        CommandSpec::new("status", "core", "查看服务器运行状态。", "status")
            .example("status")
            .handler(Arc::new(|state, _args| {
                let rooms = state.rooms.try_read().map(|r| r.len()).unwrap_or(0);
                vec![format!(
                    "  ◆ Phira-mp+ v{}  │ 端口 {}  │ 房间 {}",
                    env!("CARGO_PKG_VERSION"),
                    state.config.port,
                    rooms
                )]
            })),
        CommandSpec::new(
            "config reload",
            "runtime",
            "重新加载启动时指定的 YAML 并热更新运行时配置。",
            "config reload",
        )
        .handler(Arc::new(|state, _args| {
            let path = std::path::Path::new(&state.config.config_path);
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => return vec![format!("  ✗ 读取配置文件失败: {e}")],
            };
            let mut config: crate::server::PlusConfig = match serde_yaml::from_str(&content) {
                Ok(c) => c,
                Err(e) => return vec![format!("  ✗ 解析配置文件失败: {e}")],
            };
            config.config_path = state.config.config_path.clone();
            if let Some(monitors) = state.config.cli_monitors_override.clone() {
                config.monitors = monitors.clone();
                config.cli_monitors_override = Some(monitors);
            }
            if let Err(e) = config.normalize().and_then(|_| config.validate()) {
                return vec![format!("  ✗ 配置校验失败: {e}")];
            }

            let admin_update = if !config.admin_phira_ids.is_empty() {
                Some(
                    config
                        .admin_phira_ids
                        .iter()
                        .copied()
                        .filter(|id| *id > 0)
                        .collect::<std::collections::HashSet<_>>(),
                )
            } else {
                let admin_path = std::path::Path::new("data/admin-phira-ids.json");
                if admin_path.exists() {
                    let raw = match std::fs::read_to_string(admin_path) {
                        Ok(raw) => raw,
                        Err(e) => return vec![format!("  ✗ 读取持久化管理员列表失败: {e}")],
                    };
                    let ids = match serde_json::from_str::<Vec<i32>>(&raw) {
                        Ok(ids) => ids,
                        Err(e) => return vec![format!("  ✗ 解析持久化管理员列表失败: {e}")],
                    };
                    Some(ids.into_iter().filter(|id| *id > 0).collect())
                } else {
                    None
                }
            };

            let live = crate::server::LiveConfig::from_full(&config);
            let mut live_guard = match state.live_config.try_write() {
                Ok(guard) => guard,
                Err(_) => return vec!["  ✗ 运行时配置正在被占用，请重试".to_string()],
            };
            let admin_guard = if admin_update.is_some() {
                match state.admin_ids.try_write() {
                    Ok(guard) => Some(guard),
                    Err(_) => return vec!["  ✗ 管理员列表正在被占用，请重试".to_string()],
                }
            } else {
                None
            };

            *live_guard = live;
            if let (Some(mut guard), Some(ids)) = (admin_guard, admin_update) {
                *guard = ids;
            }

            vec![
                format!("  ✓ 已从 {} 重新加载配置", path.display()),
                "  ▸ 已热更新：chat_enabled、monitors；显式管理员同步更新".to_string(),
                "  ▸ CLI monitor 覆盖及未在 YAML/持久化文件中声明的动态状态保持不变".to_string(),
                "  ▸ 端口、目录、数据库、限流和 Runtime 策略仍需重启生效".to_string(),
            ]
        })),
        CommandSpec::new("check-config", "core", "验证当前加载的配置并显示脱敏摘要。", "check-config")
            .handler(Arc::new(|state, _args| {
                let mut lines = vec![format!("  ◆ 配置版本: {}", state.config.config_version)];
                lines.push(format!("  ◆ 服务端: {} v{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")));
                lines.push(format!("  ◆ TCP 端口: {}", state.config.port));
                lines.push(format!("  ◆ HTTP: {}:{}", state.config.http_bind_address, state.config.http_port));
                lines.push(format!("  ◆ 数据库: {}", if state.config.database_url.is_empty() { "本地默认" } else { &state.config.database_url }));
                lines.push(format!("  ◆ 插件目录: {}", state.config.plugins_dir));
                lines.push(format!("  ◆ 最大会话: {}", state.config.max_sessions));
                lines.push(format!("  ◆ 最大房间: {}", state.config.max_rooms.map(|v| v.to_string()).unwrap_or("无限制".into())));
                lines.push(format!("  ◆ 数据保留: {} 天", state.config.persistence_retention_days));
                lines.push(format!("  ◆ profile: {:?}", state.config.profile));
                if !state.config.database_url.is_empty() {
                    let db_status = crate::internal_hooks::DB.get()
                        .map(|_db| "已连接")
                        .unwrap_or("不可用");
                    lines.push(format!("  ◆ 数据库状态: {db_status}"));
                }
                lines
            })),
        CommandSpec::new("doctor", "core", "运行系统诊断检查。", "doctor")
            .handler(Arc::new(|state, _args| {
                let mut lines = vec![format!("  ◆ Phira-mp+ v{} Doctor", env!("CARGO_PKG_VERSION"))];
                if let Some(db) = crate::internal_hooks::DB.get() {
                    lines.push("  ✓ 数据库: 已连接".to_string());
                } else {
                    lines.push("  ○ 数据库: 未配置".to_string());
                }
                let sessions = state.sessions.try_read().map(|s| s.len()).unwrap_or(0);
                lines.push(format!("  ✓ 会话: {sessions} 活跃"));
                let rooms = state.rooms.try_read().map(|r| r.len()).unwrap_or(0);
                lines.push(format!("  ✓ 房间: {rooms} 个"));
                lines
            })),
    ]
}

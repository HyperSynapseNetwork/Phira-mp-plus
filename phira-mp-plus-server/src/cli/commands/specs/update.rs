//! 自动更新命令：update check / apply / force / auto。

use crate::command_registry::CommandSpec;
use std::sync::Arc;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "update",
            "core",
            "自动更新：检查/更新/强制/开关。",
            "update check|apply|force|auto [on|off]",
        )
        .advanced()
        .handler(Arc::new(|_state, _args| {
            Box::pin(async move {
                vec![
                    "  ◆ 自动更新命令:".to_string(),
                    "  │ update check          — 检查新版本".to_string(),
                    "  │ update apply          — 手动更新（检查在线玩家与空闲）".to_string(),
                    "  │ update force          — 强制立刻更新".to_string(),
                    "  │ update auto [on|off]  — 开关自动更新".to_string(),
                ]
            })
        })),
        CommandSpec::new("update check", "core", "检查是否有新版本。", "update check")
            .advanced()
            .handler(Arc::new(|state, _args| {
                let state = Arc::clone(state);
                Box::pin(async move {
                    let repo = state.live_config.read().await.auto_update.github_repo.clone();
                    match crate::auto_update::check(&repo).await {
                        Ok(info) => vec![
                            format!("  ◆ 当前版本: v{}", info.current_version),
                            format!("  ◆ 最新版本: {}（发布于 {}）", info.release_tag, info.published_at),
                            format!(
                                "  ◆ 更新状态: {}",
                                if info.update_available {
                                    "有新版本"
                                } else {
                                    "已是最新"
                                }
                            ),
                            format!("  ◆ 发布页: {}", info.release_url),
                        ],
                        Err(e) => vec![format!("  ✗ 检查更新失败: {e}")],
                    }
                })
            })),
        CommandSpec::new("update apply", "core", "手动更新（检查在线玩家与空闲时长）。", "update apply")
            .advanced()
            .handler(Arc::new(|state, _args| {
                let state = Arc::clone(state);
                Box::pin(async move {
                    match crate::auto_update::apply(&state, false).await {
                        Ok(msg) => vec![format!("  ✓ {msg}")],
                        Err(e) => vec![format!("  ✗ 更新失败: {e}")],
                    }
                })
            })),
        CommandSpec::new("update force", "core", "强制更新（跳过在线玩家检查）。", "update force")
            .advanced()
            .handler(Arc::new(|state, _args| {
                let state = Arc::clone(state);
                Box::pin(async move {
                    match crate::auto_update::apply(&state, true).await {
                        Ok(msg) => vec![format!("  ✓ {msg}")],
                        Err(e) => vec![format!("  ✗ 更新失败: {e}")],
                    }
                })
            })),
        CommandSpec::new("update auto", "core", "开关自动更新（无参数显示状态）。", "update auto [on|off]")
            .advanced()
            .handler(Arc::new(|state, args| {
                let state = Arc::clone(state);
                let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
                Box::pin(async move {
                    match args.first().map(|s| s.as_str()) {
                        Some("on" | "enable" | "1") => {
                            state.live_config.write().await.auto_update.enabled = true;
                            vec!["  ✓ 自动更新已开启".to_string()]
                        }
                        Some("off" | "disable" | "0") => {
                            state.live_config.write().await.auto_update.enabled = false;
                            vec!["  ✓ 自动更新已关闭".to_string()]
                        }
                        _ => {
                            let enabled = state.live_config.read().await.auto_update.enabled;
                            vec![format!(
                                "  ◆ 自动更新：{}",
                                if enabled { "已开启" } else { "已关闭" }
                            )]
                        }
                    }
                })
            })),
    ]
}

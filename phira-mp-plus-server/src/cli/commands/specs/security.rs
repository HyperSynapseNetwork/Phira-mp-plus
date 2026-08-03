//! Security / ban command specifications.

use crate::command_registry::{CommandHandler, CommandSpec};
use std::sync::Arc;

use super::with_args;

/// Handler for `ban` / `unban` / `banlist`, dispatching on an `ip` sub-argument.
fn ban_or_ip(global: bool) -> CommandHandler {
    let global = std::sync::Arc::new(global);
    Arc::new(move |state, args| {
        let state = Arc::clone(state);
        let global = Arc::clone(&global);
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        Box::pin(async move {
            let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let is_ip = arg_refs.first().copied() == Some("ip");
            let target: Vec<&str> = if is_ip {
                arg_refs[1..].to_vec()
            } else {
                arg_refs[..].to_vec()
            };
            let sub: Vec<String> = target.iter().map(|s| s.to_string()).collect();
            if *global {
                if is_ip {
                    crate::cli::with_cli(&state, move |h| {
                        let sub_refs: Vec<&str> = sub.iter().map(|s| s.as_str()).collect();
                        Box::pin(async move { h.dispatch_ban_ip_command(&sub_refs).await })
                    })
                    .await
                } else {
                    crate::cli::with_cli(&state, move |h| {
                        let sub_refs: Vec<&str> = sub.iter().map(|s| s.as_str()).collect();
                        Box::pin(async move { h.dispatch_global_ban_command(&sub_refs).await })
                    })
                    .await
                }
            } else if is_ip {
                crate::cli::with_cli(&state, move |h| {
                    let sub_refs: Vec<&str> = sub.iter().map(|s| s.as_str()).collect();
                    Box::pin(async move { h.dispatch_unban_ip_command(&sub_refs).await })
                })
                .await
            } else {
                crate::cli::with_cli(&state, move |h| {
                    let sub_refs: Vec<&str> = sub.iter().map(|s| s.as_str()).collect();
                    Box::pin(async move { h.dispatch_global_unban_command(&sub_refs).await })
                })
                .await
            }
        })
    })
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("ban", "security", "封禁用户或 IP。", "ban [ip] <user_id|IP> [reason]")
            .example("ban 12345")
            .example("ban 12345 多次违规")
            .example("ban ip 192.168.1.1")
            .example("ban ip 12345")
            .handler(ban_or_ip(true)),
        CommandSpec::new("unban", "security", "解封用户或 IP。", "unban [ip] <user_id|IP>")
            .example("unban 12345")
            .example("unban ip 192.168.1.1")
            .handler(ban_or_ip(false)),
        CommandSpec::new("banlist", "security", "查看封禁列表。", "banlist [ip]")
            .handler(Arc::new(|state, args| {
                let state = Arc::clone(state);
                let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
                Box::pin(async move {
                    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                    if arg_refs.first().copied() == Some("ip") {
                        crate::cli::with_cli(&state, |h| {
                            Box::pin(async move { h.dispatch_ip_banlist().await })
                        })
                        .await
                    } else {
                        crate::cli::with_cli(&state, |h| {
                            Box::pin(async move { h.ban_list().await })
                        })
                        .await
                    }
                })
            })),
        CommandSpec::new(
            "ip-history",
            "security",
            "查看某用户使用过的 IP (按次数排序)。",
            "ip-history <user_id>",
        )
        .handler(with_args(|h, args| {
            Box::pin(async move {
                let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                h.dispatch_user_ip_history(&arg_refs).await
            })
        })),
    ]
}

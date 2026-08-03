//! User / broadcast / admin-id command specifications.

use crate::command_registry::CommandSpec;

use super::{no_arg, with_args};

pub fn specs() -> Vec<CommandSpec> {
    let mut out: Vec<CommandSpec> = vec![
        CommandSpec::new("users", "users", "查看在线用户。", "users")
            .handler(no_arg(|h| Box::pin(async move { h.list_users().await }))),
        CommandSpec::new("kick", "users", "踢出在线用户。", "kick <user_id>")
            .handler(with_args(|h, args| {
                Box::pin(async move {
                    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                    h.dispatch_user_kick_command(&arg_refs).await
                })
            })),
        CommandSpec::new(
            "broadcast all",
            "users",
            "广播消息给所有用户。",
            "broadcast all <message>",
        )
        .handler(with_args(|h, args| {
            Box::pin(async move {
                let mut full: Vec<String> = vec!["all".to_string()];
                full.extend(args.iter().cloned());
                let arg_refs: Vec<&str> = full.iter().map(|s| s.as_str()).collect();
                h.dispatch_broadcast_command(&arg_refs).await
            })
        })),
        CommandSpec::new(
            "broadcast room",
            "users",
            "广播消息给指定房间。",
            "broadcast room <room_id> <message>",
        )
        .handler(with_args(|h, args| {
            Box::pin(async move {
                let mut full: Vec<String> = vec!["room".to_string()];
                full.extend(args.iter().cloned());
                let arg_refs: Vec<&str> = full.iter().map(|s| s.as_str()).collect();
                h.dispatch_broadcast_command(&arg_refs).await
            })
        })),
        CommandSpec::new(
            "broadcast user",
            "users",
            "发送消息给指定用户。",
            "broadcast user <user_id> <message>",
        )
        .handler(with_args(|h, args| {
            Box::pin(async move {
                let mut full: Vec<String> = vec!["user".to_string()];
                full.extend(args.iter().cloned());
                let arg_refs: Vec<&str> = full.iter().map(|s| s.as_str()).collect();
                h.dispatch_broadcast_command(&arg_refs).await
            })
        })),
    ];
    out.push(
        CommandSpec::new(
            "admin-id",
            "users",
            "管理游戏内管理员 Phira ID。",
            "admin-id list|add|remove",
        )
        .advanced()
        .handler(with_args(|h, args| {
            Box::pin(async move {
                let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                h.admin_ids(&arg_refs).await
            })
        })),
    );
    out
}

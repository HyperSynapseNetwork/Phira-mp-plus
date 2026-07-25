//! User / broadcast / admin-id command specifications.

use crate::command_registry::CommandSpec;

pub fn specs() -> Vec<CommandSpec> {
    let mut out: Vec<CommandSpec> = vec![
        CommandSpec::new("users", "users", "查看在线用户。", "users"),
        CommandSpec::new("kick", "users", "踢出在线用户。", "kick <user_id>"),
        CommandSpec::new(
            "broadcast all",
            "users",
            "广播消息给所有用户。",
            "broadcast all <message>",
        ),
        CommandSpec::new(
            "broadcast room",
            "users",
            "广播消息给指定房间。",
            "broadcast room <room_id> <message>",
        ),
        CommandSpec::new(
            "broadcast user",
            "users",
            "发送消息给指定用户。",
            "broadcast user <user_id> <message>",
        ),
    ];
    out.push(
        CommandSpec::new(
            "admin-id",
            "users",
            "管理游戏内管理员 Phira ID。",
            "admin-id list|add|remove",
        )
        .advanced(),
    );
    out
}

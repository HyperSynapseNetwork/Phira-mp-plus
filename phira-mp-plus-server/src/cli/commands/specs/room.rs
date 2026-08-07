//! Room command specifications.

use crate::command_registry::{CommandHandler, CommandSpec};

use super::{no_arg, with_args};

/// Wrap a `room <sub>` dispatch as a handler.
fn room_sub(sub: &'static str) -> CommandHandler {
    with_args(move |h, args| {
        Box::pin(async move {
            let mut full: Vec<String> = vec![sub.to_string()];
            full.extend(args.iter().cloned());
            let arg_refs: Vec<&str> = full.iter().map(|s| s.as_str()).collect();
            h.dispatch_room_command(&arg_refs).await
        })
    })
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("rooms", "rooms", "查看活跃房间。", "rooms")
            .handler(no_arg(|h| Box::pin(async move { h.list_rooms().await }))),
        CommandSpec::new(
            "room create-empty",
            "rooms",
            "创建无人持久空房间。",
            "room create-empty <room_id> [phira_api_endpoint]",
        )
        .advanced()
        .handler(room_sub("create-empty")),
        CommandSpec::new("room info", "rooms", "查看房间详情。", "room info <room_id>")
            .handler(room_sub("info")),
        CommandSpec::new(
            "room start",
            "rooms",
            "服务端强制发起房间游戏，等待客户端加载后开始。",
            "room start <room_id>",
        )
        .handler(room_sub("start")),
        CommandSpec::new(
            "room cancel",
            "rooms",
            "取消管理员发起的游戏开始。",
            "room cancel <room_id>",
        )
        .advanced()
        .handler(room_sub("cancel")),
        CommandSpec::new(
            "room kick",
            "rooms",
            "从房间踢出用户。",
            "room kick <room_id> <user_id>",
        )
        .advanced()
        .handler(room_sub("kick")),
        CommandSpec::new(
            "room host",
            "rooms",
            "设置房主，? 表示系统房主。",
            "room host <room_id> <user_id|?>",
        )
        .handler(room_sub("host")),
        CommandSpec::new(
            "room force-move",
            "rooms",
            "强制迁移用户到指定房间。",
            "room force-move <room_id> <user_id> [monitor]",
        )
        .advanced()
        .handler(room_sub("force-move")),
        CommandSpec::new(
            "room hide",
            "rooms",
            "隐藏房间，使其不出现在 Web API 与欢迎语。",
            "room hide <room_id> [true|false]",
        )
        .advanced()
        .handler(room_sub("hide")),
        CommandSpec::new(
            "room unhide",
            "rooms",
            "取消隐藏房间。",
            "room unhide <room_id>",
        )
        .advanced()
        .handler(room_sub("unhide")),
        CommandSpec::new(
            "room ready",
            "rooms",
            "让房间进入准备状态，或强制指定玩家准备。",
            "room ready <room_id> [user_id]",
        )
        .example("room ready my-room")
        .example("room ready my-room 12345")
        .handler(room_sub("ready")),
        CommandSpec::new("room close", "rooms", "解散房间。", "room close <room_id>")
            .handler(room_sub("close")),
        CommandSpec::new(
            "room lock",
            "rooms",
            "锁定/解锁房间。",
            "room lock <room_id> [true|false]",
        )
        .handler(room_sub("lock")),
        CommandSpec::new(
            "room cycle",
            "rooms",
            "开启/关闭房主轮换。",
            "room cycle <room_id> [true|false]",
        )
        .handler(room_sub("cycle")),
        CommandSpec::new(
            "room set",
            "rooms",
            "修改房间设置（field: lock/cycle/hidden/persistent/degraded/host/chart/api_endpoint/tournament/live）。",
            "room set <room_id> <field> <value>",
        )
        .handler(room_sub("set")),
        CommandSpec::new(
            "room history",
            "rooms",
            "查看房间游玩历史。",
            "room history <room_id>",
        )
        .advanced()
        .handler(room_sub("history")),
        CommandSpec::new(
            "room rounds",
            "rooms",
            "查看房间轮次列表。",
            "room rounds <room_id>",
        )
        .advanced()
        .handler(room_sub("rounds")),
        CommandSpec::new(
            "room round",
            "rooms",
            "查看指定轮次详情。",
            "room round <round_uuid>",
        )
        .advanced()
        .handler(room_sub("round")),
        CommandSpec::new(
            "room uuid",
            "rooms",
            "查看房间 UUID。",
            "room uuid <room_id>",
        )
        .advanced()
        .handler(room_sub("uuid")),
        CommandSpec::new(
            "room ban",
            "rooms",
            "加入房间黑名单。",
            "room ban <room_id> <user_id>",
        )
        .advanced()
        .handler(room_sub("ban")),
        CommandSpec::new(
            "room unban",
            "rooms",
            "移出房间黑名单。",
            "room unban <room_id> <user_id>",
        )
        .advanced()
        .handler(room_sub("unban")),
        CommandSpec::new(
            "room banlist",
            "rooms",
            "查看房间黑名单。",
            "room banlist <room_id>",
        )
        .advanced()
        .handler(room_sub("banlist")),
        CommandSpec::new(
            "room whitelist add",
            "rooms",
            "将用户加入房间白名单（非空时仅白名单用户 + 房主/管理员可加入）。",
            "room whitelist add <room_id> <user_id>",
        )
        .advanced()
        .handler(room_sub("whitelist-add")),
        CommandSpec::new(
            "room whitelist remove",
            "rooms",
            "将用户移出房间白名单。",
            "room whitelist remove <room_id> <user_id>",
        )
        .advanced()
        .handler(room_sub("whitelist-remove")),
        CommandSpec::new(
            "room whitelist list",
            "rooms",
            "查看房间白名单。",
            "room whitelist list <room_id>",
        )
        .advanced()
        .handler(room_sub("whitelist-list")),
        CommandSpec::new(
            "room whitelist clear",
            "rooms",
            "清空房间白名单（恢复开放）。",
            "room whitelist clear <room_id>",
        )
        .advanced()
        .handler(room_sub("whitelist-clear")),
        CommandSpec::new(
            "force-start",
            "rooms",
            "服务端强制发起房间游戏（room start 别名）。",
            "force-start <room_id>",
        )
        .advanced()
        .handler(super::with_args(|h, args| {
            Box::pin(async move {
                if let Some(room_id) = args.first() {
                    h.room_start(room_id).await;
                }
            })
        })),
    ]
}

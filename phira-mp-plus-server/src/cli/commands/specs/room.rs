//! Room command specifications.

use crate::command_registry::CommandSpec;

pub fn specs() -> Vec<CommandSpec> {
    let out = vec![
        CommandSpec::new("rooms", "rooms", "查看活跃房间。", "rooms"),
        CommandSpec::new(
            "room create-empty",
            "rooms",
            "创建无人持久空房间。",
            "room create-empty <room_id> [phira_api_endpoint]",
        )
        .advanced(),
        CommandSpec::new(
            "room info",
            "rooms",
            "查看房间详情。",
            "room info <room_id>",
        ),
        CommandSpec::new(
            "room start",
            "rooms",
            "服务端强制发起房间游戏，等待客户端加载后开始。",
            "room start <room_id>",
        ),
        CommandSpec::new(
            "room force-start",
            "rooms",
            "room start 的房间子命令兼容别名。",
            "room force-start <room_id>",
        )
        .advanced(),
        CommandSpec::new(
            "force-start",
            "rooms",
            "room start 的旧版顶层兼容命令。",
            "force-start <room_id>",
        )
        .advanced(),
        CommandSpec::new(
            "room cancel",
            "rooms",
            "取消管理员发起的游戏开始。",
            "room cancel <room_id>",
        )
        .advanced(),
        CommandSpec::new(
            "room kick",
            "rooms",
            "从房间踢出用户。",
            "room kick <room_id> <user_id>",
        )
        .advanced(),
        CommandSpec::new(
            "room host",
            "rooms",
            "设置房主，? 表示系统房主。",
            "room host <room_id> <user_id|?>",
        ),
        CommandSpec::new(
            "room force-move",
            "rooms",
            "强制迁移用户到指定房间。",
            "room force-move <room_id> <user_id> [monitor]",
        )
        .advanced(),
        CommandSpec::new(
            "room hide",
            "rooms",
            "隐藏房间，使其不出现在 Web API 与欢迎语。",
            "room hide <room_id> [true|false]",
        )
        .advanced(),
        CommandSpec::new(
            "room unhide",
            "rooms",
            "取消隐藏房间。",
            "room unhide <room_id>",
        )
        .advanced(),
        CommandSpec::new("room close", "rooms", "解散房间。", "room close <room_id>"),
        CommandSpec::new(
            "room lock",
            "rooms",
            "锁定/解锁房间。",
            "room lock <room_id> [true|false]",
        ),
        CommandSpec::new(
            "room cycle",
            "rooms",
            "开启/关闭房主轮换。",
            "room cycle <room_id> [true|false]",
        ),
        CommandSpec::new(
            "room set",
            "rooms",
            "修改房间设置。",
            "room set <room_id> <field> <value>",
        ),
        CommandSpec::new(
            "room history",
            "rooms",
            "查看房间游玩历史。",
            "room history <room_id>",
        )
        .advanced(),
        CommandSpec::new(
            "room rounds",
            "rooms",
            "查看房间轮次列表。",
            "room rounds <room_id>",
        )
        .advanced(),
        CommandSpec::new(
            "room round",
            "rooms",
            "查看指定轮次详情。",
            "room round <round_uuid>",
        )
        .advanced(),
        CommandSpec::new(
            "room uuid",
            "rooms",
            "查看房间 UUID。",
            "room uuid <room_id>",
        )
        .advanced(),
        CommandSpec::new(
            "room ban",
            "rooms",
            "加入房间黑名单。",
            "room ban <room_id> <user_id>",
        )
        .advanced(),
        CommandSpec::new(
            "room unban",
            "rooms",
            "移出房间黑名单。",
            "room unban <room_id> <user_id>",
        )
        .advanced(),
        CommandSpec::new(
            "room banlist",
            "rooms",
            "查看房间黑名单。",
            "room banlist <room_id>",
        )
        .advanced(),
    ];
    out
}

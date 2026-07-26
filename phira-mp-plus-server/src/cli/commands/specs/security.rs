//! Security / ban command specifications.

use crate::command_registry::CommandSpec;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("ban", "security", "封禁用户或 IP。", "ban [ip] <user_id|IP> [reason]")
            .example("ban 12345")
            .example("ban 12345 多次违规")
            .example("ban ip 192.168.1.1")
            .example("ban ip 12345"),
        CommandSpec::new("unban", "security", "解封用户或 IP。", "unban [ip] <user_id|IP>")
            .example("unban 12345")
            .example("unban ip 192.168.1.1"),
        CommandSpec::new("banlist", "security", "查看封禁列表。", "banlist [ip]"),
        CommandSpec::new("ip-history", "security", "查看某用户使用过的 IP (按次数排序)。", "ip-history <user_id>"),
    ]
}

//! Security / ban command specifications.

use crate::command_registry::CommandSpec;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("ban", "security", "封禁用户。", "ban <user_id> [reason]"),
        CommandSpec::new("unban", "security", "解封用户。", "unban <user_id>"),
        CommandSpec::new("banlist", "security", "查看全局封禁列表。", "banlist"),
    ]
}

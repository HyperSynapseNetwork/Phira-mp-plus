//! Plugin command specifications.

use crate::command_registry::CommandSpec;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("plugin list", "plugins", "列出所有插件。", "plugin list"),
        CommandSpec::new("plugin enable", "plugins", "启用插件。", "plugin enable <name>"),
        CommandSpec::new("plugin disable", "plugins", "禁用插件。", "plugin disable <name>"),
        CommandSpec::new("plugin remove", "plugins", "删除插件：卸载并删除插件文件和数据。", "plugin remove <name>")
            .advanced(),
        CommandSpec::new("plugin reload", "plugins", "重载所有插件。", "plugin reload")
            .advanced(),
        CommandSpec::new("plugin info", "plugins", "查看插件详情。", "plugin info <id_or_name>")
            .advanced(),
        CommandSpec::new("plugin call", "plugins", "调用插件导出 API。", "plugin call <id_or_name> <method> [JSON_ARRAY]")
            .advanced(),
    ]
}

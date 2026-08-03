//! Plugin command specifications.

use crate::command_registry::{CommandHandler, CommandSpec};

use super::with_args;

/// Wrap a `plugin <sub>` dispatch as a handler.
fn plugin_sub(sub: &'static str) -> CommandHandler {
    with_args(move |h, args| {
        let mut full = vec![sub];
        full.extend(args.iter().copied());
        Box::pin(async move { h.dispatch_plugin_command(&full).await })
    })
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("plugin list", "plugins", "列出所有插件。", "plugin list")
            .handler(plugin_sub("list")),
        CommandSpec::new("plugin enable", "plugins", "启用插件。", "plugin enable <name>")
            .handler(plugin_sub("enable")),
        CommandSpec::new("plugin disable", "plugins", "禁用插件。", "plugin disable <name>")
            .handler(plugin_sub("disable")),
        CommandSpec::new(
            "plugin remove",
            "plugins",
            "删除插件：卸载并删除插件文件和数据。",
            "plugin remove <name>",
        )
        .advanced()
        .handler(plugin_sub("remove")),
        CommandSpec::new("plugin reload", "plugins", "重载所有插件。", "plugin reload")
            .advanced()
            .handler(plugin_sub("reload")),
        CommandSpec::new(
            "plugin info",
            "plugins",
            "查看插件详情。",
            "plugin info <id_or_name>",
        )
        .advanced()
        .handler(plugin_sub("info")),
        CommandSpec::new(
            "plugin call",
            "plugins",
            "调用插件导出 API。",
            "plugin call <id_or_name> <method> [JSON_ARRAY]",
        )
        .advanced()
        .handler(plugin_sub("call")),
    ]
}

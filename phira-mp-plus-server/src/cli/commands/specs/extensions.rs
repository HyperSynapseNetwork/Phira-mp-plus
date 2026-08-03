//! Extension field command specifications.

use crate::command_registry::{CommandHandler, CommandSpec};

use super::with_args;

/// Wrap an `extension <sub>` dispatch as a handler.
fn extension_sub(sub: &'static str) -> CommandHandler {
    with_args(move |h, args| {
        let mut full = vec![sub];
        full.extend(args.iter().copied());
        Box::pin(async move { h.dispatch_extension_command(&full).await })
    })
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "extension list",
            "extensions",
            "查看已注册扩展字段。",
            "extension list",
        )
        .handler(extension_sub("list")),
        CommandSpec::new(
            "extension get",
            "extensions",
            "获取扩展数据。",
            "extension get <target> <key>",
        )
        .handler(extension_sub("get")),
    ]
}

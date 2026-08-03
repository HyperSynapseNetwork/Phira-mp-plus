//! Extension field command specifications.

use crate::command_registry::{CommandHandler, CommandSpec};

use super::with_args;

/// Wrap an `extension <sub>` dispatch as a handler.
fn extension_sub(sub: &'static str) -> CommandHandler {
    with_args(move |h, args| {
        Box::pin(async move {
            let mut full: Vec<String> = vec![sub.to_string()];
            full.extend(args.iter().cloned());
            let arg_refs: Vec<&str> = full.iter().map(|s| s.as_str()).collect();
            h.dispatch_extension_command(&arg_refs).await
        })
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

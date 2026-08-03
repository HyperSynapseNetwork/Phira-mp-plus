//! Runtime diagnostic command specifications.
//!
//! These provide status and diagnostic views into the Runtime system.

use crate::command_registry::{CommandHandler, CommandSpec};

use super::with_args;

/// Wrap a `runtime <sub>` dispatch as a handler.
fn runtime_sub(sub: &'static str) -> CommandHandler {
    with_args(move |h, args| {
        Box::pin(async move {
            let mut full: Vec<String> = vec![sub.to_string()];
            full.extend(args.iter().cloned());
            let arg_refs: Vec<&str> = full.iter().map(|s| s.as_str()).collect();
            h.dispatch_runtime_command(&arg_refs).await
        })
    })
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "runtime status",
            "runtime",
            "查看 Runtime 诊断信息。",
            "runtime status",
        )
        .handler(runtime_sub("status")),
        CommandSpec::new(
            "runtime commands",
            "runtime",
            "查看 Command Registry 统计。",
            "runtime commands",
        )
        .developer()
        .handler(runtime_sub("commands")),
        CommandSpec::new(
            "runtime phira",
            "runtime",
            "查看统一 Phira HTTP RetryClient 统计和策略。",
            "runtime phira",
        )
        .developer()
        .handler(runtime_sub("phira")),
        CommandSpec::new(
            "runtime events",
            "runtime",
            "查看事件总线统计与最近事件。",
            "runtime events",
        )
        .developer()
        .handler(runtime_sub("events")),
        CommandSpec::new(
            "runtime persistence",
            "runtime",
            "查看持久化 Worker 与遥测批处理器统计。",
            "runtime persistence",
        )
        .advanced()
        .handler(runtime_sub("persistence")),
        CommandSpec::new(
            "runtime schema",
            "runtime",
            "查看持久化 schema 说明。",
            "runtime schema",
        )
        .developer()
        .handler(runtime_sub("schema")),
        CommandSpec::new(
            "runtime latency",
            "runtime",
            "打印延迟直方图（响应 + 握手）。",
            "runtime latency",
        )
        .developer()
        .handler(runtime_sub("latency")),
    ]
}

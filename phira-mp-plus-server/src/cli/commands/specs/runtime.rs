//! Runtime diagnostic command specifications.
//!
//! These provide status and diagnostic views into the Runtime system.

use crate::command_registry::CommandSpec;
use crate::server::PlusServerState;
use std::sync::Arc;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "runtime status",
            "runtime",
            "查看 Runtime 诊断信息。",
            "runtime status",
        )
        .handler(Arc::new(|state, _args| {
            let rooms = state.rooms.try_read().map(|r| r.len()).unwrap_or(0);
            vec![format!(
                "  Runtime: {} rooms | {} commands | ABI=WIT component v2",
                rooms,
                state.command_registry.iter().count()
            )]
        })),
        CommandSpec::new(
            "runtime commands",
            "runtime",
            "查看 Command Registry 统计。",
            "runtime commands",
        )
        .developer()
        .handler(Arc::new(|state, _args| {
            let (p, a, d) = state.command_registry.command_surface_counts();
            vec![format!("  Registry: {p} primary, {a} advanced, {d} dev")]
        })),
        CommandSpec::new(
            "runtime roadmap",
            "runtime",
            "查看 Runtime 总目标工作板。",
            "runtime roadmap",
        )
        .developer(),
        CommandSpec::new(
            "runtime phira",
            "runtime",
            "查看统一 Phira HTTP RetryClient 统计和策略。",
            "runtime phira",
        )
        .developer(),
        CommandSpec::new(
            "runtime events",
            "runtime",
            "查看事件总线统计与最近事件。",
            "runtime events",
        )
        .developer(),
        CommandSpec::new(
            "runtime persistence",
            "runtime",
            "查看持久化 Worker 与遥测批处理器统计。",
            "runtime persistence",
        )
        .advanced(),
        CommandSpec::new(
            "runtime schema",
            "runtime",
            "查看持久化 schema 说明。",
            "runtime schema",
        )
        .developer(),
        CommandSpec::new(
            "runtime rooms",
            "runtime",
            "查看房间命令通道与 Actor 迁移状态。",
            "runtime rooms",
        )
        .developer(),
        CommandSpec::new(
            "runtime actors",
            "runtime",
            "查看 Actor 模型迁移蓝图。",
            "runtime actors",
        )
        .developer(),
    ]
    .into_iter()
    .map(|spec| spec.example("runtime status"))
    .collect()
}

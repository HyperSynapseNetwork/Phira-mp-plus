//! Benchmark command specifications.

use crate::command_registry::{CommandArgSpec, CommandHandler, CommandSpec};

use super::with_args;

/// Wrap a `benchmark <sub>` dispatch as a handler.
fn benchmark_sub(sub: &'static str) -> CommandHandler {
    with_args(move |h, args| {
        Box::pin(async move {
            let mut full: Vec<String> = vec![sub.to_string()];
            full.extend(args.iter().cloned());
            let arg_refs: Vec<&str> = full.iter().map(|s| s.as_str()).collect();
            h.dispatch_benchmark_command(&arg_refs).await
        })
    })
}

pub fn specs() -> Vec<CommandSpec> {
    let mut out = Vec::new();

    out.push(
        CommandSpec::new(
            "benchmark run",
            "benchmark",
            "运行基准测试（进程内内部调用，不依赖独立数据库；实时显示进度，x 键结束）。",
            "benchmark run <fixed|ramp> [options]",
        )
        .advanced()
        .arg(CommandArgSpec::optional("fixed|ramp", "运行模式：fixed 维持负载上限；ramp 加压直到 CPU/RAM 触顶"))
        .arg(CommandArgSpec::optional("--sessions", "fixed：最大会话数"))
        .arg(CommandArgSpec::optional("--playing-rooms", "fixed：最大同时在线游玩房间数"))
        .arg(CommandArgSpec::optional("--cpu", "ramp：CPU 上限（百分比 0-100）"))
        .arg(CommandArgSpec::optional("--ram", "ramp：RAM 上限（如 4096m / 4g / 字节数）"))
        .arg(CommandArgSpec::optional("--duration", "运行时长，如 30 / 10m / 2h；缺省 60s"))
        .arg(CommandArgSpec::optional("--forever", "永久运行（直到 x 键结束）"))
        .arg(CommandArgSpec::optional("--output", "输出格式：text（默认）|json|markdown"))
        .example("benchmark run fixed --sessions 1000 --playing-rooms 50 --duration 10m")
        .example("benchmark run fixed --sessions 2000 --playing-rooms 100 --forever")
        .example("benchmark run ramp --cpu 80 --ram 4g --duration 1h")
        .handler(benchmark_sub("run")),
    );

    out
}

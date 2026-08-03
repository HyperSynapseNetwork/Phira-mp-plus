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
            "benchmark list",
            "benchmark",
            "列出可用场景（scenarios）和预设（presets）。",
            "benchmark list",
        )
        .advanced()
        .example("benchmark list")
        .handler(benchmark_sub("list")),
    );
    out.push(
        CommandSpec::new(
            "benchmark run",
            "benchmark",
            "运行基准测试。",
            "benchmark run --scenario <name> --preset <name> [options]",
        )
        .advanced()
        .arg(CommandArgSpec::optional("--scenario", "负载场景名（见 benchmark list）"))
        .arg(CommandArgSpec::optional("--preset", "预设参数集：quick|standard|stress|soak"))
        .arg(CommandArgSpec::optional("--clients", "模拟客户端数"))
        .arg(CommandArgSpec::optional("--rooms", "模拟房间数"))
        .arg(CommandArgSpec::optional("--duration", "运行时长，如 30（秒）/ 10m / 2h"))
        .arg(CommandArgSpec::optional("--seed", "随机种子（用于可复现性）"))
        .arg(CommandArgSpec::optional("--output", "输出格式：text（默认）|json|markdown"))
        .example("benchmark run --scenario room-lifecycle --clients 50 --rooms 5 --duration 30")
        .example("benchmark run --scenario hot-room --clients 100 --duration 10m")
        .handler(benchmark_sub("run")),
    );
    out.push(
        CommandSpec::new(
            "benchmark suite",
            "benchmark",
            "按预设参数顺序运行所有场景，汇总输出。",
            "benchmark suite --preset <name>",
        )
        .advanced()
        .arg(CommandArgSpec::optional("--preset", "预设参数集：quick|standard（默认）|stress|soak"))
        .example("benchmark suite --preset standard")
        .example("benchmark suite --preset quick")
        .handler(benchmark_sub("suite")),
    );
    out.push(
        CommandSpec::new(
            "benchmark compare",
            "benchmark",
            "比较两份基准测试报告（JSON 文件）的差异。",
            "benchmark compare <old.json> <new.json>",
        )
        .advanced()
        .arg(CommandArgSpec::required("old.json", "原始基准测试报告 JSON 文件"))
        .arg(CommandArgSpec::required("new.json", "新基准测试报告 JSON 文件"))
        .example("benchmark compare old.json new.json")
        .handler(benchmark_sub("compare")),
    );

    out
}

//! Benchmark command specifications.
//!
//! All benchmark and simulation command specs, including the
//! legacy diagnostics-group aliases.

use crate::command_registry::{CommandArgSpec, CommandSpec};
use std::sync::Arc;

pub fn specs() -> Vec<CommandSpec> {
    let mut out = Vec::new();

    // ── Phase 4.4 benchmark commands ──
    out.push(
        CommandSpec::new(
            "benchmark list",
            "benchmark",
            "列出可用场景（scenarios）和预设（presets）。",
            "benchmark list",
        )
        .advanced()
        .example("benchmark list"),
    );
    out.push(
        CommandSpec::new(
            "benchmark run",
            "benchmark",
            "运行基准测试，仅支持 real 模式。",
            "benchmark run --mode real --scenario <name> --preset <name> [options]",
        )
        .advanced()
        .arg(CommandArgSpec::optional("--mode", "运行模式：real（默认）"))
        .arg(CommandArgSpec::optional("--scenario", "负载场景名（见 benchmark list）"))
        .arg(CommandArgSpec::optional("--preset", "预设参数集：quick|standard|stress|soak"))
        .arg(CommandArgSpec::optional("--clients", "模拟客户端数"))
        .arg(CommandArgSpec::optional("--rooms", "模拟房间数"))
        .arg(CommandArgSpec::optional("--duration", "运行时长，如 30（秒）/ 10m / 2h"))
        .arg(CommandArgSpec::optional("--seed", "随机种子（用于可复现性）"))
        .arg(CommandArgSpec::optional("--output", "输出格式：text（默认）|json|markdown"))
        .example("benchmark run --scenario room-lifecycle --clients 50 --rooms 5 --duration 30")
        .example("benchmark run --mode real --scenario hot-room --clients 100 --duration 10m"),
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
        .example("benchmark suite --preset quick"),
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
        .example("benchmark compare old.json new.json"),
    );

    // ── Diagnostics-group legacy benchmark commands ──
    out.push(
        CommandSpec::new(
            "benchmark",
            "diagnostics",
            "运行显式真实网络压测。该命令需要 Phira token，不是默认压测入口。",
            "benchmark [seconds] [rooms]",
        )
        .advanced()
        .arg(CommandArgSpec::optional("seconds", "压测时长，默认 30，范围 5..300"))
        .arg(CommandArgSpec::optional("rooms", "目标房间数，默认 100，最大 5000"))
        .example("benchmark 30 100")
        .example("benchmark run real 30 100"),
    );
    out.push(
        CommandSpec::new(
            "benchmark modes",
            "diagnostics",
            "查看压测模式说明。",
            "benchmark modes",
        )
        .advanced()
        .example("benchmark modes")
        .handler(Arc::new(|_state, _args| {
            vec![
                "  Benchmark modes:".to_string(),
                "    real  — 显式真实 TCP 协议测试（需要 Phira token）".to_string(),
            ]
        })),
    );
    out.push(
        CommandSpec::new(
            "benchmark run real",
            "diagnostics",
            "运行真实 TCP 协议测试。",
            "benchmark run real [seconds] [rooms]",
        )
        .advanced()
        .example("benchmark run real 30 100"),
    );
    out.push(
        CommandSpec::new(
            "benchmark report",
            "diagnostics",
            "查看 Benchmark 报告。",
            "benchmark report [real|limit]",
        )
        .advanced()
        .example("benchmark report")
        .example("benchmark report 16"),
    );
    out.push(
        CommandSpec::new(
            "benchmark history",
            "diagnostics",
            "查看已持久化的 BenchmarkReport 历史记录。",
            "benchmark history [real] [limit]",
        )
        .advanced()
        .example("benchmark history")
        .example("benchmark history real 20"),
    );

    out
}

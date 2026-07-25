//! Benchmark command specifications.
//!
//! All benchmark and simulation command specs, including the
//! legacy diagnostics-group aliases.

use crate::command_registry::{CommandArgSpec, CommandSpec};
use std::sync::Arc;

pub fn specs() -> Vec<CommandSpec> {
    let mut out = Vec::new();

    // ── Simulation commands ──
    for spec in [
        CommandSpec::new("benchmark simulation status", "benchmark", "查看 Simulation 状态。", "benchmark simulation status")
            .handler(Arc::new(|_state, _args| {
                vec!["  Simulation status (CLI: use `benchmark simulation status` in console)".to_string()]
            })),
        CommandSpec::new(
            "benchmark simulation run",
            "benchmark",
            "启动隔离本地压测；默认自动 tick，到达 duration 后自动停止。",
            "benchmark simulation run <baseline|small|medium|large|custom> [scenario=balanced|ready_storm|round_storm|touch_judge_burst|idle] [users=N] [rooms=N] [duration=N] [tick_ms=N] [auto=true] [persist_every=N] [touch=true] [judge=true]",
        )
        .example("benchmark simulation run baseline")
        .example("benchmark simulation run custom users=500 rooms=50 duration=300 scenario=touch_judge_burst tick_ms=1000 persist_every=30")
        .example("benchmark simulation run small auto=false"),
        CommandSpec::new("benchmark simulation scenarios", "benchmark", "列出可用 Simulation workload scenario/profile。", "benchmark simulation scenarios").advanced()
            .example("benchmark simulation scenarios"),
        CommandSpec::new(
            "benchmark simulation suite",
            "benchmark",
            "按顺序运行多个 Simulation scenario，用于一次性比较不同压力形状。",
            "benchmark simulation suite <smoke|mixed|stress> [duration=N] [tick_ms=N] [persist_every=N] [users=N] [rooms=N]",
        ).advanced()
        .example("benchmark simulation suite smoke")
        .example("benchmark simulation suite mixed duration=15 tick_ms=500 persist_every=5")
        .example("benchmark simulation suite stress users=800 rooms=80"),
        CommandSpec::new(
            "benchmark simulation report",
            "benchmark",
            "查看最近一次 Simulation suite 汇总报告，并输出统一 BenchmarkReport [simulation] 摘要。",
            "benchmark simulation report [latest|list|clear]",
        ).advanced()
        .example("benchmark simulation report")
        .example("benchmark simulation report list 8")
        .example("benchmark simulation report clear"),
        CommandSpec::new("benchmark simulation tick", "benchmark", "手动推进 Simulation tick。", "benchmark simulation tick [count]").developer()
            .example("benchmark simulation tick 10"),
        CommandSpec::new("benchmark simulation inspect", "benchmark", "查看 shadow users/rooms/rounds/recent events 样本。", "benchmark simulation inspect [limit]").developer()
            .example("benchmark simulation inspect 20"),
        CommandSpec::new("benchmark simulation stop", "benchmark", "停止当前 Simulation 运行状态并广播结束提示。", "benchmark simulation stop"),
        CommandSpec::new("benchmark simulation seed", "benchmark", "设置 deterministic simulation seed。", "benchmark simulation seed <value>").developer(),
        CommandSpec::new("benchmark simulation cleanup", "benchmark", "清理 Simulation 数据。", "benchmark simulation cleanup"),
        CommandSpec::new("benchmark simulation persist", "benchmark", "发送 Simulation 快照到持久化 Worker。", "benchmark simulation persist").developer()
            .example("benchmark simulation persist"),
        CommandSpec::new("benchmark simulation sample", "benchmark", "查看 deterministic touches/judges 示例数据规模。", "benchmark simulation sample").developer(),
    ] {
        out.push(spec);
    }

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
            "运行基准测试，支持 simulation（默认）和 real 两种模式。",
            "benchmark run --mode simulation|real --scenario <name> --preset <name> [options]",
        )
        .advanced()
        .arg(CommandArgSpec::optional("--mode", "运行模式：simulation（默认）|real"))
        .arg(CommandArgSpec::optional("--scenario", "负载场景名（见 benchmark list）"))
        .arg(CommandArgSpec::optional("--preset", "预设参数集：quick|standard|stress|soak"))
        .arg(CommandArgSpec::optional("--clients", "模拟客户端数"))
        .arg(CommandArgSpec::optional("--rooms", "模拟房间数"))
        .arg(CommandArgSpec::optional("--duration", "运行时长，如 30（秒）/ 10m / 2h"))
        .arg(CommandArgSpec::optional("--seed", "随机种子（用于可复现性）"))
        .arg(CommandArgSpec::optional("--output", "输出格式：text（默认）|json|markdown"))
        .example("benchmark run --mode simulation --scenario gameplay --preset standard")
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
            "查看三种压测模式说明。",
            "benchmark modes",
        )
        .advanced()
        .example("benchmark modes")
        .handler(Arc::new(|_state, _args| {
            vec![
                "  Benchmark modes:".to_string(),
                "    simulation  — 默认推荐压测（隔离本地，不访问 Phira，不需要 token）".to_string(),
                "    real        — 显式真实 TCP 协议测试（需要 Phira token）".to_string(),
                "    hybrid      — Hybrid Phira 探测（chart_lookup / record_lookup）".to_string(),
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
        CommandSpec::new("benchmark run hybrid", "diagnostics", "运行 Hybrid Phira 探测。", "benchmark run hybrid [duration] [authenticate=true] [chart_lookup=<id>] [record_lookup=<id>]").advanced()
            .example("benchmark run hybrid")
            .example("benchmark run hybrid authenticate=true chart_lookup=1 record_lookup=1"),
    );
    out.push(
        CommandSpec::new(
            "benchmark report",
            "diagnostics",
            "查看 Benchmark 报告。",
            "benchmark report [simulation|hybrid|real|limit]",
        )
        .advanced()
        .example("benchmark report")
        .example("benchmark report simulation")
        .example("benchmark report 16"),
    );
    out.push(
        CommandSpec::new(
            "benchmark history",
            "diagnostics",
            "查看已持久化的 BenchmarkReport 历史记录。",
            "benchmark history [simulation|hybrid|real] [limit]",
        )
        .advanced()
        .example("benchmark history")
        .example("benchmark history real 20"),
    );

    out
}

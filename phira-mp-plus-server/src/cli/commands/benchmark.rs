use super::super::*;
use crate::benchmark::mode::{BenchmarkMode, ModeParams};
use std::sync::Arc;
use std::time::Duration;

impl CliHandler {
    pub(in crate::cli) async fn dispatch_benchmark_command(&self, args: &[&str]) {
        // args = ["run", "fixed", "--playing-rooms", ...]
        if matches!(args.first().copied(), Some("run")) {
            self.dispatch_benchmark_run_command(&args[1..]).await;
            return;
        }
        self.out(format!(
            "  {} Unknown benchmark command. Use `benchmark run <fixed|ramp> --help` for usage.",
            c::yellow("?")
        ));
    }
}

/// 解析 RAM 上限：支持裸字节数 / `k` / `m` / `g` 后缀。
fn parse_ram_bytes(value: &str) -> Result<u64, String> {
    let value = value.trim().to_ascii_lowercase();
    let (num, mult) = if let Some(v) = value.strip_suffix('g') {
        (v, 1u64 << 30)
    } else if let Some(v) = value.strip_suffix('m') {
        (v, 1u64 << 20)
    } else if let Some(v) = value.strip_suffix('k') {
        (v, 1u64 << 10)
    } else {
        (value.as_str(), 1u64)
    };
    let n: u64 = num
        .parse()
        .map_err(|_| format!("invalid RAM size: {value}; use e.g. 4096m, 4g, or raw bytes"))?;
    Ok(n.saturating_mul(mult))
}

impl CliHandler {
    /// `benchmark run <fixed|ramp> [options]` —— 进程内内部调用压测。
    pub(in crate::cli) async fn dispatch_benchmark_run_command(&self, args: &[&str]) {
        let Some(mode_str) = args.first().copied() else {
            self.print_benchmark_run_help();
            return;
        };
        let mode = match mode_str {
            "fixed" => BenchmarkMode::Fixed,
            "ramp" => BenchmarkMode::Ramp,
            "help" | "--help" | "-h" => {
                self.print_benchmark_run_help();
                return;
            }
            _ => {
                self.out(format!(
                    "  {} Unknown benchmark mode: {mode_str}. Use fixed or ramp.",
                    c::red("✗")
                ));
                self.print_benchmark_run_help();
                return;
            }
        };
        let flags = &args[1..];

        let mut params = ModeParams {
            mode,
            max_playing_rooms: 0,
            max_cpu_pct: 0.0,
            max_ram_bytes: 0,
            duration: Some(Duration::from_secs(60)),
        };
        let mut output_format = OutputFormat::Text;
        let mut show_help = false;

        let mut i = 0;
        while i < flags.len() {
            match flags[i] {
                "--playing-rooms" | "--rooms" => {
                    i += 1;
                    if i >= flags.len() {
                        self.out(format!(
                            "  {} --playing-rooms requires a number",
                            c::red("✗")
                        ));
                        return;
                    }
                    match flags[i].parse::<u32>() {
                        Ok(n) => params.max_playing_rooms = n,
                        Err(_) => {
                            self.out(format!("  {} invalid number: {}", c::red("✗"), flags[i]));
                            return;
                        }
                    }
                }
                "--cpu" => {
                    i += 1;
                    if i >= flags.len() {
                        self.out(format!("  {} --cpu requires a percent (0-100)", c::red("✗")));
                        return;
                    }
                    match flags[i].parse::<f64>() {
                        Ok(n) => params.max_cpu_pct = n,
                        Err(_) => {
                            self.out(format!("  {} invalid CPU percent: {}", c::red("✗"), flags[i]));
                            return;
                        }
                    }
                }
                "--ram" => {
                    i += 1;
                    if i >= flags.len() {
                        self.out(format!("  {} --ram requires a size (e.g. 4096m / 4g)", c::red("✗")));
                        return;
                    }
                    match parse_ram_bytes(flags[i]) {
                        Ok(n) => params.max_ram_bytes = n,
                        Err(e) => {
                            self.out(format!("  {} {e}", c::red("✗")));
                            return;
                        }
                    }
                }
                "--duration" => {
                    i += 1;
                    if i >= flags.len() {
                        self.out(format!(
                            "  {} --duration requires a value (e.g. 30, 10m, 2h)",
                            c::red("✗")
                        ));
                        return;
                    }
                    match parse_benchmark_duration(flags[i]) {
                        Ok(d) => params.duration = Some(d),
                        Err(e) => {
                            self.out(format!("  {} {e}", c::red("✗")));
                            return;
                        }
                    }
                }
                "--forever" | "-f" => params.duration = None,
                "--output" => {
                    i += 1;
                    if i >= flags.len() {
                        self.out(format!(
                            "  {} --output requires a format (text|json|markdown)",
                            c::red("✗")
                        ));
                        return;
                    }
                    match flags[i].to_ascii_lowercase().as_str() {
                        "text" | "human" => output_format = OutputFormat::Text,
                        "json" => output_format = OutputFormat::Json,
                        "markdown" | "md" => output_format = OutputFormat::Markdown,
                        other => {
                            self.out(format!(
                                "  {} invalid output format: {other}. Use text, json, or markdown",
                                c::red("✗")
                            ));
                            return;
                        }
                    }
                }
                "--help" | "-h" => show_help = true,
                other => {
                    self.out(format!("  {} unknown option: {other}", c::red("✗")));
                    self.out(format!("  {} Run `benchmark run --help` for usage", c::dim("▸")));
                    return;
                }
            }
            i += 1;
        }

        if show_help {
            self.print_benchmark_run_help();
            return;
        }

        if let Err(e) = params.validate() {
            self.out(format!("  {} {e}", c::red("✗")));
            self.print_benchmark_run_help();
            return;
        }

        // ── 锁定 CLI 输入（进度矩形 + x 键取消）──────────────────
        let status = Arc::clone(&self.state.cli_status);
        let guard = crate::cli_status::CliStatusGuard::new(
            &status,
            "benchmark",
            "准备中…",
            'x',
        );
        self.out(format!(
            "  {} 开始压测: {}  (x 键结束)",
            c::green("◆"),
            params_desc(&params)
        ));

        let mut harness = crate::benchmark::harness::BenchmarkHarness::new(
            Arc::clone(&self.state),
            params.clone(),
            Arc::clone(&status),
        );
        let report = harness.run().await;

        drop(guard); // 恢复输入

        self.state.publish_benchmark_completed(&report);

        self.out(String::new());
        self.out(format!(
            "  {} {}",
            c::green("✓"),
            report.one_line_summary()
        ));
        if report.aborted {
            self.out(format!(
                "  {} {}",
                c::yellow("!"),
                report.abort_reason.as_deref().unwrap_or("已中止")
            ));
        }
        self.out(String::new());
        match output_format {
            OutputFormat::Text => {
                for line in report.format_text().lines() {
                    self.out(line.to_string());
                }
            }
            OutputFormat::Json => match report.format_json() {
                Ok(json) => self.out(json),
                Err(e) => self.out(format!(
                    "  {} JSON serialization failed: {e}",
                    c::red("✗")
                )),
            },
            OutputFormat::Markdown => {
                self.out(report.format_markdown());
            }
        }
    }
}

fn params_desc(params: &ModeParams) -> String {
    match params.mode {
        BenchmarkMode::Fixed => format!(
            "fixed playing_rooms={}{}",
            params.max_playing_rooms,
            duration_desc(params.duration),
        ),
        BenchmarkMode::Ramp => format!(
            "ramp cpu={:.0}% ram={}MB{}",
            params.max_cpu_pct,
            params.max_ram_bytes / 1024 / 1024,
            duration_desc(params.duration),
        ),
    }
}

fn duration_desc(duration: Option<Duration>) -> String {
    match duration {
        Some(d) => format!(" duration={}s", d.as_secs()),
        None => " forever".to_string(),
    }
}

impl CliHandler {
    /// 打印 `benchmark run` 帮助。
    pub(in crate::cli) fn print_benchmark_run_help(&self) {
        self.out(format!("  {} benchmark run — 运行基准测试", c::bold("用法")));
        self.out(String::new());
        self.out(format!(
            "  {}   benchmark run fixed --playing-rooms <M> [--duration <D>|--forever]",
            c::dim("▸")
        ));
        self.out(format!(
            "  {}   benchmark run ramp --cpu <P> --ram <S> [--duration <D>|--forever]",
            c::dim("▸")
        ));
        self.out(String::new());
        self.out(format!("  {} Options:", c::cyan("▸")));
        self.out(format!(
            "  {}   --playing-rooms <M> fixed：最大同时在线游玩房间数（会话数自动 = 房间 × 2）",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   --cpu <P>          ramp：CPU 上限（百分比 0-100）",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   --ram <S>          ramp：RAM 上限（如 4096m / 4g / 字节数）",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   --duration <D>     时长：30（秒）/ 10m / 2h（缺省 60s）",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   --forever          永久运行（直到 x 键结束）",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   --output <fmt>     输出：text（默认）/ json / markdown",
            c::dim("│")
        ));
        self.out(String::new());
        self.out(format!("  {} 运行期间 CLI 输入被锁定，按 x 结束并显示报告。", c::dim("▸")));
        self.out(format!(
            "  {} 进程内内部调用（虚拟会话/房间），不依赖独立数据库，结束时全清理。",
            c::dim("▸")
        ));
    }
}

fn parse_benchmark_duration(value: &str) -> Result<std::time::Duration, String> {
    let value = value.trim();
    if let Some(secs) = value.strip_suffix('s').or_else(|| value.strip_suffix('S')) {
        let secs: u64 = secs
            .parse()
            .map_err(|_| format!("invalid duration (seconds): {value}"))?;
        Ok(std::time::Duration::from_secs(secs))
    } else if let Some(mins) = value
        .strip_suffix('m')
        .or_else(|| value.strip_suffix('M'))
    {
        let mins: u64 = mins
            .parse()
            .map_err(|_| format!("invalid duration (minutes): {value}"))?;
        Ok(std::time::Duration::from_secs(mins * 60))
    } else if let Some(hours) = value
        .strip_suffix('h')
        .or_else(|| value.strip_suffix('H'))
    {
        let hours: u64 = hours
            .parse()
            .map_err(|_| format!("invalid duration (hours): {value}"))?;
        Ok(std::time::Duration::from_secs(hours * 3600))
    } else {
        let secs: u64 = value
            .parse()
            .map_err(|_| format!("invalid duration: {value}; use e.g. 30 (seconds), 10m, 2h"))?;
        Ok(std::time::Duration::from_secs(secs))
    }
}

/// 输出格式（兼容旧版）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Text,
    Json,
    Markdown,
}

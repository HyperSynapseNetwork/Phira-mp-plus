use super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn dispatch_benchmark_command(&self, args: &[&str]) {
        // ── Phase 4.4: New benchmark commands ──

        // benchmark list — list available scenarios and presets
        if matches!(args.first().copied(), Some("list")) {
            self.dispatch_benchmark_list_command().await;
            return;
        }

        // benchmark suite — run a predefined suite of scenarios
        if matches!(args.first().copied(), Some("suite")) {
            self.dispatch_benchmark_suite_command(&args[1..]).await;
            return;
        }

        // benchmark compare — compare two benchmark report JSON files
        if matches!(args.first().copied(), Some("compare")) {
            self.dispatch_benchmark_compare_command(&args[1..]).await;
            return;
        }

        // bare `benchmark run` — show help
        if matches!(args.first().copied(), Some("run")) && args.len() == 1 {
            self.print_benchmark_run_help();
            return;
        }

        // `benchmark run --<flag> …` — new-style parametric benchmark run
        if matches!(args.first().copied(), Some("run"))
            && args.len() > 1
            && args[1].starts_with("--")
        {
            self.dispatch_benchmark_run_command(args).await;
            return;
        }

        // Unknown benchmark command
        self.out(format!("  {} Unknown benchmark command. Use `benchmark run --help` for usage.", c::yellow("?")));
    }


}

// ═══════════════════════════════════════════════════════════════════════
// Phase 4.4 — New benchmark commands: list, run, suite, compare
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputFormat {
    Text,
    Json,
    Markdown,
}

impl CliHandler {
    /// `benchmark list` — list available scenarios and presets
    pub(in crate::cli) async fn dispatch_benchmark_list_command(&self) {
        self.out(format!("  {} Available benchmark scenarios", c::green("◆")));
        for scenario in crate::benchmark::command::BenchmarkScenario::all() {
            self.out(format!(
                "  {} {:<22} {}",
                c::dim("│"),
                scenario.as_str(),
                scenario.description()
            ));
        }
        self.out(String::new());
        self.out(format!("  {} Available presets", c::green("◆")));
        let all_presets = [
            crate::benchmark::command::BenchmarkPreset::Quick,
            crate::benchmark::command::BenchmarkPreset::Standard,
            crate::benchmark::command::BenchmarkPreset::Stress,
            crate::benchmark::command::BenchmarkPreset::Soak,
        ];
        for preset in &all_presets {
            let params = crate::benchmark::presets::BenchmarkPresetParams::from_preset(*preset);
            self.out(format!(
                "  {} {:<12} clients={:<5} rooms={:<5} duration={}s — {}",
                c::dim("│"),
                preset.as_str(),
                params.clients,
                params.rooms,
                params.duration.as_secs(),
                params.description(),
            ));
        }
        self.out(String::new());
        self.out(format!("  {} Usage examples:", c::cyan("▸")));
        self.out(format!(
            "  {}   benchmark run --scenario gameplay --preset standard",
            c::dim("▸")
        ));
        self.out(format!(
            "  {}   benchmark run --scenario hot-room --clients 100 --rooms 1 --duration 10m",
            c::dim("▸")
        ));
        self.out(format!(
            "  {}   benchmark suite --preset quick",
            c::dim("▸")
        ));
        self.out(format!(
            "  {}   benchmark compare old.json new.json",
            c::dim("▸")
        ));
    }

    /// `benchmark run` — parse flags and execute
    pub(in crate::cli) async fn dispatch_benchmark_run_command(&self, args: &[&str]) {
        // args = ["run", "--scenario", "gameplay", ...]
        let cmd_args = &args[1..]; // skip "run"

        let mut run_args = crate::benchmark::command::BenchmarkRunArgs::default();
        let mut output_format = OutputFormat::Text;
        let mut show_help = false;
        let mut explicit_clients = false;
        let mut explicit_rooms = false;
        let mut explicit_duration = false;

        let mut i = 0;
        while i < cmd_args.len() {
            match cmd_args[i] {
                "--scenario" => {
                    i += 1;
                    if i >= cmd_args.len() {
                        self.out(format!(
                            "  {} --scenario requires a value",
                            c::red("✗")
                        ));
                        return;
                    }
                    match crate::benchmark::command::BenchmarkScenario::parse(cmd_args[i]) {
                        Some(scenario) => run_args.scenario = scenario,
                        None => {
                            let names: Vec<&str> =
                                crate::benchmark::command::BenchmarkScenario::all()
                                    .iter()
                                    .map(|s| s.as_str())
                                    .collect();
                            self.out(format!(
                                "  {} invalid scenario: '{}'. Available: {}",
                                c::red("✗"),
                                cmd_args[i],
                                names.join(", ")
                            ));
                            return;
                        }
                    }
                }
                "--preset" => {
                    i += 1;
                    if i >= cmd_args.len() {
                        self.out(format!(
                            "  {} --preset requires a value",
                            c::red("✗")
                        ));
                        return;
                    }
                    match crate::benchmark::command::BenchmarkPreset::parse(cmd_args[i]) {
                        Some(preset) => run_args.preset = preset,
                        None => {
                            self.out(format!(
                                "  {} invalid preset: '{}'. Available: quick, standard, stress, soak",
                                c::red("✗"),
                                cmd_args[i]
                            ));
                            return;
                        }
                    }
                }
                "--clients" | "--users" => {
                    i += 1;
                    if i >= cmd_args.len() {
                        self.out(format!(
                            "  {} {} requires a number",
                            c::red("✗"),
                            cmd_args[i - 1]
                        ));
                        return;
                    }
                    match cmd_args[i].parse::<u32>() {
                        Ok(n) => {
                            run_args.clients = n;
                            explicit_clients = true;
                        }
                        Err(_) => {
                            self.out(format!(
                                "  {} invalid number: {}",
                                c::red("✗"),
                                cmd_args[i]
                            ));
                            return;
                        }
                    }
                }
                "--rooms" => {
                    i += 1;
                    if i >= cmd_args.len() {
                        self.out(format!(
                            "  {} --rooms requires a number",
                            c::red("✗")
                        ));
                        return;
                    }
                    match cmd_args[i].parse::<u32>() {
                        Ok(n) => {
                            run_args.rooms = n;
                            explicit_rooms = true;
                        }
                        Err(_) => {
                            self.out(format!(
                                "  {} invalid number: {}",
                                c::red("✗"),
                                cmd_args[i]
                            ));
                            return;
                        }
                    }
                }
                "--duration" => {
                    i += 1;
                    if i >= cmd_args.len() {
                        self.out(format!(
                            "  {} --duration requires a value (e.g. 30, 10m, 2h)",
                            c::red("✗")
                        ));
                        return;
                    }
                    match parse_benchmark_duration(cmd_args[i]) {
                        Ok(d) => {
                            run_args.duration = d;
                            explicit_duration = true;
                        }
                        Err(e) => {
                            self.out(format!("  {} {e}", c::red("✗")));
                            return;
                        }
                    }
                }
                "--seed" => {
                    i += 1;
                    if i >= cmd_args.len() {
                        self.out(format!(
                            "  {} --seed requires a number",
                            c::red("✗")
                        ));
                        return;
                    }
                    match cmd_args[i].parse::<u64>() {
                        Ok(seed) => run_args.seed = seed,
                        Err(_) => {
                            self.out(format!(
                                "  {} invalid seed: {}",
                                c::red("✗"),
                                cmd_args[i]
                            ));
                            return;
                        }
                    }
                }
                "--listen-addr" => {
                    i += 1;
                    if i >= cmd_args.len() {
                        self.out(format!(
                            "  {} --listen-addr requires an address (ip:port)",
                            c::red("✗")
                        ));
                        return;
                    }
                    let addr = cmd_args[i].to_string();
                    run_args.overrides.push(("listen-addr".to_string(), addr));
                }
                "--mock-phira-delay" => {
                    i += 1;
                    if i >= cmd_args.len() {
                        self.out(format!("  {} --mock-phira-delay requires a number (ms)", c::red("✗")));
                        return;
                    }
                    run_args.overrides.push(("mock-phira-delay".to_string(), cmd_args[i].to_string()));
                }
                "--mock-phira-jitter" => {
                    i += 1;
                    if i >= cmd_args.len() {
                        self.out(format!("  {} --mock-phira-jitter requires a number (ms)", c::red("✗")));
                        return;
                    }
                    run_args.overrides.push(("mock-phira-jitter".to_string(), cmd_args[i].to_string()));
                }
                "--mock-phira-error-rate" => {
                    i += 1;
                    if i >= cmd_args.len() {
                        self.out(format!("  {} --mock-phira-error-rate requires a float (0.0-1.0)", c::red("✗")));
                        return;
                    }
                    run_args.overrides.push(("mock-phira-error-rate".to_string(), cmd_args[i].to_string()));
                }
                "--mock-phira-timeout" => {
                    i += 1;
                    if i >= cmd_args.len() {
                        self.out(format!("  {} --mock-phira-timeout requires a number (ms)", c::red("✗")));
                        return;
                    }
                    run_args.overrides.push(("mock-phira-timeout".to_string(), cmd_args[i].to_string()));
                }
                "--output" => {
                    i += 1;
                    if i >= cmd_args.len() {
                        self.out(format!(
                            "  {} --output requires a format (text|json|markdown)",
                            c::red("✗")
                        ));
                        return;
                    }
                    match cmd_args[i].to_ascii_lowercase().as_str() {
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
                "--help" | "-h" => {
                    show_help = true;
                }
                other => {
                    self.out(format!(
                        "  {} unknown option: {other}",
                        c::red("✗")
                    ));
                    self.out(format!(
                        "  {} Run `benchmark run --help` for usage",
                        c::dim("▸")
                    ));
                    return;
                }
            }
            i += 1;
        }

        if show_help {
            self.print_benchmark_run_help();
            return;
        }

        // Apply preset defaults for values not explicitly overridden by the user.
        // If the user supplied --clients/--rooms/--duration via CLI, those take
        // priority over the preset.  Fields the user didn't touch get filled from
        // the preset's own parameters.
        let preset_params =
            crate::benchmark::presets::BenchmarkPresetParams::from_preset(run_args.preset);
        if !explicit_clients {
            run_args.clients = preset_params.clients;
        }
        if !explicit_rooms {
            run_args.rooms = preset_params.rooms;
        }
        if !explicit_duration {
            run_args.duration = preset_params.duration;
        }

        // Announce
        self.out(format!(
            "  {} Starting benchmark: scenario={} preset={} clients={} rooms={} duration={}s seed={}",
            c::green("◆"),
            run_args.scenario.as_str(),
            run_args.preset.as_str(),
            run_args.clients,
            run_args.rooms,
            run_args.duration.as_secs(),
            run_args.seed,
        ));

        // Execute via BenchmarkRunner
        let mut runner = crate::benchmark::runner::BenchmarkRunner::from_args(run_args);
        runner.set_server_state(std::sync::Arc::clone(&self.state));
        match runner.run().await {
            Ok(report) => {
                self.out(format!("  {} Benchmark completed", c::green("✓")));
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
            Err(e) => {
                self.out(format!("  {} Benchmark failed: {e}", c::red("✗")));
            }
        }
    }

    /// `benchmark suite --preset <name>` — run all scenarios sequentially
    pub(in crate::cli) async fn dispatch_benchmark_suite_command(&self, args: &[&str]) {
        let mut preset = crate::benchmark::command::BenchmarkPreset::Standard;
        let mut show_help = false;

        let mut i = 0;
        while i < args.len() {
            match args[i] {
                "--preset" => {
                    i += 1;
                    if i >= args.len() {
                        self.out(format!(
                            "  {} --preset requires a value",
                            c::red("✗")
                        ));
                        return;
                    }
                    match crate::benchmark::command::BenchmarkPreset::parse(args[i]) {
                        Some(p) => preset = p,
                        None => {
                            self.out(format!(
                                "  {} invalid preset: '{}'. Available: quick, standard, stress, soak",
                                c::red("✗"),
                                args[i]
                            ));
                            return;
                        }
                    }
                }
                "--help" | "-h" => {
                    show_help = true;
                }
                other => {
                    self.out(format!(
                        "  {} unknown option: {other}. Usage: benchmark suite --preset <name>",
                        c::red("✗")
                    ));
                    return;
                }
            }
            i += 1;
        }

        if show_help {
            self.out(format!("  {} benchmark suite — Run a benchmark suite", c::bold("Usage")));
            self.out(format!("  {}   benchmark suite --preset <name>", c::dim("▸")));
            self.out(format!("  {}   Presets: quick, standard (default), stress, soak", c::dim("▸")));
            self.out(format!("  {} Runs all 11 benchmark scenarios sequentially with the chosen preset parameters", c::dim("▸")));
            return;
        }

        let scenarios = crate::benchmark::command::BenchmarkScenario::all();
        let total = scenarios.len();
        let preset_params =
            crate::benchmark::presets::BenchmarkPresetParams::from_preset(preset);

        self.out(format!(
            "  {} Benchmark suite started: preset={} scenarios={}",
            c::green("◆"),
            preset.as_str(),
            total
        ));
        self.out(format!(
            "  {} clients={} rooms={} duration={}s",
            c::dim("│"),
            preset_params.clients,
            preset_params.rooms,
            preset_params.duration.as_secs()
        ));

        let mut all_passed = 0usize;
        let mut all_failed = 0usize;

        for (idx, scenario) in scenarios.iter().enumerate() {
            self.out(format!(
                "  {} [{}/{}] {} — {}",
                c::cyan("▸"),
                idx + 1,
                total,
                scenario.as_str(),
                scenario.description()
            ));

            let mut run_args = crate::benchmark::command::BenchmarkRunArgs::default();
            run_args.scenario = *scenario;
            run_args.preset = preset;
            run_args.clients = preset_params.clients;
            run_args.rooms = preset_params.rooms;
            run_args.duration = preset_params.duration;

            let mut runner = crate::benchmark::runner::BenchmarkRunner::from_args(run_args);
            runner.set_server_state(std::sync::Arc::clone(&self.state));
            match runner.run().await {
                Ok(report) => {
                    all_passed += 1;
                    self.out(format!(
                        "  {} [{}/{}] {} — {:.0} cmd/s, {:.0} msg/s, {} errors, p50={:.1}ms p99={:.1}ms",
                        c::green("✓"),
                        idx + 1,
                        total,
                        scenario.as_str(),
                        report.summary.avg_commands_per_sec,
                        report.summary.avg_messages_per_sec,
                        report.errors_total,
                        report.command_latency.p50_ms,
                        report.command_latency.p99_ms,
                    ));
                }
                Err(e) => {
                    all_failed += 1;
                    self.out(format!(
                        "  {} [{}/{}] {} — FAILED: {e}",
                        c::red("✗"),
                        idx + 1,
                        total,
                        scenario.as_str()
                    ));
                }
            }
        }

        self.out(format!(
            "  {} Suite complete: {}/{} passed, {}/{} failed",
            if all_failed == 0 {
                c::green("✓")
            } else {
                c::yellow("!")
            },
            all_passed,
            total,
            all_failed,
            total
        ));
    }

    /// `benchmark compare <old.json> <new.json>` — compare two benchmark report files
    pub(in crate::cli) async fn dispatch_benchmark_compare_command(&self, args: &[&str]) {
        if args.len() < 2 {
            self.out(format!(
                "  {} benchmark compare requires two file paths: compare <old.json> <new.json>",
                c::yellow("?")
            ));
            self.out(format!(
                "  {} The files should be benchmark reports in JSON format",
                c::dim("▸")
            ));
            self.out(format!(
                "  {} Example: benchmark compare reports/report-001.json reports/report-002.json",
                c::dim("▸")
            ));
            return;
        }

        let old_path = args[0];
        let new_path = args[1];

        // Read and parse old report
        let old_content = match std::fs::read_to_string(old_path) {
            Ok(c) => c,
            Err(e) => {
                self.out(format!(
                    "  {} Failed to read old report '{}': {e}",
                    c::red("✗"),
                    old_path
                ));
                return;
            }
        };
        let old_report: crate::benchmark::report::BenchmarkReport =
            match serde_json::from_str(&old_content) {
                Ok(r) => r,
                Err(e) => {
                    self.out(format!(
                        "  {} Failed to parse old report '{}': {e}",
                        c::red("✗"),
                        old_path
                    ));
                    return;
                }
            };

        // Read and parse new report
        let new_content = match std::fs::read_to_string(new_path) {
            Ok(c) => c,
            Err(e) => {
                self.out(format!(
                    "  {} Failed to read new report '{}': {e}",
                    c::red("✗"),
                    new_path
                ));
                return;
            }
        };
        let new_report: crate::benchmark::report::BenchmarkReport =
            match serde_json::from_str(&new_content) {
                Ok(r) => r,
                Err(e) => {
                    self.out(format!(
                        "  {} Failed to parse new report '{}': {e}",
                        c::red("✗"),
                        new_path
                    ));
                    return;
                }
            };

        // Print comparison header
        self.out(format!("  {} Benchmark Comparison", c::bold("═══")));
        self.out(format!(
            "  {} Old: {} ({})",
            c::dim("│"),
            old_path,
            old_report.title
        ));
        self.out(format!(
            "  {} New: {} ({})",
            c::dim("│"),
            new_path,
            new_report.title
        ));
        if old_report.config.scenario != new_report.config.scenario {
            self.out(format!(
                "  {} Note: reports have different scenarios (old: {}, new: {})",
                c::yellow("?"),
                old_report.config.scenario.as_str(),
                new_report.config.scenario.as_str()
            ));
        }
        self.out(String::new());

        // Helper to format a metric change
        let fmt_change = |old: f64, new: f64| -> String {
            if old == 0.0 && new == 0.0 {
                return "      -".to_string();
            }
            if old == 0.0 {
                return format!("    +{:.0}", new);
            }
            let pct = (new / old - 1.0) * 100.0;
            if pct >= 0.0 {
                format!("  +{:.1}%", pct)
            } else {
                format!("  {:.1}%", pct)
            }
        };

        let hdr_format = |label: &str, old_val: String, new_val: String, change: String| {
            format!(
                "  {:<24} {:>14} {:>14} {:>10}",
                label, old_val, new_val, change
            )
        };

        self.out(hdr_format(
            "Metric",
            "Old".to_string(),
            "New".to_string(),
            "Change".to_string(),
        ));
        self.out(format!("  {}", c::dim(&"─".repeat(64))));

        // Throughput
        self.out(hdr_format(
            "Commands/s",
            format!("{:.0}", old_report.summary.avg_commands_per_sec),
            format!("{:.0}", new_report.summary.avg_commands_per_sec),
            fmt_change(
                old_report.summary.avg_commands_per_sec,
                new_report.summary.avg_commands_per_sec,
            ),
        ));
        self.out(hdr_format(
            "Messages/s",
            format!("{:.0}", old_report.summary.avg_messages_per_sec),
            format!("{:.0}", new_report.summary.avg_messages_per_sec),
            fmt_change(
                old_report.summary.avg_messages_per_sec,
                new_report.summary.avg_messages_per_sec,
            ),
        ));

        // Errors
        self.out(hdr_format(
            "Errors",
            format!("{}", old_report.errors_total),
            format!("{}", new_report.errors_total),
            fmt_change(
                old_report.errors_total as f64,
                new_report.errors_total as f64,
            ),
        ));

        // Latency
        self.out(hdr_format(
            "p50 latency",
            format!("{:.1}ms", old_report.command_latency.p50_ms),
            format!("{:.1}ms", new_report.command_latency.p50_ms),
            fmt_change(
                old_report.command_latency.p50_ms,
                new_report.command_latency.p50_ms,
            ),
        ));
        self.out(hdr_format(
            "p95 latency",
            format!("{:.1}ms", old_report.command_latency.p95_ms),
            format!("{:.1}ms", new_report.command_latency.p95_ms),
            fmt_change(
                old_report.command_latency.p95_ms,
                new_report.command_latency.p95_ms,
            ),
        ));
        self.out(hdr_format(
            "p99 latency",
            format!("{:.1}ms", old_report.command_latency.p99_ms),
            format!("{:.1}ms", new_report.command_latency.p99_ms),
            fmt_change(
                old_report.command_latency.p99_ms,
                new_report.command_latency.p99_ms,
            ),
        ));
        self.out(hdr_format(
            "max latency",
            format!("{:.1}ms", old_report.command_latency.max_ms),
            format!("{:.1}ms", new_report.command_latency.max_ms),
            fmt_change(
                old_report.command_latency.max_ms,
                new_report.command_latency.max_ms,
            ),
        ));

        // Resources
        self.out(hdr_format(
            "CPU (total)",
            format!("{:.1}%", old_report.cpu.total_pct),
            format!("{:.1}%", new_report.cpu.total_pct),
            fmt_change(old_report.cpu.total_pct, new_report.cpu.total_pct),
        ));
        self.out(hdr_format(
            "RSS (peak)",
            format!("{}MB", old_report.peak_rss_bytes / 1024 / 1024),
            format!("{}MB", new_report.peak_rss_bytes / 1024 / 1024),
            fmt_change(
                old_report.peak_rss_bytes as f64,
                new_report.peak_rss_bytes as f64,
            ),
        ));

        // Database
        self.out(hdr_format(
            "DB rows/s",
            format!("{:.0}", old_report.database.avg_rows_per_sec),
            format!("{:.0}", new_report.database.avg_rows_per_sec),
            fmt_change(
                old_report.database.avg_rows_per_sec,
                new_report.database.avg_rows_per_sec,
            ),
        ));
        self.out(hdr_format(
            "DB txns/s",
            format!("{:.0}", old_report.database.avg_transactions_per_sec),
            format!("{:.0}", new_report.database.avg_transactions_per_sec),
            fmt_change(
                old_report.database.avg_transactions_per_sec,
                new_report.database.avg_transactions_per_sec,
            ),
        ));

        self.out(format!("  {}", c::dim(&"─".repeat(64))));
        if new_report.errors_total > old_report.errors_total {
            self.out(format!(
                "  {} Errors increased by {} ({} → {})",
                c::yellow("!"),
                new_report.errors_total - old_report.errors_total,
                old_report.errors_total,
                new_report.errors_total
            ));
        }
        if new_report.command_latency.p99_ms > old_report.command_latency.p99_ms * 1.5 {
            self.out(format!(
                "  {} p99 latency degraded significantly ({:.1}ms → {:.1}ms)",
                c::yellow("!"),
                old_report.command_latency.p99_ms,
                new_report.command_latency.p99_ms
            ));
        }
        if old_report.title != new_report.title {
            self.out(format!(
                "  {} Note: reports have different titles, ensure you are comparing the right pair",
                c::dim("▸")
            ));
        }
    }
}

impl CliHandler {
    /// Print help text for `benchmark run`
    pub(in crate::cli) fn print_benchmark_run_help(&self) {
        self.out(format!("  {} benchmark run — Run a benchmark", c::bold("Usage")));
        self.out(String::new());
        self.out(format!("  {} Options:", c::cyan("▸")));
        self.out(format!(
            "  {}   --scenario <scenario>  Load scenario (use benchmark list to see all)",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   --preset <preset>      Preset: quick, standard (default), stress, soak",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   --clients <N>          Number of simulated clients",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   --rooms <N>            Number of simulated rooms",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   --duration <N>         Duration (e.g. 30, 10m, 2h)",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   --seed <N>             Random seed for reproducibility",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   --output <fmt>         Output: text (default), json, markdown",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   --mock-phira-delay <ms>   Mock Phira artificial delay (default: 5ms)",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   --mock-phira-jitter <ms>  Mock Phira delay jitter (default: 2ms)",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   --mock-phira-error-rate <rate>  Mock Phira error rate (0.0-1.0)",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   --mock-phira-timeout <ms> Mock Phira timeout delay (default: 30000ms)",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   --help / -h            Show this help",
            c::dim("│")
        ));
        self.out(String::new());
        self.out(format!("  {} Examples:", c::cyan("▸")));
        self.out(format!(
            "  {}   benchmark run --scenario gameplay --preset standard",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   benchmark run --scenario room-lifecycle --clients 50 --rooms 5 --duration 30",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   benchmark run --output json > report.json",
            c::dim("│")
        ));
        self.out(format!(
            "  {} Use `benchmark list` to see all scenarios and presets",
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


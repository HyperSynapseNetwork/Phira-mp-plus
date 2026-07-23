use super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn dispatch_benchmark_command(&self, args: &[&str]) {
        // Simulation sub-commands are now under benchmark.
        // NOTE: "run --mode simulation" is intentionally NOT matched here — it
        // falls through to dispatch_benchmark_run_command where --mode simulation
        // is the default path.  Only the bare "simulation" and legacy "run simulation"
        // forms go to the simulation dispatcher.
        if matches!(args.first().copied(), Some("simulation") | Some("sim"))
            || (matches!(args.first().copied(), Some("run"))
                && matches!(args.get(1).copied(), Some("simulation") | Some("sim")))
        {
            let sim_args = if matches!(args.first().copied(), Some("run")) {
                &args[2..]
            } else {
                &args[1..]
            };
            self.dispatch_benchmark_simulation_command(sim_args).await;
            return;
        }

        if matches!(args.first().copied(), Some("token")) {
            // benchmark token bind <token...>
            let token_args = if args.len() > 2 { &args[2..] } else { &[] };
            self.bind_benchmark(token_args).await;
            return;
        }
        if matches!(args.first().copied(), Some("cleanup")) {
            self.dispatch_benchmark_cleanup_command().await;
            return;
        }

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

        // `benchmark run --mode real …` — not-yet-implemented (early exit)
        if matches!(args.first().copied(), Some("run"))
            && args.len() > 1
            && matches!(args.get(1).copied(), Some("--mode"))
            && matches!(args.get(2).copied(), Some("real"))
        {
            self.out(format!("  {} Real mode runner not yet implemented", c::yellow("?")));
            self.out(format!(
                "  {} This mode requires starting a real PMP server, connecting mock clients over real TCP, and requires PostgreSQL and optionally mock Phira HTTP",
                c::dim("▸")
            ));
            self.out(format!(
                "  {} Use --mode simulation for the in-process benchmark",
                c::dim("▸")
            ));
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

        // ── Legacy fallthrough ──
        self.start_benchmark(args).await;
    }

    pub(in crate::cli) async fn dispatch_benchmark_cleanup_command(&self) {
        self.state
            .rooms
            .write()
            .await
            .retain(|rid, _| !rid.to_string().starts_with("bench-"));
        self.out(format!("  {} 已清理 bench-* 压测房间", c::green("✓")));
    }

    async fn start_benchmark(&self, args: &[&str]) {
        if matches!(
            args.first().copied(),
            Some("modes") | Some("mode") | Some("help")
        ) {
            self.print_benchmark_modes();
            return;
        }
        if matches!(
            args.first().copied(),
            Some("report") | Some("reports") | Some("latest") | Some("history")
        ) {
            self.print_benchmark_reports(args).await;
            return;
        }
        if matches!(args.first().copied(), Some("hybrid"))
            || (matches!(args.first().copied(), Some("run"))
                && matches!(args.get(1).copied(), Some("hybrid")))
        {
            let hybrid_args: &[&str] = if matches!(args.first().copied(), Some("run")) {
                &args[2..]
            } else {
                &args[1..]
            };
            self.start_hybrid_benchmark(hybrid_args).await;
            return;
        }
        let numeric_args: &[&str] = if matches!(args.first().copied(), Some("run"))
            && matches!(args.get(1).copied(), Some("real"))
        {
            &args[2..]
        } else if matches!(args.first().copied(), Some("real")) {
            &args[1..]
        } else {
            args
        };
        let duration: u64 = numeric_args
            .first()
            .and_then(|value| value.parse().ok())
            .filter(|value| (5..=300).contains(value))
            .unwrap_or(30);
        let rooms: usize = numeric_args
            .get(1)
            .and_then(|value| value.parse().ok())
            .unwrap_or(100)
            .clamp(1, 5000);

        let (tx, rx) = std::sync::mpsc::channel();
        match self
            .state
            .bench_tx
            .try_send(crate::server::BenchRequest::real(duration, rooms, tx))
        {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                self.out(format!("  {} benchmark 已在运行或队列已满", c::red("✗")));
                return;
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                self.out(format!("  {} benchmark channel closed", c::red("✗")));
                return;
            }
        }

        self.out(format!(
            "  {} 已提交真实网络压测: {} 秒 / 目标 {} 房间（后台运行，完成后会输出结果）",
            c::green("✓"),
            duration,
            rooms,
        ));
        self.out(format!(
            "  {} 未配置账号时会提示配置 benchmark_phira_tokens；运行期间仍可继续输入其它命令",
            c::dim("▸")
        ));
        self.out(format!(
            "  {} Runtime v2 默认压测入口将是 simulation；当前 benchmark 是显式 real-network 协议测试",
            c::dim("▸")
        ));

        let out_tx = self.out_tx.clone();
        tokio::spawn(async move {
            let timeout_secs = duration.saturating_add(120);
            let result = tokio::task::spawn_blocking(move || {
                rx.recv_timeout(std::time::Duration::from_secs(timeout_secs))
            })
            .await;

            match result {
                Ok(Ok(output)) => {
                    let _ = out_tx
                        .send(format!(
                            "  ◆ benchmark 完成（{} 秒 / {} 房间）",
                            duration, rooms
                        ))
                        .await;
                    for line in output.lines() {
                        let _ = out_tx.send(line.to_string()).await;
                    }
                }
                Ok(Err(_)) => {
                    let _ = out_tx
                        .send(format!(
                            "  ✗ benchmark 超时或被取消（等待 {} 秒后仍无结果）",
                            timeout_secs
                        ))
                        .await;
                }
                Err(err) => {
                    let _ = out_tx
                        .send(format!("  ✗ benchmark 等待任务失败: {err}"))
                        .await;
                }
            }
        });
    }

    async fn start_hybrid_benchmark(&self, args: &[&str]) {
        let config = match Self::parse_hybrid_benchmark_config(args) {
            Ok(config) => config,
            Err(err) => {
                self.out(format!("  {} {}", c::red("✗"), err));
                self.out(format!("  {} 示例: benchmark run hybrid authenticate=true chart_lookup=1 record_lookup=1", c::dim("▸")));
                self.out(format!(
                    "  {} 默认不访问 Phira：benchmark run hybrid",
                    c::dim("▸")
                ));
                return;
            }
        };

        let timeout_secs = config.timeout_secs();
        let switches = config.enabled_switches();
        let (tx, rx) = std::sync::mpsc::channel();
        match self
            .state
            .bench_tx
            .try_send(crate::server::BenchRequest::hybrid(config, tx))
        {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                self.out(format!("  {} benchmark 已在运行或队列已满", c::red("✗")));
                return;
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                self.out(format!("  {} benchmark channel closed", c::red("✗")));
                return;
            }
        }

        self.out(format!(
            "  {} 已提交 hybrid benchmark probe（后台运行）",
            c::green("✓")
        ));
        if switches.is_empty() {
            self.out(format!(
                "  {} 当前未启用任何 Phira 开关；这是 dry-run，不会访问 Phira",
                c::dim("▸")
            ));
        } else {
            self.out(format!(
                "  {} 显式 Phira 开关: {}",
                c::dim("▸"),
                switches.join(", ")
            ));
        }
        self.out(format!(
            "  {} Simulation 仍是默认压测路径；hybrid 会输出统一 BenchmarkReport 摘要",
            c::dim("▸")
        ));

        let out_tx = self.out_tx.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                rx.recv_timeout(std::time::Duration::from_secs(timeout_secs))
            })
            .await;

            match result {
                Ok(Ok(output)) => {
                    let _ = out_tx.send("  ◆ benchmark hybrid 完成".to_string()).await;
                    for line in output.lines() {
                        let _ = out_tx.send(line.to_string()).await;
                    }
                }
                Ok(Err(_)) => {
                    let _ = out_tx
                        .send(format!(
                            "  ✗ benchmark hybrid 超时或被取消（等待 {} 秒后仍无结果）",
                            timeout_secs
                        ))
                        .await;
                }
                Err(err) => {
                    let _ = out_tx
                        .send(format!("  ✗ benchmark hybrid 等待任务失败: {err}"))
                        .await;
                }
            }
        });
    }

    fn parse_hybrid_benchmark_config(
        args: &[&str],
    ) -> Result<crate::server::HybridBenchmarkConfig, String> {
        let mut config = crate::server::HybridBenchmarkConfig::default();
        let mut idx = 0usize;
        if let Some(first) = args.first() {
            if !first.contains('=') {
                if let Ok(duration) = first.parse::<u64>() {
                    config.duration_secs = duration;
                    idx = 1;
                }
            }
        }

        for token in &args[idx..] {
            let token = token.trim();
            if token.is_empty() {
                continue;
            }
            if token.eq_ignore_ascii_case("authenticate") || token.eq_ignore_ascii_case("auth") {
                config.authenticate = true;
                continue;
            }
            if token.eq_ignore_ascii_case("upload_record") || token.eq_ignore_ascii_case("upload") {
                config.upload_record = true;
                continue;
            }
            let Some((raw_key, raw_value)) = token.split_once('=') else {
                return Err(format!(
                    "invalid hybrid option: {token}; use key=value switches"
                ));
            };
            let key = raw_key.trim().to_ascii_lowercase().replace('-', "_");
            let value = raw_value.trim();
            match key.as_str() {
                "duration" | "duration_secs" | "seconds" => {
                    config.duration_secs = value
                        .parse::<u64>()
                        .map_err(|_| format!("invalid hybrid duration: {value}"))?;
                }
                "authenticate" | "auth" => {
                    config.authenticate = parse_benchmark_bool(value)?;
                }
                "chart" | "chart_id" | "chart_lookup" => {
                    config.chart_lookup = parse_optional_probe_id("chart_lookup", value)?;
                }
                "record" | "record_id" | "record_lookup" => {
                    config.record_lookup = parse_optional_probe_id("record_lookup", value)?;
                }
                "upload" | "upload_record" => {
                    config.upload_record = parse_benchmark_bool(value)?;
                }
                "endpoint" | "phira_api_endpoint" => {
                    config.endpoint_override = if value.eq_ignore_ascii_case("default")
                        || value.eq_ignore_ascii_case("global")
                        || value.eq_ignore_ascii_case("none")
                    {
                        None
                    } else {
                        Some(crate::server::normalize_phira_api_endpoint(value)?)
                    };
                }
                other => return Err(format!("unknown hybrid option: {other}")),
            }
        }
        config.validate()?;
        Ok(config)
    }

    fn print_benchmark_modes(&self) {
        self.out(format!("  {} Benchmark modes", c::green("◆")));
        self.out(format!("  {} simulation  默认压测路径：不访问 Phira，不需要真实账号，suite/report 输出统一 BenchmarkReport", c::dim("│")));
        self.out(format!("  {} hybrid      显式 Phira probe：authenticate/chart_lookup/record_lookup/upload_record 独立开关，默认全关，输出统一 BenchmarkReport", c::dim("│")));
        self.out(format!("  {} real        当前 benchmark 命令：真实 TCP + 真实认证 + 真实 Phira token，输出统一 BenchmarkReport", c::dim("│")));
        self.out(format!("  {} examples", c::cyan("▸")));
        self.out("    benchmark simulation suite smoke".to_string());
        self.out("    benchmark simulation run medium scenario=touch_judge_burst duration=30".to_string());
        self.out("    benchmark run hybrid".to_string());
        self.out(
            "    benchmark run hybrid authenticate=true chart_lookup=1 record_lookup=1".to_string(),
        );
        self.out("    benchmark run real 30 100".to_string());
        self.out("    benchmark modes".to_string());
    }

    async fn print_benchmark_reports(&self, args: &[&str]) {
        let sub = args.first().copied().unwrap_or("report");
        let rest = if matches!(sub, "report" | "reports" | "latest" | "history") {
            &args[1..]
        } else {
            args
        };
        let mode = rest.first().and_then(|value| parse_benchmark_mode(value));
        let limit = rest
            .iter()
            .find_map(|value| value.parse::<usize>().ok())
            .unwrap_or(crate::runtime_diagnostics::BENCHMARK_REPORT_RECENT_DEFAULT);

        self.out(format!("  {} Benchmark reports", c::green("◆")));
        if matches!(sub, "history") {
            let rows = if let Some(db) = crate::internal_hooks::DB.get() {
                db.runtime_benchmark_report_history(
                    crate::persistence::BenchmarkReportHistoryQuery::new(mode, limit),
                )
                .await
            } else {
                Vec::new()
            };
            self.out(format!(
                "  {} persisted history rows={} source=mp_runtime_benchmark_reports",
                c::dim("│"),
                rows.len(),
            ));
            if rows.is_empty() {
                self.out(format!("  {} 暂无已持久化 benchmark report；先运行 benchmark/simulation，或检查 database_url", c::yellow("?")));
            } else {
                for row in rows {
                    self.out(format!(
                        "    #{:<4} {:<10} created_at={} duration={}s failed={} probes_failed={} title={}",
                        row.sequence,
                        row.mode.as_str(),
                        row.created_at,
                        row.duration_secs,
                        row.failed_operations.unwrap_or(0),
                        row.probes_failed,
                        row.title,
                    ));
                }
            }
            self.out(format!("  {} examples: benchmark history | benchmark history real | benchmark history hybrid 20", c::dim("▸")));
            return;
        }
        if let Some(mode) = mode {
            match self.state.benchmark_reports.latest(mode) {
                Some(entry) => {
                    self.out(format!(
                        "  {} latest {} report: seq={} at_ms={}",
                        c::dim("│"),
                        mode.as_str(),
                        entry.seq,
                        entry.at_ms,
                    ));
                    for line in entry.report.render_text().lines() {
                        self.out(line.to_string());
                    }
                }
                None => self.out(format!(
                    "  {} no {} benchmark report yet",
                    c::yellow("?"),
                    mode.as_str()
                )),
            }
            return;
        }

        let snapshot = self.state.benchmark_reports.snapshot(limit);
        self.out(format!(
            "  {} total={} retained={} recent={}",
            c::dim("│"),
            snapshot.total,
            snapshot.retained,
            snapshot.recent.len(),
        ));
        if snapshot.latest_by_mode.is_empty() {
            self.out(format!("  {} 尚无 benchmark.completed 报告；先运行 simulation suite / benchmark run hybrid / benchmark run real", c::yellow("?")));
            return;
        }
        self.out(format!("  {} latest by mode", c::cyan("▸")));
        for item in &snapshot.latest_by_mode {
            self.out(format!(
                "    {:<10} count={} latest_seq={} title={} failed={}",
                item.mode.as_str(),
                item.count,
                item.latest_seq,
                item.latest.title,
                item.latest.failed_operations,
            ));
        }
        if !snapshot.recent.is_empty() {
            self.out(format!("  {} recent", c::cyan("▸")));
            for entry in snapshot.recent {
                self.out(format!(
                    "    #{:<4} {:<10} duration={}s title={} failed={} probes_failed={}",
                    entry.seq,
                    entry.mode.as_str(),
                    entry.duration_secs,
                    entry.title,
                    entry.failed_operations,
                    entry.probes_failed,
                ));
            }
        }
        self.out(format!("  {} examples: benchmark report simulation | benchmark report hybrid | benchmark report real | benchmark report 16", c::dim("▸")));
    }

    async fn bind_benchmark(&self, args: &[&str]) {
        if args.is_empty() {
            self.out(format!(
                "  {} benchmark token bind <token1[,token2...]> 或多个 token 参数",
                c::yellow("?"),
            ));
            self.out(format!(
                "  {} 也可以直接修改 server_config.yml: benchmark_phira_tokens: [\"...\"]",
                c::dim("▸")
            ));
            self.out(format!(
                "  {} 不要把真实 Phira token 提交到 Git；优先使用本地配置或环境变量",
                c::dim("▸")
            ));
            return;
        }

        let raw = args
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>();
        match self.state.bind_benchmark_tokens(raw).await {
            Ok(count) => self.out(format!(
                "  {} 已绑定 {} 个压测账号，保存到 data/benchmark-auth.json",
                c::green("✓"),
                count,
            )),
            Err(err) => self.out(format!("  {} {}", c::red("✗"), err)),
        }
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
            "  {}   benchmark run --mode real --scenario hot-room --clients 100 --rooms 1 --duration 10m",
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
        // args = ["run", "--mode", "simulation", "--scenario", "gameplay", ...]
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
                "--mode" => {
                    i += 1;
                    if i >= cmd_args.len() {
                        self.out(format!(
                            "  {} --mode requires a value (simulation|real)",
                            c::red("✗")
                        ));
                        return;
                    }
                    match crate::benchmark::command::BenchmarkRunMode::parse(cmd_args[i]) {
                        Some(mode) => run_args.mode = mode,
                        None => {
                            self.out(format!(
                                "  {} invalid mode: '{}'. Use simulation|real",
                                c::red("✗"),
                                cmd_args[i]
                            ));
                            return;
                        }
                    }
                }
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

        // If mode is real, return clean "not yet implemented" error
        if run_args.mode == crate::benchmark::command::BenchmarkRunMode::Real {
            self.out(format!(
                "  {} Real mode benchmark is not yet implemented",
                c::yellow("?")
            ));
            self.out(format!(
                "  {} Real mode requires:",
                c::dim("▸")
            ));
            self.out(format!(
                "  {}   - Starting a real PMP server process",
                c::dim("▸")
            ));
            self.out(format!(
                "  {}   - Mock Phira HTTP server",
                c::dim("▸")
            ));
            self.out(format!(
                "  {}   - PostgreSQL database",
                c::dim("▸")
            ));
            self.out(format!(
                "  {}   - Real TCP client connections",
                c::dim("▸")
            ));
            self.out(format!(
                "  {} Use --mode simulation (default) for in-process benchmarks",
                c::dim("▸")
            ));
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
            "  {} Starting benchmark: mode=simulation scenario={} preset={} clients={} rooms={} duration={}s seed={}",
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
            run_args.mode = crate::benchmark::command::BenchmarkRunMode::Simulation;
            run_args.scenario = *scenario;
            run_args.preset = preset;
            run_args.clients = preset_params.clients;
            run_args.rooms = preset_params.rooms;
            run_args.duration = preset_params.duration;

            let mut runner = crate::benchmark::runner::BenchmarkRunner::from_args(run_args);
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
        if old_report.config.mode != new_report.config.mode
            || old_report.config.scenario != new_report.config.scenario
        {
            self.out(format!(
                "  {} Note: reports have different configurations (old: {} / {}, new: {} / {})",
                c::yellow("?"),
                old_report.config.mode.as_str(),
                old_report.config.scenario.as_str(),
                new_report.config.mode.as_str(),
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
        self.out(format!("  {}", c::dim("─".repeat(64))));

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

        self.out(format!("  {}", c::dim("─".repeat(64))));
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
            "  {}   --mode <mode>         Benchmark mode: simulation (default) or real",
            c::dim("│")
        ));
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
            "  {}   --help / -h            Show this help",
            c::dim("│")
        ));
        self.out(String::new());
        self.out(format!("  {} Examples:", c::cyan("▸")));
        self.out(format!(
            "  {}   benchmark run --mode simulation --scenario gameplay --preset standard",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   benchmark run --scenario room-lifecycle --clients 50 --rooms 5 --duration 30",
            c::dim("│")
        ));
        self.out(format!(
            "  {}   benchmark run --mode real --scenario hot-room --clients 100 --duration 10m",
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

fn parse_benchmark_bool(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "y" | "on" | "enable" | "enabled" => Ok(true),
        "0" | "false" | "no" | "n" | "off" | "disable" | "disabled" => Ok(false),
        other => Err(format!("invalid boolean value: {other}")),
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

fn parse_optional_probe_id(name: &str, value: &str) -> Result<Option<i32>, String> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("false")
        || value.eq_ignore_ascii_case("off")
        || value.eq_ignore_ascii_case("disabled")
        || value == "0"
    {
        return Ok(None);
    }
    if value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("on") {
        return Err(format!(
            "{name}=true is ambiguous; use {name}=<positive_id>"
        ));
    }
    let id = value
        .parse::<i32>()
        .map_err(|_| format!("invalid {name} id: {value}"))?;
    if id <= 0 {
        return Err(format!("{name} id must be positive"));
    }
    Ok(Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hybrid_defaults_do_not_touch_phira() {
        let config = CliHandler::parse_hybrid_benchmark_config(&[]).unwrap();
        assert!(!config.touches_phira());
        assert!(config.enabled_switches().is_empty());
    }

    #[test]
    fn hybrid_switches_parse_explicit_probe_ids() {
        let config = CliHandler::parse_hybrid_benchmark_config(&[
            "duration=15",
            "authenticate=true",
            "chart_lookup=123",
            "record_lookup=456",
        ])
        .unwrap();
        assert_eq!(config.duration_secs, 15);
        assert!(config.authenticate);
        assert_eq!(config.chart_lookup, Some(123));
        assert_eq!(config.record_lookup, Some(456));
        assert!(config.touches_phira());
    }

    #[test]
    fn hybrid_true_lookup_requires_id() {
        assert!(CliHandler::parse_hybrid_benchmark_config(&["chart_lookup=true"]).is_err());
    }
}

fn parse_benchmark_mode(value: &&str) -> Option<crate::benchmark_report::BenchmarkMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "simulation" | "sim" => Some(crate::benchmark_report::BenchmarkMode::Simulation),
        "hybrid" => Some(crate::benchmark_report::BenchmarkMode::Hybrid),
        "real" => Some(crate::benchmark_report::BenchmarkMode::Real),
        _ => None,
    }
}

use super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn dispatch_benchmark_command(&self, args: &[&str]) {
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
                        let _ = out_tx
                            .send(line.to_string())
                            .await;
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
                    let _ = out_tx
                        .send("  ◆ benchmark hybrid 完成".to_string())
                        .await;
                    for line in output.lines() {
                        let _ = out_tx
                            .send(line.to_string())
                            .await;
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
        self.out("    simulation suite smoke".to_string());
        self.out("    simulation run medium scenario=touch_judge_burst duration=30".to_string());
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

fn parse_benchmark_bool(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "y" | "on" | "enable" | "enabled" => Ok(true),
        "0" | "false" | "no" | "n" | "off" | "disable" | "disabled" => Ok(false),
        other => Err(format!("invalid boolean value: {other}")),
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

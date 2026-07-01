use super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn dispatch_benchmark_command(&self, args: &[&str]) {
        self.start_benchmark(args).await;
    }

    pub(in crate::cli) async fn dispatch_benchmark_bind_command(&self, args: &[&str]) {
        self.bind_benchmark(args).await;
    }

    pub(in crate::cli) async fn dispatch_benchmark_cleanup_command(&self) {
        self.state.cleanup_benchmark_sync();
        self.out(format!("  {} 已清理 bench-* 压测房间", c::green("✓")));
    }

    async fn start_benchmark(&self, args: &[&str]) {
        if matches!(args.first().copied(), Some("modes") | Some("mode") | Some("help")) {
            self.print_benchmark_modes();
            return;
        }
        if matches!(args.first().copied(), Some("hybrid"))
            || (matches!(args.first().copied(), Some("run")) && matches!(args.get(1).copied(), Some("hybrid")))
        {
            let hybrid_args: &[&str] = if matches!(args.first().copied(), Some("run")) {
                &args[2..]
            } else {
                &args[1..]
            };
            self.start_hybrid_benchmark(hybrid_args).await;
            return;
        }
        let numeric_args: &[&str] = if matches!(args.first().copied(), Some("run")) && matches!(args.get(1).copied(), Some("real")) {
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
        if self.state.bench_tx.send(crate::server::BenchRequest::real(duration, rooms, tx)).is_err() {
            self.out(format!("  {} benchmark channel closed", c::red("✗")));
            return;
        }

        self.out(format!(
            "  {} 已提交真实网络压测: {} 秒 / 目标 {} 房间（后台运行，完成后会输出结果）",
            c::green("✓"),
            duration,
            rooms,
        ));
        self.out(format!(
            "  {} 未配置账号时会提示执行 benchmark-bind <token1[,token2...]>；运行期间仍可继续输入其它命令",
            c::dim("▸")
        ));
        self.out(format!(
            "  {} Runtime v2 默认压测入口将是 simulation；当前 benchmark 仍是显式 real-network 兼容性测试",
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
                    let _ = out_tx.send(format!("  ◆ benchmark 完成（{} 秒 / {} 房间）", duration, rooms));
                    for line in output.lines() {
                        let _ = out_tx.send(line.to_string());
                    }
                }
                Ok(Err(_)) => {
                    let _ = out_tx.send(format!(
                        "  ✗ benchmark 超时或被取消（等待 {} 秒后仍无结果）",
                        timeout_secs
                    ));
                }
                Err(err) => {
                    let _ = out_tx.send(format!("  ✗ benchmark 等待任务失败: {err}"));
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
                self.out(format!("  {} 默认不访问 Phira：benchmark run hybrid", c::dim("▸")));
                return;
            }
        };

        let timeout_secs = config.timeout_secs();
        let switches = config.enabled_switches();
        let (tx, rx) = std::sync::mpsc::channel();
        if self.state.bench_tx.send(crate::server::BenchRequest::hybrid(config, tx)).is_err() {
            self.out(format!("  {} benchmark channel closed", c::red("✗")));
            return;
        }

        self.out(format!("  {} 已提交 hybrid benchmark probe（后台运行）", c::green("✓")));
        if switches.is_empty() {
            self.out(format!("  {} 当前未启用任何 Phira 开关；这是 dry-run，不会访问 Phira", c::dim("▸")));
        } else {
            self.out(format!("  {} 显式 Phira 开关: {}", c::dim("▸"), switches.join(", ")));
        }
        self.out(format!("  {} Simulation 仍是默认压测路径；hybrid 会输出统一 BenchmarkReport 摘要", c::dim("▸")));

        let out_tx = self.out_tx.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                rx.recv_timeout(std::time::Duration::from_secs(timeout_secs))
            })
            .await;

            match result {
                Ok(Ok(output)) => {
                    let _ = out_tx.send("  ◆ benchmark hybrid 完成".to_string());
                    for line in output.lines() {
                        let _ = out_tx.send(line.to_string());
                    }
                }
                Ok(Err(_)) => {
                    let _ = out_tx.send(format!("  ✗ benchmark hybrid 超时或被取消（等待 {} 秒后仍无结果）", timeout_secs));
                }
                Err(err) => {
                    let _ = out_tx.send(format!("  ✗ benchmark hybrid 等待任务失败: {err}"));
                }
            }
        });
    }

    fn parse_hybrid_benchmark_config(args: &[&str]) -> Result<crate::server::HybridBenchmarkConfig, String> {
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
                return Err(format!("invalid hybrid option: {token}; use key=value switches"));
            };
            let key = raw_key.trim().to_ascii_lowercase().replace('-', "_");
            let value = raw_value.trim();
            match key.as_str() {
                "duration" | "duration_secs" | "seconds" => {
                    config.duration_secs = value.parse::<u64>().map_err(|_| format!("invalid hybrid duration: {value}"))?;
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
        self.out("    benchmark run hybrid authenticate=true chart_lookup=1 record_lookup=1".to_string());
        self.out("    benchmark run real 30 100".to_string());
        self.out("    benchmark modes".to_string());
    }

    async fn bind_benchmark(&self, args: &[&str]) {
        if args.is_empty() {
            self.out(format!("  {} {} <token1[,token2...]> 或多个 token 参数", c::yellow("?"), c::bold("benchmark-bind")));
            self.out(format!("  {} 也可以直接修改 server_config.yml: benchmark_phira_tokens: [\"...\"]", c::dim("▸")));
            self.out(format!("  {} 不要把真实 Phira token 提交到 Git；优先使用本地配置或环境变量", c::dim("▸")));
            return;
        }

        let raw = args.iter().map(|value| (*value).to_string()).collect::<Vec<_>>();
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
        return Err(format!("{name}=true is ambiguous; use {name}=<positive_id>"));
    }
    let id = value.parse::<i32>().map_err(|_| format!("invalid {name} id: {value}"))?;
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
        ]).unwrap();
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

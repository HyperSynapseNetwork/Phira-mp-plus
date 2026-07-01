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
            self.out(format!("  {} benchmark hybrid 尚未启用", c::yellow("!")));
            self.out(format!("  {} 计划：authenticate/chart_lookup/record_lookup/upload_record 独立开关，默认全部关闭", c::dim("▸")));
            self.out(format!("  {} 当前请使用 simulation 作为默认压测，或 benchmark run real 做真实兼容性测试", c::dim("▸")));
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
        if self.state.bench_tx.send((duration, rooms, tx)).is_err() {
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

    fn print_benchmark_modes(&self) {
        self.out(format!("  {} Benchmark modes", c::green("◆")));
        self.out(format!("  {} simulation  默认压测路径：不访问 Phira，不需要真实账号，使用 shadow world/suite/report", c::dim("│")));
        self.out(format!("  {} hybrid      计划中：可按开关访问 authenticate/chart/record/upload，不作为默认", c::dim("│")));
        self.out(format!("  {} real        当前 benchmark 命令：真实 TCP + 真实认证 + 真实 Phira token", c::dim("│")));
        self.out(format!("  {} examples", c::cyan("▸")));
        self.out("    simulation suite smoke".to_string());
        self.out("    simulation run medium scenario=touch_judge_burst duration=30".to_string());
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

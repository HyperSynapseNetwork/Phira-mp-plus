//! Simulation suite/report CLI output.

use super::super::super::*;
use super::runner;

impl CliHandler {
    pub(in crate::cli) async fn simulation_report(&self, args: &[&str]) {
        let sub = args.first().copied().unwrap_or("latest");
        match sub {
            "latest" | "" => match self.state.simulation.latest_suite_report().await {
                Some(report) => self.print_suite_report(&report),
                None => self.out(format!(
                    "  {} 暂无 simulation suite report；先执行 simulation suite smoke",
                    c::yellow("?")
                )),
            },
            "list" => {
                let limit = args
                    .get(1)
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(8);
                let reports = self.state.simulation.suite_reports(limit).await;
                if reports.is_empty() {
                    self.out(format!("  {} 暂无 simulation suite report", c::yellow("?")));
                    return;
                }
                self.out(format!(
                    "  {} 最近 {} 份 Simulation suite report",
                    c::green("◆"),
                    reports.len()
                ));
                for report in reports {
                    self.out(format!(
                        "  {} {} suite={} completed={}/{} aborted={} workload_events={} eps={:.2} benchmark_mode=simulation",
                        c::dim("│"),
                        report.suite_run_id,
                        report.suite.as_str(),
                        report.completed_steps,
                        report.total_steps,
                        report.aborted,
                        report.workload_events,
                        report.workload_events_per_sec
                    ));
                }
            }
            _ => {
                self.out(format!(
                    "  {} 未知 simulation report 子命令: {}",
                    c::red("✗"),
                    sub
                ));
                self.out(format!(
                    "  {} 可用: simulation report | simulation report list [limit]",
                    c::dim("▸")
                ));
            }
        }
    }

    fn print_suite_report(&self, report: &crate::simulation::SimulationSuiteReport) {
        self.out(format!("  {} Simulation suite report", c::green("◆")));
        self.out(format!(
            "  {} suite_run_id: {}",
            c::dim("│"),
            report.suite_run_id
        ));
        self.out(format!(
            "  {} suite:        {}",
            c::dim("│"),
            report.suite.as_str()
        ));
        self.out(format!(
            "  {} completed:    {}/{} aborted={}",
            c::dim("│"),
            report.completed_steps,
            report.total_steps,
            report.aborted
        ));
        let duration_ms = report
            .finished_at_ms
            .saturating_sub(report.started_at_ms)
            .max(0);
        self.out(format!("  {} duration_ms:  {}", c::dim("│"), duration_ms));
        self.out(format!(
            "  {} workload:     {} events / {:.2} eps",
            c::dim("│"),
            report.workload_events,
            report.workload_events_per_sec
        ));
        self.out(format!("  {} reason:       {}", c::dim("│"), report.reason));
        self.out(workload_line(
            &format!("  {} totals", c::dim("│")),
            &report.totals,
        ));
        if !report.steps.is_empty() {
            self.out(format!("  {} steps", c::cyan("▸")));
            for step in &report.steps {
                self.out(format!(
                    "    {} scenario={} aborted={} elapsed={}s workload_events={}",
                    c::bold(&step.step_name),
                    step.scenario.as_str(),
                    step.aborted,
                    step.elapsed_secs,
                    step.workload_events
                ));
                self.out(workload_line("      └", &step.counters));
            }
        }
        self.out(format!("  {} unified benchmark view", c::cyan("▸")));
        let benchmark_report =
            crate::benchmark_report::BenchmarkReport::from_simulation_suite(report);
        for line in benchmark_report.render_text().lines() {
            self.out(line.to_string());
        }
    }

    pub(in crate::cli) async fn simulation_suite(&self, args: &[&str]) {
        let Some(first) = args.first().copied() else {
            self.print_simulation_suites();
            return;
        };
        let Some(suite) = crate::simulation::SimulationSuite::parse(first) else {
            self.out(format!(
                "  {} 未知 simulation suite: {}",
                c::red("✗"),
                first
            ));
            self.print_simulation_suites();
            return;
        };
        let seed = self.state.simulation.status().await.seed;
        let mut steps = suite.plan(seed);
        for token in &args[1..] {
            let Some((key, value)) = token.split_once('=') else {
                self.out(format!(
                    "  {} 无效 suite 参数：{}；请使用 duration=30 tick_ms=1000 persist_every=5",
                    c::red("✗"),
                    token
                ));
                return;
            };
            for step in &mut steps {
                if let Err(err) = step.config.apply_kv(key, value) {
                    self.out(format!("  {} {}", c::red("✗"), err));
                    return;
                }
            }
        }
        self.out(format!(
            "  {} simulation suite 已提交: {} ({} steps)",
            c::green("✓"),
            suite.as_str(),
            steps.len()
        ));
        for (idx, step) in steps.iter().enumerate() {
            self.out(format!(
                "  {} {:>2}. {:<22} preset={} scenario={} users={} rooms={} duration={}s tick_ms={} persist_every={}",
                c::dim("│"),
                idx + 1,
                step.name,
                step.config.preset.as_str(),
                step.config.scenario.as_str(),
                step.config.users,
                step.config.rooms,
                step.config.duration_secs,
                step.config.tick_interval_ms,
                step.config.persist_every_ticks
            ));
        }
        runner::spawn_simulation_suite_runner(
            std::sync::Arc::clone(&self.state),
            self.out_tx.clone(),
            suite,
            steps,
        );
        self.out(format!(
            "  {} suite runner 已启动；每个 step 会独立 run/stop 并写入 simulation.* 事件",
            c::dim("▸")
        ));
    }

    fn print_simulation_suites(&self) {
        self.out(format!("  {} Simulation suites", c::green("◆")));
        for suite in crate::simulation::SimulationSuite::all() {
            let plan = suite.plan(self.state.simulation.seed_hint());
            self.out(format!(
                "  {} {:<8} {} steps - {}",
                c::dim("│"),
                suite.as_str(),
                plan.len(),
                suite.description()
            ));
            for step in plan.iter().take(4) {
                self.out(format!(
                    "      {:<22} scenario={} duration={}s tick_ms={}",
                    step.name,
                    step.config.scenario.as_str(),
                    step.config.duration_secs,
                    step.config.tick_interval_ms
                ));
            }
        }
        self.out(format!(
            "  {} 用法：simulation suite smoke | simulation suite mixed duration=15 tick_ms=500",
            c::dim("▸")
        ));
    }
}

fn workload_line(prefix: &str, counters: &crate::simulation::SimulationCounters) -> String {
    format!(
        "{} ticks={} chats={} ready={} touch_batches={} judge_batches={} round_results={} workload_events={}",
        prefix,
        counters.ticks,
        counters.chat_messages,
        counters.ready_events,
        counters.touch_batches,
        counters.judge_batches,
        counters.round_results,
        counters.workload_events()
    )
}

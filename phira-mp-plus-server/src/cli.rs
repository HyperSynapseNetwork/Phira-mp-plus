//! Administrative command parsing and dispatch.

use crate::plugin::PluginEvent;
use crate::server::PlusServerState;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::info;

mod commands;
mod dispatch;

mod c {
    fn paint(code: &str, text: &str) -> String {
        if std::env::var_os("NO_COLOR").is_some() {
            text.to_string()
        } else {
            format!("\x1b[{code}m{text}\x1b[0m")
        }
    }

    pub fn green(text: &str) -> String {
        paint("32", text)
    }
    pub fn cyan(text: &str) -> String {
        paint("36", text)
    }
    pub fn yellow(text: &str) -> String {
        paint("33", text)
    }
    pub fn red(text: &str) -> String {
        paint("31", text)
    }
    pub fn bold(text: &str) -> String {
        paint("1", text)
    }
    pub fn dim(text: &str) -> String {
        paint("2", text)
    }
    pub fn magenta(text: &str) -> String {
        paint("35", text)
    }
}

fn parse_cli_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "y" | "on" | "enable" | "enabled" | "hide" | "hidden" | "锁定" | "隐藏" | "是"
    )
}

fn parse_room_host_target(value: &str) -> Result<Option<i32>, std::num::ParseIntError> {
    let trimmed = value.trim();
    if trimmed == "?"
        || trimmed == "-"
        || trimmed.eq_ignore_ascii_case("system")
        || trimmed.eq_ignore_ascii_case("none")
        || trimmed.eq_ignore_ascii_case("null")
    {
        Ok(None)
    } else {
        trimmed.parse::<i32>().map(Some)
    }
}

fn is_core_registered_command(name: &str) -> bool {
    matches!(
        name,
        "welcome-config" | "player-count" | "playtime" | "round-last"
    )
}


const CLIENT_CLI_OUTPUT_LINE_LIMIT: usize = 512;

fn strip_trailing_cli_join_marker(value: &str) -> Option<String> {
    let trimmed_len = value.trim_end().len();
    let without_trailing_ws = &value[..trimmed_len];
    without_trailing_ws
        .strip_suffix("--")
        .map(|prefix| prefix.to_string())
}

/// Merge CLI continuation fragments.
///
/// A line ending in `--` is kept pending and must be continued by the next
/// line beginning with `--`. Markers are removed before concatenation, so:
/// `room set a--` + `-- b--` + `-- c` becomes `room set a b c`.
pub fn collect_cli_continuation(
    pending: &mut Option<String>,
    line: String,
) -> Result<Option<String>, String> {
    let line = line.trim().to_string();
    if line.is_empty() {
        return Ok(None);
    }

    let mut merged = if let Some(mut prefix) = pending.take() {
        let Some(suffix) = line.trim_start().strip_prefix("--") else {
            return Err("上一条命令以 -- 结尾，下一条必须以 -- 开头；已取消续行".to_string());
        };
        prefix.push_str(suffix);
        prefix
    } else {
        line
    };

    if let Some(prefix) = strip_trailing_cli_join_marker(&merged) {
        *pending = Some(prefix);
        return Ok(None);
    }

    Ok(Some(std::mem::take(&mut merged).trim().to_string()))
}

/// Execute one CLI command and collect its output.
///
/// Used by the in-game admin `_<command>` room creation shortcut. The normal
/// interactive banner is filtered so the client receives only command output.
pub async fn execute_cli_once(state: Arc<PlusServerState>, line: String) -> Vec<String> {
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<String>();
    let handler = CliHandler::new(state, out_tx);
    let task = tokio::spawn(async move {
        handler.start(cmd_rx).await;
    });
    let _ = cmd_tx.send(line);
    drop(cmd_tx);

    let mut lines = Vec::new();
    loop {
        match tokio::time::timeout(std::time::Duration::from_millis(800), out_rx.recv()).await {
            Ok(Some(line)) => {
                let trimmed = strip_ansi_for_client(&line).trim().to_string();
                if trimmed.is_empty()
                    || trimmed.contains("管理控制台")
                    || trimmed.contains("输入 help 查看命令帮助")
                {
                    continue;
                }
                lines.push(trimmed);
                if lines.len() >= CLIENT_CLI_OUTPUT_LINE_LIMIT {
                    lines.push(format!("……输出过长，仅显示前 {CLIENT_CLI_OUTPUT_LINE_LIMIT} 行"));
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    task.abort();
    if lines.is_empty() {
        lines.push("命令已执行，无输出".to_string());
    }
    lines
}


fn cli_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
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

fn redact_cli_command_for_event(line: &str) -> String {
    let mut parts = line.split_whitespace();
    let command = parts.next().unwrap_or_default();
    match command {
        "benchmark-bind" => "benchmark-bind <redacted>".to_string(),
        "plugin" if matches!(parts.next(), Some("call")) => "plugin call <args>".to_string(),
        _ => line.to_string(),
    }
}


fn publish_simulation_tick_event(state: &PlusServerState, status: &crate::simulation::SimulationStatus) {
    state.publish_runtime_event(crate::event_bus::MpEvent::Custom {
        kind: "simulation.tick".to_string(),
        payload: serde_json::json!({
            "run_id": status.run_id.map(|id| id.to_string()),
            "ticks": status.counters.ticks,
            "elapsed_secs": status.elapsed_secs,
            "remaining_secs": status.remaining_secs,
            "scenario": status.config.scenario.as_str(),
            "chat_messages": status.counters.chat_messages,
            "ready_events": status.counters.ready_events,
            "touch_batches": status.counters.touch_batches,
            "judge_batches": status.counters.judge_batches,
            "round_results": status.counters.round_results,
        }),
    });
}

fn publish_simulation_generated_events(
    state: &PlusServerState,
    events: &[crate::simulation::SimulationGeneratedEvent],
) {
    for event in events {
        state.publish_runtime_event(crate::event_bus::MpEvent::Custom {
            kind: event.kind.clone(),
            payload: event.payload.clone(),
        });
    }
}

async fn publish_simulation_snapshot(
    state: &Arc<PlusServerState>,
    run_id: uuid::Uuid,
    status: &crate::simulation::SimulationStatus,
    source: &str,
) {
    let world = state.simulation.world_snapshot(64).await;
    state.publish_runtime_event(crate::event_bus::MpEvent::Custom {
        kind: "simulation.snapshot".to_string(),
        payload: serde_json::json!({
            "run_id": run_id.to_string(),
            "status": status,
            "world": world,
            "source": source,
        }),
    });
}

fn spawn_simulation_runner(
    state: Arc<PlusServerState>,
    out_tx: mpsc::UnboundedSender<String>,
    run_id: uuid::Uuid,
    config: crate::simulation::SimulationConfig,
) {
    tokio::spawn(async move {
        let interval = std::time::Duration::from_millis(config.tick_interval_ms);
        let _ = out_tx.send(format!(
            "  ◆ simulation runner started: run_id={} tick_ms={} duration={}s persist_every={}",
            run_id, config.tick_interval_ms, config.duration_secs, config.persist_every_ticks
        ));

        loop {
            tokio::time::sleep(interval).await;
            let (status, events) = match state.simulation.advance_ticks_for_run_with_events(run_id, 1).await {
                Ok(result) => result,
                Err(_) => break,
            };
            publish_simulation_tick_event(&state, &status);
            publish_simulation_generated_events(&state, &events);

            if config.persist_every_ticks > 0
                && status.counters.ticks > 0
                && status.counters.ticks % config.persist_every_ticks == 0
            {
                publish_simulation_snapshot(&state, run_id, &status, "simulation.runner.periodic").await;
            }

            if status.elapsed_secs >= config.duration_secs {
                let reason = format!("duration {}s reached", config.duration_secs);
                if let Some(stopped) = state.simulation.stop_if_run(run_id, reason.clone()).await {
                    state.publish_runtime_event(crate::event_bus::MpEvent::SimulationStopped {
                        run_id,
                        reason: reason.clone(),
                    });
                    let _ = state
                        .broadcast_system_message("性能测试已结束。Runtime v2 simulation runner reached its configured duration.")
                        .await;
                    let _ = out_tx.send(format!(
                        "  ✓ simulation runner stopped: run_id={} ticks={} elapsed={}s reason={}",
                        run_id, stopped.counters.ticks, stopped.elapsed_secs, reason
                    ));
                    if config.persist_every_ticks > 0 {
                        publish_simulation_snapshot(&state, run_id, &stopped, "simulation.runner.final").await;
                    }
                }
                break;
            }
        }
    });
}


fn spawn_simulation_suite_runner(
    state: Arc<PlusServerState>,
    out_tx: mpsc::UnboundedSender<String>,
    suite: crate::simulation::SimulationSuite,
    steps: Vec<crate::simulation::SimulationSuiteStep>,
) {
    tokio::spawn(async move {
        let suite_run_id = uuid::Uuid::new_v4();
        let suite_started_at_ms = cli_now_ms();
        let total_steps = steps.len();
        let plan: Vec<_> = steps
            .iter()
            .enumerate()
            .map(|(idx, step)| {
                serde_json::json!({
                    "index": idx + 1,
                    "name": step.name.as_str(),
                    "preset": step.config.preset.as_str(),
                    "scenario": step.config.scenario.as_str(),
                    "users": step.config.users,
                    "rooms": step.config.rooms,
                    "duration_secs": step.config.duration_secs,
                    "tick_ms": step.config.tick_interval_ms,
                    "persist_every": step.config.persist_every_ticks,
                })
            })
            .collect();

        state.publish_runtime_event(crate::event_bus::MpEvent::Custom {
            kind: "simulation.suite_started".to_string(),
            payload: serde_json::json!({
                "suite_run_id": suite_run_id.to_string(),
                "suite": suite.as_str(),
                "steps": plan,
            }),
        });
        let _ = out_tx.send(format!(
            "  ◆ simulation suite started: suite={} suite_run_id={} steps={}",
            suite.as_str(), suite_run_id, total_steps
        ));
        let _ = state
            .broadcast_system_message("服务器正在进行 Runtime v2 Simulation suite；期间可能出现短暂卡顿。")
            .await;

        let mut completed_steps = 0usize;
        let mut aborted = false;
        let mut abort_reason = "completed".to_string();
        let mut step_reports: Vec<crate::simulation::SimulationRunReport> = Vec::new();
        for (idx, step) in steps.into_iter().enumerate() {
            let step_index = idx + 1;
            if state.simulation.status().await.running {
                abort_reason = format!("another simulation was running before step {step_index}");
                let _ = out_tx.send(format!(
                    "  ! simulation suite aborted before step {} because another simulation is running",
                    step_index
                ));
                aborted = true;
                break;
            }

            let status = match state.simulation.start(step.config.clone()).await {
                Ok(status) => status,
                Err(err) => {
                    abort_reason = format!("step {} failed to start: {}", step.name, err);
                    let _ = out_tx.send(format!(
                        "  ✗ simulation suite step {} failed to start: {}",
                        step_index, err
                    ));
                    aborted = true;
                    break;
                }
            };
            let Some(run_id) = status.run_id else {
                abort_reason = format!("step {} started without run_id", step.name);
                let _ = out_tx.send(format!(
                    "  ✗ simulation suite step {} started without run_id; aborting suite",
                    step_index
                ));
                aborted = true;
                break;
            };

            state.publish_runtime_event(crate::event_bus::MpEvent::SimulationStarted { run_id });
            state.publish_runtime_event(crate::event_bus::MpEvent::Custom {
                kind: "simulation.suite_step_started".to_string(),
                payload: serde_json::json!({
                    "suite_run_id": suite_run_id.to_string(),
                    "suite": suite.as_str(),
                    "step_index": step_index,
                    "step_total": total_steps,
                    "step": step.name.as_str(),
                    "run_id": run_id.to_string(),
                    "scenario": step.config.scenario.as_str(),
                }),
            });
            let _ = out_tx.send(format!(
                "  ◆ suite step {}/{} started: {} scenario={} run_id={}",
                step_index,
                total_steps,
                step.name,
                step.config.scenario.as_str(),
                run_id
            ));

            let interval = std::time::Duration::from_millis(step.config.tick_interval_ms);
            loop {
                tokio::time::sleep(interval).await;
                let (status, events) = match state
                    .simulation
                    .advance_ticks_for_run_with_events(run_id, 1)
                    .await
                {
                    Ok(result) => result,
                    Err(_) => {
                        abort_reason = format!("step {} stopped externally", step.name);
                        let _ = out_tx.send(format!(
                            "  ! simulation suite step {} stopped externally; aborting remaining steps",
                            step.name
                        ));
                        aborted = true;
                        break;
                    }
                };
                publish_simulation_tick_event(&state, &status);
                publish_simulation_generated_events(&state, &events);

                if step.config.persist_every_ticks > 0
                    && status.counters.ticks > 0
                    && status.counters.ticks % step.config.persist_every_ticks == 0
                {
                    publish_simulation_snapshot(&state, run_id, &status, "simulation.suite.periodic").await;
                }

                if status.elapsed_secs >= step.config.duration_secs {
                    let reason = format!(
                        "suite {} step {} duration {}s reached",
                        suite.as_str(), step.name, step.config.duration_secs
                    );
                    if let Some(stopped) = state.simulation.stop_if_run(run_id, reason.clone()).await {
                        state.publish_runtime_event(crate::event_bus::MpEvent::SimulationStopped {
                            run_id,
                            reason: reason.clone(),
                        });
                        state.publish_runtime_event(crate::event_bus::MpEvent::Custom {
                            kind: "simulation.suite_step_completed".to_string(),
                            payload: serde_json::json!({
                                "suite_run_id": suite_run_id.to_string(),
                                "suite": suite.as_str(),
                                "step_index": step_index,
                                "step_total": total_steps,
                                "step": step.name.as_str(),
                                "run_id": run_id.to_string(),
                                "scenario": step.config.scenario.as_str(),
                                "ticks": stopped.counters.ticks,
                                "elapsed_secs": stopped.elapsed_secs,
                                "workload_events": stopped.counters.workload_events(),
                            }),
                        });
                        if step.config.persist_every_ticks > 0 {
                            publish_simulation_snapshot(&state, run_id, &stopped, "simulation.suite.final").await;
                        }
                        step_reports.push(crate::simulation::SimulationRunReport::from_status(
                            Some(suite_run_id),
                            Some(suite),
                            step.name.clone(),
                            &stopped,
                            false,
                            reason.clone(),
                        ));
                        let _ = out_tx.send(format!(
                            "  ✓ suite step {}/{} completed: {} ticks={} elapsed={}s workload_events={}",
                            step_index,
                            total_steps,
                            step.name,
                            stopped.counters.ticks,
                            stopped.elapsed_secs,
                            stopped.counters.workload_events()
                        ));
                    }
                    completed_steps += 1;
                    break;
                }
            }

            if aborted {
                break;
            }
        }

        let report = crate::simulation::SimulationSuiteReport::new(
            suite_run_id,
            suite,
            suite_started_at_ms,
            cli_now_ms(),
            total_steps,
            completed_steps,
            aborted,
            abort_reason.clone(),
            step_reports,
        );
        state.publish_runtime_event(crate::event_bus::MpEvent::Custom {
            kind: "simulation.suite_report".to_string(),
            payload: serde_json::json!({
                "suite_run_id": suite_run_id.to_string(),
                "suite": suite.as_str(),
                "completed_steps": completed_steps,
                "total_steps": total_steps,
                "aborted": aborted,
                "reason": abort_reason,
                "workload_events": report.workload_events,
                "events_per_sec": report.workload_events_per_sec,
                "totals": report.totals.clone(),
            }),
        });
        state.publish_runtime_event(crate::event_bus::MpEvent::Custom {
            kind: "simulation.suite_completed".to_string(),
            payload: serde_json::json!({
                "suite_run_id": suite_run_id.to_string(),
                "suite": suite.as_str(),
                "completed_steps": completed_steps,
                "total_steps": total_steps,
                "aborted": aborted,
                "workload_events": report.workload_events,
                "events_per_sec": report.workload_events_per_sec,
            }),
        });
        state.simulation.record_suite_report(report.clone()).await;
        let _ = state
            .broadcast_system_message("Runtime v2 Simulation suite 已结束。")
            .await;
        let _ = out_tx.send(format!(
            "  {} simulation suite finished: suite={} completed={}/{} aborted={} workload_events={} eps={:.2}",
            if aborted { "!" } else { "✓" },
            suite.as_str(),
            completed_steps,
            total_steps,
            aborted,
            report.workload_events,
            report.workload_events_per_sec
        ));
        let _ = out_tx.send("  ▸ 查看报告：simulation report".to_string());
    });
}

fn strip_ansi_for_client(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if matches!(chars.peek(), Some('[')) {
                chars.next();
                while let Some(c) = chars.next() {
                    if ('@'..='~').contains(&c) { break; }
                }
            }
            continue;
        }
        if !ch.is_control() || ch == '\t' {
            out.push(ch);
        }
    }
    out
}

/// CLI 命令处理器
pub struct CliHandler {
    state: Arc<PlusServerState>,
    running: Arc<RwLock<bool>>,
    out_tx: mpsc::UnboundedSender<String>,
}

impl CliHandler {
    pub fn new(state: Arc<PlusServerState>, out_tx: mpsc::UnboundedSender<String>) -> Self {
        Self {
            state,
            running: Arc::new(RwLock::new(true)),
            out_tx,
        }
    }

    pub fn is_running(&self) -> Arc<RwLock<bool>> {
        Arc::clone(&self.running)
    }

    /// 发送一行输出到 TUI
    fn out(&self, msg: impl Into<String>) {
        let _ = self.out_tx.send(msg.into());
    }

    /// 启动 CLI 处理器（运行在 tokio 任务中）
    pub async fn start(&self, mut cmd_rx: mpsc::UnboundedReceiver<String>) {
        info!("CLI management console started");

        self.out(String::new());
        self.out(format!("  {} Phira-mp+ v{} 管理控制台", c::bold("◆"), env!("CARGO_PKG_VERSION")));
        self.out(format!("  {} 输入 {} 查看命令帮助，{} 关闭服务器",
            c::dim("▸"), c::cyan("help"), c::red("exit")));
        self.out(String::new());

        let mut pending_line: Option<String> = None;
        while let Some(line) = cmd_rx.recv().await {
            // 如果未运行（exit 后），直接退出
            if !*self.running.read().await {
                break;
            }

            let line = match collect_cli_continuation(&mut pending_line, line) {
                Ok(Some(line)) => line,
                Ok(None) => {
                    if pending_line.is_some() {
                        self.out(format!("  {} 已暂存续行；下一条命令需以 -- 开头", c::dim("▸")));
                    }
                    continue;
                }
                Err(err) => {
                    self.out(format!("  {} {err}", c::red("✗")));
                    continue;
                }
            };
            if line.is_empty() {
                continue;
            }

            let mut parts = line.split_whitespace();
            let command = parts.next().unwrap_or("");
            let args: Vec<&str> = parts.collect();

            if !command.is_empty() {
                self.state.event_bus.publish(crate::event_bus::MpEvent::AdminCommandExecuted {
                    user_id: None,
                    command: redact_cli_command_for_event(&line),
                });
            }

            if !self.dispatch_command(command, &args).await {
                break;
            }
        }

        info!("CLI session ended");
    }

    async fn simulation_report(&self, args: &[&str]) {
        let sub = args.first().copied().unwrap_or("latest");
        match sub {
            "latest" | "" => {
                match self.state.simulation.latest_suite_report().await {
                    Some(report) => self.print_suite_report(&report),
                    None => self.out(format!("  {} 暂无 simulation suite report；先执行 simulation suite smoke", c::yellow("?"))),
                }
            }
            "list" => {
                let limit = args.get(1).and_then(|value| value.parse::<usize>().ok()).unwrap_or(8);
                let reports = self.state.simulation.suite_reports(limit).await;
                if reports.is_empty() {
                    self.out(format!("  {} 暂无 simulation suite report", c::yellow("?")));
                    return;
                }
                self.out(format!("  {} 最近 {} 份 Simulation suite report", c::green("◆"), reports.len()));
                for report in reports {
                    self.out(format!(
                        "  {} suite={} suite_run_id={} completed={}/{} aborted={} workload_events={} eps={:.2}",
                        c::dim("│"),
                        report.suite.as_str(),
                        report.suite_run_id,
                        report.completed_steps,
                        report.total_steps,
                        report.aborted,
                        report.workload_events,
                        report.workload_events_per_sec
                    ));
                }
            }
            "clear" => {
                let count = self.state.simulation.clear_suite_reports().await;
                self.out(format!("  {} 已清理 {} 份 simulation suite report", c::green("✓"), count));
            }
            other => {
                self.out(format!("  {} 未知 simulation report 子命令: {}", c::red("✗"), c::yellow(other)));
                self.out(format!("  {} 可用: simulation report | simulation report list [limit] | simulation report clear", c::dim("▸")));
            }
        }
    }

    fn print_suite_report(&self, report: &crate::simulation::SimulationSuiteReport) {
        self.out(format!(
            "  {} Simulation suite report: suite={} suite_run_id={}",
            c::green("◆"),
            report.suite.as_str(),
            report.suite_run_id
        ));
        self.out(format!(
            "  {} completed={}/{} aborted={} elapsed={}s workload_events={} eps={:.2} reason={}",
            c::dim("│"),
            report.completed_steps,
            report.total_steps,
            report.aborted,
            report.total_elapsed_secs,
            report.workload_events,
            report.workload_events_per_sec,
            report.reason
        ));
        self.out(workload_line(&format!("  {} totals", c::dim("│")), &report.totals));
        for (idx, step) in report.steps.iter().enumerate() {
            self.out(format!(
                "  {} {:>2}. {:<22} scenario={} run_id={} elapsed={}s aborted={} eps={:.2} reason={}",
                c::dim("│"),
                idx + 1,
                step.step_name,
                step.scenario.as_str(),
                step.run_id.map(|id| id.to_string()).unwrap_or_else(|| "none".to_string()),
                step.elapsed_secs,
                step.aborted,
                step.workload_events_per_sec,
                step.reason
            ));
            self.out(workload_line("      └", &step.counters));
        }
        self.out(format!("  {} reports 只记录最近 32 份；simulation report list 可查看历史摘要", c::dim("▸")));
    }

    async fn simulation_suite(&self, args: &[&str]) {
        let first = args.first().copied().unwrap_or("list");
        if matches!(first, "list" | "") {
            self.print_simulation_suites();
            return;
        }

        let Some(suite) = crate::simulation::SimulationSuite::parse(first) else {
            self.out(format!("  {} 未知 simulation suite: {}", c::red("✗"), c::yellow(first)));
            self.print_simulation_suites();
            return;
        };

        let seed = self.state.simulation.status().await.seed;
        let mut steps = suite.plan(seed);
        for token in &args[1..] {
            let Some((key, value)) = token.split_once('=') else {
                self.out(format!("  {} 无效 suite 参数：{}；请使用 duration=30 tick_ms=500 persist_every=5 users=100 rooms=10", c::red("✗"), token));
                return;
            };
            for step in &mut steps {
                if let Err(err) = step.config.apply_kv(key, value) {
                    self.out(format!("  {} {}", c::red("✗"), err));
                    return;
                }
                // A suite is always an automatic sequential runner even when the
                // user reuses generic SimulationConfig keys.
                step.config.auto_tick = true;
            }
        }

        for step in &steps {
            if let Err(err) = step.config.validate() {
                self.out(format!("  {} suite step {} invalid: {}", c::red("✗"), c::yellow(&step.name), err));
                return;
            }
        }

        if self.state.simulation.status().await.running {
            self.out(format!("  {} simulation 正在运行，请先执行 simulation stop 或等待当前 runner 结束", c::red("✗")));
            return;
        }

        self.out(format!("  {} simulation suite 计划: {} - {}", c::green("◆"), suite.as_str(), suite.description()));
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
        spawn_simulation_suite_runner(Arc::clone(&self.state), self.out_tx.clone(), suite, steps);
        self.out(format!("  {} suite runner 已启动；每个 step 会独立 run/stop 并写入 simulation.* 事件", c::dim("▸")));
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
        self.out(format!("  {} 用法：simulation suite smoke | simulation suite mixed duration=15 tick_ms=500", c::dim("▸")));
    }

    async fn simulation_run(&self, args: &[&str]) {
        let seed = self.state.simulation.status().await.seed;
        let preset = args
            .first()
            .and_then(|value| crate::simulation::SimulationPreset::parse(value))
            .unwrap_or(crate::simulation::SimulationPreset::Baseline);
        let mut config = preset.defaults(seed);
        let option_start = if args.first().and_then(|value| crate::simulation::SimulationPreset::parse(value)).is_some() { 1 } else { 0 };
        for token in &args[option_start..] {
            let Some((key, value)) = token.split_once('=') else {
                self.out(format!("  {} 无效参数：{}；请使用 users=500 rooms=50 duration=300 scenario=chat_storm tick_ms=1000 auto=true persist_every=0", c::red("✗"), token));
                return;
            };
            if let Err(err) = config.apply_kv(key, value) {
                self.out(format!("  {} {}", c::red("✗"), err));
                return;
            }
        }

        match self.state.simulation.start(config).await {
            Ok(status) => {
                if let Some(run_id) = status.run_id {
                    self.state.event_bus.publish(crate::event_bus::MpEvent::SimulationStarted { run_id });
                }
                self.broadcast_all("服务器正在进行性能测试，期间可能出现短暂卡顿。Runtime v2 当前为安全骨架模式，不会创建真实房间。").await;
                self.out(format!("  {} simulation 已启动: {:?}", c::green("✓"), status.run_id));
                self.out(format!("  {} preset={:?} scenario={} users={} rooms={} duration={}s touch={} judge={} chat={} ready={} rounds={}",
                    c::dim("│"), status.config.preset, status.config.scenario.as_str(), status.config.users, status.config.rooms,
                    status.config.duration_secs, status.config.touch, status.config.judge,
                    status.config.chat, status.config.ready, status.config.rounds));
                self.out(format!("  {} runner: auto={} tick_ms={} persist_every={}",
                    c::dim("│"), status.config.auto_tick, status.config.tick_interval_ms, status.config.persist_every_ticks));
                self.out(format!("  {} shadow world: {} users / {} rooms / {} rounds materialized",
                    c::dim("│"), status.materialized_users, status.materialized_rooms, status.materialized_rounds));
                if status.config.auto_tick {
                    if let Some(run_id) = status.run_id {
                        spawn_simulation_runner(Arc::clone(&self.state), self.out_tx.clone(), run_id, status.config.clone());
                        self.out(format!("  {} 自动 runner 已启动；到达 duration 后会自动 stop", c::dim("▸")));
                    } else {
                        self.out(format!("  {} simulation 已启动但缺少 run_id，自动 runner 未启动", c::yellow("!")));
                    }
                } else {
                    self.out(format!("  {} auto=false，需手动执行 simulation tick [n] 推进", c::dim("▸")));
                }
                self.out(format!("  {} Step 8 已启用聚合仿真事件；仍不写入真实 rooms/users 表", c::dim("▸")));
            }
            Err(err) => self.out(format!("  {} {}", c::red("✗"), err)),
        }
    }

    async fn simulation_persist(&self) {
        let status = self.state.simulation.status().await;
        let Some(run_id) = status.run_id else {
            self.out(format!("  {} simulation 未运行，无法生成 snapshot", c::red("✗")));
            return;
        };
        publish_simulation_snapshot(&self.state, run_id, &status, "cli.simulation.persist").await;
        self.out(format!("  {} simulation snapshot 已发送到 EventBus / PersistenceWorker", c::green("✓")));
        self.out(format!("  {} run_id={}", c::dim("│"), run_id));
        self.out(format!("  {} 这是 simulation 专用诊断数据，不会写入真实 mp_* 玩家/房间表", c::dim("▸")));
    }

    async fn print_simulation_status(&self) {
        let status = self.state.simulation.status().await;
        self.out(format!("  {} Runtime v2 Simulation", c::green("◆")));
        self.out(format!("  {} running:        {}", c::dim("│"), status.running));
        self.out(format!("  {} run_id:         {}", c::dim("│"), status.run_id.as_ref().map(|id| id.to_string()).unwrap_or_else(|| "-".to_string())));
        self.out(format!("  {} seed:           {}", c::dim("│"), status.seed));
        self.out(format!("  {} preset:         {:?}", c::dim("│"), status.config.preset));
        self.out(format!("  {} scenario:       {} - {}", c::dim("│"), status.config.scenario.as_str(), status.config.scenario.description()));
        self.out(format!("  {} target:         {} users / {} rooms / {}s", c::dim("│"), status.config.users, status.config.rooms, status.config.duration_secs));
        self.out(format!("  {} elapsed/remain: {}/{}s", c::dim("│"), status.elapsed_secs, status.remaining_secs.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string())));
        self.out(format!("  {} runner:         auto={} tick_ms={} persist_every={}", c::dim("│"), status.runner_enabled, status.config.tick_interval_ms, status.config.persist_every_ticks));
        self.out(format!("  {} touch/judge:    {} / {}", c::dim("│"), status.config.touch, status.config.judge));
        self.out(format!("  {} virtual state:  {} users / {} rooms", c::dim("│"), status.virtual_users, status.virtual_rooms));
        self.out(format!("  {} materialized:   {} users / {} rooms / {} rounds", c::dim("│"), status.materialized_users, status.materialized_rooms, status.materialized_rounds));
        self.out(format!("  {} counters:       ticks={} chats={} ready={} touch={} judge={} result={}",
            c::dim("│"), status.counters.ticks, status.counters.chat_messages,
            status.counters.ready_events, status.counters.touch_batches,
            status.counters.judge_batches, status.counters.round_results));
        self.out(format!("  {} note:           {}", c::dim("│"), status.note));
    }

    async fn print_simulation_world(&self, limit: usize) {
        let Some(world) = self.state.simulation.world_snapshot(limit).await else {
            self.out(format!("  {} simulation shadow world 不存在；请先执行 simulation run baseline", c::yellow("!")));
            return;
        };
        self.out(format!("  {} Simulation Shadow World", c::green("◆")));
        self.out(format!("  {} run_id:       {}", c::dim("│"), world.run_id.as_ref().map(|id| id.to_string()).unwrap_or_else(|| "-".to_string())));
        self.out(format!("  {} totals:       {} users / {} rooms", c::dim("│"), world.users_total, world.rooms_total));
        self.out(format!("  {} materialized: {} users / {} rooms / {} rounds", c::dim("│"), world.users_materialized, world.rooms_materialized, world.rounds_materialized));
        self.out(format!("  {} {}", c::dim("▸"), world.materialization_note));
        self.out(format!("  {} sample rooms", c::cyan("▸")));
        for room in world.sample_rooms.iter().take(8) {
            self.out(format!("    {} chart={} members={} ready={} playing={} round={}",
                c::bold(&room.id), room.chart_id, room.member_ids.len(), room.ready_count,
                room.playing, room.round_id.as_deref().unwrap_or("-")));
        }
        self.out(format!("  {} sample users", c::cyan("▸")));
        for user in world.sample_users.iter().take(8) {
            self.out(format!("    {} id={} room={} ready={} playing={}",
                c::bold(&user.name), user.id, user.room_id.as_deref().unwrap_or("-"), user.ready, user.playing));
        }
        if !world.sample_rounds.is_empty() {
            self.out(format!("  {} sample rounds", c::cyan("▸")));
            for round in world.sample_rounds.iter().take(6) {
                self.out(format!("    {} room={} chart={} players={} score={} touch={} judge={}",
                    c::bold(&round.round_id), round.room_id, round.chart_id, round.players,
                    round.sample_score, round.sample_touches, round.sample_judges));
            }
        }
        if !world.recent_events.is_empty() {
            self.out(format!("  {} recent events", c::cyan("▸")));
            for event in world.recent_events.iter().take(8) {
                self.out(format!("    #{:<4} {:<10} {}", event.seq, event.kind, event.message));
            }
        }
    }

    async fn print_help(&self, args: &[&str]) {
        if !args.is_empty() {
            match args {
                ["all"] => {
                    for line in self.state.command_registry.format_overview_all().lines() {
                        self.out(format!("  {line}"));
                    }
                    return;
                }
                ["groups"] | ["group"] => {
                    for line in self.state.command_registry.format_groups().lines() {
                        self.out(format!("  {line}"));
                    }
                    return;
                }
                ["group", group] => {
                    for line in self.state.command_registry.format_group(group, false).lines() {
                        self.out(format!("  {line}"));
                    }
                    return;
                }
                ["group", group, "all"] | ["group", group, "full"] => {
                    for line in self.state.command_registry.format_group(group, true).lines() {
                        self.out(format!("  {line}"));
                    }
                    return;
                }
                _ => {}
            }

            let query = args.join(" ");
            if let Some(help) = self.state.command_registry.format_help(&query) {
                for line in help.lines() {
                    self.out(format!("  {line}"));
                }
                return;
            }
            self.out(format!("  {} 未找到命令帮助: {}", c::yellow("!"), query));
            self.out(format!("  {} {}", c::dim("▸"), self.state.command_registry.format_unknown(&query)));
            return;
        }

        for line in self.state.command_registry.format_overview().lines() {
            if line.starts_with('▸') {
                self.out(format!("  {}", c::cyan(line)));
            } else if line.starts_with('─') || line.starts_with("提示") {
                self.out(format!("  {}", c::dim(line)));
            } else {
                self.out(format!("  {line}"));
            }
        }

        let plugin_cmds = self.state.plugin_manager.list_cli_commands().await;
        let (core_cmds, wasm_cmds): (Vec<_>, Vec<_>) = plugin_cmds
            .into_iter()
            .partition(|cmd| is_core_registered_command(&cmd.name));
        if !core_cmds.is_empty() {
            self.out(String::new());
            self.out(format!("  {} 内置扩展", c::cyan("▸")));
            for cmd in &core_cmds {
                self.out(format!("    {:<22} {}", c::dim(&cmd.name), cmd.description));
            }
        }
        if !wasm_cmds.is_empty() {
            self.out(String::new());
            self.out(format!("  {} WASM 插件扩展", c::magenta("▸")));
            for cmd in &wasm_cmds {
                self.out(format!("    {:<22} {}", c::dim(&cmd.name), cmd.description));
            }
        }
        self.out(String::new());
        self.out(format!("  {} help <命令> 查看统一详情；help all / help groups / help group <分组> 可展开", c::dim("▸")));
    }

    async fn list_plugins(&self) {
        let plugins = self.state.plugin_manager.list_plugins().await;
        if plugins.is_empty() {
            self.out(format!("  {} 无已加载的插件", c::dim("·")));
            return;
        }
        self.out(format!("  {} 已加载插件 ({})", c::green("◆"), plugins.len()));
        self.out(format!("  {}", c::dim("  ────────────────────────────────────────────")));
        for p in &plugins {
            let state_str = match &p.state {
                crate::plugin::PluginState::Enabled => c::green("启用"),
                crate::plugin::PluginState::Disabled => c::yellow("禁用"),
                crate::plugin::PluginState::Loaded => c::cyan("已加载"),
                crate::plugin::PluginState::Error(_) => c::red("错误"),
            };
            let stable_id = std::path::Path::new(&p.path)
                .file_stem().and_then(|value| value.to_str()).unwrap_or("?");
            self.out(format!("  {} {:<18} {} {}  {}",
                c::dim("│"), stable_id, c::dim(p.info.version.as_str()), state_str,
                c::dim(&format!("({})", p.info.name))));
        }
    }

    async fn enable_plugin(&self, name: &str) {
        match self.state.plugin_manager.enable_plugin(name).await {
            Ok(_) => self.out(format!("  {} 插件 {} 已启用", c::green("✓"), c::bold(name))),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    async fn disable_plugin(&self, name: &str) {
        match self.state.plugin_manager.disable_plugin(name).await {
            Ok(_) => self.out(format!("  {} 插件 {} 已禁用", c::green("✓"), c::bold(name))),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    async fn reload_plugins(&self) {
        self.out(format!("  {} 正在重载所有插件...", c::yellow("⟳")));
        match self.state.plugin_manager.reload_plugins().await {
            Ok(count) => self.out(format!("  {} 已重载 {} 个插件", c::green("✓"), count)),
            Err(e) => self.out(format!("  {} 重载失败: {}", c::red("✗"), e)),
        }
    }

    async fn plugin_info(&self, name: &str) {
        let plugins = self.state.plugin_manager.list_plugins().await;
        if let Some(p) = plugins.into_iter().find(|p| {
            p.info.name == name || std::path::Path::new(&p.path)
                .file_stem().and_then(|value| value.to_str()) == Some(name)
        }) {
            let state_str = match &p.state {
                crate::plugin::PluginState::Enabled => c::green("启用"),
                crate::plugin::PluginState::Disabled => c::yellow("禁用"),
                crate::plugin::PluginState::Loaded => c::cyan("已加载"),
                crate::plugin::PluginState::Error(ref e) => c::red(&format!("错误: {}", e)),
            };
            self.out(format!("  {} 插件详情: {}", c::green("◆"), c::bold(&p.info.name)));
            let stable_id = std::path::Path::new(&p.path)
                .file_stem().and_then(|value| value.to_str()).unwrap_or("?");
            self.out(format!("  {} ID:       {}", c::dim("│"), stable_id));
            self.out(format!("  {} 版本:     {}", c::dim("│"), p.info.version));
            self.out(format!("  {} 作者:     {}", c::dim("│"), p.info.author));
            self.out(format!("  {} 描述:     {}", c::dim("│"), p.info.description));
            self.out(format!("  {} 状态:     {}", c::dim("│"), state_str));
            self.out(format!("  {} 路径:     {}", c::dim("│"), c::dim(&p.path)));
        } else {
            self.out(format!("  {} 未找到插件: {}", c::yellow("!"), name));
        }
    }


    async fn plugin_call(&self, plugin: &str, method: &str, args_json: &str) {
        let args = if args_json.trim().is_empty() {
            Vec::new()
        } else {
            match serde_json::from_str::<Vec<serde_json::Value>>(args_json) {
                Ok(value) => value,
                Err(error) => {
                    self.out(format!("  {} 参数必须是 JSON 数组: {}", c::red("✗"), error));
                    return;
                }
            }
        };
        match self.state.plugin_manager.call_plugin_api(plugin, method, args).await {
            Ok(value) => self.out(format!("  {} {}", c::green("✓"), value)),
            Err(error) => self.out(format!("  {} {}", c::red("✗"), error)),
        }
    }


    async fn list_users(&self) {
        let users = self.state.users.read().await;
        let player_count = users.values().filter(|user| user.id > 0).count();
        if player_count == 0 {
            self.out(format!("  {} 当前无在线用户", c::dim("·")));
        } else {
            self.out(format!("  {} 在线用户 ({})", c::green("◆"), player_count));
            self.out(format!("  {}", c::dim("  ────────────────────────────────────────────")));
            for user in users.values().filter(|user| user.id > 0) {
                let monitor = if user.monitor.load(std::sync::atomic::Ordering::SeqCst) {
                    c::yellow(" [观]")
                } else {
                    String::new()
                };
                let in_room = {
                    let room_guard = user.room.read().await;
                    room_guard.as_ref()
                        .map(|r| format!(" {} 房间 {}", c::dim("·"), c::cyan(&r.id.to_string())))
                        .unwrap_or_default()
                };
                self.out(format!("  {} {:<6} {}{}{}",
                    c::dim("│"), user.id, c::bold(&user.name), monitor, in_room));
            }
        }
        drop(users);

        // 显示房间概要
        let rooms = self.state.rooms.read().await;
        if !rooms.is_empty() {
            self.out(String::new());
            self.out(format!("  {} 活跃房间 ({})", c::green("◆"), rooms.len()));
            for room in rooms.values() {
                let users_count = room.users().await.len();
                let monitors_count = room.monitors().await.len();
                let state_str = match &*room.state.read().await {
                    crate::room::InternalRoomState::SelectChart => c::cyan("选曲中"),
                    crate::room::InternalRoomState::WaitForReady { .. } => c::yellow("等待准备"),
                    crate::room::InternalRoomState::Playing { .. } => c::magenta("游戏中"),
                };
                self.out(format!("  {} {:<15}  {}  {}+{} 人",
                    c::dim("│"), room.id.to_string(), state_str, users_count, monitors_count));
            }
        }
    }

    async fn list_rooms(&self) {
        let rooms: Vec<Arc<crate::room::Room>> = {
            let guard = self.state.rooms.read().await;
            guard.values().map(Arc::clone).collect()
        };
        if rooms.is_empty() {
            self.out(format!("  {} 当前无活跃房间", c::dim("·")));
            return;
        }

        self.out(format!("  {} 活跃房间 ({})", c::green("◆"), rooms.len()));
        self.out(format!("  {}", c::dim("  ────────────────────────────────────────────")));
        for room in &rooms {
            self.state.refresh_room_display_metadata(room).await;
            let users_in_room = room.users().await;
            let monitors_in_room = room.monitors().await;

            let state_str = match &*room.state.read().await {
                crate::room::InternalRoomState::SelectChart => "SelectChart",
                crate::room::InternalRoomState::WaitForReady { .. } => "WaitForReady",
                crate::room::InternalRoomState::Playing { .. } => "Playing",
            };
            let locked = if room.locked.load(std::sync::atomic::Ordering::SeqCst) {
                c::yellow("锁定")
            } else {
                c::dim("未锁定")
            };
            let cycling = if room.cycle.load(std::sync::atomic::Ordering::SeqCst) {
                c::cyan("轮换")
            } else {
                c::dim("不轮换")
            };
            let hidden = if room.is_hidden() { c::magenta("隐藏") } else { c::dim("公开") };

            self.out(format!("  {} {}", c::dim("┏"), c::bold(&room.id.to_string())));
            self.out(format!("  {} 状态: {}  {}  {}  {}  {}", c::dim("┃"), state_str, locked, cycling, hidden,
                if users_in_room.len() + monitors_in_room.len() > 0 {
                    c::cyan(&format!("{} 人在线", users_in_room.len() + monitors_in_room.len()))
                } else {
                    c::dim("空闲")
                }
            ));
            if !users_in_room.is_empty() {
                let mut labels = Vec::new();
                for u in &users_in_room {
                    labels.push(format!("{}({})", c::bold(&room.display_name(u).await), u.id));
                }
                self.out(format!("  {} 玩家: {}", c::dim("┃"), labels.join(", ")));
            }
            if !monitors_in_room.is_empty() {
                let mut labels = Vec::new();
                for u in &monitors_in_room {
                    labels.push(format!("{}({})", c::bold(&room.display_name(u).await), u.id));
                }
                self.out(format!("  {} 旁观: {}", c::dim("┃"), labels.join(", ")));
            }
            self.out(format!("  {}", c::dim("  ─ ─ ─ ─ ─ ─")));
        }

        // 在线用户概要
        let users = self.state.users.read().await;
        if !users.is_empty() {
            self.out(String::new());
            self.out(format!("  {} 在线用户 ({})", c::green("◆"), users.len()));
            for user in users.values() {
                let in_room = if user.room.read().await.is_some() {
                    c::cyan(" 在房间中")
                } else {
                    c::dim(" 大厅")
                };
                self.out(format!("  {} {:<6}  {}{}", c::dim("│"), user.id, c::bold(&user.name), in_room));
            }
        }
    }

    async fn kick_from_room(&self, room_id: &str, target_id: &str) {
        let target: i32 = match target_id.parse() {
            Ok(id) => id,
            Err(_) => {
                self.out(format!("  {} 无效的用户ID: {}", c::red("✗"), target_id));
                return;
            }
        };

        match self.state.room_commands.kick_user(&self.state, room_id, target).await {
            Ok(value) => {
                let name = value.get("user_name").and_then(|v| v.as_str()).unwrap_or("");
                if name.is_empty() {
                    self.out(format!("  {} 用户 {} 已从房间 {} 踢出", c::green("✓"), target, room_id));
                } else {
                    self.out(format!("  {} 用户 {} ({}) 已从房间 {} 踢出", c::green("✓"), name, target, room_id));
                }
            }
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    async fn kick_user(&self, target_id: &str) {
        let target: i32 = match target_id.parse() {
            Ok(id) => id,
            Err(_) => {
                self.out(format!("  {} 无效的用户ID: {}", c::red("✗"), target_id));
                return;
            }
        };

        let user = {
            let users = self.state.users.read().await;
            users.get(&target).map(Arc::clone)
        };

        if let Some(user) = user {
            // 从房间移除（如果在房间中）
            {
                let room_clone = {
                    let room_guard = user.room.read().await;
                    room_guard.as_ref().map(Arc::clone)
                };
                if let Some(room) = room_clone {
                    let room_id = room.id.to_string();
                    if room.on_user_leave(&user).await {
                        self.state.rooms.write().await.remove(&room.id);
                    }
                    self.state.plugin_manager
                        .trigger(&PluginEvent::RoomLeave { user_id: target, room_id }).await;
                }
            }

            // 发送踢出消息
            {
                let sessions = self.state.sessions.read().await;
                for session in sessions.values() {
                    if session.user.id == target {
                        let _ = session.stream.send(
                            phira_mp_common::ServerCommand::Message(
                                phira_mp_common::Message::Chat {
                                    user: 0,
                                    content: "你已被管理员踢出服务器".to_string(),
                                },
                            )
                        ).await;
                        break;
                    }
                }
            }

            // 从用户列表移除
            self.state.users.write().await.remove(&target);
            info!(user = target, "kicked from server by admin");
            self.state.plugin_manager
                .trigger(&PluginEvent::UserDisconnect {
                    user_id: target,
                    user_name: user.name.clone(),
                }).await;

            self.out(format!("  {} 用户 {} ({}) 已从服务器踢出", c::green("✓"), c::bold(&user.name), target));
        } else {
            self.out(format!("  {} 未找到用户 {}", c::red("✗"), target));
        }
    }

    async fn close_room(&self, room_id: &str) {
        match self.state.room_commands.close_room(&self.state, room_id).await {
            Ok(_) => self.out(format!("  {} 房间 {} 已解散", c::green("✓"), c::bold(room_id))),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }


    /// 从字符串查找房间
    async fn find_room(&self, room_id: &str) -> Option<Arc<crate::room::Room>> {
        let rid: phira_mp_common::RoomId = room_id.to_string().try_into().ok()?;
        self.state.rooms.read().await.get(&rid).map(Arc::clone)
    }

    async fn room_info(&self, room_id: &str) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => { self.out(format!("  {} 未找到房间 {}", c::red("✗"), room_id)); return; }
        };
        self.state.refresh_room_display_metadata(&room).await;
        let users = room.users().await;
        let monitors = room.monitors().await;
        let state_str = match &*room.state.read().await {
            crate::room::InternalRoomState::SelectChart => "SelectChart",
            crate::room::InternalRoomState::WaitForReady { .. } => "WaitForReady",
            crate::room::InternalRoomState::Playing { .. } => "Playing",
        };
        let locked = if room.locked.load(std::sync::atomic::Ordering::SeqCst) { c::yellow("锁定") } else { c::dim("未锁定") };
        let cycling = if room.cycle.load(std::sync::atomic::Ordering::SeqCst) { c::cyan("轮换") } else { c::dim("不轮换") };
        let hidden = if room.is_hidden() { c::magenta("隐藏") } else { c::dim("公开") };
        let chart_info = match room.chart.read().await.as_ref() {
            Some(c) => format!("{} (id={})", c.name, c.id),
            None => "未选择".to_string(),
        };
        let endpoint_override = room.phira_api_endpoint_override().await;
        let endpoint_info = endpoint_override
            .clone()
            .unwrap_or_else(|| self.state.config.phira_api_endpoint.clone());
        let endpoint_mode = if endpoint_override.is_some() { "房间覆盖" } else { "全局默认" };
        let host_name = match room.host_id().await {
            Some(hid) => {
                let user = users.iter().chain(monitors.iter()).find(|u| u.id == hid);
                match user {
                    Some(user) => room.display_name(user).await,
                    None => hid.to_string(),
                }
            }
            None if room.is_system_host() => "?（系统房主）".to_string(),
            None => "无（等待首个玩家）".to_string(),
        };

        self.out(format!("  {} 房间: {}", c::green("◆"), c::bold(room_id)));
        let persistent = if room.is_persistent_empty() { c::cyan("无人保留") } else { c::dim("无人清除") };
        self.out(format!("  {} 状态: {} | {} | {} | {} | {}", c::dim("│"), state_str, locked, cycling, hidden, persistent));
        self.out(format!("  {} 房主: {}", c::dim("│"), host_name));
        self.out(format!("  {} 谱面: {}", c::dim("│"), chart_info));
        self.out(format!("  {} Phira API: {} ({})", c::dim("│"), endpoint_info, endpoint_mode));
        let mut user_labels = Vec::new();
        for u in &users {
            user_labels.push(format!("{}({})", room.display_name(u).await, u.id));
        }
        self.out(format!("  {} 玩家: {}", c::dim("│"), user_labels.join(", ")));
        if !monitors.is_empty() {
            let mut monitor_labels = Vec::new();
            for u in &monitors {
                monitor_labels.push(format!("{}({})", room.display_name(u).await, u.id));
            }
            self.out(format!("  {} 旁观: {}", c::dim("│"), monitor_labels.join(", ")));
        }
        // 历史记录统计
        let history = room.play_history.read().await;
        if !history.is_empty() {
            self.out(format!("  {} 历史对局: {} 轮", c::dim("│"), history.len()));
        }
    }

    /// 由管理员发起游戏，等待所有客户端完成谱面加载后再开始。
    async fn room_start(&self, room_id: &str) {
        match self.state.room_commands.start_room(&self.state, room_id).await {
            Ok(_) => self.out(format!(
                "  {} 已发起游戏，正在等待玩家和监控端加载谱面",
                c::green("✓")
            )),
            Err(e) => self.out(format!("  {} 无法开始游戏: {}", c::red("✗"), e)),
        }
    }

    /// 取消准备状态（管理员操作）
    async fn room_cancel(&self, room_id: &str) {
        match self.state.room_commands.cancel_start(&self.state, room_id).await {
            Ok(value) => {
                if value.get("canceled").and_then(|v| v.as_bool()).unwrap_or(false) {
                    self.out(format!("  {} 已取消准备状态", c::green("✓")));
                } else {
                    self.out(format!("  {} 当前状态不需要取消", c::yellow("!")));
                }
            }
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    async fn room_set_host(&self, room_id: &str, target: Option<i32>) {
        match self.state.room_commands.set_host(&self.state, room_id, target).await {
            Ok(value) => {
                if value.get("host_is_system").and_then(|v| v.as_bool()).unwrap_or(false) {
                    self.out(format!("  {} 房主已设为系统 ?", c::green("✓")));
                } else {
                    let host = value.get("host").and_then(|v| v.as_i64()).unwrap_or_default();
                    let name = value.get("host_name").and_then(|v| v.as_str()).unwrap_or("");
                    self.out(format!("  {} 房主已设为用户 {} ({})", c::green("✓"), name, host));
                }
            }
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    async fn room_create_empty(&self, room_id: &str, endpoint: Option<&str>) {
        let endpoint = match endpoint {
            Some(value) => match crate::server::parse_room_endpoint_value(value) {
                Ok(endpoint) => endpoint,
                Err(e) => { self.out(format!("  {} {}", c::red("✗"), e)); return; }
            },
            None => None,
        };
        match self.state.create_empty_room(room_id, endpoint, true).await {
            Ok(res) => {
                let effective = res.get("phira_api_endpoint").and_then(|v| v.as_str()).unwrap_or("");
                self.out(format!("  {} 已创建无人持久房间 {}，Phira API: {}", c::green("✓"), c::bold(room_id), effective));
            }
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    async fn room_force_move(&self, room_id: &str, user_id: i32, monitor: bool) {
        match self.state.force_move_user_to_room(room_id, user_id, monitor).await {
            Ok(_) => self.out(format!(
                "  {} 已强制转移用户 {} 到房间 {}{}",
                c::green("✓"), user_id, c::bold(room_id), if monitor { "（旁观）" } else { "" }
            )),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    async fn room_hide(&self, room_id: &str, hidden: bool) {
        match self.state.set_room_hidden(room_id, hidden).await {
            Ok(_) => self.out(format!(
                "  {} 房间 {} 已{}隐藏",
                c::green("✓"), c::bold(room_id), if hidden { "设为" } else { "取消" }
            )),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    async fn room_set(&self, room_id: &str, field: &str, value: &str) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => { self.out(format!("  {} 未找到房间 {}", c::red("✗"), room_id)); return; }
        };
        match field {
            "lock" => {
                let v = value == "true" || value == "1" || value == "锁定";
                match self.state.room_commands.set_lock(&self.state, room_id, v).await {
                    Ok(_) => self.out(format!("  {} 房间 {} 已{}锁定", c::green("✓"), room_id, if v { "" } else { "解除" })),
                    Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                }
            }
            "cycle" => {
                let v = value == "true" || value == "1" || value == "轮换";
                match self.state.room_commands.set_cycle(&self.state, room_id, v).await {
                    Ok(_) => self.out(format!("  {} 房间 {} 已{}轮换", c::green("✓"), room_id, if v { "开启" } else { "关闭" })),
                    Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                }
            }
            "hidden" => {
                let v = parse_cli_bool(value);
                match self.state.set_room_hidden(room_id, v).await {
                    Ok(_) => self.out(format!("  {} 房间 {} 已{}隐藏", c::green("✓"), room_id, if v { "设为" } else { "取消" })),
                    Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                }
            }
            "persistent" => {
                let v = parse_cli_bool(value);
                match self.state.set_room_persistent_empty(room_id, v).await {
                    Ok(_) => self.out(format!("  {} 房间 {} 已{}无人保留", c::green("✓"), room_id, if v { "开启" } else { "关闭" })),
                    Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                }
            }
            "phira_api_endpoint" => {
                match crate::server::parse_room_endpoint_value(value) {
                    Ok(endpoint) => match self.state.set_room_phira_api_endpoint(room_id, endpoint).await {
                        Ok(res) => {
                            let effective = res.get("phira_api_endpoint").and_then(|v| v.as_str()).unwrap_or("");
                            let using_override = res.get("using_room_override").and_then(|v| v.as_bool()).unwrap_or(false);
                            if using_override {
                                self.out(format!("  {} 房间 {} 的 Phira API 已切换为 {}，立即生效", c::green("✓"), room_id, effective));
                            } else {
                                self.out(format!("  {} 房间 {} 已恢复使用全局 Phira API {}，立即生效", c::green("✓"), room_id, effective));
                            }
                        }
                        Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                    },
                    Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                }
            }
            "host" => {
                let target = match parse_room_host_target(value) {
                    Ok(target) => target,
                    Err(_) => { self.out(format!("  {} 无效的房主目标：请使用用户ID或 ?", c::red("✗"))); return; }
                };
                self.room_set_host(room_id, target).await;
            }
            "chart-id" => {
                if !matches!(
                    *room.state.read().await,
                    crate::room::InternalRoomState::SelectChart
                ) {
                    self.out(format!("  {} 只能在选曲阶段更换谱面", c::yellow("!")));
                    return;
                }
                let cid: i32 = match value.parse() {
                    Ok(id) => id,
                    Err(_) => { self.out(format!("  {} 无效的谱面ID", c::red("✗"))); return; }
                };
                let endpoint = room.effective_phira_api_endpoint(&self.state).await;
                let chart = match reqwest::get(format!(
                    "{}/chart/{cid}",
                    endpoint.trim_end_matches('/')
                ))
                .await
                {
                    Ok(resp) => match resp.error_for_status() {
                        Ok(resp) => resp.json::<crate::server::Chart>().await.ok(),
                        Err(_) => None,
                    },
                    Err(_) => None,
                }.unwrap_or(crate::server::Chart {
                    id: cid,
                    name: format!("chart_{cid}"),
                });
                room.send(phira_mp_common::Message::SelectChart {
                    user: 0,
                    name: chart.name.clone(),
                    id: chart.id,
                }).await;
                room.chart.write().await.replace(chart);
                // The client derives its active chart id from ChangeState, not from the
                // human-readable SelectChart message. Keep both protocol paths in sync.
                room.on_state_change().await;
                room.publish_update(phira_mp_common::PartialRoomData {
                    chart: Some(cid),
                    ..Default::default()
                })
                .await;
                self.state.plugin_manager
                    .trigger(&PluginEvent::RoomModify {
                        user_id: 0,
                        room_id: room_id.to_string(),
                        data: format!(r#"{{"action":"select-chart","chart-id":{cid}}}"#),
                    }).await;
                self.out(format!("  {} 谱面已切换为 ID {}", c::green("✓"), cid));
            }
            _ => {
                self.out(format!("  {} 未知字段: {}", c::red("✗"), field));
                self.out(format!("  {} 支持: lock, cycle, hidden, persistent, host, chart-id, phira_api_endpoint", c::dim("▸")));
            }
        }
    }

    async fn room_history(&self, room_id: &str) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => { self.out(format!("  {} 未找到房间 {}", c::red("✗"), room_id)); return; }
        };
        let history = room.play_history.read().await;
        if history.is_empty() {
            self.out(format!("  {} 该房间暂无游玩记录", c::dim("·")));
            return;
        }
        self.out(format!("  {} 房间 {} 游玩记录 ({} 轮)", c::green("◆"), room_id, history.len()));
        self.out(format!("  {}", c::dim("  ────────────────────────────────────────────")));
        for (i, round) in history.iter().enumerate() {
            let round_num = i + 1;
            self.out(format!("  {} 第{}轮: {} (id={})", c::dim("┏"), round_num, c::bold(&round.chart_name), round.chart_id));
            for r in &round.results {
                let status = if r.aborted { c::yellow(" (放弃)") } else { String::new() };
                self.out(format!("  {} {:<6}  得分:{:<8} 准确率:{:<6.2}%  FC:{}{}",
                    c::dim("┃"), format!("{}({})", r.user_name, r.user_id),
                    r.score, r.accuracy * 100.0,
                    if r.full_combo { c::green("✓") } else { c::dim("✗") },
                    status,
                ));
            }
            self.out(format!("  {}", c::dim("  ─ ─ ─ ─ ─ ─")));
        }
    }


    /// 显示房间 UUID
    async fn room_show_uuid(&self, room_id: &str) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => { self.out(format!("  ✗ 未找到房间 {}", room_id)); return; }
        };
        self.out(format!("  ◆ 房间 {}  UUID: {}", room_id, room.uuid));
    }

    /// 列出房间历史轮次
    async fn room_rounds(&self, room_id: &str) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => { self.out(format!("  ✗ 未找到房间 {}", room_id)); return; }
        };
        let history = room.play_history.read().await;
        if history.is_empty() {
            self.out(format!("  · 该房间暂无轮次记录"));
            return;
        }
        self.out(format!("  ◆ 房间 {} 轮次记录 ({})", room_id, history.len()));
        for (i, r) in history.iter().enumerate() {
            self.out(format!("  │ [{i}] 轮次 {}  谱面 {} (id={})  玩家 {}", r.round_id, r.chart_name, r.chart_id, r.results.len()));
        }
    }

    /// 按轮次 UUID 查询结算详情
    async fn room_round_info(&self, round_uuid: &str) {
        let rooms = self.state.rooms.read().await;
        for room in rooms.values() {
            let history = room.play_history.read().await;
            if let Some(round) = history.iter().find(|r| r.round_id.to_string() == round_uuid) {
                self.out(format!("  ◆ 轮次 {round_uuid}"));
                self.out(format!("  │ 房间: {}  UUID: {}", room.id, room.uuid));
                self.out(format!("  │ 谱面: {} (id={})", round.chart_name, round.chart_id));
                self.out(format!("  │ 玩家: {}", round.results.len()));
                let mut sorted = round.results.clone();
                sorted.sort_by(|a, b| b.score.cmp(&a.score));
                for (i, r) in sorted.iter().enumerate() {
                    let fc = if r.full_combo { " FC" } else { "" };
                    let ab = if r.aborted { " 放弃" } else { "" };
                    self.out(format!("  │ #{} {}  {}分  {:.1}%{}{}", i+1, r.user_name, r.score, r.accuracy*100.0, fc, ab));
                }
                return;
            }
        }
        self.out(format!("  ✗ 未找到轮次 {round_uuid}"));
    }

    /// 广播给所有用户
    async fn broadcast_all(&self, message: &str) {
        let users = {
            let users = self.state.users.read().await;
            users.values().cloned().collect::<Vec<_>>()
        };
        let content = format!("[系统广播] {}", message);
        let msg = phira_mp_common::ServerCommand::Message(
            phira_mp_common::Message::Chat { user: 0, content },
        );
        let mut sent = 0usize;
        for user in &users {
            user.try_send(msg.clone()).await;
            sent += 1;
        }
        info!(sent, message = %message, "broadcast to all");
        self.state.plugin_manager
            .trigger(&PluginEvent::RoomModify {
                user_id: 0,
                room_id: "*broadcast*".to_string(),
                data: serde_json::json!({
                    "action": "broadcast",
                    "scope": "all",
                    "message": message,
                }).to_string(),
            }).await;
        self.out(format!("  {} 已广播给 {} 个用户", c::green("✓"), sent));
    }

    /// 广播给指定房间
    async fn broadcast_room(&self, room_id: &str, message: &str) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => { self.out(format!("  {} 未找到房间 {}", c::red("✗"), room_id)); return; }
        };
        let content = format!("[房间广播] {}", message);
        room.send(phira_mp_common::Message::Chat { user: 0, content }).await;
        let users = room.users().await.len();
        info!(room = room_id, message = %message, "broadcast to room");
        self.state.plugin_manager
            .trigger(&PluginEvent::RoomModify {
                user_id: 0,
                room_id: room_id.to_string(),
                data: serde_json::json!({
                    "action": "broadcast",
                    "scope": "room",
                    "message": message,
                }).to_string(),
            }).await;
        self.out(format!("  {} 已发送房间广播 ({} 人)", c::green("✓"), users));
    }

    /// 发送给指定用户
    async fn broadcast_user(&self, user_id: i32, message: &str) {
        let user = {
            let users = self.state.users.read().await;
            users.get(&user_id).cloned()
        };
        if let Some(user) = user {
            let content = format!("[管理员消息] {}", message);
            user.try_send(phira_mp_common::ServerCommand::Message(
                phira_mp_common::Message::Chat { user: 0, content },
            )).await;
            info!(user = user_id, message = %message, "message to user");
            self.out(format!("  {} 已发送给用户 {}", c::green("✓"), user_id));
        } else {
            self.out(format!("  {} 未找到用户 {}", c::red("✗"), user_id));
        }
    }


    async fn admin_ids(&self, args: &[&str]) {
        let sub = args.first().copied().unwrap_or("list");
        match sub {
            "list" | "" => {
                let mut ids: Vec<i32> = self.state.admin_ids.read().await.iter().copied().collect();
                ids.sort_unstable();
                if ids.is_empty() {
                    self.out(format!("  {} 当前没有配置管理员 Phira ID", c::yellow("!")));
                } else {
                    self.out(format!("  {} 管理员 Phira ID: {}", c::green("◆"), ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")));
                }
            }
            "add" => {
                if args.len() < 2 { self.out(format!("  {} admin-id add <PhiraID>", c::yellow("?"))); return; }
                let Ok(id) = args[1].parse::<i32>() else { self.out(format!("  {} 无效 Phira ID", c::red("✗"))); return; };
                self.state.add_admin_id(id).await;
                self.out(format!("  {} 已添加管理员 {}", c::green("✓"), id));
            }
            "remove" => {
                if args.len() < 2 { self.out(format!("  {} admin-id remove <PhiraID>", c::yellow("?"))); return; }
                let Ok(id) = args[1].parse::<i32>() else { self.out(format!("  {} 无效 Phira ID", c::red("✗"))); return; };
                self.state.remove_admin_id(id).await;
                self.out(format!("  {} 已移除管理员 {}", c::green("✓"), id));
            }
            "set" => {
                let mut ids = Vec::new();
                for arg in &args[1..] {
                    match arg.parse::<i32>() {
                        Ok(id) => ids.push(id),
                        Err(_) => { self.out(format!("  {} 无效 Phira ID: {}", c::red("✗"), arg)); return; }
                    }
                }
                self.state.set_admin_ids(ids.clone()).await;
                self.out(format!("  {} 已设置管理员列表: {}", c::green("✓"), ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")));
            }
            _ => {
                self.out(format!("  {} admin-id list|add|remove|set", c::yellow("?")));
            }
        }
    }

    async fn status(&self) {
        let users = self.state.users.read().await.values().filter(|user| user.id > 0).count();
        let rooms = self.state.rooms.read().await.len();
        let sessions = self.state.sessions.read().await.len();
        let plugins = self.state.plugin_manager.list_plugins().await.len();
        self.out(format!("  {} Phira-mp+ v{}  │ 端口 {}  │ 用户 {} 会话 {} 房间 {} 插件 {}",
            c::bold("◆"), env!("CARGO_PKG_VERSION"),
            self.state.config.port, users, sessions, rooms, plugins));
    }


    async fn ban_user(&self, target: &str, reason: &str) {
        let uid: i32 = match target.parse() {
            Ok(id) => id,
            Err(_) => {
                self.out(format!("  {} 无效的用户ID: {}", c::red("✗"), target));
                return;
            }
        };
        match self.state.ban_manager.ban_user(uid, reason).await {
            Ok(reason) => self.out(format!(
                "  {} 用户 {} 已封禁\n    理由：{}",
                c::green("✓"),
                uid,
                c::yellow(&reason),
            )),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    async fn unban_user(&self, target: &str) {
        let uid: i32 = match target.parse() {
            Ok(id) => id,
            Err(_) => {
                self.out(format!("  {} 无效的用户ID: {}", c::red("✗"), target));
                return;
            }
        };
        match self.state.ban_manager.unban_user(uid).await {
            Ok(_) => self.out(format!("  {} 用户 {} 已解封", c::green("✓"), uid)),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    async fn ban_list(&self) {
        let list = self.state.ban_manager.list_banned().await;
        if list.is_empty() {
            self.out(format!("  {} 黑名单为空", c::dim("·")));
            return;
        }
        self.out(format!("  {} 封禁用户 ({})", c::green("◆"), list.len()));
        self.out(format!("  {}", c::dim("  ────────────────────────────────────────────")));
        for entry in &list {
            self.out(format!("  {} {:<6}  {}", c::dim("│"),
                entry.user_id,
                c::dim(&entry.reason),
            ));
        }
    }

    async fn room_ban_list(&self, room_id: &str) {
        let list = self.state.ban_manager.list_room_bans(room_id).await;
        if list.is_empty() {
            self.out(format!("  {} 房间 {} 的黑名单为空", c::dim("·"), room_id));
            return;
        }
        self.out(format!("  {} 房间 {} 黑名单: {:?}", c::green("◆"), room_id, list));
    }


    async fn list_extensions(&self) {
        let user_fields = self.state.extensions.list_user_fields().await;
        let room_fields = self.state.extensions.list_room_fields().await;

        self.out(format!("  {} 用户扩展字段:", c::cyan("◆")));
        if user_fields.is_empty() {
            self.out(format!("    {} 无", c::dim("-")));
        } else {
            for f in &user_fields {
                self.out(format!("    {} {}", c::dim("·"), f));
            }
        }

        self.out(format!("  {} 房间扩展字段:", c::cyan("◆")));
        if room_fields.is_empty() {
            self.out(format!("    {} 无", c::dim("-")));
        } else {
            for f in &room_fields {
                self.out(format!("    {} {}", c::dim("·"), f));
            }
        }
    }

    async fn get_extension(&self, id: &str, key: &str) {
        if let Ok(uid) = id.parse::<i32>() {
            if let Some(val) = self.state.extensions.get_user_extra(uid, key).await {
                self.out(format!("  {} 用户 {} 的 {} = {}", c::green("◆"), uid, c::cyan(key), val));
                return;
            }
        }
        if let Some(val) = self.state.extensions.get_room_extra(id, key).await {
            self.out(format!("  {} 房间 {} 的 {} = {}", c::green("◆"), id, c::cyan(key), val));
            return;
        }
        self.out(format!("  {} 未找到扩展数据: id={}, key={}", c::yellow("!"), id, key));
    }

    /// 尝试将命令分发给插件注册的 CLI 命令
    async fn try_plugin_command(&self, command: &str, args: &[&str]) -> bool {
        let result = self.state.plugin_manager.execute_cli_command(command, args).await;
        match result {
            Some(output_lines) => {
                for line in output_lines {
                    self.out(format!("  {} {}", c::magenta("◈"), line));
                }
                true
            }
            None => false,
        }
    }
}

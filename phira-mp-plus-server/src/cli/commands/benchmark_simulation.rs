//! Benchmark simulation mode CLI command family.
//!
//! The simulation runtime itself still lives in `crate::simulation`; this module is
//! only the CLI adapter layer, now wired as a sub-mode of `benchmark`.

use super::super::*;
use crate::server::PlusServerState;
use std::sync::Arc;
use tokio::sync::mpsc;

// ═══════════════════════════════════════════════════════════════════════
// CliHandler methods — benchmark simulation <subcommand> dispatch
// ═══════════════════════════════════════════════════════════════════════

impl CliHandler {
    pub(in crate::cli) async fn dispatch_benchmark_simulation_command(&self, args: &[&str]) {
        let sub = args.first().copied().unwrap_or("status");
        match sub {
            "status" | "" => self.print_simulation_status().await,
            "run" => self.simulation_run(&args[1..]).await,
            "stop" => {
                let before = self.state.simulation.status().await;
                let status = self.state.simulation.stop("stopped by admin command").await;
                if let Some(run_id) = before.run_id {
                    self.state
                        .event_bus
                        .publish(crate::event_bus::MpEvent::SimulationStopped {
                            run_id,
                            reason: "stopped by admin command".to_string(),
                        });
                }
                self.broadcast_all("性能测试已结束。Real rooms/users were not modified by the Runtime v2 skeleton.").await;
                self.out(format!("  {} {}", c::green("✓"), status.note));
            }
            "tick" => {
                let count = args
                    .get(1)
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(1);
                match self.state.simulation.advance_ticks_with_events(count).await {
                    Ok((status, events)) => {
                        publish_simulation_tick_event(&self.state, &status);
                        publish_simulation_generated_events(&self.state, &events);
                        self.out(format!(
                            "  {} simulation 已推进 {} tick(s)",
                            c::green("✓"),
                            count.clamp(1, 10_000)
                        ));
                        self.out(format!("  {} ticks={} chats={} ready={} touch_batches={} judge_batches={} round_results={}",
                            c::dim("│"), status.counters.ticks, status.counters.chat_messages,
                            status.counters.ready_events, status.counters.touch_batches,
                            status.counters.judge_batches, status.counters.round_results));
                        self.out(format!("  {} scenario={} generated_events={} kinds=simulation.chat/ready/touch/judge/round", c::dim("│"), status.config.scenario.as_str(), events.len()));
                    }
                    Err(err) => self.out(format!("  {} {}", c::red("✗"), err)),
                }
            }
            "inspect" => {
                let limit = args
                    .get(1)
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(10);
                self.print_simulation_world(limit).await;
            }
            "scenarios" => {
                self.out(format!("  {} Simulation scenarios", c::green("◆")));
                for scenario in crate::simulation::SimulationScenario::all() {
                    self.out(format!(
                        "  {} {:<18} {}",
                        c::dim("│"),
                        scenario.as_str(),
                        scenario.description()
                    ));
                }
                self.out(format!(
                    "  {} 用法：benchmark simulation run baseline scenario=touch_judge_burst",
                    c::dim("▸")
                ));
            }
            "suite" => self.simulation_suite(&args[1..]).await,
            "report" => self.simulation_report(&args[1..]).await,
            "seed" => {
                if args.len() < 2 {
                    self.out(format!(
                        "  {} {} seed <u64>",
                        c::yellow("?"),
                        c::bold("benchmark simulation")
                    ));
                    return;
                }
                match args[1].parse::<u64>() {
                    Ok(seed) => {
                        self.state.simulation.set_seed(seed).await;
                        self.out(format!(
                            "  {} simulation seed 已设置为 {}",
                            c::green("✓"),
                            seed
                        ));
                    }
                    Err(_) => self.out(format!("  {} 无效 seed，必须是 u64", c::red("✗"))),
                }
            }
            "cleanup" => {
                let status = self.state.simulation.cleanup().await;
                self.out(format!("  {} {}", c::green("✓"), status.note));
            }
            "persist" => self.simulation_persist().await,
            "sample" => self.print_simulation_sample().await,
            _ => {
                self.out(format!(
                    "  {} 未知 benchmark simulation 子命令: {}",
                    c::red("✗"),
                    c::yellow(sub)
                ));
                self.out(format!("  {} 可用: benchmark simulation status | run <preset> | suite <name> | report | scenarios | tick [n] | inspect [limit] | persist | stop | seed <u64> | cleanup | sample", c::dim("▸")));
            }
        }
    }

    pub(in crate::cli) async fn simulation_run(&self, args: &[&str]) {
        // Check for "realistic" keyword — uses RealisticSimulationRunner
        if args.first().copied() == Some("realistic") {
            let seed = self.state.simulation.status().await.seed;
            let preset = args
                .get(1)
                .and_then(|value| crate::simulation::SimulationPreset::parse(value))
                .unwrap_or(crate::simulation::SimulationPreset::Baseline);
            let mut config = preset.defaults(seed);
            for token in &args[2..] {
                let Some((key, value)) = token.split_once('=') else {
                    self.out(format!("  {} 无效参数：{token}", c::red("✗")));
                    return;
                };
                if let Err(err) = config.apply_kv(key, value) {
                    self.out(format!("  {} {}", c::red("✗"), err));
                    return;
                }
            }
            match crate::simulation_realistic::RealisticSimulationRunner::start(&self.state, config)
                .await
            {
                Ok(runner) => {
                    let rooms = runner.rooms.len();
                    let users = runner.user_ids.len();
                    self.out(format!(
                        "  {} Realistic simulation 已启动: run_id={}",
                        c::green("✓"),
                        runner.run_id
                    ));
                    self.out(format!(
                        "  {} rooms={} users={} duration_secs={} (创建了真实 Room/User 对象)",
                        c::dim("│"),
                        rooms,
                        users,
                        runner.config.duration_secs
                    ));
                    self.out(format!(
                        "  {} 正在每 {}ms tick 模拟玩家行为...",
                        c::dim("▸"),
                        runner.config.tick_interval_ms
                    ));

                    // Spawn background tick task
                    let state = Arc::clone(&self.state);
                    let out_tx = self.out_tx.clone();
                    let mut runner = runner; // take ownership
                    tokio::spawn(async move {
                        let tick_interval = std::time::Duration::from_millis(
                            runner.config.tick_interval_ms.max(100),
                        );
                        let duration = std::time::Duration::from_secs(runner.config.duration_secs);
                        let start = tokio::time::Instant::now();
                        let mut ticks: u64 = 0;
                        loop {
                            tokio::time::sleep(tick_interval).await;
                            if start.elapsed() >= duration {
                                break;
                            }
                            let counters = runner
                                .tick(&state, runner.config.seed.wrapping_add(ticks))
                                .await;
                            ticks += 1;
                            let _ = out_tx.try_send(format!("  simulation realistic tick={ticks} chat={} ready={} touch={} judge={} round={}",
                                counters.chat_messages, counters.ready_events,
                                counters.touch_batches, counters.judge_batches, counters.round_results));
                        }
                        runner.cleanup(&state).await;
                        let _ = out_tx.try_send(format!(
                            "  {} Realistic simulation 已完成 (ticks={ticks})",
                            c::green("✓")
                        ));
                    });
                }
                Err(err) => self.out(format!("  {} {}", c::red("✗"), err)),
            }
            return;
        }

        let seed = self.state.simulation.status().await.seed;
        let preset = args
            .first()
            .and_then(|value| crate::simulation::SimulationPreset::parse(value))
            .unwrap_or(crate::simulation::SimulationPreset::Baseline);
        let mut config = preset.defaults(seed);
        let option_start = if args
            .first()
            .and_then(|value| crate::simulation::SimulationPreset::parse(value))
            .is_some()
        {
            1
        } else {
            0
        };
        for token in &args[option_start..] {
            let Some((key, value)) = token.split_once('=') else {
                self.out(format!("  {} 无效参数：{}；请使用 users=500 rooms=50 duration=300 scenario=touch_judge_burst tick_ms=1000 auto=true persist_every=0", c::red("✗"), token));
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
                    self.state
                        .event_bus
                        .publish(crate::event_bus::MpEvent::SimulationStarted { run_id });
                }
                self.broadcast_all("服务器正在进行性能测试，期间可能出现短暂卡顿。Runtime v2 当前为安全骨架模式，不会创建真实房间。").await;
                self.out(format!(
                    "  {} simulation 已启动: {:?}",
                    c::green("✓"),
                    status.run_id
                ));
                self.out(format!("  {} preset={:?} scenario={} users={} rooms={} duration={}s touch={} judge={} chat={} ready={} rounds={}",
                    c::dim("│"), status.config.preset, status.config.scenario.as_str(), status.config.users, status.config.rooms,
                    status.config.duration_secs, status.config.touch, status.config.judge,
                    status.config.chat, status.config.ready, status.config.rounds));
                self.out(format!(
                    "  {} runner: auto={} tick_ms={} persist_every={}",
                    c::dim("│"),
                    status.config.auto_tick,
                    status.config.tick_interval_ms,
                    status.config.persist_every_ticks
                ));
                self.out(format!(
                    "  {} shadow world: {} users / {} rooms / {} rounds materialized",
                    c::dim("│"),
                    status.materialized_users,
                    status.materialized_rooms,
                    status.materialized_rounds
                ));
                if status.config.auto_tick {
                    if let Some(run_id) = status.run_id {
                        spawn_simulation_runner(
                            std::sync::Arc::clone(&self.state),
                            self.out_tx.clone(),
                            run_id,
                            status.config.clone(),
                        );
                        self.out(format!(
                            "  {} 自动 runner 已启动；到达 duration 后会自动 stop",
                            c::dim("▸")
                        ));
                    } else {
                        self.out(format!(
                            "  {} simulation 已启动但缺少 run_id，自动 runner 未启动",
                            c::yellow("!")
                        ));
                    }
                } else {
                    self.out(format!(
                        "  {} auto=false，需手动执行 benchmark simulation tick [n] 推进",
                        c::dim("▸")
                    ));
                }
                self.out(format!(
                    "  {} Step 8 已启用聚合仿真事件；仍不写入真实 rooms/users 表",
                    c::dim("▸")
                ));
            }
            Err(err) => self.out(format!("  {} {}", c::red("✗"), err)),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// CliHandler methods — simulation suite/report CLI output
// ═══════════════════════════════════════════════════════════════════════

impl CliHandler {
    pub(in crate::cli) async fn simulation_report(&self, args: &[&str]) {
        let sub = args.first().copied().unwrap_or("latest");
        match sub {
            "latest" | "" => match self.state.simulation.latest_suite_report().await {
                Some(report) => self.print_suite_report(&report),
                None => self.out(format!(
                    "  {} 暂无 simulation suite report；先执行 benchmark simulation suite smoke",
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
                    "  {} 可用: benchmark simulation report | benchmark simulation report list [limit]",
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
        spawn_simulation_suite_runner(
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
            "  {} 用法：benchmark simulation suite smoke | benchmark simulation suite mixed duration=15 tick_ms=500",
            c::dim("▸")
        ));
    }
}

// ═══════════════════════════════════════════════════════════════════════
// CliHandler methods — status, world inspection, sample data, persist
// ═══════════════════════════════════════════════════════════════════════

impl CliHandler {
    pub(in crate::cli) async fn simulation_persist(&self) {
        let status = self.state.simulation.status().await;
        let Some(run_id) = status.run_id else {
            self.out(format!(
                "  {} simulation 未运行，无法生成 snapshot",
                c::red("✗")
            ));
            return;
        };
        publish_simulation_snapshot(&self.state, run_id, &status, "cli.simulation.persist")
            .await;
        self.out(format!(
            "  {} simulation snapshot 已发送到 EventBus / PersistenceWorker",
            c::green("✓")
        ));
        self.out(format!("  {} run_id={}", c::dim("│"), run_id));
        self.out(format!(
            "  {} 这是 simulation 专用诊断数据，不会写入真实 mp_* 玩家/房间表",
            c::dim("▸")
        ));
    }

    pub(in crate::cli) async fn print_simulation_status(&self) {
        let status = self.state.simulation.status().await;
        self.out(format!("  {} Runtime v2 Simulation (Benchmark mode)", c::green("◆")));
        self.out(format!(
            "  {} running:        {}",
            c::dim("│"),
            status.running
        ));
        self.out(format!(
            "  {} run_id:         {}",
            c::dim("│"),
            status
                .run_id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string())
        ));
        self.out(format!("  {} seed:           {}", c::dim("│"), status.seed));
        self.out(format!(
            "  {} preset:         {:?}",
            c::dim("│"),
            status.config.preset
        ));
        self.out(format!(
            "  {} scenario:       {} - {}",
            c::dim("│"),
            status.config.scenario.as_str(),
            status.config.scenario.description()
        ));
        self.out(format!(
            "  {} target:         {} users / {} rooms / {}s",
            c::dim("│"),
            status.config.users,
            status.config.rooms,
            status.config.duration_secs
        ));
        self.out(format!(
            "  {} elapsed/remain: {}/{}s",
            c::dim("│"),
            status.elapsed_secs,
            status
                .remaining_secs
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".to_string())
        ));
        self.out(format!(
            "  {} runner:         auto={} tick_ms={} persist_every={}",
            c::dim("│"),
            status.runner_enabled,
            status.config.tick_interval_ms,
            status.config.persist_every_ticks
        ));
        self.out(format!(
            "  {} touch/judge:    {} / {}",
            c::dim("│"),
            status.config.touch,
            status.config.judge
        ));
        self.out(format!(
            "  {} virtual state:  {} users / {} rooms",
            c::dim("│"),
            status.virtual_users,
            status.virtual_rooms
        ));
        self.out(format!(
            "  {} materialized:   {} users / {} rooms / {} rounds",
            c::dim("│"),
            status.materialized_users,
            status.materialized_rooms,
            status.materialized_rounds
        ));
        self.out(format!(
            "  {} counters:       ticks={} chats={} ready={} touch={} judge={} result={}",
            c::dim("│"),
            status.counters.ticks,
            status.counters.chat_messages,
            status.counters.ready_events,
            status.counters.touch_batches,
            status.counters.judge_batches,
            status.counters.round_results
        ));
        self.out(format!("  {} note:           {}", c::dim("│"), status.note));
    }

    pub(in crate::cli) async fn print_simulation_world(&self, limit: usize) {
        let Some(world) = self.state.simulation.world_snapshot(limit).await else {
            self.out(format!(
                "  {} simulation shadow world 不存在；请先执行 benchmark simulation run baseline",
                c::yellow("!")
            ));
            return;
        };
        self.out(format!("  {} Simulation Shadow World", c::green("◆")));
        self.out(format!(
            "  {} run_id:       {}",
            c::dim("│"),
            world
                .run_id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string())
        ));
        self.out(format!(
            "  {} totals:       {} users / {} rooms",
            c::dim("│"),
            world.users_total,
            world.rooms_total
        ));
        self.out(format!(
            "  {} materialized: {} users / {} rooms / {} rounds",
            c::dim("│"),
            world.users_materialized,
            world.rooms_materialized,
            world.rounds_materialized
        ));
        self.out(format!("  {} {}", c::dim("▸"), world.materialization_note));
        self.out(format!("  {} sample rooms", c::cyan("▸")));
        for room in world.sample_rooms.iter().take(8) {
            self.out(format!(
                "    {} chart={} members={} ready={} playing={} round={}",
                c::bold(&room.id),
                room.chart_id,
                room.member_ids.len(),
                room.ready_count,
                room.playing,
                room.round_id.as_deref().unwrap_or("-")
            ));
        }
        self.out(format!("  {} sample users", c::cyan("▸")));
        for user in world.sample_users.iter().take(8) {
            self.out(format!(
                "    {} id={} room={} ready={} playing={}",
                c::bold(&user.name),
                user.id,
                user.room_id.as_deref().unwrap_or("-"),
                user.ready,
                user.playing
            ));
        }
        if !world.sample_rounds.is_empty() {
            self.out(format!("  {} sample rounds", c::cyan("▸")));
            for round in world.sample_rounds.iter().take(6) {
                self.out(format!(
                    "    {} room={} chart={} players={} score={} touch={} judge={}",
                    c::bold(&round.round_id),
                    round.room_id,
                    round.chart_id,
                    round.players,
                    round.sample_score,
                    round.sample_touches,
                    round.sample_judges
                ));
            }
        }
        if !world.recent_events.is_empty() {
            self.out(format!("  {} recent events", c::cyan("▸")));
            for event in world.recent_events.iter().take(8) {
                self.out(format!(
                    "    #{:<4} {:<10} {}",
                    event.seq, event.kind, event.message
                ));
            }
        }
    }

    pub(in crate::cli) async fn print_simulation_sample(&self) {
        let status = self.state.simulation.status().await;
        let touches = crate::simulation::SimulationManager::sample_touches(status.seed);
        let judges = crate::simulation::SimulationManager::sample_judges(status.seed);
        self.out(format!(
            "  {} sample touches: {} 条；sample judges: {} 条；seed={}",
            c::green("◆"),
            touches.len(),
            judges.len(),
            status.seed
        ));
        if let Some(first) = touches.first() {
            self.out(format!(
                "  {} first touch: t={}ms lane={} pressed={}",
                c::dim("│"),
                first.time_ms,
                first.lane,
                first.pressed
            ));
        }
        if let Some(first) = judges.first() {
            self.out(format!(
                "  {} first judge: t={}ms {} +{}",
                c::dim("│"),
                first.time_ms,
                first.judge,
                first.score_delta
            ));
        }
        self.out(format!(
            "  {} Step 3 示例数据由 shadow world 复用，仍不写入真实数据库/房间",
            c::dim("▸")
        ));
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Free functions — background runners and EventBus bridge helpers
// ═══════════════════════════════════════════════════════════════════════

fn publish_simulation_tick_event(
    state: &PlusServerState,
    status: &crate::simulation::SimulationStatus,
) {
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
    out_tx: mpsc::Sender<String>,
    run_id: uuid::Uuid,
    config: crate::simulation::SimulationConfig,
) {
    tokio::spawn(async move {
        let interval = std::time::Duration::from_millis(config.tick_interval_ms);
        let _ = out_tx.try_send(format!(
            "  ◆ simulation runner started: run_id={} tick_ms={} duration={}s persist_every={}",
            run_id, config.tick_interval_ms, config.duration_secs, config.persist_every_ticks
        ));

        loop {
            tokio::time::sleep(interval).await;
            let (status, events) = match state
                .simulation
                .advance_ticks_for_run_with_events(run_id, 1)
                .await
            {
                Ok(result) => result,
                Err(_) => break,
            };
            publish_simulation_tick_event(&state, &status);
            publish_simulation_generated_events(&state, &events);

            if config.persist_every_ticks > 0
                && status.counters.ticks > 0
                && status.counters.ticks % config.persist_every_ticks == 0
            {
                publish_simulation_snapshot(&state, run_id, &status, "simulation.runner.periodic")
                    .await;
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
                    let _ = out_tx.try_send(format!(
                        "  ✓ simulation runner stopped: run_id={} ticks={} elapsed={}s reason={}",
                        run_id, stopped.counters.ticks, stopped.elapsed_secs, reason
                    ));
                    if config.persist_every_ticks > 0 {
                        publish_simulation_snapshot(
                            &state,
                            run_id,
                            &stopped,
                            "simulation.runner.final",
                        )
                        .await;
                    }
                }
                break;
            }
        }
    });
}

fn spawn_simulation_suite_runner(
    state: Arc<PlusServerState>,
    out_tx: mpsc::Sender<String>,
    suite: crate::simulation::SimulationSuite,
    steps: Vec<crate::simulation::SimulationSuiteStep>,
) {
    tokio::spawn(async move {
        let suite_run_id = uuid::Uuid::new_v4();
        let suite_started_at_ms = now_ms();
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
        let _ = out_tx.try_send(format!(
            "  ◆ simulation suite started: suite={} suite_run_id={} steps={}",
            suite.as_str(),
            suite_run_id,
            total_steps
        ));
        let _ = state
            .broadcast_system_message(
                "服务器正在进行 Runtime v2 Simulation suite；期间可能出现短暂卡顿。",
            )
            .await;

        let mut completed_steps = 0usize;
        let mut aborted = false;
        let mut abort_reason = "completed".to_string();
        let mut step_reports: Vec<crate::simulation::SimulationRunReport> = Vec::new();
        for (idx, step) in steps.into_iter().enumerate() {
            let step_index = idx + 1;
            if state.simulation.status().await.running {
                abort_reason = format!("another simulation was running before step {step_index}");
                let _ = out_tx.try_send(format!(
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
                    let _ = out_tx.try_send(format!(
                        "  ✗ simulation suite step {} failed to start: {}",
                        step_index, err
                    ));
                    aborted = true;
                    break;
                }
            };
            let Some(run_id) = status.run_id else {
                abort_reason = format!("step {} started without run_id", step.name);
                let _ = out_tx.try_send(format!(
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
            let _ = out_tx.try_send(format!(
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
                        let _ = out_tx.try_send(format!(
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
                    publish_simulation_snapshot(
                        &state,
                        run_id,
                        &status,
                        "simulation.suite.periodic",
                    )
                    .await;
                }

                if status.elapsed_secs >= step.config.duration_secs {
                    let reason = format!(
                        "suite {} step {} duration {}s reached",
                        suite.as_str(),
                        step.name,
                        step.config.duration_secs
                    );
                    if let Some(stopped) =
                        state.simulation.stop_if_run(run_id, reason.clone()).await
                    {
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
                            publish_simulation_snapshot(
                                &state,
                                run_id,
                                &stopped,
                                "simulation.suite.final",
                            )
                            .await;
                        }
                        step_reports.push(crate::simulation::SimulationRunReport::from_status(
                            Some(suite_run_id),
                            Some(suite),
                            step.name.clone(),
                            &stopped,
                            false,
                            reason.clone(),
                        ));
                        let _ = out_tx.try_send(format!(
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
            now_ms(),
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
        let _ = out_tx.try_send(format!(
            "  {} simulation suite finished: suite={} completed={}/{} aborted={} workload_events={} eps={:.2}",
            if aborted { "!" } else { "✓" },
            suite.as_str(),
            completed_steps,
            total_steps,
            aborted,
            report.workload_events,
            report.workload_events_per_sec
        ));
        let benchmark_report =
            crate::benchmark_report::BenchmarkReport::from_simulation_suite(&report);
        state.publish_benchmark_completed(&benchmark_report);
        for line in benchmark_report.render_text().lines() {
            let _ = out_tx.try_send(line.to_string());
        }
        let _ = out_tx.try_send("  ▸ 查看完整 suite 明细：benchmark simulation report".to_string());
    });
}

fn now_ms() -> i64 {
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

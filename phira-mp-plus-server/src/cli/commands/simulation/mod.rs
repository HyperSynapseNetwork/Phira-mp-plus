//! Runtime v2 simulation CLI command family.
//!
//! Keep simulation command routing and presentation out of `cli.rs`. The
//! simulation runtime itself still lives in `crate::simulation`; this module is
//! only the CLI adapter layer.

use super::super::*;

mod reports;
mod runner;
mod world;

impl CliHandler {
    pub(in crate::cli) async fn dispatch_simulation_command(&self, args: &[&str]) {
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
                        runner::publish_simulation_tick_event(&self.state, &status);
                        runner::publish_simulation_generated_events(&self.state, &events);
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
                    "  {} 用法：simulation run baseline scenario=chat_storm",
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
                        c::bold("simulation")
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
                    "  {} 未知 simulation 子命令: {}",
                    c::red("✗"),
                    c::yellow(sub)
                ));
                self.out(format!("  {} 可用: simulation status | run <preset> | suite <name> | report | scenarios | tick [n] | inspect [limit] | persist | stop | seed <u64> | cleanup | sample", c::dim("▸")));
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
                        runner::spawn_simulation_runner(
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
                        "  {} auto=false，需手动执行 simulation tick [n] 推进",
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

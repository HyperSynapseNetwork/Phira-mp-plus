use super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn dispatch_simulation_command(&self, args: &[&str]) {
        self.simulation_command(args).await;
    }

    async fn simulation_command(&self, args: &[&str]) {
        let sub = args.first().copied().unwrap_or("status");
        match sub {
            "status" | "" => self.print_simulation_status().await,
            "run" => self.simulation_run(&args[1..]).await,
            "stop" => {
                let before = self.state.simulation.status().await;
                let status = self.state.simulation.stop("stopped by admin command").await;
                if let Some(run_id) = before.run_id {
                    self.state.event_bus.publish(crate::event_bus::MpEvent::SimulationStopped {
                        run_id,
                        reason: "stopped by admin command".to_string(),
                    });
                }
                self.broadcast_all("性能测试已结束。Real rooms/users were not modified by the Runtime v2 skeleton.").await;
                self.out(format!("  {} {}", c::green("✓"), status.note));
            }
            "tick" => {
                let count = args.get(1).and_then(|value| value.parse::<u64>().ok()).unwrap_or(1);
                match self.state.simulation.advance_ticks_with_events(count).await {
                    Ok((status, events)) => {
                        publish_simulation_tick_event(&self.state, &status);
                        publish_simulation_generated_events(&self.state, &events);
                        self.out(format!("  {} simulation 已推进 {} tick(s)", c::green("✓"), count.clamp(1, 10_000)));
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
                let limit = args.get(1).and_then(|value| value.parse::<usize>().ok()).unwrap_or(10);
                self.print_simulation_world(limit).await;
            }
            "scenarios" => {
                self.out(format!("  {} Simulation scenarios", c::green("◆")));
                for scenario in crate::simulation::SimulationScenario::all() {
                    self.out(format!("  {} {:<18} {}", c::dim("│"), scenario.as_str(), scenario.description()));
                }
                self.out(format!("  {} 用法：simulation run baseline scenario=chat_storm", c::dim("▸")));
            }
            "suite" => self.simulation_suite(&args[1..]).await,
            "report" => self.simulation_report(&args[1..]).await,
            "seed" => {
                if args.len() < 2 {
                    self.out(format!("  {} {} seed <u64>", c::yellow("?"), c::bold("simulation")));
                    return;
                }
                match args[1].parse::<u64>() {
                    Ok(seed) => {
                        self.state.simulation.set_seed(seed).await;
                        self.out(format!("  {} simulation seed 已设置为 {}", c::green("✓"), seed));
                    }
                    Err(_) => self.out(format!("  {} 无效 seed，必须是 u64", c::red("✗"))),
                }
            }
            "cleanup" => {
                let status = self.state.simulation.cleanup().await;
                self.out(format!("  {} {}", c::green("✓"), status.note));
            }
            "persist" => self.simulation_persist().await,
            "sample" => {
                let status = self.state.simulation.status().await;
                let touches = crate::simulation::SimulationManager::sample_touches(status.seed);
                let judges = crate::simulation::SimulationManager::sample_judges(status.seed);
                self.out(format!("  {} sample touches: {} 条；sample judges: {} 条；seed={}", c::green("◆"), touches.len(), judges.len(), status.seed));
                if let Some(first) = touches.first() {
                    self.out(format!("  {} first touch: t={}ms lane={} pressed={}", c::dim("│"), first.time_ms, first.lane, first.pressed));
                }
                if let Some(first) = judges.first() {
                    self.out(format!("  {} first judge: t={}ms {} +{}", c::dim("│"), first.time_ms, first.judge, first.score_delta));
                }
                self.out(format!("  {} Step 3 示例数据由 shadow world 复用，仍不写入真实数据库/房间", c::dim("▸")));
            }
            _ => {
                self.out(format!("  {} 未知 simulation 子命令: {}", c::red("✗"), c::yellow(sub)));
                self.out(format!("  {} 可用: simulation status | run <preset> | suite <name> | report | scenarios | tick [n] | inspect [limit] | persist | stop | seed <u64> | cleanup | sample", c::dim("▸")));
            }
        }
    }

}

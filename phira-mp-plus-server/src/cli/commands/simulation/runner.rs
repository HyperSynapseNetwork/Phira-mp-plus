//! Background runners and EventBus bridge helpers for simulation CLI commands.

use crate::server::PlusServerState;
use std::sync::Arc;
use tokio::sync::mpsc;

pub(super) fn publish_simulation_tick_event(
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

pub(super) fn publish_simulation_generated_events(
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

pub(super) async fn publish_simulation_snapshot(
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

pub(super) fn spawn_simulation_runner(
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

pub(super) fn spawn_simulation_suite_runner(
    state: Arc<PlusServerState>,
    out_tx: mpsc::UnboundedSender<String>,
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
        let benchmark_report = crate::benchmark_report::BenchmarkReport::from_simulation_suite(&report);
        for line in benchmark_report.render_text().lines() {
            let _ = out_tx.send(line.to_string());
        }
        let _ = out_tx.send("  ▸ 查看完整 suite 明细：simulation report".to_string());
    });
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

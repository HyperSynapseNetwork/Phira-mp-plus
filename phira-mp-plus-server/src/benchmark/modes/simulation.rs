//! Simulation mode benchmark runner
//!
//! 仿真模式运行器。委托到现有的 `crate::simulation` 基础设施（SimulationManager），
//! 根据 `BenchmarkConfig.scenario` 派发到对应的场景实现，并在其上附加
//! 基准测试指标采集和报告生成。
//!
//! 与旧版的区别：
//! - 使用基准测试框架的 `BenchmarkConfig` 替代 `SimulationConfig`
//! - 支持 11 种场景（而非旧版的 6 种）
//! - 提供统一的指标采集和报告输出

use crate::benchmark::command::BenchmarkScenario;
use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::environment::EnvironmentSnapshot;
use crate::benchmark::report::BenchmarkReport;
use std::time::SystemTime;

/// 仿真模式运行结果
pub struct SimulationRunResult {
    /// 基准测试报告
    pub report: BenchmarkReport,
    /// 影子世界最终状态摘要
    pub shadow_summary: String,
}

/// 运行仿真模式基准测试
///
/// 根据 `BenchmarkConfig.scenario` 派发到对应场景的 `run_*` 函数，
/// 收集 `BenchmarkMetrics` 并组装为 `BenchmarkReport`。
///
/// # 场景映射
///
/// | BenchmarkScenario | SimulationScenario | 重点事件 |
/// |---|---|---|
/// | RoomLifecycle | RoundStorm | ready, rounds |
/// | Gameplay | TouchJudgeBurst | touch, judge |
/// | Connection | Balanced | chat, ready |
/// | SteadyState | Idle | chat (ping) |
/// | HotRoom | ChatStorm | chat (broadcast) |
/// | SlowConsumer | Balanced | all (backpressure pressure) |
/// | Reconnect | Balanced | chat, ready (fast tick) |
/// | PluginLoad | Balanced | all (needs plugin-system) |
/// | DatabaseWrite | Balanced | all + persist (needs postgres) |
/// | Mixed | Balanced | all |
/// | LongRun | Balanced | all (slow tick) |
pub async fn run_simulation(config: BenchmarkConfig) -> Result<SimulationRunResult, String> {
    let started_at_ms = now_ms();

    // Dispatch to the correct scenario implementation
    let metrics = match config.scenario {
        BenchmarkScenario::RoomLifecycle => {
            let params =
                crate::benchmark::scenarios::room_lifecycle::RoomLifecycleParams::default();
            crate::benchmark::scenarios::room_lifecycle::run_room_lifecycle(&config, params).await?
        }
        BenchmarkScenario::Gameplay => {
            let params = crate::benchmark::scenarios::gameplay::GameplayParams::default();
            crate::benchmark::scenarios::gameplay::run_gameplay(&config, params).await?
        }
        BenchmarkScenario::Connection => {
            let params = crate::benchmark::scenarios::connection::ConnectionParams::default();
            crate::benchmark::scenarios::connection::run_connection(&config, params).await?
        }
        BenchmarkScenario::SteadyState => {
            let params = crate::benchmark::scenarios::steady_state::SteadyStateParams::default();
            crate::benchmark::scenarios::steady_state::run_steady_state(&config, params).await?
        }
        BenchmarkScenario::HotRoom => {
            let params = crate::benchmark::scenarios::hot_room::HotRoomParams::default();
            crate::benchmark::scenarios::hot_room::run_hot_room(&config, params).await?
        }
        BenchmarkScenario::SlowConsumer => {
            let params = crate::benchmark::scenarios::slow_consumer::SlowConsumerParams::default();
            crate::benchmark::scenarios::slow_consumer::run_slow_consumer(&config, params).await?
        }
        BenchmarkScenario::Reconnect => {
            let params = crate::benchmark::scenarios::reconnect::ReconnectParams::default();
            crate::benchmark::scenarios::reconnect::run_reconnect(&config, params).await?
        }
        BenchmarkScenario::PluginLoad => {
            let params = crate::benchmark::scenarios::plugin_load::PluginLoadParams::default();
            crate::benchmark::scenarios::plugin_load::run_plugin_load(&config, params).await?
        }
        BenchmarkScenario::DatabaseWrite => {
            let params = crate::benchmark::scenarios::database_write::DatabaseWriteParams::default();
            crate::benchmark::scenarios::database_write::run_database_write(&config, params)
                .await?
        }
        BenchmarkScenario::Mixed => {
            let params = crate::benchmark::scenarios::mixed::MixedParams::default();
            crate::benchmark::scenarios::mixed::run_mixed(&config, params).await?
        }
        BenchmarkScenario::LongRun => {
            let params = crate::benchmark::scenarios::long_run::LongRunParams::default();
            crate::benchmark::scenarios::long_run::run_long_run(&config, params).await?
        }
    };

    let finished_at_ms = now_ms();

    // Build report from metrics
    let environment = EnvironmentSnapshot::capture().await;
    let mut report = BenchmarkReport::new(
        format!("Simulation: {}", config.scenario.as_str()),
        environment,
        config.clone(),
    );
    report.finished_at_ms = finished_at_ms;
    report.summary.duration_secs = metrics.elapsed_secs;
    report.summary.total_commands =
        (metrics.commands_per_sec * metrics.elapsed_secs as f64) as u64;
    report.summary.total_messages =
        (metrics.messages_per_sec * metrics.elapsed_secs as f64) as u64;
    report.summary.avg_commands_per_sec = metrics.commands_per_sec;
    report.summary.avg_messages_per_sec = metrics.messages_per_sec;
    report.command_latency = metrics.latency;
    report.touch_judge = metrics.touch_judge;
    report.errors_total = metrics.errors_total;
    report.invariant_violations = metrics.invariant_violations;
    report.notes.push(format!(
        "simulation completed: {:.0} events/s over {}s with {} touch and {} judge batches",
        metrics.commands_per_sec,
        metrics.elapsed_secs,
        metrics.touch_judge.touch_committed,
        metrics.touch_judge.judge_committed,
    ));

    let shadow_summary = format!(
        "scenario={} preset={} clients={} rooms={} duration={}s events/s={:.0}",
        config.scenario.as_str(),
        config.preset.as_str(),
        config.clients,
        config.rooms,
        metrics.elapsed_secs,
        metrics.commands_per_sec,
    );

    Ok(SimulationRunResult {
        report,
        shadow_summary,
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

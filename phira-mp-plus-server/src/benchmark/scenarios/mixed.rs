//! Mixed scenario
//!
//! 多种并发负载类型场景。同时运行多个不同类型的基准测试场景，
//! 模拟真实世界中多种操作类型同时发生的混合负载。
//! 不同于按顺序执行场景，此场景是真并发混合运行。
//!
//! Implementation: uses `SimulationManager` with the `Balanced` workload
//! (which is itself a mix of chat, ready, round, touch, and judge events).
//! All event types are enabled to produce a combined load profile.

use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::metrics::BenchmarkMetrics;
use crate::simulation::SimulationScenario;

/// 混合负载中的子场景权重
#[derive(Debug, Clone)]
pub struct MixedScenarioWeight {
    /// 场景名称
    pub scenario_name: &'static str,
    /// 该场景的并发权重（占总负载的比例）
    pub weight: f64,
}

/// 混合场景参数
#[derive(Debug, Clone)]
pub struct MixedParams {
    /// 各子场景的权重分配
    pub weights: Vec<MixedScenarioWeight>,
    /// 总并发客户端数
    pub total_clients: u32,
    /// 总房间数
    pub total_rooms: u32,
}

impl Default for MixedParams {
    fn default() -> Self {
        Self {
            weights: vec![
                MixedScenarioWeight { scenario_name: "room_lifecycle", weight: 0.15 },
                MixedScenarioWeight { scenario_name: "gameplay", weight: 0.25 },
                MixedScenarioWeight { scenario_name: "connection", weight: 0.10 },
                MixedScenarioWeight { scenario_name: "steady_state", weight: 0.20 },
                MixedScenarioWeight { scenario_name: "hot_room", weight: 0.10 },
                MixedScenarioWeight { scenario_name: "chat", weight: 0.10 },
                MixedScenarioWeight { scenario_name: "ready", weight: 0.10 },
            ],
            total_clients: 500,
            total_rooms: 50,
        }
    }
}

/// 执行混合场景
///
/// Uses `SimulationManager` with the `Balanced` workload profile and all
/// event types enabled.  The `Balanced` scenario in the legacy simulation
/// system inherently produces a mix of chat, ready, round, touch, and
/// judge events at rates proportional to user and room counts, which
/// approximates the concurrent mixed load described by `MixedParams`.
pub async fn run_mixed(
    config: &BenchmarkConfig,
    _params: MixedParams,
) -> Result<BenchmarkMetrics, String> {
    super::common::run_simulation(config, |sc| {
        sc.chat = true;
        sc.ready = true;
        sc.rounds = true;
        sc.touch = true;
        sc.judge = true;
        sc.scenario = SimulationScenario::Balanced;
    })
    .await
}

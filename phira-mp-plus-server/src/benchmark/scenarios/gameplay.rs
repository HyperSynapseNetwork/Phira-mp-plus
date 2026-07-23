//! Gameplay scenario
//!
//! 实时 Touch/Judge 游戏操作场景。模拟客户端在游戏过程中的
//! Touch（触摸）和 Judge（判定）事件，以真实速率发送。
//! 测试服务端在高频 Touch/Judge 下的吞吐和延迟。
//!
//! Implementation: uses `SimulationManager` with `TouchJudgeBurst` workload,
//! enabling only touch and judge event generation (chat, ready, rounds
//! disabled) to focus purely on gameplay event throughput.

use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::metrics::BenchmarkMetrics;
use crate::simulation::SimulationScenario;

/// Touch 事件参数
#[derive(Debug, Clone)]
pub struct TouchParams {
    /// 每次批量发送的 Touch 数
    pub batch_size: u32,
    /// 发送间隔（毫秒）
    pub interval_ms: u64,
    /// Touch 数据随机范围
    pub lane_range: std::ops::Range<u8>,
}

impl Default for TouchParams {
    fn default() -> Self {
        Self {
            batch_size: 16,
            interval_ms: 125,
            lane_range: 0..4,
        }
    }
}

/// Judge 事件参数
#[derive(Debug, Clone)]
pub struct JudgeParams {
    /// 每次批量发送的 Judge 数
    pub batch_size: u32,
    /// 发送间隔（毫秒）
    pub interval_ms: u64,
}

impl Default for JudgeParams {
    fn default() -> Self {
        Self {
            batch_size: 8,
            interval_ms: 250,
        }
    }
}

/// 游戏场景参数
#[derive(Debug, Clone)]
pub struct GameplayParams {
    pub touch: TouchParams,
    pub judge: JudgeParams,
    /// 同时处于游戏中的房间数
    pub concurrent_games: u32,
}

impl Default for GameplayParams {
    fn default() -> Self {
        Self {
            touch: TouchParams::default(),
            judge: JudgeParams::default(),
            concurrent_games: 5,
        }
    }
}

/// 执行游戏操作场景
///
/// Uses `SimulationManager` with `TouchJudgeBurst` workload.
/// Touch and judge batches are generated at the maximum rate the
/// shadow-world tick system supports; chat, ready, and round lifecycle
/// events are disabled.
pub async fn run_gameplay(
    config: &BenchmarkConfig,
    _params: GameplayParams,
) -> Result<BenchmarkMetrics, String> {
    super::common::run_simulation(config, |sc| {
        sc.chat = false;
        sc.ready = false;
        sc.rounds = false;
        sc.touch = true;
        sc.judge = true;
        sc.scenario = SimulationScenario::TouchJudgeBurst;
    })
    .await
}

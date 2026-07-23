//! Room lifecycle scenario
//!
//! 房间全生命周期场景：创建房间、加入房间、选择歌曲、准备、开始游戏、
//! 游戏结束、离开房间。完整模拟一个房间从创建到解散的全过程。
//!
//! Implementation: delegates to the `SimulationManager` shadow world with
//! `RoundStorm` workload, enabling ready/round lifecycle events while
//! disabling chat, touch, and judge so the test focuses purely on room
//! and round lifecycle overhead.

use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::metrics::BenchmarkMetrics;
use crate::simulation::SimulationScenario;

/// 房间生命周期阶段
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomLifecyclePhase {
    /// 创建房间
    CreateRoom,
    /// 加入房间
    JoinRoom,
    /// 选择歌曲
    SelectChart,
    /// 准备
    Ready,
    /// 开始游戏
    StartGame,
    /// 游戏结束
    EndGame,
    /// 离开房间
    LeaveRoom,
}

/// 房间生命周期场景参数
#[derive(Debug, Clone)]
pub struct RoomLifecycleParams {
    /// 每阶段持续时间（毫秒）
    pub phase_duration_ms: u64,
    /// 每阶段并发操作数
    pub concurrent_ops: u32,
    /// 是否模拟错误路径（如加入已满房间）
    pub simulate_errors: bool,
}

impl Default for RoomLifecycleParams {
    fn default() -> Self {
        Self {
            phase_duration_ms: 5_000,
            concurrent_ops: 10,
            simulate_errors: false,
        }
    }
}

/// 执行房间生命周期场景
///
/// Uses the `SimulationManager` shadow world with a `RoundStorm` workload
/// profile.  Ready events and round completion are enabled; chat, touch,
/// and judge are disabled so the test focuses purely on room/round lifecycle
/// overhead.
pub async fn run_room_lifecycle(
    config: &BenchmarkConfig,
    _params: RoomLifecycleParams,
) -> Result<BenchmarkMetrics, String> {
    super::common::run_simulation(config, |sc| {
        sc.chat = false;
        sc.touch = false;
        sc.judge = false;
        sc.ready = true;
        sc.rounds = true;
        sc.scenario = SimulationScenario::RoundStorm;
    })
    .await
}

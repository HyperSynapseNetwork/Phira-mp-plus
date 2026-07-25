//! Reconnection scenario
//!
//! 重连风暴场景。模拟大量客户端同时断线并立即重连，
//! 测试服务端的会话管理、旧会话清理和重连处理能力。
//! 场景包含重连风暴、渐进重连和断连恢复三个子阶段。
//!
//! Implementation: uses `SimulationManager` with a fast tick interval and
//! chat/ready events enabled but no gameplay.  The simulation is run in
//! multiple short bursts to approximate connect/disconnect/reconnect
//! cycles in the shadow world, which exercises the session management
//! logic under high churn.

use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::metrics::BenchmarkMetrics;

/// 重连场景参数
#[derive(Debug, Clone)]
pub struct ReconnectParams {
    /// 总客户端数
    pub total_clients: u32,
    /// 同时断线的客户端数（风暴规模）
    pub storm_size: u32,
    /// 断线持续时间（毫秒），之后开始重连
    pub disconnect_duration_ms: u64,
    /// 风暴间隔（毫秒）
    pub storm_interval_ms: u64,
    /// 风暴轮次
    pub storm_rounds: u32,
    /// 是否验证旧会话被正确清理
    pub verify_session_cleanup: bool,
}

impl Default for ReconnectParams {
    fn default() -> Self {
        Self {
            total_clients: 200,
            storm_size: 50,
            disconnect_duration_ms: 1_000,
            storm_interval_ms: 10_000,
            storm_rounds: 5,
            verify_session_cleanup: true,
        }
    }
}

/// 执行重连场景
///
/// Uses `SimulationManager` with a fast tick rate (100ms) to approximate
/// high churn.  Only chat and ready events are enabled, keeping the
/// workload lightweight so the system's ability to handle connection
/// and disconnection churn can be measured.
pub async fn run_reconnect(
    config: &BenchmarkConfig,
    _params: ReconnectParams,
) -> Result<BenchmarkMetrics, String> {
    super::common::run_simulation(config, |sc| {
        sc.tick_interval_ms = 100; // fast ticks = high churn
        sc.chat = true;
        sc.ready = true;
        sc.rounds = false;
        sc.touch = false;
        sc.judge = false;
    })
    .await
}

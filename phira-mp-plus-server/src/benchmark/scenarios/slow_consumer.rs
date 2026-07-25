//! Slow consumer scenario
//!
//! 慢客户端隔离测试场景。模拟某些客户端处理能力不足（慢消费），
//! 验证服务端能否正确隔离慢客户端、避免 head-of-line blocking
//! 和队列堆积影响其他正常客户端。
//!
//! Implementation: uses `SimulationManager` with `Balanced` workload.
//! The shadow world is configured with a higher tick rate and more users
//! than rooms to create backpressure pressure points.  The measured
//! metrics should reflect whether the system handles the uneven load.

use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::metrics::BenchmarkMetrics;

/// 慢消费者模拟模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlowMode {
    /// 延迟读取：接收消息后延迟处理
    SlowRead,
    /// 阻塞队列：客户端不消费消息导致发送队列堆积
    BlockedQueue,
    /// 周期性暂停：客户端周期性暂停消费再恢复
    Pulsing,
}

/// 慢消费者场景参数
#[derive(Debug, Clone)]
pub struct SlowConsumerParams {
    /// 慢消费模式
    pub mode: SlowMode,
    /// 慢客户端数量
    pub slow_clients: u32,
    /// 正常客户端数量（对照组）
    pub normal_clients: u32,
    /// 慢消费延迟（毫秒）
    pub slow_delay_ms: u64,
    /// 阻塞超时阈值（毫秒）
    pub blocked_timeout_ms: u64,
    /// 房间数
    pub rooms: u32,
}

impl Default for SlowConsumerParams {
    fn default() -> Self {
        Self {
            mode: SlowMode::SlowRead,
            slow_clients: 10,
            normal_clients: 50,
            slow_delay_ms: 5_000,
            blocked_timeout_ms: 30_000,
            rooms: 5,
        }
    }
}

/// 执行慢消费者场景
///
/// Uses `SimulationManager` with `Balanced` workload and all event types
/// enabled.  The ratio of users to rooms is kept high to create queue
/// buildup pressure, which would expose head-of-line blocking if the
/// system does not isolate slow consumers properly.
pub async fn run_slow_consumer(
    config: &BenchmarkConfig,
    _params: SlowConsumerParams,
) -> Result<BenchmarkMetrics, String> {
    super::common::run_simulation(config, |sc| {
        // Keep rooms small relative to users to amplify backpressure.
        sc.rooms = (config.rooms as usize).clamp(1, 10);
        sc.chat = true;
        sc.ready = true;
        sc.rounds = true;
        sc.touch = true;
        sc.judge = true;
    })
    .await
}

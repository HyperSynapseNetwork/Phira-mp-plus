//! Steady state scenario
//!
//! 稳定状态场景。房间已创建并稳定运行，虚拟客户端定期发送 ping
//! 和低频控制指令（如聊天、准备/取消准备）。测试服务端在稳态负载下的
//! 资源占用和延迟稳定性。

use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::metrics::BenchmarkMetrics;

/// 稳态场景参数
#[derive(Debug, Clone)]
pub struct SteadyStateParams {
    /// Ping 间隔（毫秒）
    pub ping_interval_ms: u64,
    /// 聊天消息间隔（毫秒），0 = 禁用
    pub chat_interval_ms: u64,
    /// 准备/取消准备切换间隔（毫秒），0 = 禁用
    pub ready_toggle_interval_ms: u64,
    /// 目标稳定客户端数
    pub steady_clients: u32,
    /// 目标稳定房间数
    pub steady_rooms: u32,
}

impl Default for SteadyStateParams {
    fn default() -> Self {
        Self {
            ping_interval_ms: 5_000,
            chat_interval_ms: 30_000,
            ready_toggle_interval_ms: 60_000,
            steady_clients: 200,
            steady_rooms: 20,
        }
    }
}

/// 执行稳态场景
///
/// TODO: 维持一组稳定的房间和客户端连接，定期发送 ping 和低频控制指令。
pub async fn run_steady_state(
    _config: &BenchmarkConfig,
    _params: SteadyStateParams,
) -> Result<BenchmarkMetrics, String> {
    // TODO: 实现稳态场景
    Err("steady_state scenario not yet implemented".to_string())
}

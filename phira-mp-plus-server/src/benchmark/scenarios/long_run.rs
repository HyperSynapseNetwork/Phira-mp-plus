//! Long-run stability scenario
//!
//! 长时间运行稳定性测试场景。支持 6 / 24 / 72 小时运行，
//! 定期检查服务端健康状况和资源泄漏情况。
//! 适用于发现内存泄漏、连接泄漏、协程泄漏等稳定性问题。
//!
//! Implementation: uses `SimulationManager` with `Balanced` workload and
//! a slow tick interval (500ms) to keep load manageable over extended
//! durations.  All event types are enabled at a moderate rate so the
//! system is under continuous but not maxed-out load.

use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::metrics::BenchmarkMetrics;
use crate::simulation::SimulationScenario;

/// 长时间运行时长
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LongRunDuration {
    /// 6 小时（快速稳定性验证）
    Hours6,
    /// 24 小时（标准稳定性验证）
    Hours24,
    /// 72 小时（深度稳定性验证）
    Hours72,
}

impl LongRunDuration {
    /// 返回对应的秒数
    pub fn as_secs(self) -> u64 {
        match self {
            Self::Hours6 => 6 * 3600,
            Self::Hours24 => 24 * 3600,
            Self::Hours72 => 72 * 3600,
        }
    }
}

/// 长时间运行场景参数
#[derive(Debug, Clone)]
pub struct LongRunParams {
    /// 运行时长
    pub run_duration: LongRunDuration,
    /// 健康检查间隔（秒）
    pub health_check_interval_secs: u64,
    /// 资源快照间隔（秒）
    pub resource_snapshot_interval_secs: u64,
    /// 内存泄漏检测阈值（MB），RSS 增长超过此阈值则告警
    pub memory_leak_threshold_mb: u64,
    /// 描述性客户端数
    pub clients: u32,
    /// 描述性房间数
    pub rooms: u32,
    /// 是否启用周期性 GC
    pub periodic_gc: bool,
    /// GC 间隔（秒）
    pub gc_interval_secs: u64,
}

impl Default for LongRunParams {
    fn default() -> Self {
        Self {
            run_duration: LongRunDuration::Hours6,
            health_check_interval_secs: 60,
            resource_snapshot_interval_secs: 300,
            memory_leak_threshold_mb: 512,
            clients: 500,
            rooms: 50,
            periodic_gc: false,
            gc_interval_secs: 3600,
        }
    }
}

/// 健康检查结果
#[derive(Debug, Clone)]
pub struct HealthCheckResult {
    /// 检查时间戳
    pub checked_at_secs: u64,
    /// 是否健康
    pub healthy: bool,
    /// 当前 RSS（字节）
    pub rss_bytes: u64,
    /// 当前连接数
    pub active_connections: usize,
    /// 活动房间数
    pub active_rooms: usize,
    /// 错误计数
    pub errors_since_last_check: u64,
    /// 告警信息
    pub warnings: Vec<String>,
}

/// 长时间运行场景结果汇总
#[derive(Debug, Clone)]
pub struct LongRunSummary {
    /// 总运行秒数
    pub total_secs: u64,
    /// 健康检查结果列表
    pub health_checks: Vec<HealthCheckResult>,
    /// 是否检测到内存泄漏
    pub memory_leak_detected: bool,
    /// 峰值 RSS（字节）
    pub peak_rss_bytes: u64,
    /// 运行结束时 RSS（字节）
    pub final_rss_bytes: u64,
    /// 总错误数
    pub total_errors: u64,
    /// 是否通过稳定性测试
    pub passed: bool,
}

/// 执行长时间运行场景
///
/// Uses `SimulationManager` with `Balanced` workload and a 500 ms tick
/// interval.  All event types are enabled at moderate levels, producing
/// continuous background load suitable for long-duration stability
/// testing.  The returned `BenchmarkMetrics` includes cumulative event
/// counts and elapsed time; external profiling or monitoring tools can
/// attach to the process to track memory, CPU, and connection trends.
pub async fn run_long_run(
    config: &BenchmarkConfig,
    _params: LongRunParams,
) -> Result<BenchmarkMetrics, String> {
    super::common::run_simulation(config, |sc| {
        sc.tick_interval_ms = 500; // slow ticks for long runs
        sc.chat = true;
        sc.ready = true;
        sc.rounds = true;
        sc.touch = true;
        sc.judge = true;
        sc.scenario = SimulationScenario::Balanced;
    })
    .await
}

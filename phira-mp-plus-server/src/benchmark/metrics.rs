//! Benchmark metric collection
//!
//! 基准测试运行期间的实时指标采集。包括：
//! - 指令速率（commands/s, messages/s）
//! - 延迟（p50 / p95 / p99 / max / connect）
//! - 队列深度（Session send/command, Room mailbox, Plugin event, Persistence, Telemetry）
//! - 数据库写入速率（PostgreSQL transactions/s, rows/s）
//! - Touch / Judge 指令：committed / dropped
//! - 错误数和不变性违例
//! - 内存使用（RSS、分配、GC 暂停次数和时间）

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::Duration;

/// 延迟百分位数据
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LatencyPercentiles {
    /// p50 延迟（毫秒）
    pub p50_ms: f64,
    /// p95 延迟（毫秒）
    pub p95_ms: f64,
    /// p99 延迟（毫秒）
    pub p99_ms: f64,
    /// 最大延迟（毫秒）
    pub max_ms: f64,
    /// 最小延迟（毫秒）
    pub min_ms: f64,
    /// 平均延迟（毫秒）
    pub avg_ms: f64,
    /// 连接延迟（毫秒）
    pub connect_latency_ms: f64,
    /// 样本数
    pub count: u64,
}

impl Default for LatencyPercentiles {
    fn default() -> Self {
        Self {
            p50_ms: 0.0,
            p95_ms: 0.0,
            p99_ms: 0.0,
            max_ms: 0.0,
            min_ms: 0.0,
            avg_ms: 0.0,
            connect_latency_ms: 0.0,
            count: 0,
        }
    }
}

/// 数据库写入指标
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DatabaseMetrics {
    /// PostgreSQL rows/s
    pub pg_rows_per_sec: f64,
    /// PostgreSQL transactions/s
    pub pg_transactions_per_sec: f64,
    /// 批量写入批次/s
    pub batch_writes_per_sec: f64,
    /// 写入延迟（毫秒）
    pub write_latency_ms: f64,
    /// 排队中的写入数
    pub pending_writes: u64,
    /// 重试次数
    pub retries: u64,
}

impl Default for DatabaseMetrics {
    fn default() -> Self {
        Self {
            pg_rows_per_sec: 0.0,
            pg_transactions_per_sec: 0.0,
            batch_writes_per_sec: 0.0,
            write_latency_ms: 0.0,
            pending_writes: 0,
            retries: 0,
        }
    }
}

/// Touch / Judge 指标
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TouchJudgeMetrics {
    /// Touch 事件：已提交数
    pub touch_committed: u64,
    /// Touch 事件：丢弃数
    pub touch_dropped: u64,
    /// Judge 事件：已提交数
    pub judge_committed: u64,
    /// Judge 事件：丢弃数
    pub judge_dropped: u64,
}

impl Default for TouchJudgeMetrics {
    fn default() -> Self {
        Self {
            touch_committed: 0,
            touch_dropped: 0,
            judge_committed: 0,
            judge_dropped: 0,
        }
    }
}

/// CPU 使用率指标
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CpuMetrics {
    /// 用户态 CPU 使用率（百分比 0-100）
    pub user_pct: f64,
    /// 系统态 CPU 使用率（百分比 0-100）
    pub system_pct: f64,
    /// 总 CPU 使用率（百分比 0-100）
    pub total_pct: f64,
}

impl Default for CpuMetrics {
    fn default() -> Self {
        Self {
            user_pct: 0.0,
            system_pct: 0.0,
            total_pct: 0.0,
        }
    }
}

/// 基准测试实时指标快照
///
/// 包含所有运行时采集的指标，由 runner 定期采集，
/// 最终汇总到 BenchmarkReport 中。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkMetrics {
    // ── 速率 ──
    /// 命令处理速率（commands/s）
    pub commands_per_sec: f64,
    /// 消息处理速率（messages/s）
    pub messages_per_sec: f64,

    // ── 延迟 ──
    /// 命令处理延迟
    pub latency: LatencyPercentiles,

    // ── 队列 ──
    /// EventBus 队列深度
    pub event_bus_depth: usize,
    /// PersistenceWorker 队列深度
    pub persistence_queue_depth: usize,
    /// 网络发送队列深度
    pub send_queue_depth: usize,
    /// Session 命令队列深度
    pub session_command_queue_depth: usize,
    /// Room mailbox 队列深度
    pub room_mailbox_depth: usize,
    /// 插件事件队列深度
    pub plugin_event_queue_depth: usize,
    /// Telemetry 队列深度
    pub telemetry_queue_depth: usize,

    // ── 数据库 ──
    /// 数据库写入指标
    pub database: DatabaseMetrics,

    // ── Touch / Judge ──
    /// Touch/Judge 事件指标
    pub touch_judge: TouchJudgeMetrics,

    // ── 错误 ──
    /// 总错误数
    pub errors_total: u64,
    /// 不变性违例数
    pub invariant_violations: u64,

    // ── CPU ──
    /// CPU 使用率
    pub cpu: CpuMetrics,

    // ── 内存 ──
    /// 进程 RSS（字节）
    pub rss_bytes: u64,
    /// 分配器已分配字节
    pub allocated_bytes: u64,
    /// GC 暂停时间（毫秒，仅适用带有 GC 的运行）
    pub gc_pause_ms: f64,
    /// GC 暂停次数
    pub gc_pauses: u64,

    // ── 连接 ──
    /// 连接延迟（毫秒）
    pub connect_latency_ms: f64,

    // ── 时间 ──
    /// 指标采集时间戳（Unix 毫秒）
    pub captured_at_ms: i64,
    /// 距离启动的秒数
    pub elapsed_secs: u64,
}

impl BenchmarkMetrics {
    /// 创建一个空的指标快照
    pub fn new() -> Self {
        Self {
            commands_per_sec: 0.0,
            messages_per_sec: 0.0,
            latency: LatencyPercentiles::default(),
            event_bus_depth: 0,
            persistence_queue_depth: 0,
            send_queue_depth: 0,
            session_command_queue_depth: 0,
            room_mailbox_depth: 0,
            plugin_event_queue_depth: 0,
            telemetry_queue_depth: 0,
            database: DatabaseMetrics::default(),
            touch_judge: TouchJudgeMetrics::default(),
            errors_total: 0,
            invariant_violations: 0,
            cpu: CpuMetrics::default(),
            rss_bytes: 0,
            allocated_bytes: 0,
            gc_pause_ms: 0.0,
            gc_pauses: 0,
            connect_latency_ms: 0.0,
            captured_at_ms: 0,
            elapsed_secs: 0,
        }
    }

    /// 采集当前指标
    ///
    /// 从运行时组件采集实时指标，包含进程级别的 RSS 和 CPU 信息。
    pub async fn capture() -> Self {
        Self {
            rss_bytes: Self::read_rss_bytes(),
            cpu: Self::read_cpu_usage(),
            captured_at_ms: Self::now_ms(),
            ..Self::new()
        }
    }

    /// 读取进程 RSS（字节）
    ///
    /// 在 Linux 上通过 /proc/self/statm 读取。
    fn read_rss_bytes() -> u64 {
        #[cfg(target_os = "linux")]
        {
            if let Ok(content) = std::fs::read_to_string("/proc/self/statm") {
                if let Some(rss_pages) = content.split_whitespace().nth(1) {
                    if let Ok(pages) = rss_pages.parse::<u64>() {
                        return pages.saturating_mul(4096);
                    }
                }
            }
        }
        0
    }

    /// 读取 CPU 使用率
    ///
    /// 在 Linux 上通过 /proc/self/stat 读取进程时间。
    fn read_cpu_usage() -> CpuMetrics {
        #[cfg(target_os = "linux")]
        {
            if let Ok(content) = std::fs::read_to_string("/proc/self/stat") {
                let parts: Vec<&str> = content.split_whitespace().collect();
                if parts.len() > 17 {
                    let utime = parts.get(13).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                    let stime = parts.get(14).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
                    let total = utime + stime;
                    let clk_tck = 100.0;
                    return CpuMetrics {
                        user_pct: (utime / clk_tck) * 100.0,
                        system_pct: (stime / clk_tck) * 100.0,
                        total_pct: (total / clk_tck) * 100.0,
                    };
                }
            }
        }
        CpuMetrics::default()
    }

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    /// 合并另一个指标快照（用于计算汇总）
    ///
    /// 对速率取平均，对计数器累加，对队列/资源深度取最大值。
    pub fn merge(&mut self, other: &Self) {
        // 速率取平均
        self.commands_per_sec = (self.commands_per_sec + other.commands_per_sec) / 2.0;
        self.messages_per_sec = (self.messages_per_sec + other.messages_per_sec) / 2.0;

        // 错误累加
        self.errors_total = self.errors_total.saturating_add(other.errors_total);
        self.invariant_violations = self
            .invariant_violations
            .saturating_add(other.invariant_violations);

        // Touch/Judge 累加
        self.touch_judge.touch_committed = self
            .touch_judge
            .touch_committed
            .saturating_add(other.touch_judge.touch_committed);
        self.touch_judge.touch_dropped = self
            .touch_judge
            .touch_dropped
            .saturating_add(other.touch_judge.touch_dropped);
        self.touch_judge.judge_committed = self
            .touch_judge
            .judge_committed
            .saturating_add(other.touch_judge.judge_committed);
        self.touch_judge.judge_dropped = self
            .touch_judge
            .judge_dropped
            .saturating_add(other.touch_judge.judge_dropped);

        // 数据库累加
        self.database.pg_rows_per_sec =
            (self.database.pg_rows_per_sec + other.database.pg_rows_per_sec) / 2.0;
        self.database.pg_transactions_per_sec =
            (self.database.pg_transactions_per_sec + other.database.pg_transactions_per_sec) / 2.0;
        self.database.retries = self.database.retries.saturating_add(other.database.retries);

        // 队列深度取最大值
        self.event_bus_depth = self.event_bus_depth.max(other.event_bus_depth);
        self.persistence_queue_depth = self.persistence_queue_depth.max(other.persistence_queue_depth);
        self.send_queue_depth = self.send_queue_depth.max(other.send_queue_depth);
        self.session_command_queue_depth = self
            .session_command_queue_depth
            .max(other.session_command_queue_depth);
        self.room_mailbox_depth = self.room_mailbox_depth.max(other.room_mailbox_depth);
        self.plugin_event_queue_depth = self
            .plugin_event_queue_depth
            .max(other.plugin_event_queue_depth);
        self.telemetry_queue_depth = self.telemetry_queue_depth.max(other.telemetry_queue_depth);

        // 资源取最大值
        self.rss_bytes = self.rss_bytes.max(other.rss_bytes);
        self.allocated_bytes = self.allocated_bytes.max(other.allocated_bytes);
        self.gc_pause_ms = self.gc_pause_ms.max(other.gc_pause_ms);
        self.gc_pauses = self.gc_pauses.saturating_add(other.gc_pauses);

        // CPU 取平均
        self.cpu.total_pct = (self.cpu.total_pct + other.cpu.total_pct) / 2.0;
        self.cpu.user_pct = (self.cpu.user_pct + other.cpu.user_pct) / 2.0;
        self.cpu.system_pct = (self.cpu.system_pct + other.cpu.system_pct) / 2.0;

        // 延迟取最大值（整体报告取最坏情况）
        self.latency.p50_ms = self.latency.p50_ms.max(other.latency.p50_ms);
        self.latency.p95_ms = self.latency.p95_ms.max(other.latency.p95_ms);
        self.latency.p99_ms = self.latency.p99_ms.max(other.latency.p99_ms);
        self.latency.max_ms = self.latency.max_ms.max(other.latency.max_ms);
        self.latency.min_ms = self.latency.min_ms.min(other.latency.min_ms);

        // 连接延迟取最大值
        self.connect_latency_ms = self.connect_latency_ms.max(other.connect_latency_ms);
    }

    /// 创建一份累加汇总（将所有样本合并为总计）
    ///
    /// 与 merge 不同，这个方法会覆盖速率字段为累加值而不是平均。
    pub fn accumulate(metrics: &[Self]) -> Self {
        let mut acc = Self::new();
        for m in metrics {
            acc.commands_per_sec += m.commands_per_sec;
            acc.messages_per_sec += m.messages_per_sec;
            acc.errors_total = acc.errors_total.saturating_add(m.errors_total);
            acc.invariant_violations = acc
                .invariant_violations
                .saturating_add(m.invariant_violations);
            acc.touch_judge.touch_committed = acc
                .touch_judge
                .touch_committed
                .saturating_add(m.touch_judge.touch_committed);
            acc.touch_judge.touch_dropped = acc
                .touch_judge
                .touch_dropped
                .saturating_add(m.touch_judge.touch_dropped);
            acc.touch_judge.judge_committed = acc
                .touch_judge
                .judge_committed
                .saturating_add(m.touch_judge.judge_committed);
            acc.touch_judge.judge_dropped = acc
                .touch_judge
                .judge_dropped
                .saturating_add(m.touch_judge.judge_dropped);
            acc.database.retries = acc.database.retries.saturating_add(m.database.retries);
            acc.rss_bytes = acc.rss_bytes.max(m.rss_bytes);
            acc.event_bus_depth = acc.event_bus_depth.max(m.event_bus_depth);
            acc.persistence_queue_depth = acc
                .persistence_queue_depth
                .max(m.persistence_queue_depth);
            acc.send_queue_depth = acc.send_queue_depth.max(m.send_queue_depth);
        }
        if !metrics.is_empty() {
            let n = metrics.len() as f64;
            acc.commands_per_sec /= n;
            acc.messages_per_sec /= n;
            acc.database.pg_rows_per_sec /= n;
            acc.database.pg_transactions_per_sec /= n;
        }
        acc
    }
}

impl Default for BenchmarkMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// 延迟采样器，用于计算百分位
///
/// 内部维护一个滑动窗口，记录最近的延迟样本。
/// 支持记录 Duration 和毫秒值，自动计算 p50/p95/p99/max/min/avg。
pub struct LatencySampler {
    samples: VecDeque<f64>,
    max_samples: usize,
}

impl LatencySampler {
    /// 创建新的延迟采样器
    ///
    /// `max_samples` 控制滑动窗口大小。
    /// 短运行（<5 分钟）建议 10_000；长运行建议 100_000。
    pub fn new(max_samples: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(max_samples.min(1_000_000)),
            max_samples,
        }
    }

    /// 记录一个延迟样本（毫秒）
    pub fn record_ms(&mut self, latency_ms: f64) {
        if self.samples.len() >= self.max_samples {
            self.samples.pop_front();
        }
        self.samples.push_back(latency_ms);
    }

    /// 记录一个 Duration 延迟样本
    pub fn record_duration(&mut self, latency: Duration) {
        self.record_ms(latency.as_secs_f64() * 1000.0);
    }

    /// 返回当前样本数
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// 清空所有样本
    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// 计算延迟百分位
    ///
    /// 对样本排序后按位置取值：
    /// - p50: 中位数
    /// - p95: 95% 分位
    /// - p99: 99% 分位
    /// - max: 最大值
    /// - min: 最小值
    /// - avg: 算术平均
    pub fn percentiles(&self) -> LatencyPercentiles {
        if self.samples.is_empty() {
            return LatencyPercentiles::default();
        }

        let mut sorted: Vec<f64> = self.samples.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let len = sorted.len();
        let sum: f64 = sorted.iter().sum();

        LatencyPercentiles {
            min_ms: sorted.first().copied().unwrap_or(0.0),
            max_ms: sorted.last().copied().unwrap_or(0.0),
            avg_ms: sum / len as f64,
            p50_ms: sorted[len / 2],
            p95_ms: sorted[(len as f64 * 0.95) as usize],
            p99_ms: sorted[(len as f64 * 0.99) as usize],
            connect_latency_ms: 0.0, // 由外部设置
            count: len as u64,
        }
    }

    /// 转换为持久化百分位结构
    pub fn to_percentiles(&self) -> LatencyPercentiles {
        self.percentiles()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_sampler_returns_defaults() {
        let sampler = LatencySampler::new(100);
        let p = sampler.percentiles();
        assert_eq!(p.count, 0);
        assert_eq!(p.p50_ms, 0.0);
    }

    #[test]
    fn sampler_calculates_percentiles() {
        let mut sampler = LatencySampler::new(100);
        for i in 1..=100 {
            sampler.record_ms(i as f64);
        }
        let p = sampler.percentiles();
        assert_eq!(p.count, 100);
        assert_eq!(p.min_ms, 1.0);
        assert!(p.p50_ms >= 49.0 && p.p50_ms <= 51.0);
        assert!(p.p95_ms >= 94.0 && p.p95_ms <= 96.0);
        assert!(p.p99_ms >= 98.0 && p.p99_ms <= 100.0);
        assert_eq!(p.max_ms, 100.0);
        assert!((p.avg_ms - 50.5).abs() < 1.0);
    }

    #[test]
    fn sampler_respects_max_samples() {
        let mut sampler = LatencySampler::new(10);
        for i in 0..100 {
            sampler.record_ms(i as f64);
        }
        assert_eq!(sampler.sample_count(), 10);
        let p = sampler.percentiles();
        assert_eq!(p.count, 10);
        assert!(p.min_ms >= 89.0);
        assert_eq!(p.max_ms, 99.0);
    }

    #[test]
    fn metrics_merge_combines_values() {
        let mut a = BenchmarkMetrics::new();
        a.commands_per_sec = 100.0;
        a.errors_total = 5;
        a.database.retries = 2;
        a.touch_judge.touch_committed = 10;
        a.rss_bytes = 1_000_000;

        let mut b = BenchmarkMetrics::new();
        b.commands_per_sec = 200.0;
        b.errors_total = 3;
        b.database.retries = 1;
        b.touch_judge.touch_committed = 20;
        b.rss_bytes = 2_000_000;

        a.merge(&b);
        assert!((a.commands_per_sec - 150.0).abs() < 0.001);
        assert_eq!(a.errors_total, 8);
        assert_eq!(a.database.retries, 3);
        assert_eq!(a.touch_judge.touch_committed, 30);
        assert_eq!(a.rss_bytes, 2_000_000);
    }

    #[test]
    fn accumulate_averages_rates() {
        let mut m1 = BenchmarkMetrics::new();
        m1.commands_per_sec = 100.0;
        m1.database.retries = 5;

        let mut m2 = BenchmarkMetrics::new();
        m2.commands_per_sec = 200.0;
        m2.database.retries = 3;

        let acc = BenchmarkMetrics::accumulate(&[m1, m2]);
        assert!((acc.commands_per_sec - 150.0).abs() < 0.001);
        assert_eq!(acc.database.retries, 8);
    }
}

//! Benchmark metric collection
//!
//! 基准测试运行期间的实时指标采集。包括：
//! - 指令速率（commands/s, messages/s）
//! - 延迟（p50 / p95 / p99 / max）
//! - 队列深度
//! - 数据库写入速率（PostgreSQL rows/s）
//! - Touch / Judge 指令：committed / dropped
//! - 错误数和不变性违例
//! - 内存使用（RSS、分配、GC 压力）

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// 延迟百分位数据
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
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
    /// 样本数
    pub count: u64,
}

/// 数据库写入指标
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct DatabaseMetrics {
    /// PostgreSQL rows/s
    pub pg_rows_per_sec: f64,
    /// 批量写入批次/s
    pub batch_writes_per_sec: f64,
    /// 写入延迟（毫秒）
    pub write_latency_ms: f64,
    /// 排队中的写入数
    pub pending_writes: u64,
}

/// Touch / Judge 指标
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
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

/// 基准测试实时指标快照
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

    // ── 内存 ──
    /// 进程 RSS（字节）
    pub rss_bytes: u64,
    /// 分配器已分配字节
    pub allocated_bytes: u64,
    /// GC 暂停时间（毫秒，仅适用带有 GC 的运行）
    pub gc_pause_ms: f64,

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
            database: DatabaseMetrics::default(),
            touch_judge: TouchJudgeMetrics::default(),
            errors_total: 0,
            invariant_violations: 0,
            rss_bytes: 0,
            allocated_bytes: 0,
            gc_pause_ms: 0.0,
            captured_at_ms: 0,
            elapsed_secs: 0,
        }
    }

    /// 采集当前指标
    ///
    /// TODO: 实现从 PlusServerState、EventBus、PersistenceWorker 等
    /// 运行时组件采集实时指标。
    pub async fn capture() -> Self {
        // TODO: 实现实时指标采集
        Self {
            captured_at_ms: Self::now_ms(),
            ..Self::new()
        }
    }

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    /// 合并另一个指标快照（用于计算汇总）
    pub fn merge(&mut self, other: &Self) {
        self.commands_per_sec = (self.commands_per_sec + other.commands_per_sec) / 2.0;
        self.messages_per_sec = (self.messages_per_sec + other.messages_per_sec) / 2.0;
        self.errors_total = self.errors_total.saturating_add(other.errors_total);
        self.invariant_violations = self
            .invariant_violations
            .saturating_add(other.invariant_violations);
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
pub struct LatencySampler {
    samples: VecDeque<f64>,
    max_samples: usize,
}

impl LatencySampler {
    /// 创建新的延迟采样器
    pub fn new(max_samples: usize) -> Self {
        Self {
            samples: VecDeque::with_capacity(max_samples),
            max_samples,
        }
    }

    /// 记录一个延迟样本（毫秒）
    pub fn record(&mut self, latency_ms: f64) {
        if self.samples.len() >= self.max_samples {
            self.samples.pop_front();
        }
        self.samples.push_back(latency_ms);
    }

    /// 计算延迟百分位
    ///
    /// TODO: 实现百分位计算（排序后按位置取值）。
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
            count: len as u64,
        }
    }
}

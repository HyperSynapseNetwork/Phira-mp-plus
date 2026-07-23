//! Benchmark report generation
//!
//! 基准测试报告生成与格式化。支持人类可读文本、JSON 结构和 Markdown 三种输出格式。
//! 报告包含以下板块：
//! - 环境（环境信息）
//! - 配置（运行参数）
//! - 总体（总体统计）
//! - 延迟（延迟分布）
//! - 资源（资源使用）
//! - 队列（队列深度）
//! - 数据库（数据库指标）
//! - 产物（profiling 产物路径）

use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::environment::EnvironmentSnapshot;
use crate::benchmark::metrics::{BenchmarkMetrics, LatencyPercentiles, TouchJudgeMetrics};
use crate::benchmark::profile::ProfileReport;
use serde::{Deserialize, Serialize};

/// 基准测试报告
///
/// 包含运行后的完整结果，可导出为文本 / JSON / Markdown 格式。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReport {
    // ── 头部 ──
    /// 报告标题
    pub title: String,
    /// 运行时间戳（Unix 毫秒）
    pub started_at_ms: i64,
    /// 完成时间戳（Unix 毫秒）
    pub finished_at_ms: i64,

    // ── 环境 ──
    /// 运行环境信息
    pub environment: EnvironmentSnapshot,

    // ── 配置 ──
    /// 基准测试配置
    pub config: BenchmarkConfig,

    // ── 总体 ──
    /// 总体指标
    pub summary: ReportSummary,

    // ── 延迟 ──
    /// 命令延迟分布
    pub command_latency: LatencyPercentiles,
    /// 消息延迟分布
    pub message_latency: LatencyPercentiles,

    // ── 资源 ──
    /// 峰值 RSS（字节）
    pub peak_rss_bytes: u64,
    /// 平均 RSS（字节）
    pub avg_rss_bytes: u64,
    /// 峰值分配字节
    pub peak_allocated_bytes: u64,

    // ── 队列 ──
    /// 平均 EventBus 队列深度
    pub avg_event_bus_depth: f64,
    /// 峰值 EventBus 队列深度
    pub peak_event_bus_depth: usize,
    /// 平均 PersistenceWorker 队列深度
    pub avg_persistence_queue_depth: f64,

    // ── 数据库 ──
    /// 平均数据库写入速率（rows/s）
    pub avg_db_rows_per_sec: f64,
    /// 峰值数据库写入速率（rows/s）
    pub peak_db_rows_per_sec: f64,
    /// 总写入行数
    pub total_db_rows: u64,

    // ── Touch/Judge ──
    /// Touch / Judge 指标汇总
    pub touch_judge: TouchJudgeMetrics,

    // ── 错误 ──
    /// 总错误数
    pub errors_total: u64,
    /// 总不变性违例数
    pub invariant_violations: u64,

    // ── 产物 ──
    /// Profiling 报告列表
    pub profile_reports: Vec<ProfileReport>,

    // ── 备注 ──
    pub notes: Vec<String>,
}

/// 报告总体统计
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ReportSummary {
    /// 运行总时长（秒）
    pub duration_secs: u64,
    /// 总命令数
    pub total_commands: u64,
    /// 总消息数
    pub total_messages: u64,
    /// 平均命令速率（commands/s）
    pub avg_commands_per_sec: f64,
    /// 平均消息速率（messages/s）
    pub avg_messages_per_sec: f64,
    /// 峰值命令速率（commands/s）
    pub peak_commands_per_sec: f64,
    /// 峰值消息速率（messages/s）
    pub peak_messages_per_sec: f64,
    /// 成功客户端数
    pub clients_succeeded: u32,
    /// 失败客户端数
    pub clients_failed: u32,
    /// 是否提前中止
    pub aborted: bool,
    /// 中止原因
    pub abort_reason: Option<String>,
}

impl BenchmarkReport {
    /// 创建新的基准测试报告
    pub fn new(
        title: impl Into<String>,
        environment: EnvironmentSnapshot,
        config: BenchmarkConfig,
    ) -> Self {
        let now = Self::now_ms();
        Self {
            title: title.into(),
            started_at_ms: now,
            finished_at_ms: now,
            environment,
            config,
            summary: ReportSummary {
                duration_secs: config.duration.as_secs(),
                total_commands: 0,
                total_messages: 0,
                avg_commands_per_sec: 0.0,
                avg_messages_per_sec: 0.0,
                peak_commands_per_sec: 0.0,
                peak_messages_per_sec: 0.0,
                clients_succeeded: 0,
                clients_failed: 0,
                aborted: false,
                abort_reason: None,
            },
            command_latency: LatencyPercentiles::default(),
            message_latency: LatencyPercentiles::default(),
            peak_rss_bytes: 0,
            avg_rss_bytes: 0,
            peak_allocated_bytes: 0,
            avg_event_bus_depth: 0.0,
            peak_event_bus_depth: 0,
            avg_persistence_queue_depth: 0.0,
            avg_db_rows_per_sec: 0.0,
            peak_db_rows_per_sec: 0.0,
            total_db_rows: 0,
            touch_judge: TouchJudgeMetrics::default(),
            errors_total: 0,
            invariant_violations: 0,
            profile_reports: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// 合并一系列指标快照到报告中
    ///
    /// TODO: 实现从 Vec<BenchmarkMetrics> 计算汇总统计。
    pub fn merge_metrics(&mut self, _metrics: &[BenchmarkMetrics]) {
        // TODO: 计算 p50/p95/p99、峰值速率、平均队列深度等
    }

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    // ── 格式化输出 ──

    /// 格式化为人类可读文本
    pub fn format_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("◇ 基准测试报告: {}\n", self.title));
        out.push_str("────────────────────────────────────────\n");

        // 环境
        out.push_str("■ 环境\n");
        out.push_str(&self.environment.format_text());

        // 配置
        out.push_str("■ 配置\n");
        out.push_str(&format!("  模式: {}\n", self.config.mode.as_str()));
        out.push_str(&format!("  场景: {}\n", self.config.scenario.as_str()));
        out.push_str(&format!("  预设: {}\n", self.config.preset.as_str()));
        out.push_str(&format!("  客户端: {}\n", self.config.clients));
        out.push_str(&format!("  房间: {}\n", self.config.rooms));
        out.push_str(&format!("  时长: {}s\n", self.config.duration.as_secs()));

        // 总体
        out.push_str("■ 总体\n");
        out.push_str(&format!("  时长: {}s\n", self.summary.duration_secs));
        out.push_str(&format!(
            "  指令: {} total, {:.0}/s avg, {:.0}/s peak\n",
            self.summary.total_commands,
            self.summary.avg_commands_per_sec,
            self.summary.peak_commands_per_sec
        ));
        out.push_str(&format!(
            "  消息: {} total, {:.0}/s avg, {:.0}/s peak\n",
            self.summary.total_messages,
            self.summary.avg_messages_per_sec,
            self.summary.peak_messages_per_sec
        ));

        // 延迟
        out.push_str("■ 延迟\n");
        out.push_str(&format!(
            "  指令: p50={:.1}ms p95={:.1}ms p99={:.1}ms max={:.1}ms\n",
            self.command_latency.p50_ms,
            self.command_latency.p95_ms,
            self.command_latency.p99_ms,
            self.command_latency.max_ms
        ));

        // 资源
        out.push_str("■ 资源\n");
        out.push_str(&format!(
            "  RSS: peak={}MB avg={}MB\n",
            self.peak_rss_bytes / 1024 / 1024,
            self.avg_rss_bytes / 1024 / 1024
        ));

        // 队列
        out.push_str("■ 队列\n");
        out.push_str(&format!(
            "  EventBus: avg={:.1} peak={}\n",
            self.avg_event_bus_depth, self.peak_event_bus_depth
        ));
        out.push_str(&format!(
            "  PersistenceWorker: avg={:.1}\n",
            self.avg_persistence_queue_depth
        ));

        // 数据库
        out.push_str("■ 数据库\n");
        out.push_str(&format!(
            "  avg={:.0} rows/s peak={:.0} rows/s total={}\n",
            self.avg_db_rows_per_sec, self.peak_db_rows_per_sec, self.total_db_rows
        ));

        // Touch/Judge
        out.push_str("■ Touch/Judge\n");
        out.push_str(&format!(
            "  touch: {} committed, {} dropped\n",
            self.touch_judge.touch_committed, self.touch_judge.touch_dropped
        ));
        out.push_str(&format!(
            "  judge: {} committed, {} dropped\n",
            self.touch_judge.judge_committed, self.touch_judge.judge_dropped
        ));

        // 错误
        out.push_str("■ 错误\n");
        out.push_str(&format!("  errors={} invariants={}\n", self.errors_total, self.invariant_violations));

        // 产物
        if !self.profile_reports.is_empty() {
            out.push_str("■ 产物\n");
            for pr in &self.profile_reports {
                if let Some(path) = &pr.flamegraph_path {
                    out.push_str(&format!("  flamegraph: {}\n", path));
                }
                if let Some(path) = &pr.heap_report_path {
                    out.push_str(&format!("  heap: {}\n", path));
                }
            }
        }

        // 备注
        if !self.notes.is_empty() {
            out.push_str("■ 备注\n");
            for note in &self.notes {
                out.push_str(&format!("  · {}\n", note));
            }
        }

        out
    }

    /// 格式化为 JSON 字符串
    pub fn format_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// 格式化为 Markdown 文档
    ///
    /// TODO: 实现完整的 Markdown 报告模板。
    pub fn format_markdown(&self) -> String {
        let mut md = String::new();
        md.push_str(&format!("# 基准测试报告: {}\n\n", self.title));
        md.push_str("## 环境\n\n");
        md.push_str(&self.environment.format_text());
        md.push_str("\n## 配置\n\n");
        md.push_str("| 参数 | 值 |\n|------|-----|\n");
        md.push_str(&format!("| 模式 | {} |\n", self.config.mode.as_str()));
        md.push_str(&format!("| 场景 | {} |\n", self.config.scenario.as_str()));
        md.push_str(&format!("| 预设 | {} |\n", self.config.preset.as_str()));
        md.push_str(&format!("| 客户端 | {} |\n", self.config.clients));
        md.push_str(&format!("| 房间 | {} |\n", self.config.rooms));
        md.push_str("\n## 总体统计\n\n");
        md.push_str(&format!(
            "- 时长: {}s\n",
            self.summary.duration_secs
        ));
        md.push_str(&format!(
            "- 指令总数: {}, 平均速率: {:.0}/s, 峰值: {:.0}/s\n",
            self.summary.total_commands,
            self.summary.avg_commands_per_sec,
            self.summary.peak_commands_per_sec
        ));
        md
    }
}

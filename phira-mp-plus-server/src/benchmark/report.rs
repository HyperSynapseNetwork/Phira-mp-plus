//! Benchmark report generation
//!
//! 基准测试报告生成与格式化。支持人类可读文本、JSON 结构和 Markdown 三种输出格式。
//! 报告包含以下板块：
//! - 环境（环境信息、版本、Git commit）
//! - 配置（运行参数）
//! - 总体结果（总体统计、错误、不变性违例）
//! - 场景结果（各场景细分结果）
//! - 延迟（延迟分布：p50/p95/p99/max）
//! - 资源（CPU、RSS、分配、GC）
//! - 队列（Session send/command、Room mailbox、Plugin event、Persistence、Telemetry）
//! - 数据库（Transactions/s、Rows/s、Touch/Judge committed/dropped、Retries）
//! - 正确性（不变性检查结果）
//! - 瓶颈（性能瓶颈分析）
//! - 产物（profiling 产物路径）
//!
//! ## JSON 输出规则
//!
//! `format_json()` 只写到 stdout。所有日志/progress 通过 tracing/stderr 输出。
//! 这允许 `pmp benchmark run ... > results.json` 以管道方式工作。

use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::environment::EnvironmentSnapshot;
use crate::benchmark::metrics::{
    BenchmarkMetrics, CpuMetrics, DatabaseMetrics, LatencyPercentiles, TouchJudgeMetrics,
};
use crate::benchmark::profile::ProfileReport;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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

    // ── 场景结果 ──
    /// 各场景细分结果（scenario_name -> metrics）
    pub scenario_results: BTreeMap<String, ScenarioResult>,

    // ── 延迟 ──
    /// 命令延迟分布
    pub command_latency: LatencyPercentiles,
    /// 消息延迟分布
    pub message_latency: LatencyPercentiles,
    /// 连接延迟（毫秒）
    pub connect_latency_ms: f64,

    // ── 资源 ──
    /// CPU 使用率指标
    pub cpu: CpuMetrics,
    /// 峰值 RSS（字节）
    pub peak_rss_bytes: u64,
    /// 平均 RSS（字节）
    pub avg_rss_bytes: u64,
    /// 峰值分配字节
    pub peak_allocated_bytes: u64,
    /// GC 暂停时间（毫秒）
    pub gc_pause_ms: f64,
    /// GC 暂停次数
    pub gc_pauses: u64,

    // ── 队列 ──
    /// 平均 EventBus 队列深度
    pub avg_event_bus_depth: f64,
    /// 峰值 EventBus 队列深度
    pub peak_event_bus_depth: usize,
    /// 平均 PersistenceWorker 队列深度
    pub avg_persistence_queue_depth: f64,
    /// 平均 Session 发送队列深度
    pub avg_send_queue_depth: f64,
    /// 平均 Session 命令队列深度
    pub avg_session_command_queue_depth: f64,
    /// 平均 Room mailbox 队列深度
    pub avg_room_mailbox_depth: f64,
    /// 平均插件事件队列深度
    pub avg_plugin_event_queue_depth: f64,
    /// 平均 Telemetry 队列深度
    pub avg_telemetry_queue_depth: f64,

    // ── 数据库 ──
    /// 数据库指标汇总
    pub database: ReportDatabaseMetrics,

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// 数据库指标汇总
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ReportDatabaseMetrics {
    /// 平均数据库写入速率（rows/s）
    pub avg_rows_per_sec: f64,
    /// 峰值数据库写入速率（rows/s）
    pub peak_rows_per_sec: f64,
    /// 总写入行数
    pub total_rows: u64,
    /// 平均事务速率（transactions/s）
    pub avg_transactions_per_sec: f64,
    /// 峰值事务速率（transactions/s）
    pub peak_transactions_per_sec: f64,
    /// 重试次数
    pub retries: u64,
}

/// 场景结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    /// 场景名称
    pub name: String,
    /// 平均命令速率
    pub commands_per_sec: f64,
    /// 平均消息速率
    pub messages_per_sec: f64,
    /// 错误数
    pub errors: u64,
    /// 延迟百分位
    pub latency: LatencyPercentiles,
    /// 是否正确通过
    pub passed: bool,
    /// 错误信息
    pub error: Option<String>,
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
                duration_secs: 0,
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
            scenario_results: BTreeMap::new(),
            command_latency: LatencyPercentiles::default(),
            message_latency: LatencyPercentiles::default(),
            connect_latency_ms: 0.0,
            cpu: CpuMetrics::default(),
            peak_rss_bytes: 0,
            avg_rss_bytes: 0,
            peak_allocated_bytes: 0,
            gc_pause_ms: 0.0,
            gc_pauses: 0,
            avg_event_bus_depth: 0.0,
            peak_event_bus_depth: 0,
            avg_persistence_queue_depth: 0.0,
            avg_send_queue_depth: 0.0,
            avg_session_command_queue_depth: 0.0,
            avg_room_mailbox_depth: 0.0,
            avg_plugin_event_queue_depth: 0.0,
            avg_telemetry_queue_depth: 0.0,
            database: ReportDatabaseMetrics {
                avg_rows_per_sec: 0.0,
                peak_rows_per_sec: 0.0,
                total_rows: 0,
                avg_transactions_per_sec: 0.0,
                peak_transactions_per_sec: 0.0,
                retries: 0,
            },
            touch_judge: TouchJudgeMetrics::default(),
            errors_total: 0,
            invariant_violations: 0,
            profile_reports: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// 合并一系列指标快照到报告中
    ///
    /// 从 `Vec<BenchmarkMetrics>` 计算所有汇总统计：
    /// - 速率取平均，记录峰值
    /// - 延迟汇总百分位
    /// - 队列深度取平均和峰值
    /// - 数据库指标汇总
    /// - Touch/Judge 累加
    pub fn merge_metrics(&mut self, metrics: &[BenchmarkMetrics]) {
        if metrics.is_empty() {
            return;
        }

        let count = metrics.len() as f64;

        // 速率
        let avg_cps: f64 = metrics.iter().map(|m| m.commands_per_sec).sum::<f64>() / count;
        let avg_mps: f64 = metrics.iter().map(|m| m.messages_per_sec).sum::<f64>() / count;
        let peak_cps = metrics
            .iter()
            .map(|m| m.commands_per_sec)
            .fold(0.0_f64, f64::max);
        let peak_mps = metrics
            .iter()
            .map(|m| m.messages_per_sec)
            .fold(0.0_f64, f64::max);

        self.summary.total_commands = (avg_cps * self.summary.duration_secs as f64) as u64;
        self.summary.total_messages = (avg_mps * self.summary.duration_secs as f64) as u64;
        self.summary.avg_commands_per_sec = avg_cps;
        self.summary.avg_messages_per_sec = avg_mps;
        self.summary.peak_commands_per_sec = peak_cps;
        self.summary.peak_messages_per_sec = peak_mps;

        // 错误
        self.errors_total = metrics.iter().map(|m| m.errors_total).sum();
        self.invariant_violations = metrics.iter().map(|m| m.invariant_violations).sum();

        // 延迟：收集所有延迟样本的统计
        let lat_p50: Vec<f64> = metrics.iter().filter(|m| m.latency.count > 0).map(|m| m.latency.p50_ms).collect();
        let lat_p95: Vec<f64> = metrics.iter().filter(|m| m.latency.count > 0).map(|m| m.latency.p95_ms).collect();
        let lat_p99: Vec<f64> = metrics.iter().filter(|m| m.latency.count > 0).map(|m| m.latency.p99_ms).collect();
        let lat_max: Vec<f64> = metrics.iter().map(|m| m.latency.max_ms).collect();

        if !lat_p50.is_empty() {
            self.command_latency.p50_ms = lat_p50.iter().sum::<f64>() / lat_p50.len() as f64;
        }
        if !lat_p95.is_empty() {
            self.command_latency.p95_ms = lat_p95.iter().sum::<f64>() / lat_p95.len() as f64;
        }
        if !lat_p99.is_empty() {
            self.command_latency.p99_ms = lat_p99.iter().sum::<f64>() / lat_p99.len() as f64;
        }
        self.command_latency.max_ms = lat_max.into_iter().fold(0.0_f64, f64::max);
        self.command_latency.count = metrics.iter().map(|m| m.latency.count).sum();

        // 连接延迟
        self.connect_latency_ms = metrics
            .iter()
            .map(|m| m.connect_latency_ms)
            .fold(0.0_f64, f64::max);

        // CPU
        self.cpu.total_pct = metrics.iter().map(|m| m.cpu.total_pct).sum::<f64>() / count;
        self.cpu.user_pct = metrics.iter().map(|m| m.cpu.user_pct).sum::<f64>() / count;
        self.cpu.system_pct = metrics.iter().map(|m| m.cpu.system_pct).sum::<f64>() / count;

        // RSS
        self.peak_rss_bytes = metrics.iter().map(|m| m.rss_bytes).max().unwrap_or(0);
        self.avg_rss_bytes = (metrics.iter().map(|m| m.rss_bytes).sum::<u64>() as f64 / count) as u64;

        // Allocation
        self.peak_allocated_bytes = metrics.iter().map(|m| m.allocated_bytes).max().unwrap_or(0);

        // GC
        self.gc_pause_ms = metrics.iter().map(|m| m.gc_pause_ms).fold(0.0_f64, f64::max);
        self.gc_pauses = metrics.iter().map(|m| m.gc_pauses).sum();

        // 队列深度
        let eb: Vec<usize> = metrics.iter().map(|m| m.event_bus_depth).collect();
        self.avg_event_bus_depth = eb.iter().sum::<usize>() as f64 / count;
        self.peak_event_bus_depth = *eb.iter().max().unwrap_or(&0);

        let pq: Vec<usize> = metrics.iter().map(|m| m.persistence_queue_depth).collect();
        self.avg_persistence_queue_depth = pq.iter().sum::<usize>() as f64 / count;

        self.avg_send_queue_depth = metrics.iter().map(|m| m.send_queue_depth).sum::<usize>() as f64 / count;
        self.avg_session_command_queue_depth =
            metrics.iter().map(|m| m.session_command_queue_depth).sum::<usize>() as f64 / count;
        self.avg_room_mailbox_depth = metrics.iter().map(|m| m.room_mailbox_depth).sum::<usize>() as f64 / count;
        self.avg_plugin_event_queue_depth =
            metrics.iter().map(|m| m.plugin_event_queue_depth).sum::<usize>() as f64 / count;
        self.avg_telemetry_queue_depth = metrics.iter().map(|m| m.telemetry_queue_depth).sum::<usize>() as f64 / count;

        // 数据库
        let rows_ps: Vec<f64> = metrics.iter().map(|m| m.database.pg_rows_per_sec).collect();
        self.database.avg_rows_per_sec = rows_ps.iter().sum::<f64>() / count;
        self.database.peak_rows_per_sec = rows_ps.into_iter().fold(0.0_f64, f64::max);

        let txn_ps: Vec<f64> = metrics.iter().map(|m| m.database.pg_transactions_per_sec).collect();
        self.database.avg_transactions_per_sec = txn_ps.iter().sum::<f64>() / count;
        self.database.peak_transactions_per_sec = txn_ps.into_iter().fold(0.0_f64, f64::max);

        self.database.retries = metrics.iter().map(|m| m.database.retries).sum();

        // Touch/Judge
        self.touch_judge.touch_committed = metrics.iter().map(|m| m.touch_judge.touch_committed).sum();
        self.touch_judge.touch_dropped = metrics.iter().map(|m| m.touch_judge.touch_dropped).sum();
        self.touch_judge.judge_committed = metrics.iter().map(|m| m.touch_judge.judge_committed).sum();
        self.touch_judge.judge_dropped = metrics.iter().map(|m| m.touch_judge.judge_dropped).sum();
    }

    /// 标记为已完成并设置完成时间
    pub fn mark_finished(&mut self) {
        self.finished_at_ms = Self::now_ms();
        if self.summary.duration_secs == 0 {
            self.summary.duration_secs =
                ((self.finished_at_ms - self.started_at_ms) as u64) / 1000;
        }
    }

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    // ── 格式化输出 ──

    /// 格式化为人类可读文本（输出到 stderr）
    ///
    /// 包含以下板块：
    /// 环境 -> 配置 -> 总体结果 -> 场景结果 -> 延迟 -> 资源 -> 队列 -> 数据库 -> 正确性 -> 瓶颈 -> 产物
    pub fn format_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("PMP Benchmark: {}\n", self.title));
        out.push_str(&format!(
            "  Started: {}  Finished: {}\n\n",
            self.started_at_ms, self.finished_at_ms
        ));

        // ── 环境 ──
        out.push_str("环境\n");
        out.push_str(&self.environment.format_text());
        out.push('\n');

        // ── 配置 ──
        out.push_str("配置\n");
        out.push_str(&format!("  Mode: {}\n", self.config.mode.as_str()));
        out.push_str(&format!("  Scenario: {}\n", self.config.scenario.as_str()));
        out.push_str(&format!("  Seed: {}\n", self.config.seed));
        out.push_str(&format!("  Clients: {}\n", self.config.clients));
        out.push_str(&format!("  Rooms: {}\n", self.config.rooms));
        let plugins = if self.config.plugins.is_empty() {
            "none".to_string()
        } else {
            self.config.plugins.join(", ")
        };
        out.push_str(&format!("  Plugins: {}\n", plugins));
        out.push_str(&format!("  Preset: {}\n", self.config.preset.as_str()));
        out.push_str(&format!("  Duration: {}s\n", self.config.duration.as_secs()));
        out.push('\n');

        // ── 总体结果 ──
        out.push_str("总体结果\n");
        out.push_str(&format!("  Duration: {}s\n", self.summary.duration_secs));
        out.push_str(&format!(
            "  Commands: {} total, {:.0}/s avg, {:.0}/s peak\n",
            self.summary.total_commands,
            self.summary.avg_commands_per_sec,
            self.summary.peak_commands_per_sec
        ));
        out.push_str(&format!(
            "  Messages: {} total, {:.0}/s avg, {:.0}/s peak\n",
            self.summary.total_messages,
            self.summary.avg_messages_per_sec,
            self.summary.peak_messages_per_sec
        ));
        out.push_str(&format!("  Errors: {}\n", self.errors_total));
        out.push_str(&format!(
            "  Invariant violations: {}\n",
            self.invariant_violations
        ));
        if self.summary.aborted {
            out.push_str(&format!(
                "  Aborted: {}\n",
                self.summary.abort_reason.as_deref().unwrap_or("unknown")
            ));
        }
        out.push('\n');

        // ── 场景结果 ──
        if !self.scenario_results.is_empty() {
            out.push_str("场景结果\n");
            for (name, result) in &self.scenario_results {
                let status = if result.passed { "PASS" } else { "FAIL" };
                out.push_str(&format!("  {} [{}]\n", name, status));
                out.push_str(&format!(
                    "    commands/s: {:.0}  msgs/s: {:.0}  errors: {}\n",
                    result.commands_per_sec, result.messages_per_sec, result.errors
                ));
                out.push_str(&format!(
                    "    latency: p50={:.1}ms p95={:.1}ms p99={:.1}ms\n",
                    result.latency.p50_ms, result.latency.p95_ms, result.latency.p99_ms,
                ));
                if let Some(err) = &result.error {
                    out.push_str(&format!("    error: {}\n", err));
                }
            }
            out.push('\n');
        }

        // ── 延迟 ──
        out.push_str("延迟\n");
        out.push_str(&format!(
            "  Command: p50={:.1}ms  p95={:.1}ms  p99={:.1}ms  max={:.1}ms\n",
            self.command_latency.p50_ms,
            self.command_latency.p95_ms,
            self.command_latency.p99_ms,
            self.command_latency.max_ms
        ));
        out.push_str(&format!(
            "  Message: p50={:.1}ms  p95={:.1}ms  p99={:.1}ms  max={:.1}ms\n",
            self.message_latency.p50_ms,
            self.message_latency.p95_ms,
            self.message_latency.p99_ms,
            self.message_latency.max_ms
        ));
        if self.connect_latency_ms > 0.0 {
            out.push_str(&format!(
                "  Connect: {:.1}ms\n",
                self.connect_latency_ms
            ));
        }
        out.push('\n');

        // ── 资源 ──
        out.push_str("资源\n");
        out.push_str(&format!(
            "  CPU: {:.1}% user / {:.1}% sys / {:.1}% total\n",
            self.cpu.user_pct, self.cpu.system_pct, self.cpu.total_pct
        ));
        out.push_str(&format!(
            "  RSS: peak={}MB avg={}MB\n",
            self.peak_rss_bytes / 1024 / 1024,
            self.avg_rss_bytes / 1024 / 1024
        ));
        out.push_str(&format!(
            "  Allocation: peak={}MB\n",
            self.peak_allocated_bytes / 1024 / 1024
        ));
        if self.gc_pauses > 0 {
            out.push_str(&format!(
                "  GC: {} pauses, {:.1}ms total\n",
                self.gc_pauses, self.gc_pause_ms
            ));
        }
        out.push('\n');

        // ── 队列 ──
        out.push_str("队列\n");
        out.push_str(&format!(
            "  Session send: avg={:.1}\n",
            self.avg_send_queue_depth
        ));
        out.push_str(&format!(
            "  Session command: avg={:.1}\n",
            self.avg_session_command_queue_depth
        ));
        out.push_str(&format!(
            "  Room mailbox: avg={:.1}\n",
            self.avg_room_mailbox_depth
        ));
        out.push_str(&format!(
            "  Plugin event: avg={:.1}\n",
            self.avg_plugin_event_queue_depth
        ));
        out.push_str(&format!(
            "  Persistence: avg={:.1}  peak={}\n",
            self.avg_persistence_queue_depth, self.peak_event_bus_depth
        ));
        out.push_str(&format!(
            "  Telemetry: avg={:.1}\n",
            self.avg_telemetry_queue_depth
        ));
        out.push('\n');

        // ── 数据库 ──
        out.push_str("数据库\n");
        out.push_str(&format!(
            "  Transactions/s: avg={:.0}  peak={:.0}\n",
            self.database.avg_transactions_per_sec, self.database.peak_transactions_per_sec
        ));
        out.push_str(&format!(
            "  Rows/s: avg={:.0}  peak={:.0}  total={}\n",
            self.database.avg_rows_per_sec, self.database.peak_rows_per_sec, self.database.total_rows
        ));
        out.push_str(&format!(
            "  Touch committed: {}  dropped: {}\n",
            self.touch_judge.touch_committed, self.touch_judge.touch_dropped
        ));
        out.push_str(&format!(
            "  Judge committed: {}  dropped: {}\n",
            self.touch_judge.judge_committed, self.touch_judge.judge_dropped
        ));
        if self.database.retries > 0 {
            out.push_str(&format!("  Retries: {}\n", self.database.retries));
        }
        out.push('\n');

        // ── 产物 ──
        if !self.profile_reports.is_empty() {
            out.push_str("产物\n");
            for pr in &self.profile_reports {
                match pr.kind {
                    crate::benchmark::profile::ProfileKind::Cpu => {
                        if let Some(path) = &pr.pprof_path {
                            out.push_str(&format!("  cpu.pprof: {}\n", path));
                        }
                        if let Some(path) = &pr.flamegraph_path {
                            out.push_str(&format!("  cpu_flamegraph: {}\n", path));
                        }
                    }
                    crate::benchmark::profile::ProfileKind::Heap => {
                        if let Some(path) = &pr.heap_report_path {
                            out.push_str(&format!("  heap.pprof: {}\n", path));
                        }
                    }
                }
            }
            out.push('\n');
        }

        // ── 备注 ──
        if !self.notes.is_empty() {
            out.push_str("备注\n");
            for note in &self.notes {
                out.push_str(&format!("  - {}\n", note));
            }
        }

        out
    }

    /// 格式化为 JSON 字符串（输出到 stdout）
    ///
    /// 生成的 JSON 包含完整报告。输出应通过管道重定向：
    /// `pmp benchmark run ... > report.json`
    pub fn format_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// 格式化为 Markdown 文档（写入报告文件）
    ///
    /// 生成完整的 Markdown 格式报告，包含所有板块、表格和代码块。
    pub fn format_markdown(&self) -> String {
        let mut md = String::new();

        md.push_str(&format!("# PMP Benchmark: {}\n\n", self.title));
        md.push_str(&format!(
            "| | |\n|---|---|\n| Started | `{}` |\n| Finished | `{}` |\n\n",
            self.started_at_ms, self.finished_at_ms
        ));

        // ── 环境 ──
        md.push_str("## 环境 (Environment)\n\n");
        md.push_str("| 属性 | 值 |\n|------|-----|\n");
        md.push_str(&format!("| Version | `{}` |\n", self.environment.version));
        if self.environment.git_commit != "unknown" {
            let short = &self.environment.git_commit[..self.environment.git_commit.len().min(12)];
            md.push_str(&format!("| Commit | `{}` |\n", short));
        }
        md.push_str(&format!(
            "| CPU | {} cores (`{}`) |\n",
            self.environment.cpu_cores, self.environment.cpu_model
        ));
        let total_mb = self.environment.total_memory_bytes / 1024 / 1024;
        md.push_str(&format!("| Memory | {} MB |\n", total_mb));
        md.push_str(&format!("| OS | {}", self.environment.os_name));
        if !self.environment.os_version.is_empty() {
            md.push_str(&format!(" ({})", self.environment.os_version));
        }
        md.push_str(" |\n");
        md.push_str(&format!(
            "| Rust | `{}` (`{}`) |\n",
            self.environment.rust_version, self.environment.target_triple
        ));
        if let Some(pg_ver) = &self.environment.postgres_version {
            md.push_str(&format!("| PostgreSQL | `{}` |\n", pg_ver));
        }
        md.push('\n');

        // ── 配置 ──
        md.push_str("## 配置 (Configuration)\n\n");
        md.push_str("| 参数 | 值 |\n|------|-----|\n");
        md.push_str(&format!("| Mode | `{}` |\n", self.config.mode.as_str()));
        md.push_str(&format!("| Scenario | `{}` |\n", self.config.scenario.as_str()));
        md.push_str(&format!("| Seed | `{}` |\n", self.config.seed));
        md.push_str(&format!("| Clients | `{}` |\n", self.config.clients));
        md.push_str(&format!("| Rooms | `{}` |\n", self.config.rooms));
        let plugins = if self.config.plugins.is_empty() {
            "none".to_string()
        } else {
            self.config.plugins.join(", ")
        };
        md.push_str(&format!("| Plugins | `{}` |\n", plugins));
        md.push_str(&format!("| Preset | `{}` |\n", self.config.preset.as_str()));
        md.push_str(&format!("| Duration | {}s |\n", self.config.duration.as_secs()));
        md.push('\n');

        // ── 总体结果 ──
        md.push_str("## 总体结果 (Summary)\n\n");
        md.push_str("| 指标 | 值 |\n|------|-----|\n");
        md.push_str(&format!("| Duration | {}s |\n", self.summary.duration_secs));
        md.push_str(&format!(
            "| Commands/s | {:.0} avg / {:.0} peak |\n",
            self.summary.avg_commands_per_sec, self.summary.peak_commands_per_sec
        ));
        md.push_str(&format!(
            "| Messages/s | {:.0} avg / {:.0} peak |\n",
            self.summary.avg_messages_per_sec, self.summary.peak_messages_per_sec
        ));
        md.push_str(&format!("| Total commands | {} |\n", self.summary.total_commands));
        md.push_str(&format!("| Total messages | {} |\n", self.summary.total_messages));
        md.push_str(&format!("| Errors | {} |\n", self.errors_total));
        md.push_str(&format!("| Invariant violations | {} |\n", self.invariant_violations));
        if self.summary.aborted {
            md.push_str(&format!(
                "| Aborted | {} |\n",
                self.summary.abort_reason.as_deref().unwrap_or("yes")
            ));
        }
        md.push('\n');

        // ── 场景结果 ──
        if !self.scenario_results.is_empty() {
            md.push_str("## 场景结果 (Scenario Results)\n\n");
            md.push_str("| 场景 | 状态 | commands/s | messages/s | errors | p50 | p95 | p99 |\n");
            md.push_str("|------|------|-----------|-----------|------|-----|-----|-----|\n");
            for (name, result) in &self.scenario_results {
                let status = if result.passed { "PASS" } else { "FAIL" };
                md.push_str(&format!(
                    "| {} | {} | {:.0} | {:.0} | {} | {:.1}ms | {:.1}ms | {:.1}ms |\n",
                    name,
                    status,
                    result.commands_per_sec,
                    result.messages_per_sec,
                    result.errors,
                    result.latency.p50_ms,
                    result.latency.p95_ms,
                    result.latency.p99_ms,
                ));
            }
            md.push('\n');
        }

        // ── 延迟 ──
        md.push_str("## 延迟 (Latency)\n\n");
        md.push_str("| 分位 | 命令 | 消息 |\n|------|------|------|\n");
        md.push_str(&format!(
            "| p50 | {:.1}ms | {:.1}ms |\n",
            self.command_latency.p50_ms, self.message_latency.p50_ms
        ));
        md.push_str(&format!(
            "| p95 | {:.1}ms | {:.1}ms |\n",
            self.command_latency.p95_ms, self.message_latency.p95_ms
        ));
        md.push_str(&format!(
            "| p99 | {:.1}ms | {:.1}ms |\n",
            self.command_latency.p99_ms, self.message_latency.p99_ms
        ));
        md.push_str(&format!(
            "| max | {:.1}ms | {:.1}ms |\n",
            self.command_latency.max_ms, self.message_latency.max_ms
        ));
        if self.connect_latency_ms > 0.0 {
            md.push_str(&format!(
                "| connect | {:.1}ms | - |\n",
                self.connect_latency_ms
            ));
        }
        md.push('\n');

        // ── 资源 ──
        md.push_str("## 资源 (Resources)\n\n");
        md.push_str("| 指标 | 值 |\n|------|-----|\n");
        md.push_str(&format!(
            "| CPU (user) | {:.1}% |\n",
            self.cpu.user_pct
        ));
        md.push_str(&format!(
            "| CPU (system) | {:.1}% |\n",
            self.cpu.system_pct
        ));
        md.push_str(&format!(
            "| CPU (total) | {:.1}% |\n",
            self.cpu.total_pct
        ));
        md.push_str(&format!(
            "| RSS (peak) | {}MB |\n",
            self.peak_rss_bytes / 1024 / 1024
        ));
        md.push_str(&format!(
            "| RSS (avg) | {}MB |\n",
            self.avg_rss_bytes / 1024 / 1024
        ));
        md.push_str(&format!(
            "| Allocation (peak) | {}MB |\n",
            self.peak_allocated_bytes / 1024 / 1024
        ));
        if self.gc_pauses > 0 {
            md.push_str(&format!("| GC pauses | {} |\n", self.gc_pauses));
            md.push_str(&format!("| GC pause time | {:.1}ms |\n", self.gc_pause_ms));
        }
        md.push('\n');

        // ── 队列 ──
        md.push_str("## 队列 (Queue Depths)\n\n");
        md.push_str("| 队列 | 平均深度 |\n|------|---------|\n");
        md.push_str(&format!(
            "| Session send | {:.1} |\n",
            self.avg_send_queue_depth
        ));
        md.push_str(&format!(
            "| Session command | {:.1} |\n",
            self.avg_session_command_queue_depth
        ));
        md.push_str(&format!(
            "| Room mailbox | {:.1} |\n",
            self.avg_room_mailbox_depth
        ));
        md.push_str(&format!(
            "| Plugin event | {:.1} |\n",
            self.avg_plugin_event_queue_depth
        ));
        md.push_str(&format!(
            "| Persistence | {:.1} avg / {} peak |\n",
            self.avg_persistence_queue_depth, self.peak_event_bus_depth
        ));
        md.push_str(&format!(
            "| Telemetry | {:.1} |\n",
            self.avg_telemetry_queue_depth
        ));
        md.push('\n');

        // ── 数据库 ──
        md.push_str("## 数据库 (Database)\n\n");
        md.push_str("| 指标 | 值 |\n|------|-----|\n");
        md.push_str(&format!(
            "| Transactions/s | {:.0} avg / {:.0} peak |\n",
            self.database.avg_transactions_per_sec, self.database.peak_transactions_per_sec
        ));
        md.push_str(&format!(
            "| Rows/s | {:.0} avg / {:.0} peak |\n",
            self.database.avg_rows_per_sec, self.database.peak_rows_per_sec
        ));
        md.push_str(&format!(
            "| Touch committed | {} |\n",
            self.touch_judge.touch_committed
        ));
        md.push_str(&format!(
            "| Touch dropped | {} |\n",
            self.touch_judge.touch_dropped
        ));
        md.push_str(&format!(
            "| Judge committed | {} |\n",
            self.touch_judge.judge_committed
        ));
        md.push_str(&format!(
            "| Judge dropped | {} |\n",
            self.touch_judge.judge_dropped
        ));
        if self.database.retries > 0 {
            md.push_str(&format!("| Retries | {} |\n", self.database.retries));
        }
        md.push('\n');

        // ── 正确性 ──
        md.push_str("## 正确性 (Correctness)\n\n");
        if self.invariant_violations == 0 {
            md.push_str("All invariant checks passed.\n\n");
        } else {
            md.push_str(&format!(
                "**{} invariant violation(s) detected.**\n\n",
                self.invariant_violations
            ));
        }

        // ── 瓶颈 ──
        md.push_str("## 瓶颈 (Bottlenecks)\n\n");
        let mut bottlenecks: Vec<String> = Vec::new();
        if self.cpu.total_pct > 80.0 {
            bottlenecks.push(format!("High CPU usage ({:.1}%)", self.cpu.total_pct));
        }
        if self.command_latency.p99_ms > 1000.0 {
            bottlenecks.push(format!(
                "High p99 latency ({:.1}ms)",
                self.command_latency.p99_ms
            ));
        }
        if self.avg_event_bus_depth > 1_000.0 {
            bottlenecks.push(format!(
                "EventBus queue backlog (avg={:.0})",
                self.avg_event_bus_depth
            ));
        }
        if self.avg_persistence_queue_depth > 1_000.0 {
            bottlenecks.push(format!(
                "Persistence queue backlog (avg={:.0})",
                self.avg_persistence_queue_depth
            ));
        }
        if self.errors_total > 0 {
            bottlenecks.push(format!("{} error(s) occurred", self.errors_total));
        }
        if bottlenecks.is_empty() {
            md.push_str("No significant bottlenecks detected.\n\n");
        } else {
            for b in &bottlenecks {
                md.push_str(&format!("- {}\n", b));
            }
            md.push('\n');
        }

        // ── 产物 ──
        if !self.profile_reports.is_empty() {
            md.push_str("## 产物 (Artifacts)\n\n");
            md.push_str("| 类型 | 文件 |\n|------|------|\n");
            for pr in &self.profile_reports {
                match pr.kind {
                    crate::benchmark::profile::ProfileKind::Cpu => {
                        if let Some(path) = &pr.pprof_path {
                            md.push_str(&format!("| CPU .pprof | `{}` |\n", path));
                        }
                        if let Some(path) = &pr.flamegraph_path {
                            md.push_str(&format!("| CPU flamegraph | `{}` |\n", path));
                        }
                    }
                    crate::benchmark::profile::ProfileKind::Heap => {
                        if let Some(path) = &pr.heap_report_path {
                            md.push_str(&format!("| Heap dump | `{}` |\n", path));
                        }
                    }
                }
            }
            md.push('\n');
        }

        md
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::command::{BenchmarkPreset, BenchmarkRunMode, BenchmarkScenario};
    use crate::benchmark::config::BenchmarkConfig;
    use crate::benchmark::environment::EnvironmentSnapshot;

    fn make_test_report() -> BenchmarkReport {
        let env = EnvironmentSnapshot {
            version: "0.1.0".to_string(),
            git_commit: "abc123".to_string(),
            cpu_cores: 4,
            cpu_model: "Test CPU".to_string(),
            total_memory_bytes: 8_589_934_592, // 8GB
            available_memory_bytes: 4_294_967_296,
            os_name: "linux".to_string(),
            os_version: "Ubuntu 22.04".to_string(),
            kernel_version: "6.2.0".to_string(),
            hostname: "test-host".to_string(),
            rust_version: "1.82.0".to_string(),
            target_triple: "x86_64-linux".to_string(),
            postgres_version: Some("16.2".to_string()),
            captured_at_ms: 1_000_000,
        };
        let config = BenchmarkConfig::from_preset(BenchmarkPreset::Quick);
        BenchmarkReport::new("test", env, config)
    }

    #[test]
    fn report_creation_sets_title() {
        let report = make_test_report();
        assert_eq!(report.title, "test");
    }

    #[test]
    fn merge_metrics_populates_summary() {
        let mut report = make_test_report();
        report.summary.duration_secs = 60;

        let mut m1 = BenchmarkMetrics::new();
        m1.commands_per_sec = 100.0;
        m1.messages_per_sec = 200.0;
        m1.errors_total = 5;

        let mut m2 = BenchmarkMetrics::new();
        m2.commands_per_sec = 150.0;
        m2.messages_per_sec = 250.0;
        m2.errors_total = 3;

        report.merge_metrics(&[m1, m2]);
        assert!((report.summary.avg_commands_per_sec - 125.0).abs() < 0.1);
        assert!((report.summary.avg_messages_per_sec - 225.0).abs() < 0.1);
        assert_eq!(report.errors_total, 8);
    }

    #[test]
    fn format_text_contains_all_sections() {
        let report = make_test_report();
        let text = report.format_text();
        assert!(text.contains("PMP Benchmark"));
        assert!(text.contains("环境"));
        assert!(text.contains("配置"));
        assert!(text.contains("总体结果"));
        assert!(text.contains("延迟"));
        assert!(text.contains("资源"));
        assert!(text.contains("队列"));
        assert!(text.contains("数据库"));
    }

    #[test]
    fn format_json_is_valid() {
        let report = make_test_report();
        let json = report.format_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["title"], "test");
        assert!(parsed["environment"].is_object());
        assert!(parsed["config"].is_object());
    }

    #[test]
    fn format_markdown_includes_tables() {
        let report = make_test_report();
        let md = report.format_markdown();
        assert!(md.contains("PMP Benchmark"));
        assert!(md.contains("## 环境"));
        assert!(md.contains("## 配置"));
        assert!(md.contains("## 总体结果"));
        assert!(md.contains("## 延迟"));
        assert!(md.contains("## 资源"));
        assert!(md.contains("## 队列"));
        assert!(md.contains("## 数据库"));
        assert!(md.contains("## 正确性"));
        assert!(md.contains("## 瓶颈"));
    }

    #[test]
    fn mark_finished_sets_duration() {
        let mut report = make_test_report();
        report.mark_finished();
        // Duration should be >= 0
        assert!(report.finished_at_ms >= report.started_at_ms);
    }

    #[test]
    fn scenario_results_empty_by_default() {
        let report = make_test_report();
        assert!(report.scenario_results.is_empty());
    }

    #[test]
    fn merge_metrics_with_empty_list_does_nothing() {
        let mut report = make_test_report();
        let before = report.errors_total;
        report.merge_metrics(&[]);
        assert_eq!(report.errors_total, before);
    }
}

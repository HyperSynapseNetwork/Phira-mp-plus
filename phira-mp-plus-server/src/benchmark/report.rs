//! Benchmark 报告生成与格式化。

use super::environment::EnvironmentSnapshot;
use super::mode::ModeParams;
use serde::{Deserialize, Serialize};

/// Ramp 模式最终到达的负载水平。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RampReached {
    /// 触顶时的整体 CPU%（0-100）
    pub cpu_pct: f64,
    /// 触顶时的 RSS（字节）
    pub ram_bytes: u64,
    /// 触顶时的会话数
    pub sessions: u32,
    /// 触顶时的游玩房间数
    pub playing_rooms: u32,
    /// 触顶时的命令速率（commands/s）
    pub commands_per_sec: f64,
}

/// 基准测试报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReport {
    pub title: String,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    /// 运行环境信息。
    pub environment: EnvironmentSnapshot,
    /// 模式与参数。
    pub params: ModeParams,

    // ── 总体 ──
    pub summary: ReportSummary,

    // ── 负载水平 ──
    /// 峰值会话数
    pub peak_sessions: u32,
    /// 平均会话数
    pub avg_sessions: f64,
    /// 峰值同时游玩房间数
    pub peak_playing_rooms: u32,
    /// 平均同时游玩房间数
    pub avg_playing_rooms: f64,

    // ── 资源 ──
    /// 平均整体 CPU%（0-100）
    pub cpu_avg_pct: f64,
    /// 峰值整体 CPU%（0-100）
    pub cpu_peak_pct: f64,
    /// 平均 RSS（字节）
    pub ram_avg_bytes: u64,
    /// 峰值 RSS（字节）
    pub ram_peak_bytes: u64,

    /// Ramp 模式到达点（fixed 模式为 None）。
    pub ramp: Option<RampReached>,

    /// 总错误数
    pub errors_total: u64,
    /// 是否提前中止（x 键取消）
    pub aborted: bool,
    pub abort_reason: Option<String>,
    pub notes: Vec<String>,
}

/// 报告总体统计。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportSummary {
    /// 运行总时长（秒）
    pub duration_secs: u64,
    /// 总命令数
    pub total_commands: u64,
    /// 平均命令速率（commands/s）
    pub avg_commands_per_sec: f64,
    /// 峰值命令速率（commands/s）
    pub peak_commands_per_sec: f64,
}

impl BenchmarkReport {
    pub fn new(title: impl Into<String>, environment: EnvironmentSnapshot, params: ModeParams) -> Self {
        let now = Self::now_ms();
        Self {
            title: title.into(),
            started_at_ms: now,
            finished_at_ms: now,
            environment,
            params,
            summary: ReportSummary {
                duration_secs: 0,
                total_commands: 0,
                avg_commands_per_sec: 0.0,
                peak_commands_per_sec: 0.0,
            },
            peak_sessions: 0,
            avg_sessions: 0.0,
            peak_playing_rooms: 0,
            avg_playing_rooms: 0.0,
            cpu_avg_pct: 0.0,
            cpu_peak_pct: 0.0,
            ram_avg_bytes: 0,
            ram_peak_bytes: 0,
            ramp: None,
            errors_total: 0,
            aborted: false,
            abort_reason: None,
            notes: Vec::new(),
        }
    }

    pub fn mark_finished(&mut self) {
        self.finished_at_ms = Self::now_ms();
    }

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    fn format_duration(&self) -> String {
        let secs = self.summary.duration_secs;
        if secs >= 3600 {
            format!("{}h{}m{}s", secs / 3600, (secs % 3600) / 60, secs % 60)
        } else if secs >= 60 {
            format!("{}m{}s", secs / 60, secs % 60)
        } else {
            format!("{secs}s")
        }
    }

    fn format_mode_params(&self) -> String {
        let mut out = String::new();
        match self.params.mode {
            super::mode::BenchmarkMode::Fixed => {
                out.push_str(&format!(
                    "    模式: fixed  max_sessions={}  max_playing_rooms={}\n",
                    self.params.max_sessions, self.params.max_playing_rooms
                ));
            }
            super::mode::BenchmarkMode::Ramp => {
                let ram_mb = self.params.max_ram_bytes / 1024 / 1024;
                out.push_str(&format!(
                    "    模式: ramp  max_cpu={:.0}%  max_ram={}MB\n",
                    self.params.max_cpu_pct, ram_mb
                ));
            }
        }
        match self.params.duration {
            Some(d) => out.push_str(&format!(
                "    时长: {}s ({})\n",
                d.as_secs(),
                self.format_duration()
            )),
            None => out.push_str("    时长: 永久（直到 x 键结束）\n"),
        }
        out
    }

    /// 人类可读文本报告。
    pub fn format_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("══ {} ══\n", self.title));
        out.push_str(&format!("  开始: {}\n", self.started_at_ms));
        out.push_str(&format!("  结束: {}\n", self.finished_at_ms));
        out.push('\n');
        out.push_str("── 模式与参数 ──\n");
        out.push_str(&self.format_mode_params());
        out.push('\n');
        out.push_str("── 负载水平 ──\n");
        out.push_str(&format!(
            "    峰值会话数:   {}\n",
            self.peak_sessions
        ));
        out.push_str(&format!("    平均会话数:   {:.1}\n", self.avg_sessions));
        out.push_str(&format!(
            "    峰值游玩房间: {}\n",
            self.peak_playing_rooms
        ));
        out.push_str(&format!(
            "    平均游玩房间: {:.1}\n",
            self.avg_playing_rooms
        ));
        out.push('\n');
        out.push_str("── 资源 ──\n");
        out.push_str(&format!("    平均 CPU: {:.1}%\n", self.cpu_avg_pct));
        out.push_str(&format!("    峰值 CPU: {:.1}%\n", self.cpu_peak_pct));
        out.push_str(&format!(
            "    平均 RSS: {}MB\n",
            self.ram_avg_bytes / 1024 / 1024
        ));
        out.push_str(&format!(
            "    峰值 RSS: {}MB\n",
            self.ram_peak_bytes / 1024 / 1024
        ));
        out.push('\n');
        out.push_str("── 吞吐 ──\n");
        out.push_str(&format!(
            "    时长:     {}\n",
            self.format_duration()
        ));
        out.push_str(&format!(
            "    总命令:   {}\n",
            self.summary.total_commands
        ));
        out.push_str(&format!(
            "    平均速率: {:.0} commands/s\n",
            self.summary.avg_commands_per_sec
        ));
        out.push_str(&format!(
            "    峰值速率: {:.0} commands/s\n",
            self.summary.peak_commands_per_sec
        ));
        if let Some(ramp) = &self.ramp {
            out.push('\n');
            out.push_str("── 触顶到达点 ──\n");
            out.push_str(&format!(
                "    CPU: {:.1}%  RAM: {}MB\n",
                ramp.cpu_pct,
                ramp.ram_bytes / 1024 / 1024
            ));
            out.push_str(&format!(
                "    会话: {}  游玩房间: {}  速率: {:.0} cmd/s\n",
                ramp.sessions, ramp.playing_rooms, ramp.commands_per_sec
            ));
        }
        out.push('\n');
        out.push_str(&format!(
            "  错误: {}  {}（x 键取消）\n",
            self.errors_total,
            if self.aborted {
                "已中止"
            } else {
                "正常完成"
            }
        ));
        if let Some(reason) = &self.abort_reason {
            out.push_str(&format!("  中止原因: {reason}\n"));
        }
        if !self.notes.is_empty() {
            out.push('\n');
            out.push_str("── 备注 ──\n");
            for note in &self.notes {
                out.push_str(&format!("  · {note}\n"));
            }
        }
        out
    }

    /// JSON 序列化（含全部字段）。
    pub fn format_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Markdown 摘要。
    pub fn format_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("# {}\n\n", self.title));
        out.push_str("| 指标 | 值 |\n|------|----|\n");
        out.push_str(&format!(
            "| 模式 | {} |\n",
            self.params.mode.as_str()
        ));
        out.push_str(&format!(
            "| 时长 | {} |\n",
            self.format_duration()
        ));
        out.push_str(&format!("| 峰值会话 | {} |\n", self.peak_sessions));
        out.push_str(&format!(
            "| 峰值游玩房间 | {} |\n",
            self.peak_playing_rooms
        ));
        out.push_str(&format!("| 平均 CPU | {:.1}% |\n", self.cpu_avg_pct));
        out.push_str(&format!(
            "| 峰值 CPU | {:.1}% |\n",
            self.cpu_peak_pct
        ));
        out.push_str(&format!(
            "| 平均 RSS | {}MB |\n",
            self.ram_avg_bytes / 1024 / 1024
        ));
        out.push_str(&format!(
            "| 峰值 RSS | {}MB |\n",
            self.ram_peak_bytes / 1024 / 1024
        ));
        out.push_str(&format!(
            "| 平均速率 | {:.0} cmd/s |\n",
            self.summary.avg_commands_per_sec
        ));
        out.push_str(&format!("| 错误 | {} |\n", self.errors_total));
        out.push('\n');
        out
    }

    /// 报告摘要（CLI 结束语用，单行）。
    pub fn one_line_summary(&self) -> String {
        let mut s = format!(
            "benchmark {} · {} · 峰值 {} 会话 / {} 房间 · CPU {:.1}% / RSS {}MB · {:.0} cmd/s · 错误 {}",
            self.params.mode.as_str(),
            self.format_duration(),
            self.peak_sessions,
            self.peak_playing_rooms,
            self.cpu_peak_pct,
            self.ram_peak_bytes / 1024 / 1024,
            self.summary.avg_commands_per_sec,
            self.errors_total,
        );
        if let Some(ramp) = &self.ramp {
            s.push_str(&format!(
                " · 触顶 CPU {:.1}% / RAM {}MB",
                ramp.cpu_pct,
                ramp.ram_bytes / 1024 / 1024
            ));
        }
        s
    }

}

//! Runtime v2 benchmark report primitives.
//!
//! These types are intentionally lightweight text/report contracts. They do not
//! own runner logic; real, hybrid and simulation runners can gradually emit the
//! same shape without a big-bang benchmark rewrite.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchmarkMode {
    Simulation,
    Hybrid,
    Real,
}

impl BenchmarkMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Simulation => "simulation",
            Self::Hybrid => "hybrid",
            Self::Real => "real",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchmarkFailureSample {
    pub stage: String,
    pub message: String,
}

impl BenchmarkFailureSample {
    pub fn new(stage: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            stage: stage.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchmarkProbeStats {
    pub attempted: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub blocked: u64,
    pub skipped: u64,
}

impl BenchmarkProbeStats {
    pub fn record_success(&mut self) {
        self.attempted += 1;
        self.succeeded += 1;
    }

    pub fn record_failure(&mut self) {
        self.attempted += 1;
        self.failed += 1;
    }

    pub fn record_blocked(&mut self) {
        self.blocked += 1;
    }

    pub fn record_skipped(&mut self) {
        self.skipped += 1;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReport {
    pub mode: BenchmarkMode,
    pub title: String,
    pub duration_secs: u64,
    pub target_rooms: Option<usize>,
    pub active_clients: Option<usize>,
    pub rooms_created: Option<usize>,
    pub rooms_rebuilt: Option<usize>,
    pub users_joined: Option<usize>,
    pub operations: Option<u64>,
    pub failed_operations: Option<u64>,
    pub avg_latency_ms: Option<f64>,
    pub p99_latency_ms: Option<f64>,
    pub probes: BenchmarkProbeStats,
    pub failure_samples: Vec<BenchmarkFailureSample>,
    pub notes: Vec<String>,
}

impl BenchmarkReport {
    pub fn new(mode: BenchmarkMode, title: impl Into<String>, duration_secs: u64) -> Self {
        Self {
            mode,
            title: title.into(),
            duration_secs,
            target_rooms: None,
            active_clients: None,
            rooms_created: None,
            rooms_rebuilt: None,
            users_joined: None,
            operations: None,
            failed_operations: None,
            avg_latency_ms: None,
            p99_latency_ms: None,
            probes: BenchmarkProbeStats::default(),
            failure_samples: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn with_target_rooms(mut self, target_rooms: usize) -> Self {
        self.target_rooms = Some(target_rooms);
        self
    }

    pub fn add_failure_sample(&mut self, stage: impl Into<String>, message: impl Into<String>) {
        if self.failure_samples.len() < 8 {
            self.failure_samples.push(BenchmarkFailureSample::new(stage, message));
        }
    }

    pub fn add_note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    pub fn render_text(&self) -> String {
        let mut out = String::new();
        macro_rules! o { ($($t:tt)*) => { out.push_str(&format!($($t)*)); out.push('\n'); } }

        o!("  ◆ Benchmark report [{}]", self.mode.as_str());
        o!("  │ title={} duration={}s", self.title, self.duration_secs);
        if let Some(target_rooms) = self.target_rooms {
            o!("  │ target_rooms={target_rooms}");
        }
        if self.active_clients.is_some()
            || self.rooms_created.is_some()
            || self.rooms_rebuilt.is_some()
            || self.users_joined.is_some()
        {
            o!(
                "  │ clients={} rooms_created={} rooms_rebuilt={} users_joined={}",
                self.active_clients.unwrap_or(0),
                self.rooms_created.unwrap_or(0),
                self.rooms_rebuilt.unwrap_or(0),
                self.users_joined.unwrap_or(0),
            );
        }
        if self.operations.is_some() || self.failed_operations.is_some() {
            o!(
                "  │ operations={} failed_operations={}",
                self.operations.unwrap_or(0),
                self.failed_operations.unwrap_or(0),
            );
        }
        if self.avg_latency_ms.is_some() || self.p99_latency_ms.is_some() {
            o!(
                "  │ latency avg={:.2}ms p99={:.2}ms",
                self.avg_latency_ms.unwrap_or(0.0),
                self.p99_latency_ms.unwrap_or(0.0),
            );
        }
        if self.probes.attempted > 0 || self.probes.blocked > 0 || self.probes.skipped > 0 {
            o!(
                "  │ probes attempted={} ok={} failed={} blocked={} skipped={}",
                self.probes.attempted,
                self.probes.succeeded,
                self.probes.failed,
                self.probes.blocked,
                self.probes.skipped,
            );
        }
        if !self.failure_samples.is_empty() {
            o!("  ├─ failure samples");
            for sample in &self.failure_samples {
                o!("  │  · [{}] {}", sample.stage, sample.message);
            }
        }
        if !self.notes.is_empty() {
            o!("  ├─ notes");
            for note in &self.notes {
                o!("  │  · {note}");
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_stats_count_outcomes() {
        let mut stats = BenchmarkProbeStats::default();
        stats.record_success();
        stats.record_failure();
        stats.record_blocked();
        stats.record_skipped();
        assert_eq!(stats.attempted, 2);
        assert_eq!(stats.succeeded, 1);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.blocked, 1);
        assert_eq!(stats.skipped, 1);
    }

    #[test]
    fn report_limits_failure_samples() {
        let mut report = BenchmarkReport::new(BenchmarkMode::Hybrid, "hybrid", 30);
        for i in 0..16 {
            report.add_failure_sample("probe", format!("failure-{i}"));
        }
        assert_eq!(report.failure_samples.len(), 8);
        let rendered = report.render_text();
        assert!(rendered.contains("Benchmark report [hybrid]"));
        assert!(rendered.contains("failure-7"));
        assert!(!rendered.contains("failure-8"));
    }
}

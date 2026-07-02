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
            self.failure_samples
                .push(BenchmarkFailureSample::new(stage, message));
        }
    }

    pub fn add_note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    pub fn from_simulation_suite(report: &crate::simulation::SimulationSuiteReport) -> Self {
        let duration_secs = report.total_elapsed_secs.max(duration_secs_from_ms(
            report.started_at_ms,
            report.finished_at_ms,
        ));
        let mut benchmark = Self::new(
            BenchmarkMode::Simulation,
            format!("simulation suite {}", report.suite.as_str()),
            duration_secs,
        );
        benchmark.target_rooms = report.steps.iter().map(|step| step.rooms).max();
        benchmark.active_clients = report.steps.iter().map(|step| step.users).max();
        benchmark.operations = Some(report.workload_events);
        benchmark.failed_operations = Some(if report.aborted {
            report.total_steps.saturating_sub(report.completed_steps) as u64
        } else {
            0
        });
        if report.completed_steps > 0 {
            benchmark.probes.attempted = report.total_steps as u64;
            benchmark.probes.succeeded = report.completed_steps as u64;
            benchmark.probes.failed =
                report.total_steps.saturating_sub(report.completed_steps) as u64;
        } else if report.total_steps > 0 {
            benchmark.probes.attempted = report.total_steps as u64;
            benchmark.probes.failed = report.total_steps as u64;
        }
        if report.aborted {
            benchmark.add_failure_sample("simulation_suite", report.reason.clone());
        }
        for step in report.steps.iter().filter(|step| step.aborted) {
            benchmark.add_failure_sample(step.step_name.clone(), step.reason.clone());
        }
        benchmark.add_note(format!("suite_run_id={}", report.suite_run_id));
        benchmark.add_note(format!(
            "suite={} completed_steps={}/{} aborted={}",
            report.suite.as_str(),
            report.completed_steps,
            report.total_steps,
            report.aborted,
        ));
        benchmark.add_note(format!(
            "workload_events_per_sec={:.2} ticks={} chats={} ready={} touch_batches={} judge_batches={} round_results={}",
            report.workload_events_per_sec,
            report.totals.ticks,
            report.totals.chat_messages,
            report.totals.ready_events,
            report.totals.touch_batches,
            report.totals.judge_batches,
            report.totals.round_results,
        ));
        benchmark
            .add_note("simulation reports use shadow-world data and do not require Phira access");
        benchmark
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

fn duration_secs_from_ms(started_at_ms: i64, finished_at_ms: i64) -> u64 {
    if finished_at_ms <= started_at_ms {
        return 0;
    }
    ((finished_at_ms - started_at_ms) as u64).saturating_add(999) / 1000
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

    #[test]
    fn simulation_suite_report_maps_to_benchmark_report() {
        let mut counters = crate::simulation::SimulationCounters::default();
        counters.ticks = 10;
        counters.chat_messages = 3;
        counters.ready_events = 2;
        counters.touch_batches = 4;
        counters.judge_batches = 4;
        counters.round_results = 1;
        let step = crate::simulation::SimulationRunReport {
            suite_run_id: None,
            run_id: None,
            step_name: "smoke-balanced".to_string(),
            suite: Some(crate::simulation::SimulationSuite::Smoke),
            preset: crate::simulation::SimulationPreset::Baseline,
            scenario: crate::simulation::SimulationScenario::Balanced,
            users: 20,
            rooms: 5,
            duration_secs: 10,
            tick_interval_ms: 500,
            persist_every_ticks: 5,
            started_at_ms: Some(1_000),
            finished_at_ms: 11_000,
            elapsed_secs: 10,
            aborted: false,
            reason: "completed".to_string(),
            counters: counters.clone(),
            workload_events: counters.workload_events(),
            workload_events_per_sec: counters.workload_events() as f64 / 10.0,
        };
        let suite = crate::simulation::SimulationSuiteReport::new(
            uuid::Uuid::nil(),
            crate::simulation::SimulationSuite::Smoke,
            1_000,
            11_000,
            1,
            1,
            false,
            "completed",
            vec![step],
        );
        let report = BenchmarkReport::from_simulation_suite(&suite);
        assert_eq!(report.mode, BenchmarkMode::Simulation);
        assert_eq!(report.duration_secs, 10);
        assert_eq!(report.target_rooms, Some(5));
        assert_eq!(report.active_clients, Some(20));
        assert_eq!(report.operations, Some(counters.workload_events()));
        assert_eq!(report.failed_operations, Some(0));
        assert_eq!(report.probes.succeeded, 1);
        assert!(report
            .render_text()
            .contains("Benchmark report [simulation]"));
    }

    #[test]
    fn duration_ms_rounds_up_to_seconds() {
        assert_eq!(duration_secs_from_ms(1_000, 1_001), 1);
        assert_eq!(duration_secs_from_ms(1_000, 2_000), 1);
        assert_eq!(duration_secs_from_ms(1_000, 2_001), 2);
        assert_eq!(duration_secs_from_ms(2_000, 1_000), 0);
    }
}

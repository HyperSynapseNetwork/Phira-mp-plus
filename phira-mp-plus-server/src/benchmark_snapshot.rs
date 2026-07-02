//! Low-overhead in-memory benchmark report snapshots.
//!
//! This store is a diagnostics cache, not a product-level resource limiter. The
//! optimization is architectural: normal status/list views clone small digests,
//! while full `BenchmarkReport` payloads are cloned only when a caller asks for a
//! specific latest report. Durable benchmark history belongs in PersistenceWorker
//! rather than an ever-growing runtime Vec.

use crate::benchmark_report::{BenchmarkMode, BenchmarkReport};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};

const MIN_BENCHMARK_REPORT_HISTORY: usize = 1;
const MAX_BENCHMARK_REPORT_HISTORY: usize = 512;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReportDigest {
    pub seq: u64,
    pub at_ms: i64,
    pub mode: BenchmarkMode,
    pub title: String,
    pub duration_secs: u64,
    pub failed_operations: u64,
    pub probes_failed: u64,
    pub probes_blocked: u64,
    pub probes_succeeded: u64,
    pub failure_samples: usize,
    pub notes: usize,
}

impl BenchmarkReportDigest {
    fn from_report(seq: u64, at_ms: i64, report: &BenchmarkReport) -> Self {
        Self {
            seq,
            at_ms,
            mode: report.mode,
            title: report.title.clone(),
            duration_secs: report.duration_secs,
            failed_operations: report.failed_operations.unwrap_or(0),
            probes_failed: report.probes.failed,
            probes_blocked: report.probes.blocked,
            probes_succeeded: report.probes.succeeded,
            failure_samples: report.failure_samples.len(),
            notes: report.notes.len(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReportEntry {
    pub seq: u64,
    pub at_ms: i64,
    pub mode: BenchmarkMode,
    pub digest: BenchmarkReportDigest,
    pub report: BenchmarkReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReportModeSummary {
    pub mode: BenchmarkMode,
    pub count: u64,
    pub latest_seq: u64,
    pub latest: BenchmarkReportDigest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReportSnapshot {
    pub total: u64,
    /// Number of reports currently retained in the in-memory diagnostics cache.
    pub retained: usize,
    pub latest_by_mode: Vec<BenchmarkReportModeSummary>,
    pub recent: Vec<BenchmarkReportDigest>,
}

#[derive(Debug, Clone)]
struct StoredBenchmarkReport {
    digest: BenchmarkReportDigest,
    report: Arc<BenchmarkReport>,
}

impl StoredBenchmarkReport {
    fn to_entry(&self) -> BenchmarkReportEntry {
        BenchmarkReportEntry {
            seq: self.digest.seq,
            at_ms: self.digest.at_ms,
            mode: self.digest.mode,
            digest: self.digest.clone(),
            report: (*self.report).clone(),
        }
    }
}

#[derive(Debug, Default)]
struct BenchmarkReportStoreInner {
    recent: VecDeque<StoredBenchmarkReport>,
    latest_simulation: Option<StoredBenchmarkReport>,
    latest_hybrid: Option<StoredBenchmarkReport>,
    latest_real: Option<StoredBenchmarkReport>,
    simulation_count: u64,
    hybrid_count: u64,
    real_count: u64,
}

#[derive(Debug)]
pub struct BenchmarkReportStore {
    history: usize,
    seq: AtomicU64,
    inner: RwLock<BenchmarkReportStoreInner>,
}

impl Default for BenchmarkReportStore {
    fn default() -> Self {
        Self::new(crate::runtime_diagnostics::BENCHMARK_REPORT_HISTORY)
    }
}

impl BenchmarkReportStore {
    pub fn new(history: usize) -> Self {
        let history = sanitize_history(history);
        Self {
            history,
            seq: AtomicU64::new(0),
            inner: RwLock::new(BenchmarkReportStoreInner {
                recent: VecDeque::with_capacity(history),
                ..BenchmarkReportStoreInner::default()
            }),
        }
    }

    pub fn history(&self) -> usize {
        self.history
    }

    pub fn record(&self, report: BenchmarkReport) -> BenchmarkReportEntry {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let at_ms = now_ms();
        let digest = BenchmarkReportDigest::from_report(seq, at_ms, &report);
        let stored = StoredBenchmarkReport {
            digest,
            report: Arc::new(report),
        };
        let entry = stored.to_entry();

        if let Ok(mut inner) = self.inner.write() {
            match entry.mode {
                BenchmarkMode::Simulation => {
                    inner.simulation_count = inner.simulation_count.saturating_add(1);
                    inner.latest_simulation = Some(stored.clone());
                }
                BenchmarkMode::Hybrid => {
                    inner.hybrid_count = inner.hybrid_count.saturating_add(1);
                    inner.latest_hybrid = Some(stored.clone());
                }
                BenchmarkMode::Real => {
                    inner.real_count = inner.real_count.saturating_add(1);
                    inner.latest_real = Some(stored.clone());
                }
            }
            while inner.recent.len() >= self.history {
                inner.recent.pop_front();
            }
            inner.recent.push_back(stored);
        }

        entry
    }

    pub fn latest(&self, mode: BenchmarkMode) -> Option<BenchmarkReportEntry> {
        self.inner.read().ok().and_then(|inner| match mode {
            BenchmarkMode::Simulation => inner
                .latest_simulation
                .as_ref()
                .map(StoredBenchmarkReport::to_entry),
            BenchmarkMode::Hybrid => inner
                .latest_hybrid
                .as_ref()
                .map(StoredBenchmarkReport::to_entry),
            BenchmarkMode::Real => inner
                .latest_real
                .as_ref()
                .map(StoredBenchmarkReport::to_entry),
        })
    }

    pub fn snapshot(&self, limit: usize) -> BenchmarkReportSnapshot {
        let limit = limit.clamp(1, self.history.max(1));
        let total = self.seq.load(Ordering::Relaxed);
        let Ok(inner) = self.inner.read() else {
            return BenchmarkReportSnapshot {
                total,
                retained: 0,
                latest_by_mode: Vec::new(),
                recent: Vec::new(),
            };
        };

        let mut latest_by_mode = Vec::with_capacity(3);
        push_summary(
            &mut latest_by_mode,
            BenchmarkMode::Simulation,
            inner.simulation_count,
            inner.latest_simulation.as_ref(),
        );
        push_summary(
            &mut latest_by_mode,
            BenchmarkMode::Hybrid,
            inner.hybrid_count,
            inner.latest_hybrid.as_ref(),
        );
        push_summary(
            &mut latest_by_mode,
            BenchmarkMode::Real,
            inner.real_count,
            inner.latest_real.as_ref(),
        );

        let recent = inner
            .recent
            .iter()
            .rev()
            .take(limit)
            .map(|entry| entry.digest.clone())
            .collect::<Vec<_>>();

        BenchmarkReportSnapshot {
            total,
            retained: inner.recent.len(),
            latest_by_mode,
            recent,
        }
    }
}

fn push_summary(
    out: &mut Vec<BenchmarkReportModeSummary>,
    mode: BenchmarkMode,
    count: u64,
    entry: Option<&StoredBenchmarkReport>,
) {
    if let Some(entry) = entry {
        out.push(BenchmarkReportModeSummary {
            mode,
            count,
            latest_seq: entry.digest.seq,
            latest: entry.digest.clone(),
        });
    }
}

pub fn sanitize_history(history: usize) -> usize {
    history.clamp(MIN_BENCHMARK_REPORT_HISTORY, MAX_BENCHMARK_REPORT_HISTORY)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(mode: BenchmarkMode, title: &str) -> BenchmarkReport {
        let mut report = BenchmarkReport::new(mode, title, 1);
        report.failed_operations = Some(3);
        report.probes.record_failure();
        report.add_note("large details stay in the full report");
        report
    }

    #[test]
    fn store_keeps_digest_snapshots_and_lazy_full_reports() {
        let store = BenchmarkReportStore::new(2);
        store.record(report(BenchmarkMode::Simulation, "sim-1"));
        store.record(report(BenchmarkMode::Hybrid, "hybrid-1"));
        store.record(report(BenchmarkMode::Simulation, "sim-2"));

        let snapshot = store.snapshot(8);
        assert_eq!(snapshot.total, 3);
        assert_eq!(snapshot.retained, 2);
        assert_eq!(snapshot.recent.len(), 2);
        assert_eq!(snapshot.recent[0].title, "sim-2");
        assert_eq!(snapshot.recent[0].failed_operations, 3);
        assert_eq!(snapshot.recent[0].probes_failed, 1);

        let latest = store.latest(BenchmarkMode::Simulation).unwrap();
        assert_eq!(latest.report.title, "sim-2");
        assert_eq!(latest.report.notes.len(), 1);
        assert_eq!(
            store.latest(BenchmarkMode::Hybrid).unwrap().report.title,
            "hybrid-1"
        );
        assert!(store.latest(BenchmarkMode::Real).is_none());
    }

    #[test]
    fn history_window_is_sanitized() {
        assert_eq!(sanitize_history(0), MIN_BENCHMARK_REPORT_HISTORY);
        assert_eq!(
            sanitize_history(MAX_BENCHMARK_REPORT_HISTORY + 1),
            MAX_BENCHMARK_REPORT_HISTORY
        );
    }
}

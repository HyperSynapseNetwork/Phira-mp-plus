//! Bounded in-memory benchmark report snapshots.
//!
//! Runtime v2 keeps this store deliberately small: benchmark reports are useful
//! for CLI/TUI/Web readonly diagnostics, but they must not grow without bound on
//! long-running low-RAM servers. Persistence can mirror reports later; this store
//! is the fast, lock-light, bounded runtime view.

use crate::benchmark_report::{BenchmarkMode, BenchmarkReport};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

const MIN_BENCHMARK_REPORT_CAPACITY: usize = 1;
const DEFAULT_BENCHMARK_REPORT_CAPACITY: usize = 32;
const MAX_BENCHMARK_REPORT_CAPACITY: usize = 512;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReportEntry {
    pub seq: u64,
    pub at_ms: i64,
    pub mode: BenchmarkMode,
    pub report: BenchmarkReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReportModeSummary {
    pub mode: BenchmarkMode,
    pub count: u64,
    pub latest_seq: u64,
    pub latest: BenchmarkReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReportSnapshot {
    pub total: u64,
    pub capacity: usize,
    pub latest_by_mode: Vec<BenchmarkReportModeSummary>,
    pub recent: Vec<BenchmarkReportEntry>,
}

#[derive(Debug, Default)]
struct BenchmarkReportStoreInner {
    recent: VecDeque<BenchmarkReportEntry>,
    latest_simulation: Option<BenchmarkReportEntry>,
    latest_hybrid: Option<BenchmarkReportEntry>,
    latest_real: Option<BenchmarkReportEntry>,
    simulation_count: u64,
    hybrid_count: u64,
    real_count: u64,
}

#[derive(Debug)]
pub struct BenchmarkReportStore {
    capacity: usize,
    seq: AtomicU64,
    inner: Mutex<BenchmarkReportStoreInner>,
}

impl Default for BenchmarkReportStore {
    fn default() -> Self {
        Self::new(DEFAULT_BENCHMARK_REPORT_CAPACITY)
    }
}

impl BenchmarkReportStore {
    pub fn new(capacity: usize) -> Self {
        let capacity = sanitize_capacity(capacity);
        Self {
            capacity,
            seq: AtomicU64::new(0),
            inner: Mutex::new(BenchmarkReportStoreInner {
                recent: VecDeque::with_capacity(capacity),
                ..BenchmarkReportStoreInner::default()
            }),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn record(&self, report: BenchmarkReport) -> BenchmarkReportEntry {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let entry = BenchmarkReportEntry {
            seq,
            at_ms: now_ms(),
            mode: report.mode,
            report,
        };

        if let Ok(mut inner) = self.inner.lock() {
            match entry.mode {
                BenchmarkMode::Simulation => {
                    inner.simulation_count = inner.simulation_count.saturating_add(1);
                    inner.latest_simulation = Some(entry.clone());
                }
                BenchmarkMode::Hybrid => {
                    inner.hybrid_count = inner.hybrid_count.saturating_add(1);
                    inner.latest_hybrid = Some(entry.clone());
                }
                BenchmarkMode::Real => {
                    inner.real_count = inner.real_count.saturating_add(1);
                    inner.latest_real = Some(entry.clone());
                }
            }
            while inner.recent.len() >= self.capacity {
                inner.recent.pop_front();
            }
            inner.recent.push_back(entry.clone());
        }

        entry
    }

    pub fn latest(&self, mode: BenchmarkMode) -> Option<BenchmarkReportEntry> {
        self.inner.lock().ok().and_then(|inner| match mode {
            BenchmarkMode::Simulation => inner.latest_simulation.clone(),
            BenchmarkMode::Hybrid => inner.latest_hybrid.clone(),
            BenchmarkMode::Real => inner.latest_real.clone(),
        })
    }

    pub fn snapshot(&self, limit: usize) -> BenchmarkReportSnapshot {
        let limit = limit.clamp(1, self.capacity.max(1));
        let total = self.seq.load(Ordering::Relaxed);
        let Ok(inner) = self.inner.lock() else {
            return BenchmarkReportSnapshot {
                total,
                capacity: self.capacity,
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
            .cloned()
            .collect::<Vec<_>>();

        BenchmarkReportSnapshot {
            total,
            capacity: self.capacity,
            latest_by_mode,
            recent,
        }
    }
}

fn push_summary(
    out: &mut Vec<BenchmarkReportModeSummary>,
    mode: BenchmarkMode,
    count: u64,
    entry: Option<&BenchmarkReportEntry>,
) {
    if let Some(entry) = entry {
        out.push(BenchmarkReportModeSummary {
            mode,
            count,
            latest_seq: entry.seq,
            latest: entry.report.clone(),
        });
    }
}

pub fn sanitize_capacity(capacity: usize) -> usize {
    capacity.clamp(MIN_BENCHMARK_REPORT_CAPACITY, MAX_BENCHMARK_REPORT_CAPACITY)
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
        BenchmarkReport::new(mode, title, 1)
    }

    #[test]
    fn store_is_bounded_and_latest_is_by_mode() {
        let store = BenchmarkReportStore::new(2);
        store.record(report(BenchmarkMode::Simulation, "sim-1"));
        store.record(report(BenchmarkMode::Hybrid, "hybrid-1"));
        store.record(report(BenchmarkMode::Simulation, "sim-2"));

        let snapshot = store.snapshot(8);
        assert_eq!(snapshot.total, 3);
        assert_eq!(snapshot.capacity, 2);
        assert_eq!(snapshot.recent.len(), 2);
        assert_eq!(store.latest(BenchmarkMode::Simulation).unwrap().report.title, "sim-2");
        assert_eq!(store.latest(BenchmarkMode::Hybrid).unwrap().report.title, "hybrid-1");
        assert!(store.latest(BenchmarkMode::Real).is_none());
    }

    #[test]
    fn capacity_is_sanitized() {
        assert_eq!(sanitize_capacity(0), MIN_BENCHMARK_REPORT_CAPACITY);
        assert_eq!(sanitize_capacity(MAX_BENCHMARK_REPORT_CAPACITY + 1), MAX_BENCHMARK_REPORT_CAPACITY);
    }
}

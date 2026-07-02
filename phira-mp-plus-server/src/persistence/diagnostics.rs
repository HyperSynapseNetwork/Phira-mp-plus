//! Low-cost persistence diagnostics helpers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistencePipeline {
    EventMirror,
    TelemetryBatcher,
    BenchmarkReport,
    Simulation,
    DirectWrite,
}

impl PersistencePipeline {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EventMirror => "event_mirror",
            Self::TelemetryBatcher => "telemetry_batcher",
            Self::BenchmarkReport => "benchmark_report",
            Self::Simulation => "simulation",
            Self::DirectWrite => "direct_write",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistenceQueueHealth {
    Idle,
    Healthy,
    Backlogged,
    Dropping,
}

impl PersistenceQueueHealth {
    pub fn from_counts(pending: u64, capacity: usize, dropped: u64) -> Self {
        if dropped > 0 {
            return Self::Dropping;
        }
        if pending == 0 {
            return Self::Idle;
        }
        let capacity = capacity.max(1) as u64;
        if pending.saturating_mul(4) >= capacity.saturating_mul(3) {
            Self::Backlogged
        } else {
            Self::Healthy
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Healthy => "healthy",
            Self::Backlogged => "backlogged",
            Self::Dropping => "dropping",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_health_is_derived_from_observation_not_a_throttle() {
        assert_eq!(
            PersistenceQueueHealth::from_counts(0, 100, 0),
            PersistenceQueueHealth::Idle
        );
        assert_eq!(
            PersistenceQueueHealth::from_counts(10, 100, 0),
            PersistenceQueueHealth::Healthy
        );
        assert_eq!(
            PersistenceQueueHealth::from_counts(75, 100, 0),
            PersistenceQueueHealth::Backlogged
        );
        assert_eq!(
            PersistenceQueueHealth::from_counts(1, 100, 1),
            PersistenceQueueHealth::Dropping
        );
    }
}

//! Typed PersistenceWorker message envelope.

use crate::benchmark_report::{BenchmarkMode, BenchmarkReport};
use serde_json::Value;

#[derive(Debug, Clone)]
pub enum PersistenceEvent {
    RoomSnapshot {
        room_id: String,
        payload: Value,
        simulation: bool,
    },
    ServerEvent {
        kind: String,
        payload: Value,
        simulation: bool,
    },
    TouchBatch {
        round_id: String,
        user_id: i32,
        payload: Value,
        simulation: bool,
    },
    JudgeBatch {
        round_id: String,
        user_id: i32,
        payload: Value,
        simulation: bool,
    },
    BenchmarkReport {
        report: BenchmarkReport,
    },
    Flush,
    Shutdown,
}

impl PersistenceEvent {
    pub fn kind(&self) -> String {
        match self {
            Self::RoomSnapshot { .. } => "room_snapshot".to_string(),
            Self::ServerEvent { kind, .. } => kind.clone(),
            Self::TouchBatch { .. } => "touch_batch".to_string(),
            Self::JudgeBatch { .. } => "judge_batch".to_string(),
            Self::BenchmarkReport { .. } => "benchmark.completed".to_string(),
            Self::Flush => "flush".to_string(),
            Self::Shutdown => "shutdown".to_string(),
        }
    }

    pub fn is_simulation(&self) -> bool {
        match self {
            Self::RoomSnapshot { simulation, .. }
            | Self::ServerEvent { simulation, .. }
            | Self::TouchBatch { simulation, .. }
            | Self::JudgeBatch { simulation, .. } => *simulation,
            Self::BenchmarkReport { report } => report.mode == BenchmarkMode::Simulation,
            Self::Flush | Self::Shutdown => false,
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::RoomSnapshot {
                room_id,
                simulation,
                ..
            } => {
                format!("room_id={room_id} simulation={simulation}")
            }
            Self::ServerEvent {
                kind, simulation, ..
            } => {
                format!("kind={kind} simulation={simulation}")
            }
            Self::TouchBatch {
                round_id,
                user_id,
                simulation,
                ..
            } => {
                format!("round_id={round_id} user_id={user_id} simulation={simulation}")
            }
            Self::JudgeBatch {
                round_id,
                user_id,
                simulation,
                ..
            } => {
                format!("round_id={round_id} user_id={user_id} simulation={simulation}")
            }
            Self::BenchmarkReport { report } => format!(
                "mode={} title={} failed_operations={}",
                report.mode.as_str(),
                report.title.as_str(),
                report.failed_operations.unwrap_or(0),
            ),
            Self::Flush => "flush".to_string(),
            Self::Shutdown => "shutdown".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_report_is_simulation_when_report_mode_is_simulation() {
        let report = BenchmarkReport::new(BenchmarkMode::Simulation, "simulation", 1);
        let event = PersistenceEvent::BenchmarkReport { report };
        assert!(event.is_simulation());
        assert_eq!(event.kind(), "benchmark.completed");
    }
}

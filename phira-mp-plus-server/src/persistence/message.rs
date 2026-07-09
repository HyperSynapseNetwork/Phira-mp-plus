//! Typed PersistenceWorker message envelope.

use crate::benchmark_report::{BenchmarkMode, BenchmarkReport};
use serde_json::Value;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum PersistenceEvent {
    RoomSnapshot {
        room_id: String,
        payload: Arc<Value>,
        simulation: bool,
    },
    ServerEvent {
        kind: String,
        payload: Arc<Value>,
        simulation: bool,
    },
    TouchBatch {
        round_id: String,
        user_id: i32,
        payload: Arc<Value>,
        simulation: bool,
    },
    JudgeBatch {
        round_id: String,
        user_id: i32,
        payload: Arc<Value>,
        simulation: bool,
    },
    BenchmarkReport {
        report: BenchmarkReport,
    },
    /// User room history entry (per-join). Low-frequency production write.
    /// Migrated from server.rs direct db call as first PersistenceWorker path.
    UserRoomHistory {
        user_id: i32,
        room_id: String,
        room_uuid: String,
        joined_at: i64,
    },
    /// User online status (per-connect). Low-frequency production write.
    UserOnline {
        user_id: i32,
    },
    /// User offline status (per-disconnect). Low-frequency production write.
    UserOffline {
        user_id: i32,
    },
    /// User disconnect event (per-disconnect). Low-frequency production write.
    UserDisconnect {
        user_id: i32,
        user_name: String,
    },
    /// User seen timestamp (per-command). Higher-frequency production write.
    UserSeen {
        user_id: i32,
        user_name: String,
        language: String,
        ip: String,
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
            Self::UserRoomHistory { .. } => "user_room_history".to_string(),
            Self::UserOnline { .. } => "user_online".to_string(),
            Self::UserOffline { .. } => "user_offline".to_string(),
            Self::UserDisconnect { .. } => "user_disconnect".to_string(),
            Self::UserSeen { .. } => "user_seen".to_string(),
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
            Self::UserRoomHistory { .. } | Self::UserOnline { .. } | Self::UserOffline { .. } | Self::UserDisconnect { .. } | Self::UserSeen { .. } | Self::Flush | Self::Shutdown => false,
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
            Self::UserRoomHistory { user_id, room_id, .. } => {
                format!("user_id={user_id} room_id={room_id}")
            }
            Self::UserOnline { user_id } => format!("user_id={user_id}"),
            Self::UserOffline { user_id } => format!("user_id={user_id}"),
            Self::UserDisconnect { user_id, .. } => format!("user_id={user_id}"),
            Self::UserSeen { user_id, user_name, .. } => format!("user_id={user_id} user_name={user_name}"),
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

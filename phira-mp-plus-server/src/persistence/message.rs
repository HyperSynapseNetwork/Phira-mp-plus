//! Typed PersistenceWorker message envelope.

use crate::benchmark_report::{BenchmarkMode, BenchmarkReport};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// User identity/last-seen snapshot captured at authenticated session setup.
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
            Self::UserRoomHistory { .. }
            | Self::UserOnline { .. }
            | Self::UserOffline { .. }
            | Self::UserDisconnect { .. }
            | Self::UserSeen { .. }
            | Self::Flush
            | Self::Shutdown => false,
        }
    }

    /// Lossless JSON representation used by the local persistence dead-letter
    /// journal after all configured database retries are exhausted. Control
    /// markers are not persistence work and therefore return `None`.
    pub fn dead_letter_payload(&self) -> Option<Value> {
        match self {
            Self::RoomSnapshot {
                room_id,
                payload,
                simulation,
            } => Some(json!({
                "room_id": room_id,
                "payload": payload.as_ref(),
                "simulation": simulation,
            })),
            Self::ServerEvent {
                kind,
                payload,
                simulation,
            } => Some(json!({
                "kind": kind,
                "payload": payload.as_ref(),
                "simulation": simulation,
            })),
            Self::TouchBatch {
                round_id,
                user_id,
                payload,
                simulation,
            } => Some(json!({
                "round_id": round_id,
                "user_id": user_id,
                "payload": payload.as_ref(),
                "simulation": simulation,
            })),
            Self::JudgeBatch {
                round_id,
                user_id,
                payload,
                simulation,
            } => Some(json!({
                "round_id": round_id,
                "user_id": user_id,
                "payload": payload.as_ref(),
                "simulation": simulation,
            })),
            Self::BenchmarkReport { report } => Some(json!({ "report": report })),
            Self::UserRoomHistory {
                user_id,
                room_id,
                room_uuid,
                joined_at,
            } => Some(json!({
                "user_id": user_id,
                "room_id": room_id,
                "room_uuid": room_uuid,
                "joined_at": joined_at,
            })),
            Self::UserOnline { user_id } | Self::UserOffline { user_id } => {
                Some(json!({ "user_id": user_id }))
            }
            Self::UserDisconnect { user_id, user_name } => Some(json!({
                "user_id": user_id,
                "user_name": user_name,
            })),
            Self::UserSeen {
                user_id,
                user_name,
                language,
                ip,
            } => Some(json!({
                "user_id": user_id,
                "user_name": user_name,
                "language": language,
                "ip": ip,
            })),
            Self::Flush | Self::Shutdown => None,
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
            Self::UserRoomHistory {
                user_id, room_id, ..
            } => {
                format!("user_id={user_id} room_id={room_id}")
            }
            Self::UserOnline { user_id } => format!("user_id={user_id}"),
            Self::UserOffline { user_id } => format!("user_id={user_id}"),
            Self::UserDisconnect { user_id, .. } => format!("user_id={user_id}"),
            Self::UserSeen {
                user_id, user_name, ..
            } => format!("user_id={user_id} user_name={user_name}"),
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

    #[test]
    fn dead_letter_payload_preserves_user_seen_fields() {
        let event = PersistenceEvent::UserSeen {
            user_id: 42,
            user_name: "tester".to_string(),
            language: "zh-CN".to_string(),
            ip: "127.0.0.1".to_string(),
        };
        let payload = event
            .dead_letter_payload()
            .expect("data event must be serializable");
        assert_eq!(payload["user_id"], 42);
        assert_eq!(payload["user_name"], "tester");
        assert_eq!(payload["language"], "zh-CN");
        assert_eq!(payload["ip"], "127.0.0.1");
    }

    #[test]
    fn control_markers_are_not_written_to_dead_letter() {
        assert!(PersistenceEvent::Flush.dead_letter_payload().is_none());
        assert!(PersistenceEvent::Shutdown.dead_letter_payload().is_none());
    }
}

//! Typed PersistenceWorker message envelope.

#![allow(clippy::large_enum_variant)]

use crate::benchmark::report::BenchmarkReport;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PersistenceEvent {
    RoomSnapshot {
        room_id: String,
        payload: Arc<Value>,
    },
    ServerEvent {
        kind: String,
        payload: Arc<Value>,
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

    /// Lossless JSON representation used by the local persistence dead-letter
    /// journal after all configured database retries are exhausted. Control
    /// markers are not persistence work and therefore return `None`.
    pub fn dead_letter_payload(&self) -> Option<Value> {
        match self {
            Self::RoomSnapshot {
                room_id,
                payload,
            } => Some(json!({
                "room_id": room_id,
                "payload": payload.as_ref(),
            })),
            Self::ServerEvent {
                kind,
                payload,
            } => Some(json!({
                "kind": kind,
                "payload": payload.as_ref(),
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
                ..
            } => {
                format!("room_id={room_id}")
            }
            Self::ServerEvent {
                kind, ..
            } => {
                format!("kind={kind}")
            }
            Self::BenchmarkReport { report } => format!(
                "title={} errors={}",
                report.title,
                report.errors_total,
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
    use crate::benchmark::config::BenchmarkConfig;
    use crate::benchmark::environment::EnvironmentSnapshot;

    #[allow(dead_code)]
    fn make_benchmark_report() -> BenchmarkReport {
        let env = EnvironmentSnapshot {
            version: "0.1.0".to_string(),
            git_commit: "abc123".to_string(),
            cpu_cores: 4,
            cpu_model: "Test CPU".to_string(),
            total_memory_bytes: 8_589_934_592,
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
        let config = BenchmarkConfig::from_preset(crate::benchmark::command::BenchmarkPreset::Quick);
        BenchmarkReport::new("benchmark", env, config)
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

//! Typed PersistenceWorker message envelope.

#![allow(clippy::large_enum_variant)]

use crate::benchmark::report::BenchmarkReport;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

/// Outcome of enqueuing an event to the persistence worker.
///
/// - `Queued`: event admitted to both WAL and the in-memory queue (will be
///   processed on this runtime iteration).
/// - `WalOnly`: event admitted to WAL only (in-memory queue was full).  The
///   event is safely stored in WAL and will be recovered by the periodic WAL
///   recovery scanner (or on restart replay) — no data is lost.
/// - `AdmittedDegraded`: the WAL frame was durably fsync'd (the event is safe
///   and will be replayed/processed) but the instance marker could not be
///   updated.  The caller must NOT roll back the event; it only indicates the
///   deletion guard is temporarily degraded (P0-A).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionOutcome {
    Queued,
    WalOnly,
    AdmittedDegraded,
}

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
    /// User offline status (per-disconnect). Low-frequency production write.
    /// Carries server_instance_id + session_id so that old offline events
    /// cannot close a NEWER session's playtime after a same-instance reconnect.
    UserOffline {
        user_id: i32,
        #[serde(default)]
        server_instance_id: String,
        #[serde(default)]
        session_id: String,
        /// Time the disconnect occurred (ms since epoch), preserved through
        /// replay so delayed offline events use the original disconnect time.
        #[serde(default)]
        occurred_at: i64,
    },
    /// User disconnect event (per-disconnect). Low-frequency production write.
    /// Carries server_instance_id + session_id for the same generation
    /// protection.
    UserDisconnect {
        user_id: i32,
        user_name: String,
        #[serde(default)]
        server_instance_id: String,
        #[serde(default)]
        session_id: String,
        /// Time the disconnect occurred (ms since epoch).  Carried in the event
        /// so replay preserves the original disconnect time (P1).
        #[serde(default)]
        occurred_at: i64,
    },
    /// User authenticated event — merged UserSeen + UserOnline for atomic
    /// admission before the auth OK frame is sent.  Blocks until WAL enqueue
    /// succeeds so a user is never authenticated without being persisted.
    /// Contains event_id and session_id for idempotency — retry/replay cannot
    /// duplicate login_count.
    ///
    /// The `server_instance_id` is captured when the event is created (not at
    /// replay time) so that WAL/DLQ replay on a new instance preserves the
    /// original session ownership — preventing phantom online sessions after
    /// crash recovery.
    UserAuthenticated {
        event_id: String,
        session_id: String,
        user_id: i32,
        user_name: String,
        language: String,
        ip: String,
        connected_at: i64,
        #[serde(default)]
        server_instance_id: String,
    },
    /// Round result persistence (migrated from direct SQL in save_round_history).
    /// Contains a single player's result for a completed round. Low-frequency.
    RoundResult {
        round_uuid: String,
        room_id: String,
        result: crate::room::PlayResult,
    },
    /// Batch round completed event (all results in one atomic event).
    /// Replaces per-player RoundResult for atomic admission — partial
    /// admission is no longer possible.
    RoundCompleted {
        round_uuid: String,
        room_id: String,
        event_id: String,
        results: Vec<crate::room::PlayResult>,
        finished_at: i64,
        aborted_users: Vec<i32>,
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
            Self::UserOffline { .. } => "user_offline".to_string(),
            Self::UserDisconnect { .. } => "user_disconnect".to_string(),
            Self::UserAuthenticated { .. } => "user_authenticated".to_string(),
            Self::RoundResult { .. } => "round_result".to_string(),
            Self::RoundCompleted { .. } => "round_completed".to_string(),
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
            Self::UserOffline {
                user_id,
                server_instance_id,
                session_id,
                occurred_at,
            } => Some(json!({
                "user_id": user_id,
                "server_instance_id": server_instance_id,
                "session_id": session_id,
                "occurred_at": occurred_at,
            })),
            Self::UserDisconnect {
                user_id,
                user_name,
                server_instance_id,
                session_id,
                occurred_at,
            } => Some(json!({
                "user_id": user_id,
                "user_name": user_name,
                "server_instance_id": server_instance_id,
                "session_id": session_id,
                "occurred_at": occurred_at,
            })),
            Self::UserAuthenticated {
                event_id,
                session_id,
                user_id,
                user_name,
                language,
                ip,
                connected_at,
                server_instance_id,
            } => Some(json!({
                "event_id": event_id,
                "session_id": session_id,
                "user_id": user_id,
                "user_name": user_name,
                "language": language,
                "ip": ip,
                "connected_at": connected_at,
                "server_instance_id": server_instance_id,
            })),
            Self::Flush | Self::Shutdown => None,
            Self::RoundResult {
                round_uuid, room_id, result
            } => Some(json!({
                "round_uuid": round_uuid,
                "room_id": room_id,
                "result": result,
            })),
            Self::RoundCompleted {
                round_uuid,
                room_id,
                event_id,
                results,
                finished_at,
                aborted_users,
            } => Some(json!({
                "round_uuid": round_uuid,
                "room_id": room_id,
                "event_id": event_id,
                "results": results,
                "finished_at": finished_at,
                "aborted_users": aborted_users,
            })),
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
            Self::UserOffline { user_id, .. } => format!("user_id={user_id}"),
            Self::UserDisconnect { user_id, .. } => format!("user_id={user_id}"),
            Self::UserAuthenticated {
                event_id, user_id, user_name, ..
            } => format!("event_id={event_id} user_id={user_id} user_name={user_name}"),
            Self::RoundResult { round_uuid, .. } => {
                format!("round_uuid={round_uuid}")
            }
            Self::RoundCompleted { round_uuid, .. } => {
                format!("round_uuid={round_uuid}")
            }
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
    fn dead_letter_payload_preserves_user_offline_fields() {
        let event = PersistenceEvent::UserOffline {
            user_id: 42,
            server_instance_id: "inst-1".to_string(),
            session_id: "sess-1".to_string(),
            occurred_at: 1_000_000,
        };
        let payload = event
            .dead_letter_payload()
            .expect("data event must be serializable");
        assert_eq!(payload["user_id"], 42);
    }

    #[test]
    fn control_markers_are_not_written_to_dead_letter() {
        assert!(PersistenceEvent::Flush.dead_letter_payload().is_none());
        assert!(PersistenceEvent::Shutdown.dead_letter_payload().is_none());
    }
}

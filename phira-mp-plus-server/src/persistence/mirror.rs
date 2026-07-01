//! EventBus -> PersistenceWorker mirror.

use crate::persistence::message::PersistenceEvent;
use crate::persistence::worker::PersistenceWorker;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::warn;

pub fn spawn_event_bus_mirror(
    event_bus: Arc<crate::event_bus::EventBus>,
    worker: Arc<PersistenceWorker>,
) {
    let mut rx = event_bus.subscribe();
    let _handle = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let Some(persistence_event) = mirror_event_bus_event(&event) {
                        worker.record_mirrored_from_event_bus().await;
                        let _ = worker.enqueue(persistence_event).await;
                    } else {
                        worker.record_skipped_event_bus_event().await;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    worker.record_bridge_lagged(skipped).await;
                    warn!(skipped, "persistence worker event-bus mirror lagged");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

pub(crate) fn mirror_event_bus_event(event: &crate::event_bus::MpEvent) -> Option<PersistenceEvent> {
    use crate::event_bus::MpEvent;

    match event {
        MpEvent::UserConnected { user_id, .. } => server_event(event.kind(), json!({ "user_id": user_id }), false),
        MpEvent::UserDisconnected { user_id } => server_event(event.kind(), json!({ "user_id": user_id }), false),
        MpEvent::RoomCreated { room_id, room_uuid } => Some(PersistenceEvent::RoomSnapshot {
            room_id: room_id.to_string(),
            payload: json!({
                "event": event.kind(),
                "room_id": room_id.to_string(),
                "room_uuid": room_uuid.to_string(),
            }),
            simulation: false,
        }),
        MpEvent::RoomJoined { room_id, user_id } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "user_id": user_id }),
            false,
        ),
        MpEvent::RoomLeft { room_id, user_id } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "user_id": user_id }),
            false,
        ),
        MpEvent::RoomUpdated { room_id } => Some(PersistenceEvent::RoomSnapshot {
            room_id: room_id.to_string(),
            payload: json!({ "event": event.kind(), "room_id": room_id.to_string() }),
            simulation: false,
        }),
        MpEvent::RoomLocked { room_id, locked } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "locked": locked }),
            false,
        ),
        MpEvent::RoomCycled { room_id, cycle } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "cycle": cycle }),
            false,
        ),
        MpEvent::RoomStateChanged { room_id, state } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "state": state }),
            false,
        ),
        MpEvent::HostChanged { room_id, host } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "host": host }),
            false,
        ),
        MpEvent::ChartSelected { room_id, chart_id } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "chart_id": chart_id }),
            false,
        ),
        MpEvent::GameStarted { room_id, round_id } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "round_id": round_id }),
            false,
        ),
        MpEvent::PlayerReadyChanged { room_id, user_id, ready } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "user_id": user_id, "ready": ready }),
            false,
        ),
        // Production Touch/Judge persistence is staged from session telemetry with the full payload.
        MpEvent::TouchesReceived { .. } | MpEvent::JudgesReceived { .. } => None,
        MpEvent::RoundCompleted { room_id, round_id } => server_event(
            event.kind(),
            json!({ "room_id": room_id.to_string(), "round_id": round_id }),
            false,
        ),
        MpEvent::ChatMessage { room_id, user_id } => server_event(
            event.kind(),
            json!({ "room_id": room_id.as_ref().map(|id| id.to_string()), "user_id": user_id }),
            false,
        ),
        MpEvent::AdminCommandExecuted { user_id, command } => server_event(
            event.kind(),
            json!({ "user_id": user_id, "command": command }),
            false,
        ),
        MpEvent::SimulationStarted { run_id } => server_event(
            event.kind(),
            json!({ "run_id": run_id.to_string() }),
            true,
        ),
        MpEvent::SimulationStopped { run_id, reason } => server_event(
            event.kind(),
            json!({ "run_id": run_id.to_string(), "reason": reason }),
            true,
        ),
        MpEvent::PersistenceWritten { .. } => None,
        MpEvent::BenchmarkCompleted { report } => Some(PersistenceEvent::BenchmarkReport { report: report.clone() }),
        MpEvent::Custom { kind, payload } if kind.starts_with("simulation.") => simulation_custom_event(kind, payload),
        MpEvent::Custom { kind, payload } if kind == "room.command" || kind.starts_with("room.command.") => {
            server_event(kind, payload.clone(), false)
        }
        MpEvent::Custom { .. } => None,
    }
}

fn simulation_custom_event(kind: &str, payload: &Value) -> Option<PersistenceEvent> {
    match kind {
        "simulation.touch" => Some(PersistenceEvent::TouchBatch {
            round_id: payload
                .get("sample_round_id")
                .and_then(Value::as_str)
                .unwrap_or("simulation-touch")
                .to_string(),
            user_id: payload
                .get("sample_user_id")
                .and_then(Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
                .unwrap_or(0),
            payload: payload.clone(),
            simulation: true,
        }),
        "simulation.judge" => Some(PersistenceEvent::JudgeBatch {
            round_id: payload
                .get("sample_round_id")
                .and_then(Value::as_str)
                .unwrap_or("simulation-judge")
                .to_string(),
            user_id: payload
                .get("sample_user_id")
                .and_then(Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
                .unwrap_or(0),
            payload: payload.clone(),
            simulation: true,
        }),
        _ => server_event(kind, payload.clone(), true),
    }
}

fn server_event(kind: &str, payload: Value, simulation: bool) -> Option<PersistenceEvent> {
    Some(PersistenceEvent::ServerEvent {
        kind: kind.to_string(),
        payload,
        simulation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_completed_mirrors_as_typed_report() {
        let report = crate::benchmark_report::BenchmarkReport::new(
            crate::benchmark_report::BenchmarkMode::Hybrid,
            "hybrid",
            1,
        );
        let event = crate::event_bus::MpEvent::BenchmarkCompleted { report };
        let Some(PersistenceEvent::BenchmarkReport { report }) = mirror_event_bus_event(&event) else {
            panic!("benchmark.completed should mirror as typed PersistenceEvent::BenchmarkReport");
        };
        assert_eq!(report.mode, crate::benchmark_report::BenchmarkMode::Hybrid);
    }
}

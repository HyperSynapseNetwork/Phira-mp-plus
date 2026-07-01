//! Runtime v2 event bus.
//!
//! The current server still uses direct calls for most side effects. This bus is
//! introduced as an opt-in spine for new Runtime v2 features first, so old
//! room/session/plugin behavior can be migrated gradually instead of rewritten
//! in one risky patch.

use phira_mp_common::RoomId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::broadcast;
use uuid::Uuid;

const MAX_EVENT_TRACE: usize = 256;

#[derive(Debug, Clone)]
pub enum MpEvent {
    /// A normal game client authenticated or reconnected. Monitor/console sessions are excluded.
    UserConnected { user_id: i32 },
    UserDisconnected { user_id: i32 },
    RoomCreated { room_id: RoomId, room_uuid: Uuid },
    RoomJoined { room_id: RoomId, user_id: i32 },
    RoomLeft { room_id: RoomId, user_id: i32 },
    RoomUpdated { room_id: RoomId },
    RoomLocked { room_id: RoomId, locked: bool },
    RoomCycled { room_id: RoomId, cycle: bool },
    RoomStateChanged { room_id: RoomId, state: String },
    HostChanged { room_id: RoomId, host: Option<i32> },
    ChartSelected { room_id: RoomId, chart_id: i32 },
    GameStarted { room_id: RoomId, round_id: String },
    PlayerReadyChanged { room_id: RoomId, user_id: i32, ready: bool },
    TouchesReceived { room_id: RoomId, user_id: i32, count: usize },
    JudgesReceived { room_id: RoomId, user_id: i32, count: usize },
    RoundCompleted { room_id: RoomId, round_id: String },
    ChatMessage { room_id: Option<RoomId>, user_id: i32 },
    AdminCommandExecuted { user_id: Option<i32>, command: String },
    SimulationStarted { run_id: Uuid },
    SimulationStopped { run_id: Uuid, reason: String },
    PersistenceWritten { table: String, rows: usize },
    BenchmarkCompleted { report: crate::benchmark_report::BenchmarkReport },
    Custom { kind: String, payload: Value },
}

impl MpEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::UserConnected { .. } => "user.connected",
            Self::UserDisconnected { .. } => "user.disconnected",
            Self::RoomCreated { .. } => "room.created",
            Self::RoomJoined { .. } => "room.joined",
            Self::RoomLeft { .. } => "room.left",
            Self::RoomUpdated { .. } => "room.updated",
            Self::RoomLocked { .. } => "room.locked",
            Self::RoomCycled { .. } => "room.cycled",
            Self::RoomStateChanged { .. } => "room.state_changed",
            Self::HostChanged { .. } => "room.host_changed",
            Self::ChartSelected { .. } => "room.chart_selected",
            Self::GameStarted { .. } => "game.started",
            Self::PlayerReadyChanged { .. } => "player.ready_changed",
            Self::TouchesReceived { .. } => "touches.received",
            Self::JudgesReceived { .. } => "judges.received",
            Self::RoundCompleted { .. } => "round.completed",
            Self::ChatMessage { .. } => "chat.message",
            Self::AdminCommandExecuted { .. } => "admin.command_executed",
            Self::SimulationStarted { .. } => "simulation.started",
            Self::SimulationStopped { .. } => "simulation.stopped",
            Self::PersistenceWritten { .. } => "persistence.written",
            Self::BenchmarkCompleted { .. } => "benchmark.completed",
            Self::Custom { .. } => "custom",
        }
    }

    pub fn summary(&self) -> String {
        match self {
            Self::UserConnected { user_id } => format!("user_id={user_id}"),
            Self::UserDisconnected { user_id } => format!("user_id={user_id}"),
            Self::RoomCreated { room_id, room_uuid } => format!("room_id={room_id} uuid={room_uuid}"),
            Self::RoomJoined { room_id, user_id } => format!("room_id={room_id} user_id={user_id}"),
            Self::RoomLeft { room_id, user_id } => format!("room_id={room_id} user_id={user_id}"),
            Self::RoomUpdated { room_id } => format!("room_id={room_id}"),
            Self::RoomLocked { room_id, locked } => format!("room_id={room_id} locked={locked}"),
            Self::RoomCycled { room_id, cycle } => format!("room_id={room_id} cycle={cycle}"),
            Self::RoomStateChanged { room_id, state } => format!("room_id={room_id} state={state}"),
            Self::HostChanged { room_id, host } => format!("room_id={room_id} host={host:?}"),
            Self::ChartSelected { room_id, chart_id } => format!("room_id={room_id} chart_id={chart_id}"),
            Self::GameStarted { room_id, round_id } => format!("room_id={room_id} round_id={round_id}"),
            Self::PlayerReadyChanged { room_id, user_id, ready } => format!("room_id={room_id} user_id={user_id} ready={ready}"),
            Self::TouchesReceived { room_id, user_id, count } => format!("room_id={room_id} user_id={user_id} count={count}"),
            Self::JudgesReceived { room_id, user_id, count } => format!("room_id={room_id} user_id={user_id} count={count}"),
            Self::RoundCompleted { room_id, round_id } => format!("room_id={room_id} round_id={round_id}"),
            Self::ChatMessage { room_id, user_id } => format!("room_id={room_id:?} user_id={user_id}"),
            Self::AdminCommandExecuted { user_id, command } => format!("user_id={user_id:?} command={command}"),
            Self::SimulationStarted { run_id } => format!("run_id={run_id}"),
            Self::SimulationStopped { run_id, reason } => format!("run_id={run_id} reason={reason}"),
            Self::PersistenceWritten { table, rows } => format!("table={table} rows={rows}"),
            Self::BenchmarkCompleted { report } => format!(
                "mode={} title={} failed_operations={} probes_failed={} probes_blocked={}",
                report.mode.as_str(),
                report.title.as_str(),
                report.failed_operations.unwrap_or(0),
                report.probes.failed,
                report.probes.blocked,
            ),
            Self::Custom { kind, .. } => format!("kind={kind}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventTraceEntry {
    pub seq: u64,
    pub at_ms: i64,
    pub kind: String,
    pub subscribers: usize,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventKindStats {
    pub kind: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventBusStats {
    pub published: u64,
    pub delivered_total: u64,
    pub no_subscriber: u64,
    pub lagged_or_closed: u64,
    pub receiver_count: usize,
    pub by_kind: Vec<EventKindStats>,
    pub recent: Vec<EventTraceEntry>,
}

#[derive(Debug, Default)]
struct EventBusCounters {
    published: AtomicU64,
    delivered_total: AtomicU64,
    no_subscriber: AtomicU64,
    lagged_or_closed: AtomicU64,
}

#[derive(Debug)]
pub struct EventBus {
    tx: broadcast::Sender<MpEvent>,
    counters: EventBusCounters,
    recent: Mutex<VecDeque<EventTraceEntry>>,
    by_kind: Mutex<BTreeMap<String, u64>>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(16));
        Self {
            tx,
            counters: EventBusCounters::default(),
            recent: Mutex::new(VecDeque::with_capacity(MAX_EVENT_TRACE)),
            by_kind: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn publish(&self, event: MpEvent) -> usize {
        let seq = self.counters.published.fetch_add(1, Ordering::Relaxed) + 1;
        let kind = event.kind().to_string();
        let summary = event.summary();
        self.bump_kind(&kind);
        let subscribers = self.tx.receiver_count();
        let delivered = match self.tx.send(event) {
            Ok(delivered) => delivered,
            Err(_) => {
                self.counters.lagged_or_closed.fetch_add(1, Ordering::Relaxed);
                0
            }
        };
        if delivered == 0 {
            self.counters.no_subscriber.fetch_add(1, Ordering::Relaxed);
        }
        self.counters.delivered_total.fetch_add(delivered as u64, Ordering::Relaxed);
        self.push_trace(EventTraceEntry {
            seq,
            at_ms: now_ms(),
            kind,
            subscribers,
            summary,
        });
        delivered
    }

    pub fn subscribe(&self) -> broadcast::Receiver<MpEvent> {
        self.tx.subscribe()
    }

    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }

    pub fn stats(&self, limit: usize) -> EventBusStats {
        let limit = limit.clamp(1, MAX_EVENT_TRACE);
        let recent = self
            .recent
            .lock()
            .map(|trace| trace.iter().rev().take(limit).cloned().collect())
            .unwrap_or_default();
        let by_kind = self
            .by_kind
            .lock()
            .map(|stats| {
                stats
                    .iter()
                    .map(|(kind, count)| EventKindStats {
                        kind: kind.clone(),
                        count: *count,
                    })
                    .collect()
            })
            .unwrap_or_default();
        EventBusStats {
            published: self.counters.published.load(Ordering::Relaxed),
            delivered_total: self.counters.delivered_total.load(Ordering::Relaxed),
            no_subscriber: self.counters.no_subscriber.load(Ordering::Relaxed),
            lagged_or_closed: self.counters.lagged_or_closed.load(Ordering::Relaxed),
            receiver_count: self.receiver_count(),
            by_kind,
            recent,
        }
    }

    fn bump_kind(&self, kind: &str) {
        if let Ok(mut stats) = self.by_kind.lock() {
            *stats.entry(kind.to_string()).or_insert(0) += 1;
        }
    }

    fn push_trace(&self, entry: EventTraceEntry) {
        if let Ok(mut trace) = self.recent.lock() {
            if trace.len() >= MAX_EVENT_TRACE {
                trace.pop_front();
            }
            trace.push_back(entry);
        }
    }
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

    #[test]
    fn benchmark_completed_has_stable_kind_and_summary() {
        let mut report = crate::benchmark_report::BenchmarkReport::new(
            crate::benchmark_report::BenchmarkMode::Hybrid,
            "hybrid probe",
            30,
        );
        report.failed_operations = Some(2);
        report.probes.record_failure();
        report.probes.record_blocked();

        let event = MpEvent::BenchmarkCompleted { report };
        assert_eq!(event.kind(), "benchmark.completed");
        let summary = event.summary();
        assert!(summary.contains("mode=hybrid"));
        assert!(summary.contains("failed_operations=2"));
        assert!(summary.contains("probes_failed=1"));
        assert!(summary.contains("probes_blocked=1"));
    }
}

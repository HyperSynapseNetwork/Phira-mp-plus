//! Runtime v2 actor-model blueprint.
//!
//! This module is intentionally a blueprint/diagnostic layer first.  It gives the
//! project a concrete target for moving responsibilities out of `server.rs`,
//! `session.rs`, `room.rs` and `cli.rs` without forcing a risky rewrite of the
//! current protocol hot path.
//!
//! The migration rule is: mirror first, then route reads, then route writes, then
//! delete the old direct call once the actor path has been proven by tests and
//! simulation suites.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActorBoundary {
    pub name: String,
    pub responsibility: String,
    pub source_files: Vec<String>,
    pub status: ActorBoundaryStatus,
    pub next_step: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorBoundaryStatus {
    Planned,
    Mirrored,
    ReadRouted,
    WriteRouted,
    Owned,
}

impl ActorBoundaryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Mirrored => "mirrored",
            Self::ReadRouted => "read_routed",
            Self::WriteRouted => "write_routed",
            Self::Owned => "owned",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActorRuntimeStats {
    pub phase: String,
    pub web_management_api: String,
    pub rule: String,
    pub boundaries: Vec<ActorBoundary>,
}

#[derive(Debug)]
pub struct ActorRuntime {
    boundaries: RwLock<BTreeMap<String, ActorBoundary>>,
}

impl ActorRuntime {
    pub fn new_blueprint() -> Self {
        let mut boundaries = BTreeMap::new();
        for boundary in default_boundaries() {
            boundaries.insert(boundary.name.clone(), boundary);
        }
        Self {
            boundaries: RwLock::new(boundaries),
        }
    }

    pub async fn stats(&self) -> ActorRuntimeStats {
        let boundaries = self.boundaries.read().await.values().cloned().collect();
        ActorRuntimeStats {
            phase: "blueprint".to_string(),
            web_management_api: "out_of_scope".to_string(),
            rule: "mirror first; then route reads; then route writes; delete old direct calls last".to_string(),
            boundaries,
        }
    }

    pub async fn mark_status(&self, name: &str, status: ActorBoundaryStatus, next_step: impl Into<String>) {
        let mut boundaries = self.boundaries.write().await;
        if let Some(boundary) = boundaries.get_mut(name) {
            boundary.status = status;
            boundary.next_step = next_step.into();
        }
    }
}

fn default_boundaries() -> Vec<ActorBoundary> {
    vec![
        ActorBoundary {
            name: "server-supervisor".to_string(),
            responsibility: "Own process lifecycle, shutdown, listener startup, and actor supervision instead of accumulating feature glue in server.rs.".to_string(),
            source_files: vec!["server.rs".to_string()],
            status: ActorBoundaryStatus::Planned,
            next_step: "keep PlusServerState as compatibility facade; introduce actor handles behind it".to_string(),
        },
        ActorBoundary {
            name: "session-actor".to_string(),
            responsibility: "Own one client connection, authentication state, inbound command decoding, and outbound send queue.".to_string(),
            source_files: vec!["session.rs".to_string()],
            status: ActorBoundaryStatus::Planned,
            next_step: "extract command side effects behind message handlers; keep protocol behavior unchanged".to_string(),
        },
        ActorBoundary {
            name: "room-actor".to_string(),
            responsibility: "Own one room state machine, membership, host transfer, ready/start/play/result lifecycle, and telemetry fan-in.".to_string(),
            source_files: vec!["room.rs".to_string(), "room_actor/".to_string()],
            status: ActorBoundaryStatus::Planned,
            next_step: "RoomCommandGateway has been split into modules; next replace ad-hoc JSON command results with typed RoomCommandResult values".to_string(),
        },
        ActorBoundary {
            name: "persistence-actor".to_string(),
            responsibility: "Own database batching, backpressure, retry, shutdown flush, simulation isolation, and high-frequency Touch/Judge writes.".to_string(),
            source_files: vec!["db.rs".to_string(), "round_store.rs".to_string(), "persistence_worker.rs".to_string()],
            status: ActorBoundaryStatus::Mirrored,
            next_step: "move low-frequency writes from direct db.rs calls into PersistenceWorker before high-frequency production telemetry".to_string(),
        },
        ActorBoundary {
            name: "simulation-actor".to_string(),
            responsibility: "Own shadow users, shadow rooms, scenario suites, deterministic replay, and synthetic workload generation.".to_string(),
            source_files: vec!["simulation.rs".to_string()],
            status: ActorBoundaryStatus::Mirrored,
            next_step: "add suite report and regression snapshots before touching real Room writes".to_string(),
        },
        ActorBoundary {
            name: "plugin-actor".to_string(),
            responsibility: "Own plugin dispatch, capability checks, event fanout, and slow-plugin isolation.".to_string(),
            source_files: vec!["plugin.rs".to_string(), "wasm_host.rs".to_string(), "plugin_http.rs".to_string()],
            status: ActorBoundaryStatus::Planned,
            next_step: "route EventBus events into plugin dispatch without blocking Room/Session hot paths".to_string(),
        },
        ActorBoundary {
            name: "cli-actor".to_string(),
            responsibility: "Own CLI/TUI/admin-command execution through Command Registry without command logic spreading across cli.rs.".to_string(),
            source_files: vec!["cli.rs".to_string(), "cli_tui.rs".to_string(), "command_registry.rs".to_string()],
            status: ActorBoundaryStatus::Mirrored,
            next_step: "extract command handlers into registry-backed modules while keeping old aliases stable".to_string(),
        },
    ]
}

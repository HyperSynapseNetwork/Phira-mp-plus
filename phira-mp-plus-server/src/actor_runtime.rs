//! Runtime v2 actor model — 迁移状态记录。
//!
//! Actor 边界已定义，部分路径已完成迁移：
//!
//! | Actor | 状态 |
//! |-------|------|
//! | Session Actor | 全局单邮箱，所有命令通过 mailbox 路由；旧路径仍作 fallback |
//! | Room Actor | 每房间独立 mailbox，7 个房间命令已串行化；但状态仍由 `room.rs` + `RwLock` 拥有 |
//! | Persistence Actor | PersistenceWorker 已运行，但 DirectOnly/WorkerPreferred 双路径并存，直接写入仍是权威源 |
//! | Supervisor Actor | 邮箱骨架已建，状态查询和关闭通知通过 mailbox |
//! | Simulation Actor | 读取路径已迁移，事件总线命令待完成 |
//! | Plugin Actor | WIT 生命周期已接入，dispatch 待入 mailbox |
//! | CLI Actor | 顶层派发已拆分，具体命令体仍在 CliHandler |
//!
//! 迁移原则：镜像 → 路由读 → 路由写 → 删除旧直接调用。

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
            phase: "migration_in_progress".to_string(),
            web_management_api: "out_of_scope".to_string(),
            rule: "mirror first; then route reads; then route writes; delete old direct calls last"
                .to_string(),
            boundaries,
        }
    }

    pub async fn mark_status(
        &self,
        name: &str,
        status: ActorBoundaryStatus,
        next_step: impl Into<String>,
    ) {
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
            source_files: vec!["server.rs".to_string(), "supervisor_actor.rs".to_string()],
            status: ActorBoundaryStatus::ReadRouted,
            next_step: "Mailbox skeleton created (supervisor_actor.rs). Status queries and shutdown notifications routed through mailbox. Next: move server startup lifecycle events through supervisor.".to_string(),
        },
        ActorBoundary {
            name: "session-actor".to_string(),
            responsibility: "Own one client connection, authentication state, inbound command decoding, and outbound send queue.".to_string(),
            source_files: vec!["session.rs".to_string(), "session_dispatch.rs".to_string(), "session_auth.rs".to_string(), "session_room.rs".to_string(), "session_telemetry.rs".to_string(), "session_actor.rs".to_string()],
            status: ActorBoundaryStatus::WriteRouted,
            next_step: "Per-connection mailboxes implemented (Session::actor_tx, init_session_mailbox). Global static MAILBOX removed. Next: remove fallback paths for non-critical commands.".to_string(),
        },
        ActorBoundary {
            name: "room-actor".to_string(),
            responsibility: "Own one room state machine, membership, host transfer, ready/start/play/result lifecycle, and telemetry fan-in.".to_string(),
            source_files: vec!["room.rs".to_string(), "room_actor/".to_string()],
            status: ActorBoundaryStatus::WriteRouted,
            next_step: "7 room commands routed through per-room mailbox. Lock/cycle/host owned state as source of truth. RoomActor created per room with snapshot publishing. Room state still partially in room.rs+PlusServerState.rooms — next: move remaining state into actor.".to_string(),
        },
        ActorBoundary {
            name: "persistence-actor".to_string(),
            responsibility: "Own database batching, backpressure, retry, shutdown flush, simulation isolation, and high-frequency Touch/Judge writes.".to_string(),
            source_files: vec!["persistence/".to_string(), "db.rs".to_string(), "persistence_worker.rs".to_string()],
            status: ActorBoundaryStatus::WriteRouted,
            next_step: "Non-telemetry writes moved to Worker exclusive (RoomEvent, ServerEvent, UserOnline/Offline, UserDisconnect, UserSeen, UserRoomHistory, Chat). Touch/Judge telemetry remain direct writes for performance. Fallback paths removed.".to_string(),
        },
        ActorBoundary {
            name: "simulation-actor".to_string(),
            responsibility: "Own shadow users, shadow rooms, scenario suites, deterministic replay, and synthetic workload generation.".to_string(),
            source_files: vec!["simulation.rs".to_string()],
            status: ActorBoundaryStatus::ReadRouted,
            next_step: "suite reports and lifecycle contract tests added; next: typed EventBus commands for simulation control".to_string(),
        },
        ActorBoundary {
            name: "plugin-actor".to_string(),
            responsibility: "Own plugin dispatch, capability checks, event fanout, and slow-plugin isolation.".to_string(),
            source_files: vec!["plugin.rs".to_string(), "wasm_host.rs".to_string(), "plugin_http.rs".to_string(), "plugin_abi/".to_string(), "wit_host.rs".to_string()],
            status: ActorBoundaryStatus::ReadRouted,
            next_step: "WIT lifecycle wired, all host API methods implemented (with capability errors where appropriate). WASM integration tests added. Capability mapping contract tests added. JSON ABI removed. Next: move plugin dispatch through typed mailbox.".to_string(),
        },
        ActorBoundary {
            name: "cli-actor".to_string(),
            responsibility: "Own CLI/TUI/admin-command execution through Command Registry without command logic spreading across cli.rs.".to_string(),
            source_files: vec!["cli.rs".to_string(), "cli_tui.rs".to_string(), "command_registry.rs".to_string()],
            status: ActorBoundaryStatus::WriteRouted,
            next_step: "Top-level dispatch and command-family routing are split. Concrete command bodies still live on CliHandler. Next: move implementation bodies only when it removes real coupling; do not add more command-surface-only steps.".to_string(),
        },
    ]
}

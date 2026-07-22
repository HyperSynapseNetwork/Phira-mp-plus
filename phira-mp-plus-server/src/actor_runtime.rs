//! Runtime v2 actor model — 当前真实迁移状态。
//!
//! | Actor | 当前边界 |
//! |-------|----------|
//! | Session Actor | 每连接独立有界邮箱；缺失/关闭/超时均关闭连接，不再切换到直接执行；协议快路径仍直接处理 |
//! | Room Actor | 每房间有界邮箱串行化 9 个管理命令；控制面已收敛到一致性锁，成员/谱面/轮次仍未完全 Actor-owned |
//! | Persistence Actor | 有界背压、有限重试、确认式 flush/shutdown；无本地 WAL，崩溃级零丢失未承诺 |
//! | Supervisor Actor | 跟踪、观测、取消并等待具名后台任务；不对任意任务自动重启 |
//! | Simulation Actor | 查询与报告已接入；控制面仍未完全事件化 |
//! | Plugin Actor | 有界事件分发、capability、fuel/Store limiter 与超时隔离/单执行闸门已接入；仍为进程内隔离 |
//! | CLI Actor | 顶层派发已拆分，具体命令体仍保留在现有处理器 |
//!
//! PMP 内部 HTTP 仅作为受控网络中的兼容、诊断和插件接口。
//! 迁移原则：先建立单一写边界与可观测故障语义，再删除共享状态和兼容路径。

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
            phase: "hardening_in_progress".to_string(),
            web_management_api: "delegated_to_ppb".to_string(),
            rule: "single write boundary; bounded queues; explicit uncertainty; remove compatibility paths only after tests"
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
            responsibility: "Track process-lifetime background tasks and perform bounded, ordered shutdown.".to_string(),
            source_files: vec!["server.rs".to_string(), "supervisor_actor.rs".to_string(), "main.rs".to_string()],
            status: ActorBoundaryStatus::WriteRouted,
            next_step: "Define explicit restart policies only for tasks whose startup and side effects are idempotent; the current supervisor intentionally observes and cancels but does not blindly restart.".to_string(),
        },
        ActorBoundary {
            name: "session-actor".to_string(),
            responsibility: "Serialize one authenticated connection's ordered business commands while keeping protocol liveness paths separate.".to_string(),
            source_files: vec!["session.rs".to_string(), "session_dispatch.rs".to_string(), "session_auth.rs".to_string(), "session_room.rs".to_string(), "session_telemetry.rs".to_string(), "session_actor.rs".to_string()],
            status: ActorBoundaryStatus::WriteRouted,
            next_step: "Mailbox fallback is removed. Retain Ping/auth/telemetry fast paths only where ordering contracts are documented and add reconnect/failure-injection tests for mailbox loss.".to_string(),
        },
        ActorBoundary {
            name: "room-actor".to_string(),
            responsibility: "Serialize room management transitions and ultimately own membership, host, chart, round and lifecycle state.".to_string(),
            source_files: vec!["room.rs".to_string(), "room_actor/".to_string()],
            status: ActorBoundaryStatus::WriteRouted,
            next_step: "Nine management commands are mailbox-only and control fields share one generation-stamped snapshot. The structural next step is to move membership, chart, current round and lifecycle transitions into one actor-owned RoomState.".to_string(),
        },
        ActorBoundary {
            name: "persistence-actor".to_string(),
            responsibility: "Own reliable normal-operation writes, bounded backpressure, retry and acknowledged shutdown drain.".to_string(),
            source_files: vec!["persistence/".to_string(), "db.rs".to_string()],
            status: ActorBoundaryStatus::WriteRouted,
            next_step: "Add an append-only WAL or transactional outbox only if crash/power-loss durability is a product requirement; current guarantees cover accepted work during normal operation and graceful shutdown, not kill -9.".to_string(),
        },
        ActorBoundary {
            name: "simulation-actor".to_string(),
            responsibility: "Own shadow users, rooms, scenarios, deterministic replay and synthetic workload reporting.".to_string(),
            source_files: vec!["simulation.rs".to_string()],
            status: ActorBoundaryStatus::ReadRouted,
            next_step: "Move remaining simulation controls to typed commands and isolate simulation quotas from production session/room limits.".to_string(),
        },
        ActorBoundary {
            name: "plugin-actor".to_string(),
            responsibility: "Own bounded plugin event dispatch, capability enforcement and guest resource budgets.".to_string(),
            source_files: vec!["plugin.rs".to_string(), "wasm_host.rs".to_string(), "wasm_host_helpers.rs".to_string(), "wit_host.rs".to_string()],
            status: ActorBoundaryStatus::WriteRouted,
            next_step: "For fully untrusted plugins, move execution to a separate process/cgroup. In-process timeouts cannot forcibly terminate a blocking host function after it has entered native code.".to_string(),
        },
        ActorBoundary {
            name: "cli-actor".to_string(),
            responsibility: "Route administrative commands through the same state-transition boundaries used by sessions and plugins.".to_string(),
            source_files: vec!["cli.rs".to_string(), "cli_tui.rs".to_string(), "command_registry.rs".to_string()],
            status: ActorBoundaryStatus::WriteRouted,
            next_step: "Continue moving only mutating operations that still bypass canonical server/room gateways; avoid surface-only file splitting.".to_string(),
        },
    ]
}

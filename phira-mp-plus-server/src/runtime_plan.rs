//! Runtime v2 master workboard.
//!
//! This is intentionally code, not only documentation: CLI, TUI and diagnostic
//! APIs can query the same objective list so the project does not lose the
//! original Runtime v2 targets as the conversation and patch history grow.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeObjective {
    pub key: &'static str,
    pub title: &'static str,
    pub status: &'static str,
    pub priority: &'static str,
    pub next_step: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimePlanSnapshot {
    pub total: usize,
    pub active: usize,
    pub planned: usize,
    pub blocked: usize,
    pub done: usize,
    pub objectives: Vec<RuntimeObjective>,
    pub no_web_management_api: bool,
    pub final_architecture: &'static str,
}

#[derive(Debug)]
pub struct RuntimePlan {
    objectives: Vec<RuntimeObjective>,
}

impl RuntimePlan {
    pub fn master_plan() -> Self {
        Self {
            objectives: vec![
                RuntimeObjective {
                    key: "simulation",
                    title: "Simulation default benchmark path",
                    status: "active",
                    priority: "P0",
                    next_step: "Necessary as the default no-Phira stress path. Keep it active, but avoid new simulation features until suite reports, BenchmarkReport snapshots and cleanup hardening are validated under bounded-memory diagnostics.",
                },
                RuntimeObjective {
                    key: "benchmark-modes",
                    title: "Benchmark modes: simulation / hybrid / real",
                    status: "active",
                    priority: "P1",
                    next_step: "Useful, but not a core ownership migration. Keep simulation/hybrid/real on the shared BenchmarkReport path; do not add more modes until persisted report history and queue backpressure are proven.",
                },
                RuntimeObjective {
                    key: "low-overhead-diagnostics",
                    title: "Low CPU/RAM diagnostics architecture",
                    status: "active",
                    priority: "P0",
                    next_step: "Runtime v2 must reduce CPU/RAM by architecture: digest snapshots, lazy full-report cloning, bounded diagnostic windows and dirty-render/readonly snapshot caches. Do not expose crude resource throttles as product features.",
                },
                RuntimeObjective {
                    key: "actor-model",
                    title: "Actor model migration",
                    status: "active",
                    priority: "P0",
                    next_step: "Necessary. Room commands route through typed mailbox boundaries, but RoomActor still does NOT own room state and SessionActor does NOT own lifecycle. Next: move a small Room state slice into the mailbox worker before adding more facade commands.",
                },
                RuntimeObjective {
                    key: "touch-judge-persistence",
                    title: "Touches/Judges persistence without active monitor",
                    status: "done",
                    priority: "P0",
                    next_step: "Session telemetry persists without active monitor. Telemetry cutover modes: direct_only / worker_preferred. persistence/telemetry.rs extracted from db.rs. No further work planned.",
                },
                RuntimeObjective {
                    key: "phira-http",
                    title: "Unified Phira HTTP RetryClient",
                    status: "done",
                    priority: "P0",
                    next_step: "fetch_phira_user_name and fetch_phira_chart migrated from bare reqwest to PhiraRetryClient. No bare reqwest remains outside phira_client.rs and wasm_host.rs. Allowed-server-line patterns removed from phira_http_contracts. Simulation defaults never touch Phira.",
                },
                RuntimeObjective {
                    key: "persistence-worker",
                    title: "Persistence Worker ownership",
                    status: "active",
                    priority: "P1",
                    next_step: "PersistenceWorker split into message/stats/mirror/pipeline/worker modules. db.rs persistence extracted into persistence/{benchmark,telemetry,simulation,events,users,admin,queries}.rs (15 modules). TelemetryBatcher in telemetry.rs + telemetry_batcher.rs facade. db.rs: 1863→1072 lines. Direct async writes still through DbManager spawn pattern; not all writes go through PersistenceWorker.",
                },
                RuntimeObjective {
                    key: "eventbus",
                    title: "EventBus as runtime spine",
                    status: "done",
                    priority: "P1",
                    next_step: "benchmark.completed typed, cached, mirrored through PersistenceWorker. CLI/Web readonly history available.",
                },
                RuntimeObjective {
                    key: "plugin-abi-v2",
                    title: "Typed WASM plugin ABI",
                    status: "active",
                    priority: "P1",
                    next_step: "call_on_event and call_api are wired with full PluginEvent→WIT variant conversion. All WIT host APIs are real implementations with capability enforcement: room-mgmt requires room.manage, admin writes require admin, config.set requires config, simulation control requires admin. Persistence stubs return capability errors. Remaining: contract tests for every host API method, and capability-integration tests for write gating.",
                },
                RuntimeObjective {
                    key: "test-coverage",
                    title: "Unit and integration test coverage",
                    status: "active",
                    priority: "P1",
                    next_step: "102 unit tests pass (up from 97). Capability model contract tests added. WIT lifecycle contract tests added (JSON round-trip, variant coverage, public API checks). Lock exclusivity test added. Simulation isolation tests added (5 new). Remaining: integration tests for each WIT host API method in a running-server context. Do not hard-code test totals in the plan.",
                },
                RuntimeObjective {
                    key: "technical-debt-triage",
                    title: "Source debt-comment backlog discipline",
                    status: "active",
                    priority: "P1",
                    next_step: "All WIT host 'not yet implemented' stubs have been resolved — either implemented with real capability checks or replaced with explicit capability errors. No TODO/FIXME markers remain in production code. Keep scanning new code for unchecked markers.",
                },
                RuntimeObjective {
                    key: "tui-v2",
                    title: "TUI v2 observability panels",
                    status: "planned",
                    priority: "P3",
                    next_step: "Defer. Useful only after Actor/Persistence/Benchmark signals stabilize; do not start a TUI panel pass while ownership boundaries are still moving.",
                },
                RuntimeObjective {
                    key: "web-management-api",
                    title: "Privileged Web management API",
                    status: "blocked",
                    priority: "never",
                    next_step: "Do not implement. Web remains read-only diagnostics unless explicitly reversed by project policy.",
                },
            ],
        }
    }

    pub fn snapshot(&self) -> RuntimePlanSnapshot {
        let mut active = 0;
        let mut planned = 0;
        let mut blocked = 0;
        let mut done = 0;
        for item in &self.objectives {
            match item.status {
                "active" => active += 1,
                "planned" => planned += 1,
                "blocked" => blocked += 1,
                "done" => done += 1,
                _ => {}
            }
        }
        RuntimePlanSnapshot {
            total: self.objectives.len(),
            active,
            planned,
            blocked,
            done,
            objectives: self.objectives.clone(),
            no_web_management_api: true,
            final_architecture: "actor_model",
        }
    }
}

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
                    status: "done",
                    priority: "P0",
                    next_step: "Architectural guardrail retained by contract tests (18 simulation_contracts). Simulation is the default no-Phira stress path with deterministic seed, shadow-world isolation, suite reports, cleanup hardening. No active development planned.",
                },
                RuntimeObjective {
                    key: "benchmark-modes",
                    title: "Benchmark modes: simulation / hybrid / real",
                    status: "done",
                    priority: "P1",
                    next_step: "Documentation/housekeeping only. Modes share BenchmarkReport path. Hybrid/real require explicit opt-in. No active development planned.",
                },
                RuntimeObjective {
                    key: "low-overhead-diagnostics",
                    title: "Low CPU/RAM diagnostics architecture",
                    status: "done",
                    priority: "P0",
                    next_step: "Architectural guardrail retained by contract tests. Bounded diagnostic windows, digest snapshots, lazy full-report cloning all in place. No active resource-throttle work planned.",
                },
                RuntimeObjective {
                    key: "actor-model",
                    title: "Actor model migration",
                    status: "active",
                    priority: "P0",
                    next_step: "RoomActor: all 7 commands mailboxed (room_mailbox_only), lock/cycle/host owned-tracked. Owned. SessionActor: 12 command variants WriteRouted through mailbox. Remaining: server-supervisor Mirrored, persistence-actor ReadRouted, plugin-actor ReadRouted.",
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
                    status: "done",
                    priority: "P1",
                    next_step: "6/7 production writes migrated through PersistenceWorker with DB fallback. round_store round_data stays direct (permanent — high-frequency Touch/Judge data bypasses worker deliberately; dual-write via DirectOnly/WorkerPreferred telemetry cutover provides sufficient safety). ExtensionManager has worker reference. Telemetry cutover modes in place. Write-path audit contract tests added.",
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
                    next_step: "All host APIs implemented with capability enforcement. WIT lifecycle wired (init/cleanup/on-event/on-api). WitPluginHost decoupled from PlusServerState via WitHostContext. SSE stream registration + event translation (sse.register_stream). Capability model contract tests added (23 wit_abi_contracts). WASM integration tests added (lifecycle + host API). SDK provides wit_bindgen! macro. JSON ABI removed. MIGRATION_PHASE 3.",
                },
                RuntimeObjective {
                    key: "test-coverage",
                    title: "Unit and integration test coverage",
                    status: "active",
                    priority: "P1",
                    next_step: "Contract tests exist for: runtime objectives, WIT ABI, docs, persistence, telemetry cutover, phira-http, simulation. WASM lifecycle and host API integration tests added. Capability model contract tests added. Server-supervisor Mirrored and persistence-actor ReadRouted are the remaining untested boundaries. Do not hard-code test counts in the plan.",
                },
                RuntimeObjective {
                    key: "technical-debt-triage",
                    title: "Source debt-comment backlog discipline",
                    status: "done",
                    priority: "P1",
                    next_step: "No TODO/FIXME markers remain in production code. Keep scanning new code for unchecked markers.",
                },
                RuntimeObjective {
                    key: "step-38-closure-gate",
                    title: "Step 38: Runtime v2 closure gate",
                    status: "active",
                    priority: "P0",
                    next_step: "Closure pending: actor-model (server-supervisor Mirrored, persistence-actor ReadRouted, plugin-actor ReadRouted) and test-coverage (missing actor boundary integration tests) remain active. All other objectives done. workspace tests pass.",
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

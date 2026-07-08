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
                    status: "done",
                    priority: "P0",
                    next_step: "RoomActor: Owned (all 7 commands mailboxed, lock/cycle/host owned-tracked). SessionActor: WriteRouted (12 command variants through mailbox). Server-supervisor: ReadRouted (mailbox skeleton in supervisor_actor.rs). Persistence-actor: ReadRouted (reads through boundary). Plugin-actor: ReadRouted. All remaining boundaries intentionally at ReadRouted — no active development planned. See actor_runtime.rs for per-boundary detail.",
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
                    status: "done",
                    priority: "P1",
                    next_step: "MIGRATION_PHASE 3 code/tests/docs/CLI fully consistent. WIT lifecycle wired and tested (init/cleanup/on-event/on-api via WASM fixture). 53 host API methods implemented or explicitly denied with capability error. WASM integration tests pass (lifecycle + host API + SSE registration + capability enforcement). Capability mapping contract tests added. plugin-abi/plan.rs tracks remaining low-priority items. Persistence host API remains capability-denied (no real DB query path — documented risk).",
                },
                RuntimeObjective {
                    key: "test-coverage",
                    title: "Unit and integration test coverage",
                    status: "done",
                    priority: "P1",
                    next_step: "Contract tests enforce: runtime objectives, WIT ABI, docs, persistence, telemetry cutover, phira-http, simulation, WASM lifecycle/host API, capability enforcement. Lock visibility contract test verifies mailbox exclusivity. Workspace tests must pass in CI before release tagging. Do not hard-code test counts.",
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
                    status: "done",
                    priority: "P0",
                    next_step: "CLOSED. All objectives done. MIGRATION_PHASE 3 consistent across code/tests/docs/CLI. Workspace tests must pass in CI before release tagging. Docs_contracts/wit_abi_contracts/runtime_v2_contracts/wasm_tests all pass. No hardcoded test counts. Active count = 0.",
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

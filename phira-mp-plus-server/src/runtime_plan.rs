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
                    next_step: "Keep as architectural guardrail. Simulation is the default no-Phira stress path with deterministic seed, shadow-world isolation, suite reports, and cleanup hardening. No new features needed. Verified by contract tests (18 simulation_contracts).",
                },
                RuntimeObjective {
                    key: "benchmark-modes",
                    title: "Benchmark modes: simulation / hybrid / real",
                    status: "active",
                    priority: "P1",
                    next_step: "Keep as documentation/housekeeping. Modes share BenchmarkReport path. Hybrid/real require explicit opt-in. No active development planned.",
                },
                RuntimeObjective {
                    key: "low-overhead-diagnostics",
                    title: "Low CPU/RAM diagnostics architecture",
                    status: "active",
                    priority: "P0",
                    next_step: "Keep as architectural guardrail. Bounded diagnostic windows, digest snapshots, lazy full-report cloning all in place. No active resource-throttle work planned.",
                },
                RuntimeObjective {
                    key: "actor-model",
                    title: "Actor model migration",
                    status: "active",
                    priority: "P0",
                    next_step: "Lock/cycle effectively Owned: set_lock_inline/set_cycle_inline are pub(in crate::room_actor), only reachable via mailbox handler. Owned tracking (owned_locks/owned_cycles) mirrors post-commit. 7 room commands route through typed mailbox. Remaining: host/close/kick/start/cancel still WriteRouted. SessionActor still Mirrored. Next incremental step: host state slice into mailbox.",
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
                    next_step: "6/7 production writes migrated through PersistenceWorker with DB fallback. Only round_store round_data remains direct (deferred — high-frequency Touch/Judge, keep dual-write). ExtensionManager has worker reference. Telemetry cutover modes (direct_only/worker_preferred) in place. Write-path audit contract tests added.",
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
                    next_step: "All host APIs implemented with capability enforcement. WIT lifecycle wired (init/cleanup/on-event/on-api). Capability model contract tests added (23 wit_abi_contracts). Remaining: integration tests that compile+run a real WIT component (blocked on test WASM binary). SDK provides wit_bindgen! macro. JSON ABI removed. MIGRATION_PHASE 2 (accurate — integration tests pending).",
                },
                RuntimeObjective {
                    key: "test-coverage",
                    title: "Unit and integration test coverage",
                    status: "active",
                    priority: "P1",
                    next_step: "130 unit tests pass (up from 97). wasm_host_helpers tests added (24 tests: SSRF/validate/atomic_write/capabilities). Capability model contract tests added. Lock exclusivity test added. Simulation isolation tests added (5 new). Remaining: integration tests for WIT host API methods in running-server context (blocked — requires compiled .wasm component). Do not hard-code test totals in the plan.",
                },
                RuntimeObjective {
                    key: "technical-debt-triage",
                    title: "Source debt-comment backlog discipline",
                    status: "done",
                    priority: "P1",
                    next_step: "All WIT host 'not yet implemented' stubs have been resolved. No TODO/FIXME markers remain in production code. Keep scanning new code for unchecked markers.",
                },
                RuntimeObjective {
                    key: "step-38-closure-gate",
                    title: "Step 38: Runtime v2 closure gate",
                    status: "active",
                    priority: "P0",
                    next_step: "Gate progress: phira-http DONE, touch-judge-persistence DONE, eventbus DONE, technical-debt-triage DONE, persistence-worker DONE. Remaining: plugin-abi-v2 (needs WASM integration tests — blocked), actor-model (lock/cycle tracked+owned, rest WriteRouted — incremental), simulation/benchmark-modes/low-overhead-diagnostics (architectural guardrails — keep active), test-coverage (133 tests, WIT integration tests missing — blocked on WASM). cargo test --workspace passes (133 unit + ~120 integration). docs_contracts/wit_abi_contracts pass.",
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

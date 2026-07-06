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
                    status: "active",
                    priority: "P0",
                    next_step: "Necessary. PhiraRetryClient owns retry/backoff/circuit-breaker policy for core paths, but legacy metadata helpers still use direct reqwest. Next: route fetch_phira_user_name/fetch_phira_chart through RetryClient or a metadata worker.",
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
                    next_step: "Necessary and under-complete. The repo declares WIT-only (MIGRATION_PHASE=2), but component lifecycle dispatch, PluginEvent conversion, on-api and many host traits are still stubs. Next: make WIT lifecycle real before claiming plugin ABI completion.",
                },
                RuntimeObjective {
                    key: "test-coverage",
                    title: "Unit and integration test coverage",
                    status: "active",
                    priority: "P1",
                    next_step: "Broad contract coverage exists, but this objective remains necessary until CI proves the suite and WIT lifecycle/host API behavior has contract tests. Do not hard-code test totals in the plan.",
                },
                RuntimeObjective {
                    key: "technical-debt-triage",
                    title: "Source debt-comment backlog discipline",
                    status: "active",
                    priority: "P1",
                    next_step: "Previous TODO markers around WIT lifecycle, WebSocket draft, SSRF DNS and legacy Phira helpers have been converted to working code. Keep this objective for future debt comments; scan new code for unchecked TODO/FIXME markers.",
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

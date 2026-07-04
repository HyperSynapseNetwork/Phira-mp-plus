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
                    next_step: "Simulation suite reports adapt into BenchmarkReport, publish benchmark.completed and update readonly report snapshots; next step is cleanup hardening now that persisted report history infrastructure exists.",
                },
                RuntimeObjective {
                    key: "benchmark-modes",
                    title: "Benchmark modes: simulation / hybrid / real",
                    status: "active",
                    priority: "P0",
                    next_step: "Simulation, hybrid and real share BenchmarkReport output, emit benchmark.completed, populate readonly CLI/Web snapshots and mirror report history into PostgreSQL.",
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
                    next_step: "AdminCommand trait with 10 commands (help/exit/status/ban/unban/banlist/banip/unbanip/banlistip) wired into PlusServerState + CLI dispatch. Room actor: 7 commands through typed mailbox boundary with typed_or_err delivery. 5/7 actor boundaries advanced. Next: migrate more commands to AdminCommand trait, move room state into mailbox worker.",
                },
                RuntimeObjective {
                    key: "touch-judge-persistence",
                    title: "Touches/Judges persistence without active monitor",
                    status: "active",
                    priority: "P0",
                    next_step: "Session telemetry path is audited and contract-tested: active monitors only control realtime broadcast, while Touch/Judge persistence still runs for active rounds without monitors. Telemetry cutover modes: direct_only / worker_preferred (dual_write/fallback_only removed). persistence/telemetry.rs extracted from db.rs. Next: add contract tests for telemetry persistence cutover.",
                },
                RuntimeObjective {
                    key: "phira-http",
                    title: "Unified Phira HTTP RetryClient",
                    status: "active",
                    priority: "P0",
                    next_step: "PhiraRetryClient now owns timeout/retry/backoff/circuit-breaker policy, failure classification and half-open probe behavior; next step is endpoint-level health and metadata worker routing.",
                },
                RuntimeObjective {
                    key: "persistence-worker",
                    title: "Persistence Worker ownership",
                    status: "active",
                    priority: "P1",
                    next_step: "PersistenceWorker split into message/stats/mirror/pipeline/worker modules. db.rs persistence extracted into persistence/{benchmark,telemetry,simulation,events}.rs. TelemetryBatcher in telemetry.rs + telemetry_batcher.rs facade. Next: move remaining direct db.rs production writes into PersistenceWorker mailbox.",
                },
                RuntimeObjective {
                    key: "eventbus",
                    title: "EventBus as runtime spine",
                    status: "active",
                    priority: "P1",
                    next_step: "benchmark.completed is typed, cached as low-overhead BenchmarkReport digests and mirrored through the split PersistenceWorker pipeline into mp_runtime_benchmark_reports for CLI/Web readonly history.",
                },
                RuntimeObjective {
                    key: "plugin-abi-v2",
                    title: "Typed WASM plugin ABI",
                    status: "active",
                    priority: "P1",
                    next_step: "plugin_abi.rs split into typed submodule (plan/json/dto). WIT bindgen compiled behind wit-bindgen feature (MIGRATION_PHASE=0 default). WitPluginHost skeleton implements phira_host::Host. wasm_host.rs (1600+ lines) still uses JSON bridge. Next: extract JSON ABI encode/decode from wasm_host.rs into plugin_abi/json.rs, reduce wasm_host.rs.",
                },
                RuntimeObjective {
                    key: "test-coverage",
                    title: "Unit and integration test coverage",
                    status: "active",
                    priority: "P1",
                    next_step: "Add contract tests around plugin ABI, command registry, telemetry cutover, room gateway and session handlers; monitor-independent Touch/Judge persistence now has a focused session telemetry contract.",
                },
                RuntimeObjective {
                    key: "technical-debt-triage",
                    title: "Source debt-comment backlog discipline",
                    status: "active",
                    priority: "P1",
                    next_step: "Current source audit found no inline debt markers in phira-mp-plus-server/src; future debt markers must be converted into RuntimePlan objectives, tests or tracked issues instead of being left as drifting comments.",
                },
                RuntimeObjective {
                    key: "tui-v2",
                    title: "TUI v2 observability panels",
                    status: "planned",
                    priority: "P2",
                    next_step: "Build panels from EventBus/Simulation/Persistence/Actor stats after core signals stabilize.",
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

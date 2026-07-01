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
                    next_step: "Add batch reports, scenario tuning, cleanup hardening and real load validation.",
                },
                RuntimeObjective {
                    key: "benchmark-modes",
                    title: "Benchmark modes: simulation / hybrid / real",
                    status: "active",
                    priority: "P0",
                    next_step: "Keep real benchmark explicit; introduce hybrid switches only after Phira client is centralized.",
                },
                RuntimeObjective {
                    key: "actor-model",
                    title: "Actor model migration",
                    status: "active",
                    priority: "P0",
                    next_step: "Expand typed command/result boundaries and gradually move room/session ownership into actors.",
                },
                RuntimeObjective {
                    key: "touch-judge-persistence",
                    title: "Touches/Judges persistence without active monitor",
                    status: "active",
                    priority: "P0",
                    next_step: "Production Touch/Judge now has explicit legacy_only/dual_write/worker_only/fallback_only cutover modes backed by Runtime v2 telemetry batch/item schema.",
                },
                RuntimeObjective {
                    key: "phira-http",
                    title: "Unified Phira HTTP RetryClient",
                    status: "active",
                    priority: "P0",
                    next_step: "PhiraRetryClient now owns timeout/retry/backoff/circuit-breaker policy; next step is endpoint-level health and metadata worker routing.",
                },
                RuntimeObjective {
                    key: "persistence-worker",
                    title: "Persistence Worker ownership",
                    status: "active",
                    priority: "P1",
                    next_step: "Low-frequency production events dual-write through PersistenceWorker; high-frequency telemetry now has an explicit cutover switch before final worker ownership.",
                },
                RuntimeObjective {
                    key: "eventbus",
                    title: "EventBus as runtime spine",
                    status: "active",
                    priority: "P1",
                    next_step: "Mirror fewer ad-hoc plugin/server calls and add typed subscribers for persistence and TUI.",
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

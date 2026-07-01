//! Runtime v2 simulation manager.
//!
//! Step 3 creates an isolated **shadow world** for deterministic simulation.
//! Shadow users/rooms/rounds are stored only inside [`SimulationManager`]; they
//! are not inserted into the real `PlusServerState.rooms` / `users` maps, are not
//! returned by `/api/rooms`, and are not written to normal persistence tables.
//! This gives us something useful to inspect and test before moving any real
//! Room/Session state-machine logic.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashSet, VecDeque},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tokio::sync::RwLock;
use uuid::Uuid;

pub const DEFAULT_SIMULATION_SEED: u64 = 114_514;
const MAX_SHADOW_USERS: usize = 50_000;
const MAX_SHADOW_ROOMS: usize = 10_000;
const MAX_ROOM_MEMBERS: usize = 16;
const MAX_EVENT_LOG: usize = 256;
const MAX_SUITE_REPORTS: usize = 32;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SimulationPreset {
    Baseline,
    Small,
    Medium,
    Large,
    Custom,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SimulationScenario {
    Balanced,
    ChatStorm,
    ReadyStorm,
    RoundStorm,
    TouchJudgeBurst,
    Idle,
}

impl SimulationScenario {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "balanced" | "balance" | "default" | "normal" | "mixed" => Some(Self::Balanced),
            "chat" | "chat_storm" | "chatstorm" => Some(Self::ChatStorm),
            "ready" | "ready_storm" | "readystorm" => Some(Self::ReadyStorm),
            "round" | "rounds" | "round_storm" | "roundstorm" => Some(Self::RoundStorm),
            "touch" | "judge" | "touch_judge" | "touch_judge_burst" | "touchjudgeburst" | "burst" => {
                Some(Self::TouchJudgeBurst)
            }
            "idle" | "noop" | "quiet" => Some(Self::Idle),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::ChatStorm => "chat_storm",
            Self::ReadyStorm => "ready_storm",
            Self::RoundStorm => "round_storm",
            Self::TouchJudgeBurst => "touch_judge_burst",
            Self::Idle => "idle",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Balanced => "mixed chat/ready/round/touch/judge load",
            Self::ChatStorm => "chat-heavy workload with smaller gameplay pressure",
            Self::ReadyStorm => "ready/cancel-ready toggle storm",
            Self::RoundStorm => "frequent room state and round completion pressure",
            Self::TouchJudgeBurst => "touch/judge batch-heavy pressure path",
            Self::Idle => "shadow world stays running with almost no workload",
        }
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::Balanced,
            Self::ChatStorm,
            Self::ReadyStorm,
            Self::RoundStorm,
            Self::TouchJudgeBurst,
            Self::Idle,
        ]
    }
}

impl Default for SimulationScenario {
    fn default() -> Self {
        Self::Balanced
    }
}


#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SimulationSuite {
    Smoke,
    Mixed,
    Stress,
}

impl SimulationSuite {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
            "smoke" | "quick" | "q" => Some(Self::Smoke),
            "mixed" | "default" | "all" | "suite" => Some(Self::Mixed),
            "stress" | "heavy" | "load" => Some(Self::Stress),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Smoke => "smoke",
            Self::Mixed => "mixed",
            Self::Stress => "stress",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Smoke => "short sanity suite: balanced + idle, useful for CI smoke checks",
            Self::Mixed => "balanced scenario sweep: chat, ready, round, touch/judge",
            Self::Stress => "heavier scenario sweep for sustained EventBus/PersistenceWorker pressure",
        }
    }

    pub fn all() -> &'static [Self] {
        &[Self::Smoke, Self::Mixed, Self::Stress]
    }

    pub fn plan(self, seed: u64) -> Vec<SimulationSuiteStep> {
        let mut steps = match self {
            Self::Smoke => vec![
                suite_step("smoke-balanced", SimulationPreset::Baseline, SimulationScenario::Balanced, seed, 10, 500, 5),
                suite_step("smoke-idle", SimulationPreset::Baseline, SimulationScenario::Idle, seed.wrapping_add(1), 5, 500, 0),
            ],
            Self::Mixed => vec![
                suite_step("mixed-chat", SimulationPreset::Small, SimulationScenario::ChatStorm, seed, 20, 500, 10),
                suite_step("mixed-ready", SimulationPreset::Small, SimulationScenario::ReadyStorm, seed.wrapping_add(1), 20, 500, 10),
                suite_step("mixed-round", SimulationPreset::Small, SimulationScenario::RoundStorm, seed.wrapping_add(2), 20, 500, 10),
                suite_step("mixed-touch-judge", SimulationPreset::Small, SimulationScenario::TouchJudgeBurst, seed.wrapping_add(3), 20, 500, 10),
            ],
            Self::Stress => vec![
                suite_step("stress-chat", SimulationPreset::Medium, SimulationScenario::ChatStorm, seed, 45, 250, 20),
                suite_step("stress-touch-judge", SimulationPreset::Medium, SimulationScenario::TouchJudgeBurst, seed.wrapping_add(1), 45, 250, 20),
                suite_step("stress-round", SimulationPreset::Medium, SimulationScenario::RoundStorm, seed.wrapping_add(2), 45, 250, 20),
            ],
        };
        for step in &mut steps {
            step.suite = self;
        }
        steps
    }
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationRunReport {
    pub suite_run_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub step_name: String,
    pub suite: Option<SimulationSuite>,
    pub preset: SimulationPreset,
    pub scenario: SimulationScenario,
    pub users: usize,
    pub rooms: usize,
    pub duration_secs: u64,
    pub tick_interval_ms: u64,
    pub persist_every_ticks: u64,
    pub started_at_ms: Option<i64>,
    pub finished_at_ms: i64,
    pub elapsed_secs: u64,
    pub aborted: bool,
    pub reason: String,
    pub counters: SimulationCounters,
    pub workload_events: u64,
    pub workload_events_per_sec: f64,
}

impl SimulationRunReport {
    pub fn from_status(
        suite_run_id: Option<Uuid>,
        suite: Option<SimulationSuite>,
        step_name: impl Into<String>,
        status: &SimulationStatus,
        aborted: bool,
        reason: impl Into<String>,
    ) -> Self {
        let workload_events = status.counters.workload_events();
        let elapsed_secs = status.elapsed_secs.max(1);
        Self {
            suite_run_id,
            run_id: status.run_id,
            step_name: step_name.into(),
            suite,
            preset: status.config.preset,
            scenario: status.config.scenario,
            users: status.config.users,
            rooms: status.config.rooms,
            duration_secs: status.config.duration_secs,
            tick_interval_ms: status.config.tick_interval_ms,
            persist_every_ticks: status.config.persist_every_ticks,
            started_at_ms: status.started_at_ms,
            finished_at_ms: now_ms(),
            elapsed_secs: status.elapsed_secs,
            aborted,
            reason: reason.into(),
            counters: status.counters.clone(),
            workload_events,
            workload_events_per_sec: workload_events as f64 / elapsed_secs as f64,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationSuiteReport {
    pub suite_run_id: Uuid,
    pub suite: SimulationSuite,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub total_steps: usize,
    pub completed_steps: usize,
    pub aborted: bool,
    pub reason: String,
    pub steps: Vec<SimulationRunReport>,
    pub totals: SimulationCounters,
    pub total_elapsed_secs: u64,
    pub workload_events: u64,
    pub workload_events_per_sec: f64,
}

impl SimulationSuiteReport {
    pub fn new(
        suite_run_id: Uuid,
        suite: SimulationSuite,
        started_at_ms: i64,
        finished_at_ms: i64,
        total_steps: usize,
        completed_steps: usize,
        aborted: bool,
        reason: impl Into<String>,
        steps: Vec<SimulationRunReport>,
    ) -> Self {
        let mut totals = SimulationCounters::default();
        let mut total_elapsed_secs = 0u64;
        for step in &steps {
            totals.add_assign(&step.counters);
            total_elapsed_secs = total_elapsed_secs.saturating_add(step.elapsed_secs);
        }
        let workload_events = totals.workload_events();
        let denom = total_elapsed_secs.max(1);
        Self {
            suite_run_id,
            suite,
            started_at_ms,
            finished_at_ms,
            total_steps,
            completed_steps,
            aborted,
            reason: reason.into(),
            steps,
            totals,
            total_elapsed_secs,
            workload_events,
            workload_events_per_sec: workload_events as f64 / denom as f64,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationSuiteStep {
    pub suite: SimulationSuite,
    pub name: String,
    pub config: SimulationConfig,
}

fn suite_step(
    name: &str,
    preset: SimulationPreset,
    scenario: SimulationScenario,
    seed: u64,
    duration_secs: u64,
    tick_interval_ms: u64,
    persist_every_ticks: u64,
) -> SimulationSuiteStep {
    let mut config = preset.defaults(seed);
    config.scenario = scenario;
    config.duration_secs = duration_secs;
    config.tick_interval_ms = tick_interval_ms;
    config.persist_every_ticks = persist_every_ticks;
    config.auto_tick = true;
    SimulationSuiteStep {
        suite: SimulationSuite::Smoke,
        name: name.to_string(),
        config,
    }
}

impl SimulationPreset {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "baseline" | "base" | "b" => Some(Self::Baseline),
            "small" | "s" => Some(Self::Small),
            "medium" | "m" => Some(Self::Medium),
            "large" | "l" => Some(Self::Large),
            "custom" | "c" => Some(Self::Custom),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
            Self::Custom => "custom",
        }
    }

    pub fn defaults(self, seed: u64) -> SimulationConfig {
        let (users, rooms, duration_secs) = match self {
            Self::Baseline => (20, 5, 60),
            Self::Small => (100, 10, 120),
            Self::Medium => (500, 50, 300),
            Self::Large => (1_500, 150, 600),
            Self::Custom => (20, 5, 60),
        };
        SimulationConfig {
            preset: self,
            users,
            rooms,
            duration_secs,
            touch: true,
            judge: true,
            seed,
            scenario: SimulationScenario::Balanced,
            chat: true,
            ready: true,
            rounds: true,
            auto_tick: true,
            tick_interval_ms: 1_000,
            persist_every_ticks: 0,
        }
    }
}

impl Default for SimulationPreset {
    fn default() -> Self {
        Self::Baseline
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationConfig {
    pub preset: SimulationPreset,
    pub users: usize,
    pub rooms: usize,
    pub duration_secs: u64,
    pub touch: bool,
    pub judge: bool,
    pub seed: u64,
    pub scenario: SimulationScenario,
    pub chat: bool,
    pub ready: bool,
    pub rounds: bool,
    pub auto_tick: bool,
    pub tick_interval_ms: u64,
    /// 0 means disabled. When enabled, the CLI runner emits simulation.snapshot
    /// events every N ticks so PersistenceWorker can write mp_sim_events.
    pub persist_every_ticks: u64,
}

impl SimulationConfig {
    pub fn apply_kv(&mut self, key: &str, value: &str) -> Result<(), String> {
        match key.trim().to_ascii_lowercase().as_str() {
            "users" | "user" => {
                self.users = value.parse::<usize>().map_err(|_| "users must be usize".to_string())?;
            }
            "rooms" | "room" => {
                self.rooms = value.parse::<usize>().map_err(|_| "rooms must be usize".to_string())?;
            }
            "duration" | "duration_secs" | "seconds" | "secs" => {
                self.duration_secs = value.parse::<u64>().map_err(|_| "duration must be u64 seconds".to_string())?;
            }
            "touch" | "touches" => self.touch = parse_bool(value)?,
            "judge" | "judges" => self.judge = parse_bool(value)?,
            "chat" | "chats" => self.chat = parse_bool(value)?,
            "ready" | "readies" => self.ready = parse_bool(value)?,
            "round" | "rounds" | "game" | "games" => self.rounds = parse_bool(value)?,
            "auto" | "auto_tick" | "autotick" | "runner" => self.auto_tick = parse_bool(value)?,
            "tick_ms" | "interval_ms" | "tick_interval_ms" => {
                self.tick_interval_ms = value
                    .parse::<u64>()
                    .map_err(|_| "tick_ms must be u64 milliseconds".to_string())?;
            }
            "persist_every" | "persist_every_ticks" | "snapshot_every" => {
                self.persist_every_ticks = value
                    .parse::<u64>()
                    .map_err(|_| "persist_every must be u64 ticks; 0 disables periodic snapshot".to_string())?;
            }
            "seed" => self.seed = value.parse::<u64>().map_err(|_| "seed must be u64".to_string())?,
            "scenario" | "profile" | "mode" | "workload" => {
                self.scenario = SimulationScenario::parse(value)
                    .ok_or_else(|| format!("unknown simulation scenario: {value}"))?;
            }
            other => return Err(format!("unknown simulation option: {other}")),
        }
        self.preset = SimulationPreset::Custom;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.users == 0 {
            return Err("simulation users must be greater than 0".to_string());
        }
        if self.rooms == 0 {
            return Err("simulation rooms must be greater than 0".to_string());
        }
        if self.duration_secs == 0 {
            return Err("simulation duration must be greater than 0".to_string());
        }
        if self.users > 200_000 {
            return Err("simulation users is too large for the current safe shadow-world stage".to_string());
        }
        if self.rooms > 20_000 {
            return Err("simulation rooms is too large for the current safe shadow-world stage".to_string());
        }
        if !(50..=60_000).contains(&self.tick_interval_ms) {
            return Err("simulation tick_ms must be between 50 and 60000".to_string());
        }
        Ok(())
    }
}

impl Default for SimulationConfig {
    fn default() -> Self {
        SimulationPreset::Baseline.defaults(DEFAULT_SIMULATION_SEED)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleTouch {
    pub time_ms: u32,
    pub lane: u8,
    pub pressed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleJudge {
    pub time_ms: u32,
    pub judge: String,
    pub score_delta: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SimulationCounters {
    pub ticks: u64,
    pub chat_messages: u64,
    pub ready_events: u64,
    pub touch_batches: u64,
    pub judge_batches: u64,
    pub round_results: u64,
}

impl SimulationCounters {
    pub fn workload_events(&self) -> u64 {
        self.chat_messages
            .saturating_add(self.ready_events)
            .saturating_add(self.touch_batches)
            .saturating_add(self.judge_batches)
            .saturating_add(self.round_results)
    }

    pub fn add_assign(&mut self, other: &SimulationCounters) {
        self.ticks = self.ticks.saturating_add(other.ticks);
        self.chat_messages = self.chat_messages.saturating_add(other.chat_messages);
        self.ready_events = self.ready_events.saturating_add(other.ready_events);
        self.touch_batches = self.touch_batches.saturating_add(other.touch_batches);
        self.judge_batches = self.judge_batches.saturating_add(other.judge_batches);
        self.round_results = self.round_results.saturating_add(other.round_results);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualUser {
    pub id: i32,
    pub name: String,
    pub room_id: Option<String>,
    pub ready: bool,
    pub playing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualRoom {
    pub id: String,
    pub hidden: bool,
    pub chart_id: i32,
    pub member_ids: Vec<i32>,
    pub ready_count: usize,
    pub playing: bool,
    pub round_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualRound {
    pub round_id: String,
    pub room_id: String,
    pub chart_id: i32,
    pub players: usize,
    pub sample_score: i32,
    pub sample_touches: usize,
    pub sample_judges: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationEventLogEntry {
    pub seq: u64,
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationGeneratedEvent {
    pub kind: String,
    pub run_id: Option<Uuid>,
    pub tick: u64,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationWorldSnapshot {
    pub run_id: Option<Uuid>,
    pub users_total: usize,
    pub rooms_total: usize,
    pub users_materialized: usize,
    pub rooms_materialized: usize,
    pub rounds_materialized: usize,
    pub sample_users: Vec<VirtualUser>,
    pub sample_rooms: Vec<VirtualRoom>,
    pub sample_rounds: Vec<VirtualRound>,
    pub recent_events: Vec<SimulationEventLogEntry>,
    pub materialization_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationStatus {
    pub running: bool,
    pub run_id: Option<Uuid>,
    pub seed: u64,
    pub config: SimulationConfig,
    pub virtual_users: usize,
    pub virtual_rooms: usize,
    pub materialized_users: usize,
    pub materialized_rooms: usize,
    pub materialized_rounds: usize,
    pub counters: SimulationCounters,
    pub started_at_ms: Option<i64>,
    pub last_tick_at_ms: Option<i64>,
    pub elapsed_secs: u64,
    pub remaining_secs: Option<u64>,
    pub runner_enabled: bool,
    pub note: String,
}

#[derive(Debug, Clone)]
struct SimulationWorld {
    users_total: usize,
    rooms_total: usize,
    users: Vec<VirtualUser>,
    rooms: Vec<VirtualRoom>,
    rounds: Vec<VirtualRound>,
    events: VecDeque<SimulationEventLogEntry>,
    next_seq: u64,
}

#[derive(Debug)]
struct SimulationState {
    running: bool,
    run_id: Option<Uuid>,
    config: SimulationConfig,
    virtual_users: usize,
    virtual_rooms: usize,
    counters: SimulationCounters,
    started_at_ms: Option<i64>,
    last_tick_at_ms: Option<i64>,
    world: Option<SimulationWorld>,
    suite_reports: VecDeque<SimulationSuiteReport>,
    note: String,
}

#[derive(Debug)]
pub struct SimulationManager {
    seed: AtomicU64,
    state: Arc<RwLock<SimulationState>>,
}

impl SimulationManager {
    pub fn new() -> Self {
        let config = SimulationConfig::default();
        Self {
            seed: AtomicU64::new(config.seed),
            state: Arc::new(RwLock::new(SimulationState {
                running: false,
                run_id: None,
                config,
                virtual_users: 0,
                virtual_rooms: 0,
                counters: SimulationCounters::default(),
                started_at_ms: None,
                last_tick_at_ms: None,
                world: None,
                suite_reports: VecDeque::with_capacity(MAX_SUITE_REPORTS),
                note: "Runtime v2 simulation manager is installed; no real rooms/users are modified.".to_string(),
            })),
        }
    }

    pub async fn status(&self) -> SimulationStatus {
        let state = self.state.read().await;
        self.snapshot(&state)
    }

    pub fn seed_hint(&self) -> u64 {
        self.seed.load(Ordering::Relaxed)
    }

    pub async fn set_seed(&self, seed: u64) {
        self.seed.store(seed, Ordering::Relaxed);
        let mut state = self.state.write().await;
        state.config.seed = seed;
        state.note = format!("simulation seed updated to {seed}; deterministic replay uses it for sample data");
        if let Some(world) = &mut state.world {
            push_event(world, "seed", format!("seed updated to {seed}; running world will use it on next restart"));
        }
    }

    pub async fn start(&self, config: SimulationConfig) -> Result<SimulationStatus, String> {
        config.validate()?;
        self.seed.store(config.seed, Ordering::Relaxed);
        let mut state = self.state.write().await;
        if state.running {
            return Err(format!(
                "simulation is already running: {}",
                state.run_id.as_ref().map(|id| id.to_string()).unwrap_or_else(|| "unknown".to_string())
            ));
        }
        let run_id = Uuid::new_v4();
        let mut world = build_shadow_world(run_id, &config);
        push_event(
            &mut world,
            "started",
            format!(
                "shadow world started: preset={} scenario={} users={} rooms={} seed={}",
                config.preset.as_str(),
                config.scenario.as_str(),
                config.users,
                config.rooms,
                config.seed
            ),
        );
        state.running = true;
        state.run_id = Some(run_id);
        state.virtual_users = config.users;
        state.virtual_rooms = config.rooms;
        state.counters = SimulationCounters::default();
        let started_at_ms = now_ms();
        state.started_at_ms = Some(started_at_ms);
        state.last_tick_at_ms = Some(started_at_ms);
        state.note = format!(
            "simulation {} scenario={} started with isolated shadow world; real rooms/users are untouched",
            config.preset.as_str(),
            config.scenario.as_str()
        );
        state.config = config;
        state.world = Some(world);
        Ok(self.snapshot(&state))
    }

    pub async fn stop(&self, reason: impl Into<String>) -> SimulationStatus {
        let mut state = self.state.write().await;
        let reason = reason.into();
        if let Some(world) = &mut state.world {
            push_event(world, "stopped", reason.clone());
        }
        state.running = false;
        state.note = if let Some(run_id) = state.run_id.take() {
            format!("simulation {run_id} stopped: {reason}")
        } else {
            format!("simulation is not running: {reason}")
        };
        state.virtual_users = 0;
        state.virtual_rooms = 0;
        state.last_tick_at_ms = Some(now_ms());
        self.snapshot(&state)
    }

    pub async fn cleanup(&self) -> SimulationStatus {
        let mut state = self.state.write().await;
        state.running = false;
        state.run_id = None;
        state.virtual_users = 0;
        state.virtual_rooms = 0;
        state.counters = SimulationCounters::default();
        state.started_at_ms = None;
        state.last_tick_at_ms = None;
        state.world = None;
        state.note = "simulation shadow world cleaned; real rooms/users were not touched".to_string();
        self.snapshot(&state)
    }


    pub async fn advance_ticks(&self, count: u64) -> Result<SimulationStatus, String> {
        self.advance_ticks_with_events(count)
            .await
            .map(|(status, _events)| status)
    }

    pub async fn advance_ticks_with_events(
        &self,
        count: u64,
    ) -> Result<(SimulationStatus, Vec<SimulationGeneratedEvent>), String> {
        let mut state = self.state.write().await;
        if !state.running {
            return Err("simulation is not running".to_string());
        }
        let events = advance_state(&mut state, count);
        Ok((self.snapshot(&state), events))
    }

    pub async fn advance_ticks_for_run(&self, run_id: Uuid, count: u64) -> Result<SimulationStatus, String> {
        self.advance_ticks_for_run_with_events(run_id, count)
            .await
            .map(|(status, _events)| status)
    }

    pub async fn advance_ticks_for_run_with_events(
        &self,
        run_id: Uuid,
        count: u64,
    ) -> Result<(SimulationStatus, Vec<SimulationGeneratedEvent>), String> {
        let mut state = self.state.write().await;
        if !state.running || state.run_id != Some(run_id) {
            return Err("simulation run changed or stopped".to_string());
        }
        let events = advance_state(&mut state, count);
        Ok((self.snapshot(&state), events))
    }

    pub async fn stop_if_run(&self, run_id: Uuid, reason: impl Into<String>) -> Option<SimulationStatus> {
        let mut state = self.state.write().await;
        if !state.running || state.run_id != Some(run_id) {
            return None;
        }
        let reason = reason.into();
        if let Some(world) = &mut state.world {
            push_event(world, "stopped", reason.clone());
        }
        state.running = false;
        state.note = format!("simulation {run_id} stopped: {reason}");
        state.run_id = None;
        state.virtual_users = 0;
        state.virtual_rooms = 0;
        state.last_tick_at_ms = Some(now_ms());
        Some(self.snapshot(&state))
    }

    pub async fn world_snapshot(&self, limit: usize) -> Option<SimulationWorldSnapshot> {
        let state = self.state.read().await;
        let world = state.world.as_ref()?;
        let limit = limit.clamp(1, 200);
        Some(SimulationWorldSnapshot {
            run_id: state.run_id,
            users_total: world.users_total,
            rooms_total: world.rooms_total,
            users_materialized: world.users.len(),
            rooms_materialized: world.rooms.len(),
            rounds_materialized: world.rounds.len(),
            sample_users: world.users.iter().take(limit).cloned().collect(),
            sample_rooms: world.rooms.iter().take(limit).cloned().collect(),
            sample_rounds: world.rounds.iter().take(limit).cloned().collect(),
            recent_events: world.events.iter().rev().take(limit).cloned().collect(),
            materialization_note: materialization_note(world.users_total, world.rooms_total),
        })
    }

    pub async fn record_suite_report(&self, report: SimulationSuiteReport) {
        let mut state = self.state.write().await;
        if state.suite_reports.len() >= MAX_SUITE_REPORTS {
            state.suite_reports.pop_front();
        }
        state.note = format!(
            "recorded simulation suite report: suite={} completed={}/{} aborted={}",
            report.suite.as_str(),
            report.completed_steps,
            report.total_steps,
            report.aborted
        );
        state.suite_reports.push_back(report);
    }

    pub async fn suite_reports(&self, limit: usize) -> Vec<SimulationSuiteReport> {
        let state = self.state.read().await;
        let limit = limit.clamp(1, MAX_SUITE_REPORTS);
        state.suite_reports.iter().rev().take(limit).cloned().collect()
    }

    pub async fn latest_suite_report(&self) -> Option<SimulationSuiteReport> {
        let state = self.state.read().await;
        state.suite_reports.back().cloned()
    }

    pub async fn clear_suite_reports(&self) -> usize {
        let mut state = self.state.write().await;
        let count = state.suite_reports.len();
        state.suite_reports.clear();
        state.note = format!("cleared {count} simulation suite report(s)");
        count
    }

    pub fn sample_touches(seed: u64) -> Vec<SampleTouch> {
        let offset = (seed % 17) as u32;
        (0..16)
            .map(|idx| SampleTouch {
                time_ms: 500 + offset + idx * 125,
                lane: (idx % 4) as u8,
                pressed: idx % 2 == 0,
            })
            .collect()
    }

    pub fn sample_judges(seed: u64) -> Vec<SampleJudge> {
        let offset = (seed % 23) as u32;
        ["perfect", "perfect", "good", "perfect", "bad", "perfect", "miss", "perfect"]
            .into_iter()
            .enumerate()
            .map(|(idx, judge)| SampleJudge {
                time_ms: 700 + offset + idx as u32 * 250,
                judge: judge.to_string(),
                score_delta: match judge {
                    "perfect" => 1_000,
                    "good" => 650,
                    "bad" => 150,
                    _ => 0,
                },
            })
            .collect()
    }

    fn snapshot(&self, state: &SimulationState) -> SimulationStatus {
        let (materialized_users, materialized_rooms, materialized_rounds) = state
            .world
            .as_ref()
            .map(|world| (world.users.len(), world.rooms.len(), world.rounds.len()))
            .unwrap_or((0, 0, 0));
        SimulationStatus {
            running: state.running,
            run_id: state.run_id,
            seed: self.seed.load(Ordering::Relaxed),
            config: state.config.clone(),
            virtual_users: state.virtual_users,
            virtual_rooms: state.virtual_rooms,
            materialized_users,
            materialized_rooms,
            materialized_rounds,
            counters: state.counters.clone(),
            started_at_ms: state.started_at_ms,
            last_tick_at_ms: state.last_tick_at_ms,
            elapsed_secs: elapsed_secs(state),
            remaining_secs: remaining_secs(state),
            runner_enabled: state.running && state.config.auto_tick,
            note: state.note.clone(),
        }
    }
}

impl Default for SimulationManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
struct WorkloadDeltas {
    chat_messages: u64,
    ready_events: u64,
    touch_batches: u64,
    judge_batches: u64,
    round_results: u64,
}

fn workload_deltas(config: &SimulationConfig) -> WorkloadDeltas {
    let users = config.users.max(1) as u64;
    let rooms = config.rooms.max(1) as u64;
    let (chat_base, ready_base, touch_base, judge_base, round_base) = match config.scenario {
        SimulationScenario::Balanced => ((users / 10).max(1), users, rooms, rooms, rooms),
        SimulationScenario::ChatStorm => ((users / 2).max(1), (users / 10).max(1), (rooms / 4).max(1), (rooms / 4).max(1), (rooms / 5).max(1)),
        SimulationScenario::ReadyStorm => ((users / 20).max(1), users.saturating_mul(3), 0, 0, (rooms / 10).max(1)),
        SimulationScenario::RoundStorm => ((users / 20).max(1), users, rooms, rooms, rooms.saturating_mul(3)),
        SimulationScenario::TouchJudgeBurst => ((users / 50).max(1), (users / 5).max(1), rooms.saturating_mul(5), rooms.saturating_mul(5), rooms),
        SimulationScenario::Idle => (0, 0, 0, 0, 0),
    };
    WorkloadDeltas {
        chat_messages: if config.chat { chat_base } else { 0 },
        ready_events: if config.ready { ready_base } else { 0 },
        touch_batches: if config.touch { touch_base } else { 0 },
        judge_batches: if config.judge { judge_base } else { 0 },
        round_results: if config.rounds { round_base } else { 0 },
    }
}

fn advance_state(state: &mut SimulationState, count: u64) -> Vec<SimulationGeneratedEvent> {
    let count = count.clamp(1, 10_000);
    let mut generated = Vec::new();
    for _ in 0..count {
        let tick = state.counters.ticks + 1;
        let config = state.config.clone();
        let run_id = state.run_id;

        let deltas = workload_deltas(&config);
        let chat_delta = deltas.chat_messages;
        let ready_delta = deltas.ready_events;
        let touch_delta = deltas.touch_batches;
        let judge_delta = deltas.judge_batches;
        let round_delta = deltas.round_results;

        state.counters.ticks = tick;
        state.counters.chat_messages += chat_delta;
        state.counters.ready_events += ready_delta;
        state.counters.touch_batches += touch_delta;
        state.counters.judge_batches += judge_delta;
        state.counters.round_results += round_delta;

        let counters_snapshot = state.counters.clone();
        let world_tick = if let Some(world) = &mut state.world {
            Some(advance_shadow_world(world, tick, &config, &counters_snapshot))
        } else {
            None
        };

        generated.extend(generated_events_for_tick(
            run_id,
            tick,
            &config,
            chat_delta,
            ready_delta,
            touch_delta,
            judge_delta,
            round_delta,
            world_tick.as_ref(),
        ));
    }
    state.last_tick_at_ms = Some(now_ms());
    state.note = format!("simulation advanced by {count} deterministic tick(s)");
    generated
}

fn generated_events_for_tick(
    run_id: Option<Uuid>,
    tick: u64,
    config: &SimulationConfig,
    chat_delta: u64,
    ready_delta: u64,
    touch_delta: u64,
    judge_delta: u64,
    round_delta: u64,
    world_tick: Option<&SimulationWorldTick>,
) -> Vec<SimulationGeneratedEvent> {
    let mut events = Vec::with_capacity(5);
    if chat_delta > 0 {
        events.push(sim_event(
            "simulation.chat",
            run_id,
            tick,
            json!({
                "scenario": config.scenario.as_str(),
                "messages": chat_delta,
                "sample_user_id": world_tick.and_then(|tick| tick.sample_user_id),
                "sample_room_id": world_tick.and_then(|tick| tick.sample_room_id.clone()),
            }),
        ));
    }
    if ready_delta > 0 {
        events.push(sim_event(
            "simulation.ready",
            run_id,
            tick,
            json!({
                "scenario": config.scenario.as_str(),
                "changed": ready_delta,
                "ready_users_materialized": world_tick.map(|tick| tick.ready_users).unwrap_or(0),
            }),
        ));
    }
    if touch_delta > 0 {
        events.push(sim_event(
            "simulation.touch",
            run_id,
            tick,
            json!({
                "scenario": config.scenario.as_str(),
                "batches": touch_delta,
                "sample_touches": SimulationManager::sample_touches(config.seed),
                "sample_room_id": world_tick.and_then(|tick| tick.sample_room_id.clone()),
                "sample_round_id": world_tick.and_then(|tick| tick.sample_round_id.clone()),
            }),
        ));
    }
    if judge_delta > 0 {
        events.push(sim_event(
            "simulation.judge",
            run_id,
            tick,
            json!({
                "scenario": config.scenario.as_str(),
                "batches": judge_delta,
                "sample_judges": SimulationManager::sample_judges(config.seed),
                "sample_room_id": world_tick.and_then(|tick| tick.sample_room_id.clone()),
                "sample_round_id": world_tick.and_then(|tick| tick.sample_round_id.clone()),
            }),
        ));
    }
    if round_delta > 0 {
        events.push(sim_event(
            "simulation.round",
            run_id,
            tick,
            json!({
                "scenario": config.scenario.as_str(),
                "completed": round_delta,
                "playing_rooms_materialized": world_tick.map(|tick| tick.playing_rooms).unwrap_or(0),
                "sample_round_id": world_tick.and_then(|tick| tick.sample_round_id.clone()),
            }),
        ));
    }
    events
}

fn sim_event(kind: &str, run_id: Option<Uuid>, tick: u64, mut payload: Value) -> SimulationGeneratedEvent {
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("tick".to_string(), json!(tick));
        if let Some(run_id) = run_id {
            obj.insert("run_id".to_string(), json!(run_id.to_string()));
        }
    }
    SimulationGeneratedEvent {
        kind: kind.to_string(),
        run_id,
        tick,
        payload,
    }
}

fn elapsed_secs(state: &SimulationState) -> u64 {
    let Some(started) = state.started_at_ms else {
        return 0;
    };
    let end = if state.running {
        now_ms()
    } else {
        state.last_tick_at_ms.unwrap_or_else(now_ms)
    };
    end.saturating_sub(started).max(0) as u64 / 1000
}

fn remaining_secs(state: &SimulationState) -> Option<u64> {
    if !state.running {
        return None;
    }
    Some(state.config.duration_secs.saturating_sub(elapsed_secs(state)))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn build_shadow_world(run_id: Uuid, config: &SimulationConfig) -> SimulationWorld {
    let users_materialized = config.users.min(MAX_SHADOW_USERS);
    let rooms_materialized = config.rooms.min(MAX_SHADOW_ROOMS);
    let seed_offset = (config.seed % 10_000) as i32;
    let mut users = Vec::with_capacity(users_materialized);
    let mut rooms = Vec::with_capacity(rooms_materialized);
    for idx in 0..users_materialized {
        let id = -1_000_000 - seed_offset - idx as i32;
        let room_idx = if rooms_materialized > 0 { idx % rooms_materialized } else { 0 };
        users.push(VirtualUser {
            id,
            name: format!("sim-user-{idx:05}"),
            room_id: Some(format!("sim-{:04}", room_idx)),
            ready: config.ready && idx % 3 != 0,
            playing: false,
        });
    }
    for idx in 0..rooms_materialized {
        let member_ids = users
            .iter()
            .skip(idx)
            .step_by(rooms_materialized.max(1))
            .take(MAX_ROOM_MEMBERS)
            .map(|user| user.id)
            .collect::<Vec<_>>();
        let ready_count = member_ids
            .iter()
            .filter(|id| users.iter().any(|user| user.id == **id && user.ready))
            .count();
        rooms.push(VirtualRoom {
            id: format!("sim-{idx:04}"),
            hidden: true,
            chart_id: 10_000_000 + ((config.seed as usize + idx) % 10_000) as i32,
            member_ids,
            ready_count,
            playing: false,
            round_id: None,
        });
    }
    let mut world = SimulationWorld {
        users_total: config.users,
        rooms_total: config.rooms,
        users,
        rooms,
        rounds: Vec::new(),
        events: VecDeque::with_capacity(MAX_EVENT_LOG),
        next_seq: 1,
    };
    push_event(&mut world, "world", format!("shadow world allocated for run {run_id}"));
    world
}

#[derive(Debug, Clone)]
struct SimulationWorldTick {
    ready_users: usize,
    playing_rooms: usize,
    sample_user_id: Option<i32>,
    sample_room_id: Option<String>,
    sample_round_id: Option<String>,
}

fn advance_shadow_world(
    world: &mut SimulationWorld,
    tick: u64,
    config: &SimulationConfig,
    counters: &SimulationCounters,
) -> SimulationWorldTick {
    if config.ready {
        for (idx, user) in world.users.iter_mut().enumerate() {
            user.ready = ((idx as u64 + tick + config.seed) % 4) != 0;
        }
    }
    if config.rounds {
        let seed = config.seed;
        let touches_len = SimulationManager::sample_touches(seed).len();
        let judges_len = SimulationManager::sample_judges(seed).len();
        let mut new_rounds = Vec::new();
        for (idx, room) in world.rooms.iter_mut().enumerate().take(128) {
            let should_play = (idx as u64 + tick + seed) % 2 == 0;
            room.playing = should_play;
            room.ready_count = room.member_ids.len().saturating_sub(((idx as u64 + tick) % 2) as usize);
            if should_play {
                let round_id = format!("sim-round-{tick:06}-{idx:04}");
                room.round_id = Some(round_id.clone());
                new_rounds.push(VirtualRound {
                    round_id,
                    room_id: room.id.clone(),
                    chart_id: room.chart_id,
                    players: room.member_ids.len(),
                    sample_score: 1_000_000 - ((seed as i32 + idx as i32 + tick as i32) % 50_000),
                    sample_touches: if config.touch { touches_len } else { 0 },
                    sample_judges: if config.judge { judges_len } else { 0 },
                });
            } else {
                room.round_id = None;
            }
        }
        world.rounds = new_rounds;
        let playing_rooms = world
            .rooms
            .iter()
            .filter(|room| room.playing)
            .map(|room| room.id.clone())
            .collect::<HashSet<_>>();
        for user in &mut world.users {
            user.playing = user
                .room_id
                .as_ref()
                .map(|room_id| playing_rooms.contains(room_id))
                .unwrap_or(false);
        }
    }
    let ready_users = world.users.iter().filter(|user| user.ready).count();
    let playing_rooms = world.rooms.iter().filter(|room| room.playing).count();
    let sample_user_id = world.users.first().map(|user| user.id);
    let sample_room_id = world.rooms.first().map(|room| room.id.clone());
    let sample_round_id = world.rounds.first().map(|round| round.round_id.clone());
    push_event(
        world,
        "tick",
        format!(
            "tick={tick} chat={} ready={} touch_batches={} judge_batches={} round_results={}",
            counters.chat_messages,
            counters.ready_events,
            counters.touch_batches,
            counters.judge_batches,
            counters.round_results
        ),
    );
    SimulationWorldTick {
        ready_users,
        playing_rooms,
        sample_user_id,
        sample_room_id,
        sample_round_id,
    }
}

fn push_event(world: &mut SimulationWorld, kind: impl Into<String>, message: impl Into<String>) {
    if world.events.len() >= MAX_EVENT_LOG {
        world.events.pop_front();
    }
    world.events.push_back(SimulationEventLogEntry {
        seq: world.next_seq,
        kind: kind.into(),
        message: message.into(),
    });
    world.next_seq += 1;
}

fn materialization_note(users_total: usize, rooms_total: usize) -> String {
    let users_capped = users_total > MAX_SHADOW_USERS;
    let rooms_capped = rooms_total > MAX_SHADOW_ROOMS;
    match (users_capped, rooms_capped) {
        (false, false) => "all shadow users and rooms are materialized in memory".to_string(),
        (true, false) => format!("shadow users capped at {MAX_SHADOW_USERS}; totals still track requested user count"),
        (false, true) => format!("shadow rooms capped at {MAX_SHADOW_ROOMS}; totals still track requested room count"),
        (true, true) => format!("shadow users capped at {MAX_SHADOW_USERS}, rooms capped at {MAX_SHADOW_ROOMS}; totals still track requested counts"),
    }
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "y" | "on" | "enable" | "enabled" | "是" => Ok(true),
        "false" | "0" | "no" | "n" | "off" | "disable" | "disabled" | "否" => Ok(false),
        _ => Err(format!("invalid bool value: {value}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_defaults_are_deterministic() {
        let config = SimulationPreset::Medium.defaults(42);
        assert_eq!(config.users, 500);
        assert_eq!(config.rooms, 50);
        assert_eq!(config.seed, 42);
        assert!(config.auto_tick);
        assert_eq!(config.tick_interval_ms, 1_000);
    }

    #[test]
    fn custom_kv_updates_config() {
        let mut config = SimulationConfig::default();
        config.apply_kv("users", "123").unwrap();
        config.apply_kv("touch", "false").unwrap();
        config.apply_kv("auto", "false").unwrap();
        config.apply_kv("tick_ms", "250").unwrap();
        config.apply_kv("persist_every", "5").unwrap();
        config.apply_kv("scenario", "chat-storm").unwrap();
        assert_eq!(config.preset, SimulationPreset::Custom);
        assert_eq!(config.users, 123);
        assert_eq!(config.scenario, SimulationScenario::ChatStorm);
        assert!(!config.touch);
        assert!(!config.auto_tick);
        assert_eq!(config.tick_interval_ms, 250);
        assert_eq!(config.persist_every_ticks, 5);
    }

    #[test]
    fn scenario_changes_workload_shape() {
        let mut chat = SimulationPreset::Baseline.defaults(1);
        chat.scenario = SimulationScenario::ChatStorm;
        let mut touch = SimulationPreset::Baseline.defaults(1);
        touch.scenario = SimulationScenario::TouchJudgeBurst;
        assert!(workload_deltas(&chat).chat_messages > workload_deltas(&SimulationPreset::Baseline.defaults(1)).chat_messages);
        assert!(workload_deltas(&touch).touch_batches > workload_deltas(&SimulationPreset::Baseline.defaults(1)).touch_batches);
        assert_eq!(SimulationScenario::parse("ready-storm"), Some(SimulationScenario::ReadyStorm));
    }

    #[test]
    fn sample_data_is_seeded() {
        assert_ne!(SimulationManager::sample_touches(1)[0].time_ms, SimulationManager::sample_touches(2)[0].time_ms);
        assert_eq!(SimulationManager::sample_judges(114_514).len(), 8);
    }

    #[test]
    fn suite_plans_are_repeatable() {
        let smoke = SimulationSuite::Smoke.plan(99);
        assert_eq!(smoke.len(), 2);
        assert_eq!(smoke[0].suite, SimulationSuite::Smoke);
        assert_eq!(smoke[0].config.scenario, SimulationScenario::Balanced);
        assert_eq!(smoke[1].config.scenario, SimulationScenario::Idle);
        assert_eq!(SimulationSuite::parse("touch"), None);
        assert_eq!(SimulationSuite::parse("stress"), Some(SimulationSuite::Stress));
    }

    #[test]
    fn suite_report_sums_step_counters() {
        let mut counters = SimulationCounters::default();
        counters.ticks = 2;
        counters.chat_messages = 3;
        counters.ready_events = 4;
        assert_eq!(counters.workload_events(), 7);
        let mut other = SimulationCounters::default();
        other.touch_batches = 5;
        counters.add_assign(&other);
        assert_eq!(counters.touch_batches, 5);
        assert_eq!(counters.workload_events(), 12);
    }

    #[tokio::test]
    async fn start_builds_isolated_shadow_world() {
        let manager = SimulationManager::new();
        let status = manager.start(SimulationPreset::Small.defaults(7)).await.unwrap();
        assert!(status.running);
        assert_eq!(status.virtual_users, 100);
        let world = manager.world_snapshot(5).await.unwrap();
        assert_eq!(world.users_total, 100);
        assert_eq!(world.rooms_total, 10);
        assert!(!world.sample_rooms.is_empty());
        assert!(world.sample_rooms[0].hidden);
    }
}

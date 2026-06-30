//! Runtime v2 simulation manager.
//!
//! Step 3 creates an isolated **shadow world** for deterministic simulation.
//! Shadow users/rooms/rounds are stored only inside [`SimulationManager`]; they
//! are not inserted into the real `PlusServerState.rooms` / `users` maps, are not
//! returned by `/api/rooms`, and are not written to normal persistence tables.
//! This gives us something useful to inspect and test before moving any real
//! Room/Session state-machine logic.

use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SimulationPreset {
    Baseline,
    Small,
    Medium,
    Large,
    Custom,
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
                note: "Runtime v2 simulation manager is installed; no real rooms/users are modified.".to_string(),
            })),
        }
    }

    pub async fn status(&self) -> SimulationStatus {
        let state = self.state.read().await;
        self.snapshot(&state)
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
                "shadow world started: preset={} users={} rooms={} seed={}",
                config.preset.as_str(),
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
            "simulation {} started with isolated shadow world; real rooms/users are untouched",
            config.preset.as_str()
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
        let mut state = self.state.write().await;
        if !state.running {
            return Err("simulation is not running".to_string());
        }
        advance_state(&mut state, count);
        Ok(self.snapshot(&state))
    }

    pub async fn advance_ticks_for_run(&self, run_id: Uuid, count: u64) -> Result<SimulationStatus, String> {
        let mut state = self.state.write().await;
        if !state.running || state.run_id != Some(run_id) {
            return Err("simulation run changed or stopped".to_string());
        }
        advance_state(&mut state, count);
        Ok(self.snapshot(&state))
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

fn advance_state(state: &mut SimulationState, count: u64) {
    let count = count.clamp(1, 10_000);
    for _ in 0..count {
        let tick = state.counters.ticks + 1;
        let config = state.config.clone();

        state.counters.ticks = tick;
        if config.chat {
            state.counters.chat_messages += (config.users.max(1) / 10).max(1) as u64;
        }
        if config.ready {
            state.counters.ready_events += config.users.max(1) as u64;
        }
        if config.touch {
            state.counters.touch_batches += config.rooms.max(1) as u64;
        }
        if config.judge {
            state.counters.judge_batches += config.rooms.max(1) as u64;
        }
        if config.rounds {
            state.counters.round_results += config.rooms.max(1) as u64;
        }

        let counters_snapshot = state.counters.clone();
        if let Some(world) = &mut state.world {
            advance_shadow_world(world, tick, &config, &counters_snapshot);
        }
    }
    state.last_tick_at_ms = Some(now_ms());
    state.note = format!("simulation advanced by {count} deterministic tick(s)");
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

fn advance_shadow_world(
    world: &mut SimulationWorld,
    tick: u64,
    config: &SimulationConfig,
    counters: &SimulationCounters,
) {
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
        assert_eq!(config.preset, SimulationPreset::Custom);
        assert_eq!(config.users, 123);
        assert!(!config.touch);
        assert!(!config.auto_tick);
        assert_eq!(config.tick_interval_ms, 250);
        assert_eq!(config.persist_every_ticks, 5);
    }

    #[test]
    fn sample_data_is_seeded() {
        assert_ne!(SimulationManager::sample_touches(1)[0].time_ms, SimulationManager::sample_touches(2)[0].time_ms);
        assert_eq!(SimulationManager::sample_judges(114_514).len(), 8);
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

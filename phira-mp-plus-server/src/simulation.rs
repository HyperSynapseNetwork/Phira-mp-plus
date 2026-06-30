//! Runtime v2 simulation skeleton.
//!
//! Step 2 still does not create virtual users or rooms. It adds safe lifecycle
//! state (`run`/`stop`/`cleanup`) and deterministic sample data so CLI/TUI/Web
//! integration can be tested before touching the real room/session state machine.

use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::RwLock;
use uuid::Uuid;

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
            return Err("simulation users is too large for the safe skeleton stage".to_string());
        }
        if self.rooms > 20_000 {
            return Err("simulation rooms is too large for the safe skeleton stage".to_string());
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationStatus {
    pub running: bool,
    pub run_id: Option<Uuid>,
    pub seed: u64,
    pub config: SimulationConfig,
    pub virtual_users: usize,
    pub virtual_rooms: usize,
    pub note: String,
}

#[derive(Debug)]
struct SimulationState {
    running: bool,
    run_id: Option<Uuid>,
    config: SimulationConfig,
    virtual_users: usize,
    virtual_rooms: usize,
    note: String,
}

#[derive(Debug)]
pub struct SimulationManager {
    seed: AtomicU64,
    state: Arc<RwLock<SimulationState>>,
}

pub const DEFAULT_SIMULATION_SEED: u64 = 114_514;

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
                note: "Runtime v2 simulation skeleton is installed; no virtual rooms are created in step 2.".to_string(),
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
        state.running = true;
        state.run_id = Some(run_id);
        state.virtual_users = config.users;
        state.virtual_rooms = config.rooms;
        state.note = format!(
            "simulation {} started in safe skeleton mode; virtual users/rooms are counters only and do not touch real room state",
            config.preset.as_str()
        );
        state.config = config;
        Ok(self.snapshot(&state))
    }

    pub async fn stop(&self, reason: impl Into<String>) -> SimulationStatus {
        let mut state = self.state.write().await;
        let reason = reason.into();
        state.running = false;
        state.note = if let Some(run_id) = state.run_id.take() {
            format!("simulation {run_id} stopped: {reason}")
        } else {
            format!("simulation is not running: {reason}")
        };
        state.virtual_users = 0;
        state.virtual_rooms = 0;
        self.snapshot(&state)
    }

    pub async fn cleanup(&self) -> SimulationStatus {
        let mut state = self.state.write().await;
        state.running = false;
        state.run_id = None;
        state.virtual_users = 0;
        state.virtual_rooms = 0;
        state.note = "simulation memory state cleaned; real rooms/users were not touched".to_string();
        self.snapshot(&state)
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
        SimulationStatus {
            running: state.running,
            run_id: state.run_id,
            seed: self.seed.load(Ordering::Relaxed),
            config: state.config.clone(),
            virtual_users: state.virtual_users,
            virtual_rooms: state.virtual_rooms,
            note: state.note.clone(),
        }
    }
}

impl Default for SimulationManager {
    fn default() -> Self {
        Self::new()
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
    }

    #[test]
    fn custom_kv_updates_config() {
        let mut config = SimulationConfig::default();
        config.apply_kv("users", "123").unwrap();
        config.apply_kv("touch", "false").unwrap();
        assert_eq!(config.preset, SimulationPreset::Custom);
        assert_eq!(config.users, 123);
        assert!(!config.touch);
    }

    #[test]
    fn sample_data_is_seeded() {
        assert_ne!(SimulationManager::sample_touches(1)[0].time_ms, SimulationManager::sample_touches(2)[0].time_ms);
        assert_eq!(SimulationManager::sample_judges(114_514).len(), 8);
    }
}

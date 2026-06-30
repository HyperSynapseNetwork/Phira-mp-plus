//! Runtime v2 simulation skeleton.
//!
//! Step 1 deliberately does not create virtual users or rooms.  It only gives
//! the server a durable place to store simulation configuration/status and a
//! deterministic sample data generator.  Real state mutation will be added in
//! later patches after data isolation rules are in place.

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

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            preset: SimulationPreset::Baseline,
            users: 20,
            rooms: 5,
            duration_secs: 60,
            touch: true,
            judge: true,
            seed: DEFAULT_SIMULATION_SEED,
        }
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
                note: "Runtime v2 simulation skeleton is installed; no virtual rooms are created in step 1.".to_string(),
            })),
        }
    }

    pub async fn status(&self) -> SimulationStatus {
        let state = self.state.read().await;
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

    pub async fn set_seed(&self, seed: u64) {
        self.seed.store(seed, Ordering::Relaxed);
        let mut state = self.state.write().await;
        state.config.seed = seed;
        state.note = format!("simulation seed updated to {seed}; deterministic replay will use it in later steps");
    }

    pub async fn cleanup(&self) -> SimulationStatus {
        let mut state = self.state.write().await;
        state.running = false;
        state.run_id = None;
        state.virtual_users = 0;
        state.virtual_rooms = 0;
        state.note = "simulation memory state cleaned; real rooms/users were not touched".to_string();
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
}

impl Default for SimulationManager {
    fn default() -> Self {
        Self::new()
    }
}

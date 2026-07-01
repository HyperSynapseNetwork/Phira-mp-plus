//! Runtime v2 realistic simulation runner.
//!
//! Creates real `Room` and `User` objects through their normal constructors,
//! inserts them into `PlusServerState.rooms` / `.users`, and drives
//! lifecycle events directly.  Unlike the shadow-world tick counter, this
//! path exercises the actual state machine so performance/load tests cover
//! real room/user overhead.
//!
//! All simulation entities use reserved ID ranges:
//! - User IDs: `-2_000_000_000 - offset` (avoiding real users (`>0`) and
//!   game monitors (`-1_000_000_000`))
//! - Room IDs: `sim-*` (filtered by hidden flag in Web API)

use crate::l10n::Language;
use crate::room::Room;
use crate::server::PlusServerState;
use crate::session::User;
use crate::simulation::{SimulationConfig, SimulationCounters};
use phira_mp_common::RoomId;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;
use uuid::Uuid;

/// Base for simulation user IDs (below game-monitor range).
const SIM_USER_BASE: i32 = -2_000_000_000;

/// Maximum realistic entities per run to keep memory bounded.
const MAX_REALISTIC_USERS: usize = 500;
const MAX_REALISTIC_ROOMS: usize = 100;

/// Check whether a room ID belongs to a simulation world.
pub fn is_simulation_room_id(id: &str) -> bool {
    id.starts_with("sim-")
}

/// Check whether a user ID belongs to a simulation world.
pub fn is_simulation_user_id(id: i32) -> bool {
    id <= SIM_USER_BASE
}

/// Realistic runner that creates real rooms and users inside
/// `PlusServerState` for true lifecycle pressure testing.
pub struct RealisticSimulationRunner {
    pub run_id: Uuid,
    pub config: SimulationConfig,
    pub rooms: Vec<Arc<Room>>,
    pub user_ids: Vec<i32>,
    pub counters: Arc<RwLock<SimulationCounters>>,
    started_at: tokio::time::Instant,
}

impl RealisticSimulationRunner {
    /// Create a new realistic run and populate `state` with virtual
    /// rooms and users.
    pub async fn start(
        state: &Arc<PlusServerState>,
        config: SimulationConfig,
    ) -> Result<Self, String> {
        config.validate()?;
        let run_id = Uuid::new_v4();
        let mut rooms = Vec::new();
        let mut user_ids = Vec::new();
        let users_to_create = config.users.min(MAX_REALISTIC_USERS);
        let rooms_to_create = config.rooms.min(MAX_REALISTIC_ROOMS);

        // Create virtual users (headless — no Session objects)
        for i in 0..users_to_create {
            let uid = SIM_USER_BASE - i as i32;
            let name = format!("sim-player-{i:05}");
            let user = Arc::new(User::new(
                uid,
                name,
                Language::default(),
                Arc::clone(state),
                None,
            ));
            state.users.write().await.insert(uid, user);
            user_ids.push(uid);
        }

        // Create virtual rooms
        for i in 0..rooms_to_create {
            let rid_str = format!("sim-{i:04}");
            let rid: RoomId = rid_str
                .clone()
                .try_into()
                .map_err(|_| format!("invalid room id: {rid_str}"))?;
            let host_id = user_ids[i % user_ids.len()];
            let host = state
                .users
                .read()
                .await
                .get(&host_id)
                .map(Arc::clone)
                .ok_or_else(|| format!("sim user {host_id} not found"))?;

            let room = Arc::new(Room::new(
                rid.clone(),
                Arc::downgrade(&host),
                Some(Arc::clone(&state.plugin_manager)),
                Arc::downgrade(state),
                state.config.max_users_per_room.unwrap_or(8),
                Some(Arc::clone(&state.round_store)),
            ));
            room.set_hidden(true);
            state.rooms.write().await.insert(rid, room.clone());
            rooms.push(room);
        }

        info!(
            run_id = %run_id,
            users = user_ids.len(),
            rooms = rooms.len(),
            "realistic simulation started"
        );

        // Publish EventBus event for subscribers (broadcast etc.)
        state.publish_runtime_event(crate::event_bus::MpEvent::SimulationStarted { run_id });

        Ok(Self {
            run_id,
            config,
            rooms,
            user_ids,
            counters: Arc::new(RwLock::new(SimulationCounters::default())),
            started_at: tokio::time::Instant::now(),
        })
    }

    /// Seconds elapsed since start.
    pub fn elapsed_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Remove all virtual rooms and users from `state`.
    pub async fn cleanup(&mut self, state: &Arc<PlusServerState>) {
        {
            let mut rooms = state.rooms.write().await;
            for room in &self.rooms {
                rooms.remove(&room.id);
            }
        }
        {
            let mut users = state.users.write().await;
            for uid in &self.user_ids {
                users.remove(uid);
            }
        }
        info!(run_id = %self.run_id, "realistic simulation cleaned up");

        // Publish stop event for subscribers
        state.publish_runtime_event(crate::event_bus::MpEvent::SimulationStopped {
            run_id: self.run_id,
            reason: "cleanup".to_string(),
        });

        self.rooms.clear();
        self.user_ids.clear();
    }
}

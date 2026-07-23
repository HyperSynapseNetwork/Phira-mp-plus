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
            // TODO(Phase2-WorkC): Set hidden through gateway once the room
            // mailbox is registered. The hidden flag is now actor-owned.
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

    /// Advance one tick of simulation activity.
    ///
    /// Simulates: chat messages, ready toggles, gameplay cycles (SelectChart
    /// → start → touches/judges → results → back to SelectChart).
    /// Uses deterministic data from `SimulationManager::sample_*` so same
    /// seed produces the same activity pattern.
    /// Records all events to `mp_sim_events` when PG is configured.
    pub async fn tick(&self, state: &Arc<PlusServerState>, seed: u64) -> SimulationCounters {
        let mut counters = SimulationCounters::default();
        let mut counts_by_room: Vec<(usize, usize)> = Vec::new();
        for room in &self.rooms {
            let user_ids_here = {
                let users = room.users().await;
                users.iter().map(|u| u.id).collect::<Vec<_>>()
            };
            if user_ids_here.is_empty() {
                continue;
            }

            // 1. Chat: pick a user and simulate a message (skip if user gone)
            let chatter = user_ids_here[0];
            if let Some(user) = state.users.read().await.get(&chatter).map(Arc::clone) {
                room.send_as(&user, format!("sim chat from user {chatter}"))
                    .await;
                counters.chat_messages += 1;
            }

            // 2. Room lifecycle: DISABLED after Phase 2 Work C — Room no longer
            // holds state/chart fields. State transitions must go through the
            // RoomCommandGateway. TODO(Phase2-WorkD): Route simulation lifecycle
            // through gateway once mailbox support for simulated rooms is added.
            // The shadow-world simulation in simulation.rs remains the primary
            // pressure path.
            counts_by_room.push((user_ids_here.len(), 1));
        }
        counters.ticks += 1;
        {
            let mut c = self.counters.write().await;
            c.add_assign(&counters);
        }

        // Record tick summary to mp_sim_events when PG is available
        state.db_manager.record_sim_event_sync(
            Some(self.run_id.to_string()),
            "sim.tick",
            serde_json::json!({
                "tick": counters.ticks,
                "chat": counters.chat_messages,
                "ready": counters.ready_events,
                "touch": counters.touch_batches,
                "judge": counters.judge_batches,
                "round": counters.round_results,
                "rooms": self.rooms.len(),
                "users": self.user_ids.len(),
            }),
        );
        counters
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

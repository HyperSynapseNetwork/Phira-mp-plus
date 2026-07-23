//! Shared utilities for benchmark scenario implementations.
//!
//! Provides mapping functions, simulation config building,
//! metric conversion, and a generic simulation runner loop.
//! Each scenario calls [`run_simulation`] with a customization
//! closure to set scenario-specific fields on the
//! [`SimulationConfig`] before starting.

use crate::benchmark::command::BenchmarkPreset;
use crate::benchmark::config::BenchmarkConfig;
use crate::benchmark::metrics::BenchmarkMetrics;
use crate::simulation::{
    SimulationConfig, SimulationManager, SimulationPreset, SimulationScenario,
};
use std::time::SystemTime;

// ---------------------------------------------------------------------------
// Mapping helpers
// ---------------------------------------------------------------------------

/// Map a benchmark preset to the legacy simulation preset scale.
pub fn map_preset(preset: BenchmarkPreset) -> SimulationPreset {
    match preset {
        BenchmarkPreset::Quick => SimulationPreset::Baseline,
        BenchmarkPreset::Standard => SimulationPreset::Small,
        BenchmarkPreset::Stress => SimulationPreset::Medium,
        BenchmarkPreset::Soak => SimulationPreset::Large,
        BenchmarkPreset::Custom => SimulationPreset::Custom,
    }
}

/// Pick the simulation workload profile that best approximates the
/// benchmark scenario's intent.
pub fn pick_scenario(config: &BenchmarkConfig) -> SimulationScenario {
    use crate::benchmark::command::BenchmarkScenario;
    match config.scenario {
        BenchmarkScenario::RoomLifecycle => SimulationScenario::RoundStorm,
        BenchmarkScenario::Gameplay => SimulationScenario::TouchJudgeBurst,
        BenchmarkScenario::Connection => SimulationScenario::Balanced,
        BenchmarkScenario::SteadyState => SimulationScenario::Idle,
        BenchmarkScenario::HotRoom => SimulationScenario::Balanced,
        BenchmarkScenario::SlowConsumer => SimulationScenario::Balanced,
        BenchmarkScenario::Reconnect => SimulationScenario::Balanced,
        BenchmarkScenario::PluginLoad => SimulationScenario::Balanced,
        BenchmarkScenario::DatabaseWrite => SimulationScenario::Balanced,
        BenchmarkScenario::Mixed => SimulationScenario::Balanced,
        BenchmarkScenario::LongRun => SimulationScenario::Balanced,
    }
}

// ---------------------------------------------------------------------------
// Config construction
// ---------------------------------------------------------------------------

/// Build a [`SimulationConfig`] from a [`BenchmarkConfig`].
///
/// Starts from the preset's defaults, then applies the benchmark config's
/// scale parameters and scenario workload profile.  The returned config
/// may be further customised by each scenario via the `tweak` closure in
/// [`run_simulation`].
pub fn build_sim_config(config: &BenchmarkConfig) -> SimulationConfig {
    let preset = map_preset(config.preset);
    let mut sc = preset.defaults(config.seed);
    sc.scenario = pick_scenario(config);
    sc.users = config.clients as usize;
    sc.rooms = config.rooms as usize;
    sc.duration_secs = config.duration.as_secs();
    sc.tick_interval_ms = config.tick_interval_ms;
    sc.auto_tick = false; // manual advance in our loop
    if config.persist_events {
        sc.persist_every_ticks = 10;
    }
    sc
}

// ---------------------------------------------------------------------------
// Metrics conversion
// ---------------------------------------------------------------------------

/// Convert a finished [`SimulationStatus`] into a [`BenchmarkMetrics`]
/// snapshot.
pub fn status_to_metrics(status: &crate::simulation::SimulationStatus) -> BenchmarkMetrics {
    use crate::benchmark::metrics::{DatabaseMetrics, LatencyPercentiles, TouchJudgeMetrics};

    let elapsed_secs = status.elapsed_secs.max(1);
    let workload = status.counters.workload_events();
    let rate = workload as f64 / elapsed_secs as f64;

    BenchmarkMetrics {
        commands_per_sec: rate,
        messages_per_sec: rate,
        latency: LatencyPercentiles::default(),
        event_bus_depth: 0,
        persistence_queue_depth: 0,
        send_queue_depth: 0,
        database: DatabaseMetrics::default(),
        touch_judge: TouchJudgeMetrics {
            touch_committed: status.counters.touch_batches,
            judge_committed: status.counters.judge_batches,
            ..Default::default()
        },
        errors_total: 0,
        invariant_violations: 0,
        rss_bytes: 0,
        allocated_bytes: 0,
        gc_pause_ms: 0.0,
        captured_at_ms: now_ms(),
        connect_latency_ms: 0,
        cpu: crate::benchmark::metrics::CpuMetrics::default(),
        gc_pauses: 0,
        session_command_queue_depth: 0,
        room_mailbox_depth: 0,
        plugin_event_queue_depth: 0,
        telemetry_queue_depth: 0,
        elapsed_secs: status.elapsed_secs,
    }
}

/// Current unix-epoch milliseconds.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Simulation runner
// ---------------------------------------------------------------------------

/// Run a simulation for the configured duration and collect metrics.
///
/// The `tweak` closure receives a mutable reference to the
/// [`SimulationConfig`] so each scenario can override scenario-specific
/// fields (e.g. disable chat, enable only touch/judge, change tick rate)
/// before the simulation starts.
pub async fn run_simulation(
    config: &BenchmarkConfig,
    tweak: impl FnOnce(&mut SimulationConfig),
) -> Result<BenchmarkMetrics, String> {
    let mut sim_config = build_sim_config(config);
    tweak(&mut sim_config);
    sim_config
        .validate()
        .map_err(|e| format!("simulation config validation failed: {e}"))?;

    let manager = SimulationManager::new();
    manager
        .start(sim_config.clone())
        .await
        .map_err(|e| format!("failed to start simulation: {e}"))?;

    let total_ticks = config.duration.as_millis() as u64 / config.tick_interval_ms;
    let mut remaining = total_ticks;
    const BATCH_SIZE: u64 = 100;

    while remaining > 0 {
        let batch = remaining.min(BATCH_SIZE);
        manager
            .advance_ticks(batch)
            .await
            .map_err(|e| format!("simulation tick error: {e}"))?;
        remaining = remaining.saturating_sub(batch);
    }

    let status = manager.stop("scenario completed").await;
    Ok(status_to_metrics(&status))
}

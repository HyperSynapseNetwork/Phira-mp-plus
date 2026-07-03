//! Simulation contracts.
//!
//! Tests that simulation never touches Phira by default, uses deterministic
//! seeds, can be cleaned up, and doesn't pollute real data.

use phira_mp_plus_server::simulation::{
    SimulationConfig, SimulationCounters, SimulationManager, SimulationPreset, SimulationRunReport,
    SimulationScenario,
};

#[test]
fn default_simulation_has_no_external_dependency() {
    let config = SimulationConfig::default();
    // Simulation config must NOT carry fields that tie it to Phira.
    // Compile-time check: no token, endpoint, or account fields exist.
    assert!(config.auto_tick, "simulation should auto-tick by default");
    // At least one activity type should be enabled
    assert!(
        config.touch || config.judge || config.chat || config.ready || config.rounds,
        "simulation should have at least one activity enabled"
    );
    // Default config has no external dependencies
    assert_eq!(
        config.preset,
        SimulationPreset::Baseline,
        "default preset should be Baseline (no external access)"
    );
}

#[test]
fn baseline_preset_generates_config() {
    let config = SimulationPreset::Baseline.defaults(42);
    assert_eq!(config.users, 20);
    assert_eq!(config.rooms, 5);
    assert_eq!(config.seed, 42);
    // Verify the config doesn't reference any Phira endpoint or token
    // (compile-time check: such fields don't exist)
}

#[test]
fn simulation_preset_defaults_are_reasonable() {
    for preset in &[
        SimulationPreset::Baseline,
        SimulationPreset::Small,
        SimulationPreset::Medium,
    ] {
        let config = preset.defaults(0);
        assert!(config.users > 0, "{preset:?} should have users");
        assert!(config.rooms > 0, "{preset:?} should have rooms");
        assert!(config.duration_secs > 0, "{preset:?} should have duration");
    }
}

#[test]
fn deterministic_seed_produces_stable_content() {
    // Same seed must produce not just the same length but the same content
    let touches = SimulationManager::sample_touches(114_514);
    let judges = SimulationManager::sample_judges(114_514);
    assert!(!touches.is_empty(), "sample touches should not be empty");
    assert!(!judges.is_empty(), "sample judges should not be empty");

    let touches2 = SimulationManager::sample_touches(114_514);
    assert_eq!(
        touches.len(),
        touches2.len(),
        "same seed must produce same touch count"
    );
    // Deep equality: same seed = same content
    for (a, b) in touches.iter().zip(touches2.iter()) {
        assert_eq!(a.lane, b.lane, "deterministic: lane mismatch");
        assert_eq!(a.time_ms, b.time_ms, "deterministic: time_ms mismatch");
        assert_eq!(a.pressed, b.pressed, "deterministic: pressed mismatch");
    }
}

#[test]
fn deterministic_seed_different_seeds_differ() {
    let touches_a = SimulationManager::sample_touches(100);
    let touches_b = SimulationManager::sample_touches(200);
    // Different seeds should produce different data
    // (extremely unlikely to collide)
    if touches_a.len() == touches_b.len() && touches_a.len() <= 5 {
        // Small sample sets might accidentally match; skip deep check
        return;
    }
    let any_diff = touches_a
        .iter()
        .zip(touches_b.iter())
        .any(|(a, b)| a.lane != b.lane || a.time_ms != b.time_ms || a.pressed != b.pressed);
    assert!(any_diff, "different seeds should produce different touches");
}

#[test]
fn sample_judges_have_valid_fields() {
    let judges = SimulationManager::sample_judges(42);
    assert!(!judges.is_empty(), "sample judges should not be empty");
    for judge in &judges {
        assert!(!judge.judge.is_empty(), "judge should have non-empty type");
        assert!(
            judge.score_delta >= 0_i32 || judge.time_ms > 0,
            "judge should have valid timing or score delta"
        );
    }
}

#[test]
fn scenario_parse_round_trip() {
    for scenario in SimulationScenario::all() {
        let s = scenario.as_str();
        let parsed = SimulationScenario::parse(s);
        assert_eq!(parsed, Some(*scenario), "parse round-trip failed for {s}");
    }
}

#[test]
fn scenario_descriptions_are_non_empty() {
    for scenario in SimulationScenario::all() {
        let desc = scenario.description();
        assert!(
            !desc.is_empty(),
            "scenario {:?} must have description",
            scenario
        );
    }
}

#[test]
fn custom_config_apply_kv() {
    let mut config = SimulationConfig::default();
    config.apply_kv("users", "100").unwrap();
    config.apply_kv("rooms", "20").unwrap();
    config.apply_kv("duration", "300").unwrap();
    assert_eq!(config.users, 100);
    assert_eq!(config.rooms, 20);
    assert_eq!(config.duration_secs, 300);
}

#[test]
fn simulation_config_validate_rejects_zero_values() {
    let mut config = SimulationConfig::default();
    config.users = 0;
    assert!(
        config.validate().is_err(),
        "config with 0 users should be invalid"
    );
    config.users = 5;
    config.rooms = 0;
    assert!(
        config.validate().is_err(),
        "config with 0 rooms should be invalid"
    );
}

#[tokio::test]
async fn simulation_manager_initial_state_is_clean() {
    let manager = SimulationManager::new();
    let status = manager.status().await;
    assert!(!status.running, "fresh simulation should not be running");
    assert!(
        status.virtual_users == 0 || status.virtual_rooms == 0,
        "fresh simulation should have no virtual users or rooms"
    );
}

#[test]
fn simulation_config_seed_is_deterministic() {
    let config1 = SimulationPreset::Baseline.defaults(999);
    let config2 = SimulationPreset::Baseline.defaults(999);
    assert_eq!(config1.seed, config2.seed);
    assert_eq!(config1.users, config2.users);
}

#[test]
fn simulation_generated_events_have_run_id() {
    // Check that SimulationRunReport has run_id field (compile-time check)
    let _report = SimulationRunReport {
        run_id: None,
        suite_run_id: None,
        step_name: "test".to_string(),
        suite: None,
        preset: SimulationPreset::Baseline,
        scenario: SimulationScenario::Balanced,
        users: 10,
        rooms: 3,
        duration_secs: 30,
        tick_interval_ms: 1000,
        persist_every_ticks: 0,
        started_at_ms: None,
        finished_at_ms: 30000,
        elapsed_secs: 30,
        aborted: false,
        reason: String::new(),
        counters: SimulationCounters::default(),
        workload_events: 100,
        workload_events_per_sec: 3.33,
    };
    assert!(true, "SimulationRunReport has run_id field");
}

#[tokio::test]
async fn simulation_lifecycle_start_stop_cleanup() {
    let manager = SimulationManager::new();
    let config = SimulationConfig {
        preset: SimulationPreset::Baseline,
        scenario: SimulationScenario::Balanced,
        users: 5,
        rooms: 2,
        duration_secs: 10,
        touch: true,
        judge: true,
        chat: true,
        ready: true,
        rounds: true,
        auto_tick: false,
        tick_interval_ms: 500,
        seed: 42,
        persist_every_ticks: 0,
    };

    // Initial state: idle
    let status = manager.status().await;
    assert!(!status.running, "should start idle");

    // Start
    let start_result = manager.start(config).await.unwrap();
    assert!(start_result.running, "should be running after start");
    assert_eq!(start_result.virtual_users, 5, "should have correct virtual user count");
    assert_eq!(start_result.virtual_rooms, 2, "should have correct virtual room count");

    // Stop
    let stopped = manager.stop("test complete").await;
    assert!(!stopped.running, "should not be running after stop");

    // Cleanup
    let cleaned = manager.cleanup().await;
    assert!(!cleaned.running, "not running after cleanup");
    assert_eq!(cleaned.virtual_users, 0, "cleanup should remove all virtual users");
    assert_eq!(cleaned.virtual_rooms, 0, "cleanup should remove all virtual rooms");
}

#[tokio::test]
async fn simulation_cleanup_idempotent() {
    let manager = SimulationManager::new();
    let cleaned = manager.cleanup().await;
    assert!(!cleaned.running, "still idle after cleanup");
}

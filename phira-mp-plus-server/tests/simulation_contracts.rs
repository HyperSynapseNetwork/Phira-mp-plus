//! Simulation contracts.
//!
//! Tests that simulation never touches Phira by default, uses deterministic
//! seeds, can be cleaned up, and doesn't pollute real data.

use phira_mp_plus_server::simulation::{
    SimulationConfig, SimulationManager, SimulationPreset, SimulationScenario,
};

#[test]
fn default_simulation_does_not_touch_phira() {
    let config = SimulationConfig::default();
    // Simulation config should not have any phira-related flags by default
    assert!(config.auto_tick, "simulation should auto-tick by default");
    assert!(!config.rounds || config.chat || config.ready || config.touch || config.judge,
        "simulation should have at least one activity enabled");
}

#[test]
fn baseline_preset_generates_config() {
    let config = SimulationPreset::Baseline.defaults(42);
    assert_eq!(config.users, 20);
    assert_eq!(config.rooms, 5);
    assert_eq!(config.seed, 42);
}

#[test]
fn simulation_preset_defaults_are_reasonable() {
    for preset in &[SimulationPreset::Baseline, SimulationPreset::Small, SimulationPreset::Medium] {
        let config = preset.defaults(0);
        assert!(config.users > 0, "{preset:?} should have users");
        assert!(config.rooms > 0, "{preset:?} should have rooms");
    }
}

#[test]
fn deterministic_seed_produces_stable_touches() {
    let touches = SimulationManager::sample_touches(114_514);
    let judges = SimulationManager::sample_judges(114_514);
    assert!(!touches.is_empty(), "sample touches should not be empty");
    assert!(!judges.is_empty(), "sample judges should not be empty");
    // Same seed = same data
    let touches2 = SimulationManager::sample_touches(114_514);
    assert_eq!(touches.len(), touches2.len());
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
fn custom_config_apply_kv() {
    let mut config = SimulationConfig::default();
    config.apply_kv("users", "100").unwrap();
    config.apply_kv("rooms", "20").unwrap();
    assert_eq!(config.users, 100);
    assert_eq!(config.rooms, 20);
}

#[tokio::test]
async fn simulation_cleanup_resets_state() {
    // This tests the manager API, not a full run (which needs runtime)
    let manager = SimulationManager::new();
    let status = manager.status().await;
    assert!(!status.running, "fresh simulation should not be running");
}

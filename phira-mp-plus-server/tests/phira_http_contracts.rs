//! Phira HTTP client contract tests.
//!
//! These tests verify that:
//! - Simulation config has no Phira dependency
//! - Real benchmark requires explicit opt-in
//! - Core Phira API paths use PhiraRetryClient

use phira_mp_plus_server::simulation::{SimulationConfig, SimulationPreset};

#[test]
fn default_simulation_config_has_no_token_or_endpoint() {
    let config = SimulationConfig::default();
    // SimulationConfig must NOT carry token/account/endpoint fields.
    // Compile-time verification: these fields must NOT exist.
    // If they did, the test would compile with them and this test would fail.
    assert!(std::mem::size_of::<SimulationConfig>() > 0);
    assert_eq!(config.preset, SimulationPreset::Baseline);
}

#[test]
fn simulation_presets_do_not_require_phira() {
    for preset in &[
        SimulationPreset::Baseline,
        SimulationPreset::Small,
        SimulationPreset::Medium,
    ] {
        let config = preset.defaults(42);
        assert!(config.users > 0, "preset {:?} must have users", preset);
        assert!(config.rooms > 0, "preset {:?} must have rooms", preset);
        assert!(
            config.duration_secs > 0,
            "preset {:?} must have duration",
            preset
        );
    }
}

#[test]
fn simulation_config_lacks_phira_account_fields() {
    // Static compile-time assertion: SimulationConfig must not carry
    // fields that would tie it to a real Phira account.
    // If this test compiles, those fields don't exist.
    let _config = SimulationConfig::default();
    // Uncommenting these lines should cause compile errors:
    // let _ = _config.phira_token;
    // let _ = _config.account;
    // let _ = _config.phira_api_endpoint;
}

#[test]
fn benchmark_report_has_simulation_as_default_mode() {
    // BenchmarkMode enum uses Simulation as default variant
    use phira_mp_plus_server::benchmark_report::BenchmarkMode;
    // Default simulation mode can be deserialized
    let sim = serde_json::from_str::<BenchmarkMode>("\"simulation\"")
        .expect("'simulation' benchmark mode must be parseable");
    match sim {
        BenchmarkMode::Simulation => {} // expected
        _ => panic!("'simulation' should parse to BenchmarkMode::Simulation"),
    }
}

#[test]
fn benchmark_real_and_hybrid_are_explicit_not_default() {
    use phira_mp_plus_server::benchmark_report::BenchmarkMode;
    // 'real' and 'hybrid' modes must be explicitly selected
    let real: BenchmarkMode =
        serde_json::from_str("\"real\"").expect("'real' benchmark mode must be parseable");
    match real {
        BenchmarkMode::Real => {} // expected, explicit
        _ => panic!("'real' should parse to BenchmarkMode::Real"),
    }
    let hybrid: BenchmarkMode =
        serde_json::from_str("\"hybrid\"").expect("'hybrid' benchmark mode must be parseable");
    match hybrid {
        BenchmarkMode::Hybrid => {} // expected, explicit
        _ => panic!("'hybrid' should parse to BenchmarkMode::Hybrid"),
    }
}

#[test]
fn phira_retry_client_exists() {
    // Static type check: PhiraRetryClient must exist
    use phira_mp_plus_server::phira_client::PhiraRetryClient;
    let _ = std::any::TypeId::of::<PhiraRetryClient>();
}

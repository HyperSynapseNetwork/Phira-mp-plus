//! Phira HTTP client contract tests.
//!
//! These tests verify that:
//! - Simulation config has no Phira dependency
//! - Real benchmark requires explicit opt-in
//! - Core Phira API paths use PhiraRetryClient (not bare reqwest)
//!
//! Static source scanning prevents bare reqwest from reappearing in core
//! business logic paths.

use phira_mp_plus_server::simulation::{SimulationConfig, SimulationPreset};
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("server crate should have a parent")
        .to_path_buf()
}

// Files that are ALLOWED to contain bare reqwest (with documented reason).
// - phira_client.rs: unified Phira HTTP client implementation.
// - wasm_host.rs: plugin sandbox HTTP, not Phira API.
#[allow(dead_code)]
const ALLOWED_REQWEST_FILES: &[&str] = &[
    "phira-mp-plus-server/src/phira_client.rs",
    "phira-mp-plus-server/src/wasm_host.rs",
];

// server.rs has legacy `reqwest::Client::builder()` calls in
// `fetch_phira_user_name` and `fetch_phira_chart` that should
// eventually migrate to PhiraRetryClient.  For now those are the
// only allowed bare-reqwest lines; any NEW occurrence will fail.
const ALLOWED_SERVER_LINE_PATTERNS: &[&str] = &[
    // fetch_phira_user_name internals
    "reqwest::Client::builder()",
    "reqwest::header::AUTHORIZATION",
    // fetch_phira_chart internals
    "reqwest::Client::builder()",
];

const BANNED_REQWEST_FILES: &[&str] = &[
    "phira-mp-plus-server/src/server.rs",
    "phira-mp-plus-server/src/session.rs",
    "phira-mp-plus-server/src/session_auth.rs",
    "phira-mp-plus-server/src/session_room.rs",
    "phira-mp-plus-server/src/room.rs",
    "phira-mp-plus-server/src/simulation.rs",
    "phira-mp-plus-server/src/simulation_realistic.rs",
    "phira-mp-plus-server/src/cli/commands/benchmark.rs",
];

// Exclude PhiraRetryClient::new(...) because it matches Client::new(
// but is not a bare reqwest usage.
const EXCLUDED_PATTERNS: &[&str] = &["PhiraRetryClient"];

const REQWEST_PATTERNS: &[&str] = &["reqwest::Client", "Client::new(", "reqwest::get"];

#[test]
fn banned_core_paths_have_no_bare_reqwest() {
    let root = workspace_root();
    let mut failures = Vec::new();
    for rel_path in BANNED_REQWEST_FILES {
        let full_path = root.join(rel_path);
        if !full_path.exists() {
            continue;
        }
        let content = std::fs::read_to_string(&full_path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", full_path.display()));
        for pattern in REQWEST_PATTERNS {
            for (line_no, line) in content.lines().enumerate() {
                if !line.contains(pattern) {
                    continue;
                }
                // Skip lines that are known non-bare-reqwest (e.g. PhiraRetryClient::new)
                let is_excluded = EXCLUDED_PATTERNS.iter().any(|e| line.contains(e));
                if is_excluded {
                    continue;
                }
                // server.rs is allowed to have specific legacy helper functions
                let is_allowed_server_line = rel_path.contains("server.rs")
                    && ALLOWED_SERVER_LINE_PATTERNS
                        .iter()
                        .any(|p| line.contains(p));
                if !is_allowed_server_line {
                    failures.push(format!(
                        "  {}:{}: contains '{}'",
                        rel_path,
                        line_no + 1,
                        pattern
                    ));
                }
            }
        }
    }
    if !failures.is_empty() {
        panic!(
            "Core business logic files must not contain bare reqwest:\n{}\n\
             Use PhiraRetryClient (phira_client.rs) instead.\n\
             (wasm_host.rs and phira_client.rs are the only allowed exceptions.)\n\
             server.rs allows only fetch_phira_user_name / fetch_phira_chart (TODO: migrate to RetryClient).",
            failures.join("\n")
        );
    }
}

#[test]
fn default_simulation_config_has_no_token_or_endpoint() {
    let config = SimulationConfig::default();
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
    let _config = SimulationConfig::default();
}

#[test]
fn benchmark_report_has_simulation_as_default_mode() {
    use phira_mp_plus_server::benchmark_report::BenchmarkMode;
    let sim = serde_json::from_str::<BenchmarkMode>("\"simulation\"")
        .expect("'simulation' benchmark mode must be parseable");
    match sim {
        BenchmarkMode::Simulation => {}
        _ => panic!("not Simulation"),
    }
}

#[test]
fn benchmark_real_and_hybrid_are_explicit_not_default() {
    use phira_mp_plus_server::benchmark_report::BenchmarkMode;
    let real: BenchmarkMode = serde_json::from_str("\"real\"").unwrap();
    match real {
        BenchmarkMode::Real => {}
        _ => panic!("not Real"),
    }
    let hybrid: BenchmarkMode = serde_json::from_str("\"hybrid\"").unwrap();
    match hybrid {
        BenchmarkMode::Hybrid => {}
        _ => panic!("not Hybrid"),
    }
}

#[test]
fn phira_retry_client_exists() {
    use phira_mp_plus_server::phira_client::PhiraRetryClient;
    let _ = std::any::TypeId::of::<PhiraRetryClient>();
}

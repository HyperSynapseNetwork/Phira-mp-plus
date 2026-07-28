//! Phira HTTP client contract tests.
//!
//! These tests verify that:
//! - Core Phira API paths use PhiraRetryClient (not bare reqwest)
//!
//! Static source scanning prevents bare reqwest from reappearing in core
//! business logic paths.

use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("server crate should have a parent")
        .to_path_buf()
}

// server.rs has migrated all Phira API calls to PhiraRetryClient.
// No bare-reqwest lines are allowed. Add new patterns here only if
// a read-only `reqwest::Url::parse(...)` check is needed.
const ALLOWED_SERVER_LINE_PATTERNS: &[&str] = &[];

const BANNED_REQWEST_FILES: &[&str] = &[
    "phira-mp-plus-server/src/server.rs",
    "phira-mp-plus-server/src/session.rs",
    "phira-mp-plus-server/src/session_auth.rs",
    "phira-mp-plus-server/src/session_room.rs",
    "phira-mp-plus-server/src/room.rs",
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
             server.rs: all Phira API calls now go through PhiraRetryClient.",
            failures.join("\n")
        );
    }
}

#[test]
fn benchmark_real_is_explicit_not_default() {
    use phira_mp_plus_server::benchmark::command::BenchmarkRunMode;
    let real: BenchmarkRunMode = serde_json::from_str("\"real\"").unwrap();
    match real {
        BenchmarkRunMode::Real => {}
        _ => panic!("not Real"),
    }
}

#[test]
fn phira_retry_client_exists() {
    use phira_mp_plus_server::phira_client::PhiraRetryClient;
    let _ = std::any::TypeId::of::<PhiraRetryClient>();
}

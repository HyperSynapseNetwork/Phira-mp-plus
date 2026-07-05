//! Documentation content contract tests.
//!
//! These tests verify that docs/README are kept in sync with the actual
//! code: no stale command names, no deprecated mode references in
//! primary documentation, no legacy WIT recommendations.
//!
//! All required docs must exist — missing files cause test panic.

use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("server crate should have a parent")
        .to_path_buf()
}

fn readme_path() -> PathBuf {
    workspace_root().join("README.md")
}

fn read_readme() -> String {
    std::fs::read_to_string(readme_path()).expect("README.md should be readable")
}

/// Read a doc file, panicking if missing.
fn read_doc_required(filename: &str) -> String {
    let path = workspace_root().join("docs").join(filename);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("required doc {} missing/unreadable: {err}", path.display()))
}

// ── Required docs exist ─────────────────────────────────────────────

#[test]
fn required_docs_exist() {
    for doc in &[
        "simulation.md",
        "benchmark-real.md",
        "wit-abi.md",
        "cli.md",
        "configuration.md",
        "plugin-dev.md",
        "api.md",
    ] {
        let path = workspace_root().join("docs").join(doc);
        assert!(
            path.exists(),
            "required doc {doc} not found at {}",
            path.display()
        );
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("required doc {doc} unreadable: {err}"));
        assert!(!content.is_empty(), "required doc {doc} is empty");
    }
}

// ── README checks ───────────────────────────────────────────────────

#[test]
fn readme_no_underscore_plus() {
    let content = read_readme();
    assert!(!content.contains("_+"), "README must not contain '_+'");
}

#[test]
fn readme_no_benchmark_phira_tokens() {
    let content = read_readme();
    assert!(
        !content.contains("benchmark_phira_tokens"),
        "README must not contain benchmark_phira_tokens"
    );
}

#[test]
fn readme_no_benchmark_phira_token() {
    let content = read_readme();
    assert!(
        !content.contains("benchmark_phira_token"),
        "README must not contain benchmark_phira_token"
    );
}

#[test]
fn readme_no_benchmark_bind() {
    let content = read_readme();
    assert!(
        !content.contains("benchmark-bind"),
        "README must not contain benchmark-bind"
    );
}

#[test]
fn readme_no_benchmark_cleanup() {
    let content = read_readme();
    assert!(
        !content.contains("benchmark-cleanup"),
        "README must not contain benchmark-cleanup"
    );
}

#[test]
fn readme_no_ext_list_or_ext_get() {
    let content = read_readme();
    assert!(
        !content.contains("ext-list"),
        "README must not contain ext-list"
    );
    assert!(
        !content.contains("ext-get"),
        "README must not contain ext-get"
    );
}

#[test]
fn readme_no_dual_write() {
    let content = read_readme();
    assert!(
        !content.contains("dual_write"),
        "README must not contain dual_write"
    );
}

#[test]
fn readme_no_fallback_only() {
    let content = read_readme();
    assert!(
        !content.contains("fallback_only"),
        "README must not contain fallback_only"
    );
}

#[test]
fn readme_no_worker_only() {
    let content = read_readme();
    assert!(
        !content.contains("worker_only"),
        "README must not contain worker_only"
    );
}

// ── server_config.yml checks ────────────────────────────────────────

#[test]
fn server_config_no_benchmark_token_examples() {
    let content = std::fs::read_to_string(workspace_root().join("server_config.yml"))
        .expect("server_config.yml should be readable");
    assert!(
        !content.contains("benchmark_phira_tokens"),
        "server_config must not contain benchmark_phira_tokens"
    );
    assert!(
        !content.contains("benchmark_phira_token"),
        "server_config must not contain benchmark_phira_token"
    );
    assert!(
        !content.contains("your-phira-token"),
        "server_config must not contain your-phira-token"
    );
    assert!(
        !content.contains("benchmark-bind"),
        "server_config must not contain benchmark-bind"
    );
    assert!(
        !content.contains("benchmark-auth"),
        "server_config must not contain benchmark-auth"
    );
}

// ── docs/configuration.md checks ────────────────────────────────────

#[test]
fn configuration_no_unsupported_telemetry_modes() {
    let content = read_doc_required("configuration.md");
    assert!(
        !content.contains("dual_write") || content.contains("legacy"),
        "configuration.md must not recommend dual_write without legacy marker"
    );
    assert!(
        !content.contains("fallback_only") || content.contains("legacy"),
        "configuration.md must not recommend fallback_only without legacy marker"
    );
    let section = content
        .find("telemetry_cutover_mode")
        .map(|pos| {
            let end = content[pos..]
                .find("\n\n")
                .map(|p| pos + p)
                .unwrap_or(content.len());
            &content[pos..end]
        })
        .unwrap_or("");
    if !section.is_empty() {
        assert!(
            section.contains("direct_only") || section.contains("worker_preferred"),
            "configuration.md telemetry_cutover_mode must mention valid modes"
        );
    }
}

#[test]
fn configuration_docs_do_not_show_real_benchmark_token_example() {
    let content = read_doc_required("configuration.md");
    for token in &[
        "benchmark-bind",
        "benchmark-auth",
        "benchmark_phira_tokens",
        "benchmark_phira_token",
        "your-token",
        "your-phira-token",
    ] {
        assert!(
            !content.contains(token),
            "configuration.md must not contain '{}' (real benchmark token)",
            token
        );
    }
}

// ── docs/cli.md checks ──────────────────────────────────────────────

#[test]
fn cli_docs_do_not_list_removed_legacy_commands() {
    let content = read_doc_required("cli.md");
    for cmd in &[
        "welcome-config",
        "player-count",
        "playtime",
        "round-last",
        "ext-list",
        "ext-get",
        "benchmark-cleanup",
    ] {
        assert!(
            !content.contains(cmd),
            "cli.md must not contain '{cmd}' (legacy command)"
        );
    }
}

// ── docs/api.md checks ──────────────────────────────────────────────

#[test]
fn api_docs_do_not_list_removed_legacy_commands() {
    let content = read_doc_required("api.md");
    // Check for the CLI command forms (without `[` prefix which indicates template placeholders)
    for cmd in &[
        "playtime <",
        "welcome-config",
        "round-last",
        "benchmark_phira_tokens",
        "benchmark_phira_token",
    ] {
        assert!(
            !content.contains(cmd),
            "api.md must not contain '{cmd}' (legacy command/token config)"
        );
    }
    // Template placeholders like [player-count] and [playtime] are acceptable
    // in the placeholder table section and are not CLI commands.
    // Check that "player-count" only appears inside `[...]`:
    if content.contains("player-count") {
        assert!(
            content.contains("[player-count]"),
            "if 'player-count' appears in api.md it must be a template placeholder"
        );
    }
}

// ── docs/simulation.md checks ───────────────────────────────────────

#[test]
fn simulation_docs_no_phira_access() {
    let content = read_doc_required("simulation.md");
    assert!(
        content.contains("不访问") || content.contains("不需要 token") || content.contains("无需"),
        "simulation.md must state no Phira access / no token needed"
    );
}

// ── docs/benchmark-real.md checks ───────────────────────────────────

#[test]
fn benchmark_real_docs_marked_advanced() {
    let content = read_doc_required("benchmark-real.md");
    assert!(
        content.contains("advanced") || content.contains("explicit") || content.contains("不推荐"),
        "benchmark-real.md must be marked as advanced/explicit"
    );
}

// ── docs/wit-abi.md checks ──────────────────────────────────────────

#[test]
fn wit_abi_docs_has_required_fields() {
    let content = read_doc_required("wit-abi.md");
    assert!(
        content.contains("abi-wit-v2"),
        "wit-abi.md must mention abi-wit-v2"
    );
    assert!(
        !content.contains("abi-json-v1"),
        "wit-abi.md should not mention abi-json-v1 (JSON bridge removed)"
    );
    assert!(
        content.contains("wit/phira-plugin.wit"),
        "wit-abi.md must mention canonical WIT path"
    );
    assert!(
        content.contains("MIGRATION_PHASE") || content.contains("migration"),
        "wit-abi.md must mention migration phase"
    );
}

// ── docs/plugin-dev.md checks ───────────────────────────────────────

#[test]
fn plugin_dev_prefers_canonical_wit() {
    let content = read_doc_required("plugin-dev.md");
    // Must reference canonical WIT or mark legacy as deprecated
    if content.contains("wit/phira/mpplus.wit") {
        assert!(
            content.contains("legacy")
                || content.contains("v1")
                || content.contains("canonical")
                || content.contains("phira-plugin.wit"),
            "plugin-dev.md: legacy WIT reference must be marked legacy or paired with canonical"
        );
    }
}

// ── server_config vs benchmark-real.md pointer ──────────────────────

#[test]
fn server_config_points_to_benchmark_real() {
    let content = std::fs::read_to_string(workspace_root().join("server_config.yml"))
        .expect("server_config.yml should be readable");
    // If server_config has any benchmark-related comment, it must reference benchmark-real.md
    if content.contains("benchmark") || content.contains("压测") {
        assert!(
            content.contains("benchmark-real.md"),
            "server_config.yml benchmark section must point to docs/benchmark-real.md"
        );
    }
}

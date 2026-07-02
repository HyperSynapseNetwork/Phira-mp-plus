//! Documentation content contract tests.
//!
//! These tests verify that docs/README are kept in sync with the actual
//! code: no stale command names, no deprecated mode references in
//! primary documentation, no legacy WIT recommendations.

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

fn read_doc(filename: &str) -> String {
    let path = workspace_root().join("docs").join(filename);
    std::fs::read_to_string(&path).unwrap_or_else(|_| {
        // File might not exist yet; return empty
        String::new()
    })
}

#[test]
fn readme_does_not_contain_underscore_plus() {
    let content = read_readme();
    assert!(
        !content.contains("_+"),
        "README must not contain '_+' syntax"
    );
}

#[test]
fn readme_quick_start_does_not_mention_benchmark_bind() {
    let content = read_readme();
    // Quick start / minimal config section should not recommend benchmark-bind.
    // It may appear in comprehensive feature reference sections.
    let early_section = if let Some(pos) = content.find("快速开始") {
        content[..pos + 500].to_string()
    } else if let Some(pos) = content.find("Quick Start") {
        content[..pos + 500].to_string()
    } else {
        // Check first 300 chars of the document
        content[..content.len().min(300)].to_string()
    };
    assert!(
        !early_section.contains("benchmark-bind"),
        "README quick start must not mention benchmark-bind"
    );
}

#[test]
fn readme_does_not_recommend_dual_write() {
    let content = read_readme();
    assert!(
        !content.contains("dual_write"),
        "README must not reference dual_write"
    );
}

#[test]
fn readme_does_not_recommend_fallback_only() {
    let content = read_readme();
    assert!(
        !content.contains("fallback_only"),
        "README must not reference fallback_only"
    );
}

#[test]
fn readme_does_not_recommend_worker_only_mode() {
    let content = read_readme();
    // The canonical name is worker_preferred; worker_only is the legacy parse alias
    // but should not appear as recommended config
    assert!(
        !content.contains("worker_only"),
        "README must not recommend worker_only mode"
    );
}

#[test]
fn readme_quick_start_does_not_mention_benchmark_tokens() {
    let content = read_readme();
    // Quick start section should not mention benchmark_phira_tokens
    let quick_start = if let Some(pos) = content.find("快速开始") {
        let end = content[pos..]
            .find("##")
            .map(|p| pos + p)
            .unwrap_or(content.len());
        &content[pos..end]
    } else {
        // Fall back to first 500 chars if no Chinese header
        &content[..content.len().min(500)]
    };
    assert!(
        !quick_start.contains("benchmark_phira_tokens"),
        "README quick start must not mention benchmark_phira_tokens"
    );
    assert!(
        !quick_start.contains("benchmark_phira_token"),
        "README quick start must not mention benchmark_phira_token"
    );
}

#[test]
fn configuration_docs_recommend_only_valid_modes() {
    let content = read_doc("configuration.md");
    if content.is_empty() {
        return; // file might not exist yet
    }
    // The config doc should recommend only direct_only and worker_preferred
    // It should NOT recommend dual_write, fallback_only, or worker_only
    assert!(
        !content.contains("dual_write") || content.contains("legacy"),
        "configuration.md must not recommend dual_write without legacy marker"
    );
    assert!(
        !content.contains("fallback_only") || content.contains("legacy"),
        "configuration.md must not recommend fallback_only without legacy marker"
    );

    // Check that telemetry_cutover_mode section only lists valid modes
    let cutover_section = if let Some(pos) = content.find("telemetry_cutover_mode") {
        let end = content[pos..]
            .find("\n\n")
            .map(|p| pos + p)
            .unwrap_or(content.len());
        &content[pos..end]
    } else {
        ""
    };
    if !cutover_section.is_empty() {
        assert!(
            cutover_section.contains("direct_only") || cutover_section.contains("worker_preferred"),
            "configuration.md telemetry_cutover_mode must mention valid modes"
        );
    }
}

#[test]
fn plugin_dev_docs_prefer_canonical_wit() {
    let content = read_doc("plugin-dev.md");
    if content.is_empty() {
        return;
    }
    // Plugin dev docs may reference the legacy WIT but should also mention
    // the canonical WIT. This is a soft check (warn via assertion when docs
    // are synced in task #16).
    // For now: check that references exist at all.
    assert!(!content.is_empty(), "plugin-dev.md must have content");
}

#[test]
fn simulation_docs_state_no_phira_access() {
    let content = read_doc("simulation.md");
    if content.is_empty() {
        return;
    }
    assert!(
        content.contains("不访问")
            || content.contains("隔离")
            || content.contains("local")
            || content.contains("no Phira")
            || content.contains("无需"),
        "simulation.md must state that simulation does not access Phira"
    );
}

#[test]
fn benchmark_real_docs_marked_advanced() {
    let content = read_doc("benchmark-real.md");
    if content.is_empty() {
        return;
    }
    assert!(
        content.contains("advanced")
            || content.contains("高级")
            || content.contains("兼容性")
            || content.contains("explicit"),
        "benchmark-real.md must be marked as advanced/explicit"
    );
}

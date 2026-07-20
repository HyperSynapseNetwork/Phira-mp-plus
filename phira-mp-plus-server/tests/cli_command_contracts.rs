//! CLI command surface contracts.
//!
//! These tests verify the CommandRegistry help output, command counts,
//! and that canonical commands surface correctly.

use phira_mp_plus_server::command_registry::runtime_v2_registry;

#[test]
fn default_help_is_concise() {
    let registry = runtime_v2_registry();
    let overview = registry.format_overview();
    // Default help should not be a wall of text
    assert!(
        overview.len() < 4500,
        "default help too long: {} chars",
        overview.len()
    );
}

#[test]
fn default_help_shows_primary_count() {
    let registry = runtime_v2_registry();
    let (primary, advanced, developer) = registry.command_surface_counts();
    assert!(
        primary >= 15,
        "expected at least 15 primary commands, got {primary}"
    );
    assert!(primary <= 40, "primary count {primary} exceeds 40 limit");
    assert!(
        developer >= 5,
        "expected at least 5 developer commands, got {developer}"
    );
    let total = primary + advanced + developer;
    assert!(
        (50..=85).contains(&total),
        "unexpected total command count: {total}"
    );
}

#[test]
fn help_all_shows_all_commands() {
    let registry = runtime_v2_registry();
    let all = registry.format_overview_all();
    assert!(
        all.contains("primary="),
        "help all should show primary count"
    );
    assert!(
        all.contains("advanced="),
        "help all should show advanced count"
    );
    assert!(all.contains("dev="), "help all should show dev count");
}

#[test]
fn help_command_format_is_unified() {
    let registry = runtime_v2_registry();
    let help = registry
        .format_help("status")
        .expect("status command should exist");
    assert!(help.contains("NAME"), "help should contain NAME section");
    assert!(
        help.contains("DESCRIPTION"),
        "help should contain DESCRIPTION"
    );
    assert!(help.contains("USAGE"), "help should contain USAGE");
}

#[test]
fn help_unknown_command_shows_suggestion() {
    let registry = runtime_v2_registry();
    let suggestion = registry.format_unknown("notacommand");
    assert!(
        suggestion.contains("未知命令"),
        "should show unknown command message"
    );
}

#[test]
fn help_group_is_available() {
    let registry = runtime_v2_registry();
    let group_help = registry.format_group("rooms", false);
    assert!(
        group_help.contains("rooms"),
        "rooms group help should contain group name"
    );
}

#[test]
fn command_count_is_stable() {
    let registry = runtime_v2_registry();
    let count = registry.iter().count();
    // If this fails, update the count — this test prevents drift
    assert!(
        (50..=85).contains(&count),
        "unexpected command count: {count}"
    );
}

#[test]
fn deprecated_commands_removed_from_registry() {
    let registry = runtime_v2_registry();
    for name in &[
        "ext-list",
        "ext-get",
        "welcome-config",
        "player-count",
        "playtime",
        "round-last",
    ] {
        assert!(
            registry.get(name).is_none(),
            "'{name}' must be removed from registry"
        );
    }
}

#[test]
fn benchmark_bind_and_cleanup_removed_from_registry() {
    let registry = runtime_v2_registry();
    assert!(
        registry.get("benchmark-bind").is_none(),
        "benchmark-bind should be removed from registry"
    );
    assert!(
        registry.get("benchmark-cleanup").is_none(),
        "benchmark-cleanup should be removed from registry"
    );
}

#[test]
fn developer_commands_not_in_default_overview() {
    let registry = runtime_v2_registry();
    let overview = registry.format_overview();
    for name in &[
        "runtime roadmap",
        "runtime schema",
        "runtime actors",
        "runtime rooms",
        "simulation tick",
        "simulation persist",
        "simulation seed",
    ] {
        assert!(
            !overview.contains(name),
            "dev command '{name}' leaked into default overview"
        );
    }
    // But should appear in dev view
    let dev = registry.format_dev();
    for name in &["runtime roadmap", "runtime schema"] {
        assert!(
            dev.contains(name),
            "dev command '{name}' should appear in dev view"
        );
    }
}

#[test]
fn help_advanced_shows_benchmark_commands() {
    let registry = runtime_v2_registry();
    let adv = registry.format_advanced();
    assert!(
        adv.contains("benchmark modes"),
        "advanced view should show benchmark modes"
    );
    assert!(
        adv.contains("benchmark run real"),
        "advanced view should show benchmark run real"
    );
}

#[test]
fn canonical_help_is_primary_command() {
    let registry = runtime_v2_registry();
    let spec = registry.get("help").expect("help should exist");
    assert_eq!(spec.name, "help");
    assert!(spec.audience == phira_mp_plus_server::command_registry::CommandAudience::Primary);
}

#[test]
fn canonical_exit_is_command() {
    let registry = runtime_v2_registry();
    let spec = registry.get("exit").expect("exit should exist");
    assert_eq!(spec.name, "exit");
}

#[test]
fn canonical_namespaces_exist() {
    let registry = runtime_v2_registry();
    // Namespace-only commands: room, plugin, runtime, simulation
    for name in &["room", "plugin", "runtime", "simulation"] {
        assert!(
            registry.get(name).is_none(),
            "'{name}' as a direct command should not exist (it's a namespace)"
        );
        assert!(
            !registry.child_commands(name).is_empty(),
            "namespace '{name}' should have child commands"
        );
    }
    // benchmark is both a leaf command and a parent namespace
    assert!(
        registry.get("benchmark").is_some(),
        "benchmark should exist"
    );
    assert!(
        !registry.child_commands("benchmark").is_empty(),
        "benchmark should have child commands"
    );
}

#[test]
fn registry_has_no_legacy_or_alias_surface() {
    let registry = runtime_v2_registry();
    // Aliases should not resolve
    assert!(registry.get("h").is_none());
    assert!(registry.get("q").is_none());
    assert!(registry.get("quit").is_none());
    assert!(registry.get("room list").is_none());
    // Legacy commands should not exist
    assert!(registry.get("benchmark-bind").is_none());
    assert!(registry.get("benchmark-cleanup").is_none());
    assert!(registry.get("ext-list").is_none());
    assert!(registry.get("ext-get").is_none());
    assert!(registry.get("playtime").is_none());
    assert!(registry.get("round-last").is_none());
}

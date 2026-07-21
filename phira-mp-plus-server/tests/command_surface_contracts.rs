//! Command surface contract tests.
//!
//! These tests verify the CommandAudience separation, that primary commands
//! are within the productised limit, and that advanced/dev views work
//! correctly. Also validates help formatting, deprecated command removal,
//! and namespace structure.

use phira_mp_plus_server::command_registry::{runtime_v2_registry, CommandAudience};

// ── Command surface counts ────────────────────────────────────────────

#[test]
fn primary_count_is_within_product_limit() {
    let registry = runtime_v2_registry();
    let (primary, _advanced, _developer) = registry.command_surface_counts();
    assert!(
        primary <= 40,
        "primary count {primary} exceeds 40 product limit"
    );
    assert!(primary >= 15, "primary count {primary} too low");
}

#[test]
fn command_count_is_stable() {
    let registry = runtime_v2_registry();
    let count = registry.iter().count();
    assert!(
        (50..=85).contains(&count),
        "unexpected command count: {count}"
    );
}

#[test]
fn default_help_is_concise() {
    let registry = runtime_v2_registry();
    let overview = registry.format_overview();
    assert!(
        overview.len() < 4500,
        "default help too long: {} chars",
        overview.len()
    );
}

// ── Default overview (Primary-only) ───────────────────────────────────

#[test]
fn default_overview_omits_advanced() {
    let registry = runtime_v2_registry();
    let overview = registry.format_overview();
    assert!(
        !overview.contains("advanced="),
        "default overview must not show advanced=N"
    );
}

#[test]
fn default_overview_shows_primary_commands() {
    let registry = runtime_v2_registry();
    let overview = registry.format_overview();
    for cmd_name in &[
        "help",
        "exit",
        "status",
        "users",
        "rooms",
        "room start",
        "plugin list",
    ] {
        assert!(
            overview.contains(cmd_name),
            "primary command '{cmd_name}' should appear in default help"
        );
    }
}

#[test]
fn default_overview_omits_developer_commands() {
    let registry = runtime_v2_registry();
    let overview = registry.format_overview();
    for cmd_name in &[
        "runtime roadmap",
        "runtime schema",
        "runtime actors",
        "runtime rooms",
        "runtime events",
        "simulation tick",
        "simulation persist",
        "simulation seed",
    ] {
        assert!(
            !overview.contains(cmd_name),
            "developer command '{cmd_name}' leaked into default overview"
        );
    }
}

// ── Help views ────────────────────────────────────────────────────────

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
    assert!(
        adv.contains("runtime cutover"),
        "advanced view should show runtime cutover"
    );
}

#[test]
fn help_dev_shows_developer_commands() {
    let registry = runtime_v2_registry();
    let dev = registry.format_dev();
    assert!(!dev.is_empty(), "dev view should have commands");
    assert!(
        dev.contains("runtime roadmap"),
        "dev view should show runtime roadmap"
    );
    assert!(
        dev.contains("simulation tick"),
        "dev view should show simulation tick"
    );
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

// ── Canonical commands ────────────────────────────────────────────────

#[test]
fn canonical_help_is_primary_command() {
    let registry = runtime_v2_registry();
    let spec = registry.get("help").expect("help should exist");
    assert_eq!(spec.name, "help");
    assert_eq!(spec.audience, CommandAudience::Primary);
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
    assert!(
        registry.get("benchmark").is_some(),
        "benchmark should exist"
    );
    assert!(
        !registry.child_commands("benchmark").is_empty(),
        "benchmark should have child commands"
    );
}

// ── Audience validation ───────────────────────────────────────────────

#[test]
fn developer_commands_have_developer_audience() {
    let registry = runtime_v2_registry();
    for name in &[
        "runtime roadmap",
        "runtime schema",
        "simulation tick",
        "simulation persist",
    ] {
        let spec = registry.get(name).expect("{name} should be in registry");
        assert_eq!(
            spec.audience,
            CommandAudience::Developer,
            "command '{name}' must be developer"
        );
    }
}

#[test]
fn all_primary_commands_have_valid_help() {
    let registry = runtime_v2_registry();
    for cmd in registry.iter() {
        if cmd.audience == CommandAudience::Primary {
            let help = registry.format_help(&cmd.name);
            assert!(
                help.is_some(),
                "primary command '{}' should have format_help output",
                cmd.name
            );
        }
    }
}

// ── Force-start aliases ───────────────────────────────────────────────

#[test]
fn force_start_compatibility_command_is_registered() {
    let registry = runtime_v2_registry();
    assert!(registry.get("force-start").is_some());
    assert!(registry.get("room force-start").is_some());
    assert!(registry.get("room start").is_some());
}

// ── Deprecated / legacy command removal ───────────────────────────────

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
        "benchmark-bind must be removed from registry"
    );
    assert!(
        registry.get("benchmark-cleanup").is_none(),
        "benchmark-cleanup must be removed from registry"
    );
}

#[test]
fn registry_has_no_legacy_or_alias_surface() {
    let registry = runtime_v2_registry();
    assert!(registry.get("h").is_none());
    assert!(registry.get("q").is_none());
    assert!(registry.get("quit").is_none());
    assert!(registry.get("room list").is_none());
    assert!(registry.get("benchmark-bind").is_none());
    assert!(registry.get("benchmark-cleanup").is_none());
    assert!(registry.get("ext-list").is_none());
    assert!(registry.get("ext-get").is_none());
    assert!(registry.get("playtime").is_none());
    assert!(registry.get("round-last").is_none());
}

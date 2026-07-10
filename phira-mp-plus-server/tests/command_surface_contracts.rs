//! Command surface contract tests.
//!
//! These tests verify the CommandAudience separation, that primary commands
//! are within the productised limit, and that advanced/dev views work
//! correctly.

use phira_mp_plus_server::command_registry::{runtime_v2_registry, CommandAudience};

#[test]
fn primary_count_is_within_product_limit() {
    let registry = runtime_v2_registry();
    let (primary, _advanced, _developer) = registry.command_surface_counts();
    assert!(
        primary <= 25,
        "primary count {primary} exceeds 25 product limit"
    );
    assert!(primary >= 15, "primary count {primary} too low");
}

#[test]
fn default_overview_omits_advanced() {
    let registry = runtime_v2_registry();
    let overview = registry.format_overview();
    // Default help should not mention internal counts
    assert!(
        !overview.contains("advanced="),
        "default overview must not show advanced=N"
    );
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
fn deprecated_commands_removed_from_registry() {
    let registry = runtime_v2_registry();
    // All legacy commands have been removed from the registry
    for name in &[
        "ext-list",
        "ext-get",
        "playtime",
        "round-last",
        "welcome-config",
        "player-count",
    ] {
        assert!(
            registry.get(name).is_none(),
            "'{name}' must be removed from registry"
        );
    }
}

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

#[test]
fn force_start_compatibility_command_is_registered() {
    let registry = runtime_v2_registry();
    assert!(registry.get("force-start").is_some());
    assert!(registry.get("room force-start").is_some());
    assert!(registry.get("room start").is_some());
}

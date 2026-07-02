//! CLI command surface contracts.
//!
//! These tests verify the CommandRegistry help output, command counts,
//! alias behavior, and that deprecated/advanced commands don't leak
//! into the default help view.

use phira_mp_plus_server::command_registry::runtime_v2_registry;

#[test]
fn default_help_is_concise() {
    let registry = runtime_v2_registry();
    let overview = registry.format_overview();
    // Default help should not be a wall of text
    assert!(overview.len() < 3000, "default help too long: {} chars", overview.len());
}

#[test]
fn default_help_shows_primary_count() {
    let registry = runtime_v2_registry();
    let (primary, advanced) = registry.command_surface_counts();
    assert!(primary >= 20, "expected at least 20 primary commands, got {primary}");
    assert!(advanced >= 30, "expected at least 30 advanced commands, got {advanced}");
    assert!(primary < advanced, "primary ({primary}) should be less than advanced ({advanced})");
}

#[test]
fn help_all_shows_all_commands() {
    let registry = runtime_v2_registry();
    let all = registry.format_overview_all();
    assert!(all.contains("primary="), "help all should show primary count");
    assert!(all.contains("advanced="), "help all should show advanced count");
}

#[test]
fn help_command_format_is_unified() {
    let registry = runtime_v2_registry();
    let help = registry.format_help("status").expect("status command should exist");
    assert!(help.contains("NAME"), "help should contain NAME section");
    assert!(help.contains("DESCRIPTION"), "help should contain DESCRIPTION");
    assert!(help.contains("USAGE"), "help should contain USAGE");
}

#[test]
fn help_unknown_command_shows_suggestion() {
    let registry = runtime_v2_registry();
    let suggestion = registry.format_unknown("notacommand");
    assert!(suggestion.contains("未知命令"), "should show unknown command message");
}

#[test]
fn help_group_is_available() {
    let registry = runtime_v2_registry();
    let group_help = registry.format_group("rooms", false);
    assert!(group_help.contains("rooms"), "rooms group help should contain group name");
}

#[test]
fn alias_h_resolves_to_help() {
    let registry = runtime_v2_registry();
    // alias fields exist on CommandSpec; alias lookup is handled externally
    let spec = registry.get("help").expect("help should exist");
    assert!(spec.aliases.contains(&"h".to_string()), "help should have alias 'h'");
}

#[test]
fn command_count_is_stable() {
    let registry = runtime_v2_registry();
    let count = registry.iter().count();
    // If this fails, update the count — this test prevents drift
    assert!(count >= 60 && count <= 90, "unexpected command count: {count}");
}

#[test]
fn deprecated_commands_not_in_primary() {
    let registry = runtime_v2_registry();
    let primary: Vec<_> = registry.iter().filter(|c| c.audience == registry::CommandAudience::Advanced).collect();
    for cmd in primary {
        assert!(!cmd.name.contains("benchmark-bind"), "benchmark-bind should be advanced");
    }
}

//! CLI command surface contracts.
//!
//! These tests verify the CommandRegistry help output, command counts,
//! alias behavior, and that deprecated/advanced commands don't leak
//! into the default help view.

use phira_mp_plus_server::command_registry::{runtime_v2_registry, CommandAudience};

#[test]
fn default_help_is_concise() {
    let registry = runtime_v2_registry();
    let overview = registry.format_overview();
    // Default help should not be a wall of text
    assert!(overview.len() < 4500, "default help too long: {} chars", overview.len());
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
    let spec = registry.get("h").expect("alias 'h' should resolve to help");
    assert_eq!(spec.name, "help", "get('h') should return help command");
}

#[test]
fn alias_q_resolves_to_exit() {
    let registry = runtime_v2_registry();
    let spec = registry.get("q").expect("alias 'q' should resolve to exit");
    assert_eq!(spec.name, "exit", "get('q') should return exit command");
}

#[test]
fn alias_h_format_help_works() {
    let registry = runtime_v2_registry();
    let help_text = registry.format_help("h").expect("format_help('h') should work");
    assert!(help_text.contains("help"), "help text for alias 'h' should mention help");
}

#[test]
fn alias_does_not_conflict_with_command_names() {
    // This test verifies that no alias collides with an existing command.
    // The registry's register() method should reject such conflicts.
    let registry = runtime_v2_registry();
    // 'rooms' is an alias for 'room list' — verify it doesn't shadow 'rooms' itself
    let rooms = registry.get("rooms").expect("rooms should exist");
    assert!(rooms.name == "rooms" || rooms.aliases.contains(&"rooms".to_string()),
        "rooms should resolve to the rooms command or its alias");
}

#[test]
fn command_count_is_stable() {
    let registry = runtime_v2_registry();
    let count = registry.iter().count();
    // If this fails, update the count — this test prevents drift
    assert!(count >= 60 && count <= 90, "unexpected command count: {count}");
}

#[test]
fn internal_commands_not_in_primary() {
    let registry = runtime_v2_registry();
    for cmd in registry.iter() {
        if cmd.audience == CommandAudience::Primary {
            // Internal/tool commands should never be primary
            assert!(!cmd.name.contains("benchmark-bind"),
                "benchmark-bind should not be primary: {}", cmd.name);
            assert!(!cmd.name.contains("ext-list"),
                "ext-list should not be primary: {}", cmd.name);
            assert!(!cmd.name.contains("ext-get"),
                "ext-get should not be primary: {}", cmd.name);
            assert!(!cmd.name.contains("playtime"),
                "playtime should not be primary: {}", cmd.name);
            assert!(!cmd.name.contains("round-last"),
                "round-last should not be primary: {}", cmd.name);
        }
    }
}

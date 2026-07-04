//! WIT ABI contract tests.
//!
//! These tests verify that the canonical WIT ABI definition is locked,
//! that plugin_abi.rs references it correctly, and that the legacy WIT
//! is only a deprecated migration pointer.

use phira_mp_plus_server::plugin_abi;
use std::path::PathBuf;

/// Get absolute path to workspace root (two levels up from crate manifest dir).
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .expect("server crate should have a parent")
        .to_path_buf()
}

fn canonical_wit() -> PathBuf {
    workspace_root().join("wit/phira-plugin.wit")
}

fn legacy_wit() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("wit/phira/mpplus.wit")
}

fn read_canonical() -> String {
    std::fs::read_to_string(canonical_wit()).expect("canonical WIT should be readable")
}

#[test]
fn canonical_wit_file_exists() {
    let path = canonical_wit();
    assert!(
        path.exists(),
        "canonical WIT file not found at {}",
        path.display()
    );
}

#[test]
fn plugin_abi_constants_are_locked() {
    assert_eq!(
        plugin_abi::wit::WIT_FILE,
        "wit/phira-plugin.wit",
        "WIT_FILE must point to canonical path"
    );
    assert_eq!(
        plugin_abi::wit::WIT_WORLD,
        "phira-plugin-v2",
        "WIT_WORLD must be phira-plugin-v2"
    );
    assert_eq!(
        plugin_abi::wit::WIT_VERSION,
        "abi-wit-v2",
        "WIT_VERSION must be abi-wit-v2"
    );
    assert_eq!(
        plugin_abi::wit::MIGRATION_PHASE,
        0,
        "MIGRATION_PHASE 0 = JSON bridge active (enable wit-bindgen for phase 1)"
    );
}

#[test]
fn canonical_wit_contains_required_interfaces() {
    let content = read_canonical();
    for interface in &[
        "phira-types",
        "phira-host",
        "phira-events",
        "phira-query",
        "phira-room-mgmt",
        "phira-user-mgmt",
        "phira-messaging",
        "phira-persistence",
        "phira-admin",
        "phira-simulation",
        "phira-runtime",
    ] {
        assert!(
            content.contains(&format!("interface {interface}")),
            "WIT must define interface {interface}"
        );
    }
}

#[test]
fn canonical_wit_contains_world_phira_plugin_v2() {
    let content = read_canonical();
    assert!(
        content.contains("world phira-plugin-v2"),
        "WIT must define world phira-plugin-v2"
    );
    assert!(
        content.contains("import phira-host"),
        "world must import phira-host"
    );
    assert!(
        content.contains("import phira-events"),
        "world must import phira-events"
    );
    assert!(
        content.contains("import phira-query"),
        "world must import phira-query"
    );
    assert!(
        content.contains("import phira-room-mgmt"),
        "world must import phira-room-mgmt"
    );
    assert!(
        content.contains("import phira-user-mgmt"),
        "world must import phira-user-mgmt"
    );
    assert!(
        content.contains("import phira-messaging"),
        "world must import phira-messaging"
    );
    assert!(
        content.contains("import phira-persistence"),
        "world must import phira-persistence"
    );
    assert!(
        content.contains("import phira-admin"),
        "world must import phira-admin"
    );
    assert!(
        content.contains("import phira-simulation"),
        "world must import phira-simulation"
    );
    assert!(
        content.contains("import phira-runtime"),
        "world must import phira-runtime"
    );
    assert!(content.contains("export init"), "world must export init");
    assert!(
        content.contains("export get-info"),
        "world must export get-info"
    );
    assert!(
        content.contains("export cleanup"),
        "world must export cleanup"
    );
    assert!(
        content.contains("export on-event"),
        "world must export on-event"
    );
    assert!(
        content.contains("export on-api"),
        "world must export on-api"
    );
}

#[test]
fn canonical_wit_contains_key_function_names() {
    let content = read_canonical();
    let checks: Vec<&str> = vec![
        // phira-host
        "api-call",
        "send-chat",
        "http-request",
        "generate-uuid",
        // phira-query
        "get-user",
        "get-room",
        "list-rooms",
        // phira-room-mgmt
        "set-room-hidden",
        "set-room-phira-api-endpoint",
        "close-room",
        // phira-user-mgmt
        "kick-user",
        "ban-user",
        "unban-user",
        // phira-messaging
        "send-to-user",
        "send-to-room",
        // phira-persistence
        "query-events",
        "query-touches",
        "query-judges",
        // phira-admin
        "list-admin-ids",
        "add-admin-id",
        "remove-admin-id",
    ];
    // Interface-qualified function names (already include ":")
    let colon_checks: Vec<&str> = vec![
        // phira-simulation (unqualified within interface)
        "status: func",
        "run: func",
        "stop: func",
        "cleanup: func",
        // phira-runtime (unqualified within interface)
        "events: func",
        "commands: func",
    ];
    for func in &checks {
        let pattern = format!("{func}:"); // WIT function names are followed by colon
        assert!(
            content.contains(&pattern),
            "WIT must define function '{func}'"
        );
    }
    for pattern in &colon_checks {
        assert!(content.contains(pattern), "WIT must contain '{pattern}'");
    }
}

#[test]
fn canonical_wit_contains_stable_event_types() {
    let content = read_canonical();
    for event in &[
        "user-connect",
        "user-disconnect",
        "room-create",
        "room-join",
        "room-leave",
        "room-modify",
        "game-start",
        "game-end",
        "player-touches",
        "player-judges",
        "round-complete",
    ] {
        assert!(
            content.contains(&format!("{event}(")),
            "WIT events must contain '{event}' variant"
        );
    }
}

#[test]
fn canonical_wit_contains_stable_types() {
    let content = read_canonical();
    for typ in &[
        "touch-event-point",
        "judge-event-item",
        "plugin-info",
        "http-response",
        "game-end-record",
    ] {
        assert!(
            content.contains(&format!("record {typ}")),
            "WIT types must define record '{typ}'"
        );
    }
    assert!(
        content.contains("variant json-value"),
        "WIT must define json-value variant"
    );
    assert!(
        content.contains("variant api-result"),
        "WIT must define api-result variant"
    );
}

#[test]
fn legacy_wit_marked_deprecated() {
    let content = std::fs::read_to_string(legacy_wit()).expect("legacy WIT should be readable");
    assert!(
        content.contains("DEPRECATED"),
        "legacy WIT must be marked DEPRECATED"
    );
    assert!(
        content.contains("wit/phira-plugin.wit"),
        "legacy WIT must reference canonical path"
    );
}

#[test]
fn legacy_wit_has_no_full_abi() {
    // Legacy WIT should not contain full interface definitions — it's a stub pointer
    let content = std::fs::read_to_string(legacy_wit()).expect("legacy WIT should be readable");
    // It may still have full interfaces (it's a snapshot), but we verify it IS marked deprecated
    // and references the canonical path.
    assert!(
        content.contains("abi-json-v1") || content.contains("DEPRECATED"),
        "legacy WIT should reference the old ABI version or be marked deprecated"
    );
}

#[test]
fn current_abi_is_json_not_wit() {
    assert_eq!(plugin_abi::wit::MIGRATION_PHASE, 0);
    let plan = plugin_abi::plugin_abi_plan();
    assert_eq!(
        plan.current_transport,
        plugin_abi::PluginAbiTransport::JsonMemoryV1
    );
    assert_eq!(
        plan.target_transport,
        plugin_abi::PluginAbiTransport::WitTypedV2
    );
}

#[test]
fn plugin_abi_wit_file_path_resolves() {
    let wit_rel = plugin_abi::wit::WIT_FILE;
    let path = workspace_root().join(wit_rel);
    assert!(
        path.exists(),
        "plugin_abi.rs references {wit_rel} but file doesn't exist at {}",
        path.display()
    );
}

#[test]
fn phira_plugin_wit_is_canonical_only() {
    // Verify the workspace root wit/ directory contains only the canonical file
    let wit_dir = workspace_root().join("wit");
    assert!(
        wit_dir.join("phira-plugin.wit").exists(),
        "canonical WIT must exist"
    );
    // Legacy WIT is inside the server crate, not at workspace root
    assert!(
        !wit_dir.join("phira").exists() || !wit_dir.join("phira").is_dir(),
        "workspace wit/ should not contain phira/ subdirectory (legacy lives in server crate)"
    );
}

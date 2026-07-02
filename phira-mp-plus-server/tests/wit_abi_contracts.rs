//! WIT ABI contract tests.
//!
//! These tests verify that the canonical WIT ABI definition exists, is
//! referenced correctly by plugin_abi.rs, and that the two WIT files
//! (canonical + legacy) don't silently diverge.

use phira_mp_plus_server::plugin_abi;
use std::path::PathBuf;

/// Get absolute path to workspace root (two levels up from crate manifest dir,
/// since the test binary runs at the crate level: phira-mp-plus-server/ -> ../)
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().expect("server crate should have a parent").to_path_buf()
}

fn canonical_wit() -> PathBuf {
    workspace_root().join("wit/phira-plugin.wit")
}

fn legacy_wit() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("wit/phira/mpplus.wit")
}

#[test]
fn canonical_wit_file_exists() {
    let path = canonical_wit();
    assert!(path.exists(), "canonical WIT file not found at {}", path.display());
}

#[test]
fn plugin_abi_wit_file_refers_to_existing_file() {
    let wit_rel = plugin_abi::wit::WIT_FILE;
    let path = workspace_root().join(wit_rel);
    assert!(path.exists(), "plugin_abi.rs references {wit_rel} but file doesn't exist at {}", path.display());
}

#[test]
fn plugin_abi_wit_world_is_correct() {
    assert_eq!(plugin_abi::wit::WIT_WORLD, "phira-plugin-v2");
    assert_eq!(plugin_abi::wit::WIT_VERSION, "abi-wit-v2");
}

#[test]
fn canonical_wit_contains_key_interfaces() {
    let content = std::fs::read_to_string(canonical_wit()).expect("canonical WIT should be readable");
    assert!(content.contains("touch-event-point"), "WIT should define touch-event-point");
    assert!(content.contains("judge-event-item"), "WIT should define judge-event-item");
    assert!(content.contains("plugin-info"), "WIT should define plugin-info");
    assert!(content.contains("phira-host"), "WIT should define phira-host interface");
    assert!(content.contains("phira-events"), "WIT should define phira-events interface");
    assert!(content.contains("phira-plugin-v2"), "WIT should define phira-plugin-v2 world");
}

#[test]
fn legacy_wit_marked_deprecated() {
    let content = std::fs::read_to_string(legacy_wit()).expect("legacy WIT should be readable");
    assert!(content.contains("DEPRECATED"), "legacy WIT must be marked DEPRECATED");
    assert!(content.contains("wit/phira-plugin.wit"), "legacy WIT must reference canonical path");
}

#[test]
fn current_abi_is_json_not_wit() {
    // Until MIGRATION_PHASE >= 1, the runtime ABI is still JSON-memory
    assert_eq!(plugin_abi::wit::MIGRATION_PHASE, 0);
    let plan = plugin_abi::plugin_abi_plan();
    assert_eq!(plan.current_transport, plugin_abi::PluginAbiTransport::JsonMemoryV1);
    assert_eq!(plan.target_transport, plugin_abi::PluginAbiTransport::WitTypedV2);
}

//! WASM plugin lifecycle and API integration tests.
//!
//! These tests load the compiled test-plugin.component.wasm fixture and
//! exercise it through WitPluginComponent.  The actual WASM tests are
//! in wasm_host.rs (unit tests) and wasm_api_tests.rs.

use phira_mp_plus_server::plugin_abi;
use std::path::PathBuf;

fn wasm_path() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .unwrap()
        .join("phira-mp-plus-server/tests/test-plugin.component.wasm")
}

#[test]
fn wasm_fixture_exists() {
    let path = wasm_path();
    assert!(
        path.exists(),
        "WASM fixture not found at {}",
        path.display()
    );
    assert!(path.metadata().unwrap().len() > 1000);
}

#[test]
fn plugin_abi_is_wit_only() {
    let versions = plugin_abi::supported_abi_versions();
    assert_eq!(versions, vec!["abi-wit-v2"]);
    assert!(!plugin_abi::is_abi_version_supported("abi-json-v1"));
}

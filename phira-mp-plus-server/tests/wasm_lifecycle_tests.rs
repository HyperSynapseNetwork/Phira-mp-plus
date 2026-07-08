//! WASM plugin lifecycle tests.
//!
//! Loads the compiled test-plugin.component.wasm and exercises the
//! init / get-info / cleanup lifecycle.

use phira_mp_plus_server::plugin_abi;
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap().to_path_buf()
}

fn wasm_path() -> PathBuf {
    workspace_root().join("phira-mp-plus-server/tests/test-plugin.component.wasm")
}

fn load_test_plugin_bytes() -> Vec<u8> {
    std::fs::read(wasm_path()).expect("test-plugin.component.wasm not found — run make in tests/test-plugin/")
}

#[test]
fn wasm_fixture_exists() {
    let path = wasm_path();
    assert!(path.exists(), "WASM fixture not found at {}", path.display());
    let meta = std::fs::metadata(&path).unwrap();
    assert!(meta.len() > 1000, "WASM fixture too small: {} bytes", meta.len());
}

#[test]
fn plugin_abi_is_wit_only() {
    let versions = plugin_abi::supported_abi_versions();
    assert_eq!(versions, vec!["abi-wit-v2"]);
    assert!(!plugin_abi::is_abi_version_supported("abi-json-v1"));
}

// ── Lifecycle tests (with full WitPluginComponent) ──
//
// These tests use #[cfg(feature = "wit-bindgen")] to compile only when
// the WIT bindings are available (the server's default build).

#[cfg(feature = "wit-bindgen")]
mod component_tests {
    use super::*;
    use phira_mp_plus_server::wasm_host::{WitPluginComponent, WasmPluginServices};
    use phira_mp_plus_server::extensions::ExtensionManager;
    use phira_mp_plus_server::plugin::WasmRuntimeConfig;
    use std::sync::Arc;

    fn test_services() -> Arc<WasmPluginServices> {
        // Minimal services for testing — extensions and state query
        // are available but return empty/default data.
        let cli_commands = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let api_handlers = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        Arc::new(WasmPluginServices::new(
            Arc::new(ExtensionManager::new_in_memory()),
            cli_commands,
            api_handlers,
        ))
    }

    fn load_component() -> WitPluginComponent {
        let bytes = load_test_plugin_bytes();
        let services = test_services();
        WitPluginComponent::new(
            &bytes,
            "test-plugin".into(),
            services,
            WasmRuntimeConfig::default(),
        )
        .expect("WitPluginComponent::new should succeed")
    }

    #[test]
    fn load_test_component() {
        load_component();
    }

    #[test]
    fn init_returns_ok() {
        let mut component = load_component();
        component.call_init().expect("init should return Ok(())");
        assert!(component.initialized, "component should be marked initialized");
    }

    #[test]
    fn info_matches_expected() {
        let component = load_component();
        assert_eq!(component.info.name, "test-plugin");
        assert_eq!(component.info.version, "0.1.0-test");
        assert_eq!(component.info.author, "phira-mp-plus");
        assert_eq!(component.info.description, "Integration test WASM plugin");
    }

    #[test]
    fn cleanup_does_not_panic() {
        let mut component = load_component();
        component.call_init().unwrap();
        component.call_cleanup(); // should not panic
        assert!(!component.initialized, "component should be uninitialized after cleanup");
    }

    #[test]
    fn double_cleanup_is_safe() {
        let mut component = load_component();
        component.call_cleanup(); // cleanup without init
        component.call_cleanup(); // double cleanup
    }

    #[test]
    fn on_event_returns_false() {
        use phira_mp_plus_server::plugin::PluginEvent;
        let mut component = load_component();
        component.call_init().unwrap();
        let result = component.call_on_event(&PluginEvent::UserConnect {
            user_id: 1,
            user_name: "test".into(),
            user_ip: "127.0.0.1".into(),
        });
        assert_eq!(result, Ok(0), "unhandled event should return 0 (false)");
    }

    #[test]
    fn on_api_ping() {
        use serde_json::json;
        let mut component = load_component();
        component.call_init().unwrap();
        let result = component.call_api("ping", &[]);
        assert_eq!(result, Ok(json!(null)), "ping should return null");
    }

    #[test]
    fn on_api_echo_roundtrip() {
        use serde_json::json;
        let mut component = load_component();
        component.call_init().unwrap();
        let result = component.call_api("echo", &[json!("hello")]);
        assert_eq!(result, Ok(json!("hello")));
    }

    #[test]
    fn on_api_count_increments() {
        use serde_json::json;
        let mut component = load_component();
        component.call_init().unwrap();
        assert_eq!(component.call_api("count", &[]), Ok(json!(0)));
        assert_eq!(component.call_api("count", &[]), Ok(json!(1)));
        assert_eq!(component.call_api("count", &[]), Ok(json!(2)));
    }

    #[test]
    fn on_api_unknown_method_returns_error() {
        let mut component = load_component();
        component.call_init().unwrap();
        let result = component.call_api("nonexistent", &[]);
        assert!(result.is_err(), "unknown method should return error");
    }
}

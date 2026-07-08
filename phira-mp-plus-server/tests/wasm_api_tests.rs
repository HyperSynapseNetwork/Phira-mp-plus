//! WASM plugin host API tests.
//!
//! Exercises plugin→host calls: api_call, http_request, log.
//! These tests require the full WitPluginComponent with services.

#[cfg(feature = "wit-bindgen")]
mod tests {
    use phira_mp_plus_server::wasm_host::{WitPluginComponent, WasmPluginServices};
    use phira_mp_plus_server::extensions::ExtensionManager;
    use phira_mp_plus_server::plugin::WasmRuntimeConfig;
    use phira_mp_plus_server_api as api;
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn workspace_root() -> PathBuf {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest.parent().unwrap().to_path_buf()
    }

    fn wasm_path() -> PathBuf {
        workspace_root().join("phira-mp-plus-server/tests/test-plugin.component.wasm")
    }

    fn mock_state_query() -> api::ServerStateQuery {
        api::ServerStateQuery::new(|method: &str, args: &[serde_json::Value]| {
            match method {
                "rooms.list" => Ok(serde_json::json!([
                    {"name": "test-room", "player_count": 3}
                ])),
                "user_name" => Ok(serde_json::json!({
                    "user_id": args.get(0).and_then(|v| v.as_i64()).unwrap_or(0),
                    "name": "test-user"
                })),
                _ => Err(format!("mock: unknown method {method}")),
            }
        })
    }

    fn test_services() -> Arc<WasmPluginServices> {
        let cli_commands = Arc::new(Mutex::new(HashMap::new()));
        let api_handlers = Arc::new(Mutex::new(HashMap::new()));
        let services = Arc::new(WasmPluginServices::new(
            Arc::new(ExtensionManager::new_in_memory()),
            cli_commands,
            api_handlers,
        ));
        // Set mock state query so plugin can call host_api("api-call", …)
        if let Ok(mut sq) = services.state_query.lock() {
            *sq = Some(mock_state_query());
        }
        services
    }

    fn load_component() -> WitPluginComponent {
        let bytes = std::fs::read(wasm_path()).expect("WASM fixture not found");
        let services = test_services();
        WitPluginComponent::new(&bytes, "test-plugin".into(), services, WasmRuntimeConfig::default())
            .expect("WitPluginComponent::new should succeed")
    }

    #[test]
    fn plugin_calls_host_api_call() {
        // The test plugin's "host.api_call" method calls phira_host::api_call
        // with the given method name and args, then returns the result.
        let mut component = load_component();
        component.call_init().unwrap();

        let result = component.call_api("host.api_call", &[
            json!("rooms.list"),
        ]).expect("host.api_call should succeed");

        let rooms = result.as_array().expect("rooms.list should return an array");
        assert!(!rooms.is_empty(), "should have at least one room");
        assert_eq!(rooms[0]["name"], "test-room");
    }

    #[test]
    fn plugin_calls_host_api_call_with_args() {
        let mut component = load_component();
        component.call_init().unwrap();

        let result = component.call_api("host.api_call", &[
            json!("user_name"),
            json!(42),
        ]).expect("host.api_call should succeed");

        assert_eq!(result["user_id"], 42);
        assert_eq!(result["name"], "test-user");
    }

    #[test]
    fn plugin_calls_host_api_call_unknown_method() {
        let mut component = load_component();
        component.call_init().unwrap();

        let result = component.call_api("host.api_call", &[
            json!("nonexistent.method"),
        ]).expect("host.api_call should return a result object");

        assert!(result.get("error").is_some(), "unknown method should return error object");
    }

    #[test]
    fn plugin_logs_via_host() {
        let mut component = load_component();
        component.call_init().unwrap();
        // log() via host — just ensure it doesn't panic
        let result = component.call_api("log", &[
            json!("info"),
            json!("test log message"),
        ]);
        assert!(result.is_ok(), "log should succeed");
    }
}

//! WASM plugin host API tests.
//!
//! Exercises plugin→host calls: api_call, http_request, log.
//! Uses `WitPluginComponent::from_bytes_ctx` with a mock context.

#[cfg(feature = "wit-bindgen")]
mod tests {
    use phira_mp_plus_server::plugin::WasmRuntimeConfig;
    use phira_mp_plus_server::wasm_host::WitPluginComponent;
    use phira_mp_plus_server::wit_host::WitHostContext;
    use phira_mp_plus_server_api as api;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn workspace_root() -> PathBuf {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest.parent().unwrap().to_path_buf()
    }

    fn wasm_path() -> PathBuf {
        workspace_root().join("phira-mp-plus-server/tests/test-plugin.component.wasm")
    }

    fn mock_context() -> Arc<WitHostContext> {
        let state_query =
            api::ServerStateQuery::new(|method: &str, args: &[serde_json::Value]| match method {
                "rooms.list" => Ok(json!([
                    {"name": "test-room", "player_count": 3}
                ])),
                "user_name" => Ok(json!({
                    "user_id": args.first().and_then(|v| v.as_i64()).unwrap_or(0),
                    "name": "test-user"
                })),
                _ => Err(format!("mock: unknown method {method}")),
            });
        let extensions =
            Arc::new(phira_mp_plus_server::extensions::ExtensionManager::new_in_memory());
        Arc::new(WitHostContext {
            state_query,
            extensions: Arc::clone(&extensions),
            room_commands: Arc::new(phira_mp_plus_server::room_actor::RoomCommandGateway::new()),
            ban_manager: Arc::new(phira_mp_plus_server::ban::BanManager::new(extensions)),
            simulation: Arc::new(phira_mp_plus_server::simulation::SimulationManager::new()),
            event_bus: Arc::new(phira_mp_plus_server::event_bus::EventBus::new(1024)),
            capabilities: Arc::new(phira_mp_plus_server::wasm_host_helpers::default_capabilities()),
            send_chat: None,
            http_timeout_secs: 10,
            http_max_body: 1024 * 1024,
            http_allow_private_network: false,
        })
    }

    fn load_component() -> WitPluginComponent {
        let bytes = std::fs::read(wasm_path()).expect("WASM fixture not found");
        let ctx = mock_context();
        WitPluginComponent::from_bytes_ctx(
            &bytes,
            "test-plugin".into(),
            ctx,
            WasmRuntimeConfig::default(),
        )
        .expect("WitPluginComponent::from_bytes_ctx should succeed")
    }

    #[test]
    fn plugin_calls_host_api_call() {
        let mut component = load_component();
        component.call_init().unwrap();

        let result = component
            .call_api("host.api_call", &[json!("rooms.list")])
            .expect("host.api_call should succeed");

        let rooms = result
            .as_array()
            .expect("rooms.list should return an array");
        assert!(!rooms.is_empty(), "should have at least one room");
        assert_eq!(rooms[0]["name"], "test-room");
    }

    #[test]
    fn plugin_calls_host_api_call_with_args() {
        let mut component = load_component();
        component.call_init().unwrap();

        let result = component
            .call_api("host.api_call", &[json!("user_name"), json!(42)])
            .expect("host.api_call should succeed");

        assert_eq!(result["user_id"], 42);
        assert_eq!(result["name"], "test-user");
    }

    #[test]
    fn plugin_calls_host_api_call_unknown_method() {
        let mut component = load_component();
        component.call_init().unwrap();

        let result = component
            .call_api("host.api_call", &[json!("nonexistent.method")])
            .expect("host.api_call should return a result object");

        assert!(
            result.get("error").is_some(),
            "unknown method should return error object"
        );
    }

    #[test]
    fn plugin_logs_via_host() {
        let mut component = load_component();
        component.call_init().unwrap();
        let result = component.call_api("log", &[json!("info"), json!("test log message")]);
        assert!(result.is_ok(), "log should succeed");
    }
}

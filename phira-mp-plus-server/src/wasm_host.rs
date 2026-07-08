//! WASM plugin host runtime — WIT/component-model only.
//!
//! JSON bridge ABI (phira_init, phira_get_info, phira_cleanup, phira_on_event,
//! phira_on_api) has been removed. All plugins must be WIT components targeting
//! the phira-plugin-v2 world.
//!
//! Guest exports (expected from every WIT plugin):
//! - `init() -> result<_, string>`
//! - `get-info() -> plugin-info`
//! - `cleanup()`
//! - `on-event(event: plugin-event) -> result<bool, string>`
//! - `on-api(method: string, args: list<json-value>) -> api-result`

use crate::extensions::ExtensionManager;
use crate::plugin::{CliCommand, PluginEvent, PluginInfo, WasmRuntimeConfig};
use phira_mp_plus_server_api as api;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

// ── Shared host services (used by WitPluginComponent) ──

pub struct WasmPluginServices {
    pub capabilities: Mutex<HashMap<String, Vec<String>>>,
    pub extensions: Arc<ExtensionManager>,
    pub server_state: Mutex<Option<Weak<crate::server::PlusServerState>>>,
    /// Shared with PluginManager — do not write separately.
    pub api_handlers: Arc<Mutex<HashMap<String, api::PluginApiHandler>>>,
    /// Shared with PluginManager — do not write separately.
    pub cli_commands: Arc<Mutex<HashMap<String, CliCommand>>>,
    /// WASM-only: HTTP handle set by PluginManager::set_http_handle (now unused by WASM).
    pub http_handle: Mutex<Option<api::HttpHandle>>,
    /// WASM-only: state query set by PluginManager::set_default_state.
    pub state_query: Mutex<Option<api::ServerStateQuery>>,
    /// WASM-only: chat callback set by PluginManager::set_send_chat.
    pub send_chat: Mutex<Option<Arc<dyn Fn(i32, String) + Send + Sync>>>,
}

impl WasmPluginServices {
    pub fn new(
        extensions: Arc<ExtensionManager>,
        cli_commands: Arc<Mutex<HashMap<String, CliCommand>>>,
        api_handlers: Arc<Mutex<HashMap<String, api::PluginApiHandler>>>,
    ) -> Self {
        Self {
            capabilities: Mutex::new(HashMap::new()),
            extensions,
            server_state: Mutex::new(None),
            api_handlers,
            cli_commands,
            http_handle: Mutex::new(None),
            state_query: Mutex::new(None),
            send_chat: Mutex::new(None),
        }
    }

    pub fn set_capabilities(&self, plugin: &str, caps: Vec<String>) {
        if let Ok(mut map) = self.capabilities.lock() {
            map.insert(plugin.to_string(), caps);
        }
    }

    pub fn clear_dynamic_registrations(&self) {
        if let Ok(mut cmds) = self.cli_commands.lock() {
            cmds.clear();
        }
        if let Ok(mut api) = self.api_handlers.lock() {
            api.clear();
        }
    }

    pub fn register_plugin_runtime(&self, _name: &str) {
        // Runtime diagnostics automatically available via WIT host.
    }

    pub fn set_server_state(&self, state: &Weak<crate::server::PlusServerState>) {
        if let Ok(mut s) = self.server_state.lock() {
            *s = Some(state.clone());
        }
    }
}

// ════════════════════════════════════════════════════════════════════
// WIT / Component-model host types
// ════════════════════════════════════════════════════════════════════

/// Per-instance state carried in the wasmtime Store for WIT components.
#[cfg(feature = "wit-bindgen")]
pub struct WitHostState {
    pub host: crate::wit_host::WitPluginHost,
}

/// WIT component model plugin instance.
/// Wraps a compiled component and its store, providing lifecycle API.
#[cfg(feature = "wit-bindgen")]
pub struct WitPluginComponent {
    store: wasmtime::Store<WitHostState>,
    component: crate::plugin_abi::wit_abi::PhiraPluginV2,
    pub info: PluginInfo,
    pub plugin_name: String,
    pub initialized: bool,
}

#[cfg(feature = "wit-bindgen")]
impl WitPluginComponent {
    /// Create a WitHostContext from services that have server_state set.
    fn build_context_from_services(services: &Arc<WasmPluginServices>) -> Result<Arc<crate::wit_host::WitHostContext>, String> {
        let state_ref = services.server_state.lock().unwrap_or_else(|e| e.into_inner());
        let s = state_ref.as_ref().and_then(|w| w.upgrade())
            .ok_or_else(|| "server state not available — set via PluginManager::set_server_state".to_string())?;
        let state_query = services.state_query.lock().unwrap_or_else(|e| e.into_inner()).clone()
            .ok_or_else(|| "state query not available — set via PluginManager::set_default_state".to_string())?;
        Ok(Arc::new(crate::wit_host::WitHostContext {
            state_query,
            extensions: Arc::clone(&services.extensions),
            room_commands: Arc::clone(&s.room_commands),
            ban_manager: Arc::clone(&s.ban_manager),
            simulation: Arc::clone(&s.simulation),
            event_bus: Arc::clone(&s.event_bus),
            http_timeout_secs: s.config.wasm_runtime.http_timeout_secs,
            http_max_body: s.config.wasm_runtime.max_http_response_bytes,
        }))
    }

    /// Create a new WIT component plugin from raw WASM bytes.
    ///
    /// Requires `WasmPluginServices` with `server_state` and `state_query`
    /// already set by `PluginManager::set_server_state` / `set_default_state`.
    /// Tests should use `new_with_ctx` instead.
    pub fn new(
        wasm_bytes: &[u8],
        plugin_name: String,
        services: Arc<WasmPluginServices>,
        runtime: WasmRuntimeConfig,
    ) -> Result<Self, String> {
        use crate::plugin_abi::wit_abi;
        let mut engine_config = wasmtime::Config::new();
        engine_config.wasm_component_model(true);
        engine_config.wasm_bulk_memory(true);
        engine_config.wasm_multi_value(true);
        engine_config.wasm_backtrace(true);
        // Fuel metering disabled — wasm init needs full execution to complete
        engine_config.max_wasm_stack(runtime.max_stack_bytes.max(64 * 1024));
        let engine = wasmtime::Engine::new(&engine_config)
            .map_err(|e| format!("engine creation: {e}"))?;
        let component = wasmtime::component::Component::new(&engine, wasm_bytes)
            .map_err(|e| format!("component compile: {e}"))?;
        let mut linker = wasmtime::component::Linker::<WitHostState>::new(&engine);
        wit_abi::PhiraPluginV2::add_to_linker(&mut linker, |state| &mut state.host)
            .map_err(|e| format!("linker setup: {e}"))?;
        let ctx = Self::build_context_from_services(&services)?;
        Self::new_with_context(engine, component, linker, ctx, plugin_name)
    }

    /// Create a WIT component from raw bytes with a pre-built host context.
    ///
    /// Tests that don't have a running server can construct a
    /// `WitHostContext` directly (or use a mock `ServerStateQuery`)
    /// and pass it here.  The engine and linker are set up internally.
    pub fn from_bytes_ctx(
        wasm_bytes: &[u8],
        plugin_name: String,
        ctx: Arc<crate::wit_host::WitHostContext>,
        runtime: WasmRuntimeConfig,
    ) -> Result<Self, String> {
        use crate::plugin_abi::wit_abi;
        let mut engine_config = wasmtime::Config::new();
        engine_config.wasm_component_model(true);
        engine_config.wasm_bulk_memory(true);
        engine_config.wasm_multi_value(true);
        engine_config.wasm_backtrace(true);
        engine_config.max_wasm_stack(runtime.max_stack_bytes.max(64 * 1024));
        let engine = wasmtime::Engine::new(&engine_config)
            .map_err(|e| format!("engine creation: {e}"))?;
        let component = wasmtime::component::Component::new(&engine, wasm_bytes)
            .map_err(|e| format!("component compile: {e}"))?;
        let mut linker = wasmtime::component::Linker::<WitHostState>::new(&engine);
        wit_abi::PhiraPluginV2::add_to_linker(&mut linker, |state| &mut state.host)
            .map_err(|e| format!("linker setup: {e}"))?;
        Self::new_with_context(engine, component, linker, ctx, plugin_name)
    }

    /// Create a WIT component from pre-compiled engine/component/linker
    /// and a pre-built host context.  Used by `new()` and by tests that
    /// supply their own context.
    #[allow(clippy::too_many_arguments)]
    fn new_with_context(
        engine: wasmtime::Engine,
        component: wasmtime::component::Component,
        linker: wasmtime::component::Linker<WitHostState>,
        ctx: Arc<crate::wit_host::WitHostContext>,
        plugin_name: String,
    ) -> Result<Self, String> {
        use crate::plugin_abi::wit_abi as wit;
        let host = crate::wit_host::WitPluginHost::new(ctx, plugin_name.clone());
        let host_state = WitHostState { host };
        let mut store = wasmtime::Store::new(&engine, host_state);
        let instance = linker.instantiate(&mut store, &component)
            .map_err(|e| format!("instantiate component: {e}"))?;
        let component_handle = wit::PhiraPluginV2::new(&mut store, &instance)
            .map_err(|e| format!("get component handle: {e}"))?;
        let info = PluginInfo {
            name: plugin_name.clone(),
            version: "0.1.0".to_string(),
            author: "unknown".to_string(),
            description: "WIT component plugin".to_string(),
        };
        Ok(Self { store, component: component_handle, info, plugin_name, initialized: false })
    }

    pub fn call_init(&mut self) -> Result<(), String> {
        let result = self.component.call_init(&mut self.store)
            .map_err(|e| format!("component init trap: {e}"))?;
        result.map_err(|e| format!("component init returned error: {e}"))?;
        self.initialized = true;
        Ok(())
    }

    pub fn call_cleanup(&mut self) {
        if !self.initialized { return; }
        let _ = self.component.call_cleanup(&mut self.store);
        self.initialized = false;
    }

    pub fn call_on_event(&mut self, event: &PluginEvent) -> Result<i32, String> {
        use crate::plugin_abi::wit_abi::phira::plugin::phira_events as wit_events;
        let wit_event = match event {
            PluginEvent::UserConnect { user_id, user_name, user_ip } => {
                wit_events::PluginEvent::UserConnect(wit_events::UserConnectInfo {
                    user_id: *user_id as u32, user_name: user_name.clone(), user_ip: user_ip.clone(),
                })
            }
            PluginEvent::UserDisconnect { user_id, user_name } => {
                wit_events::PluginEvent::UserDisconnect(wit_events::UserDisconnectInfo {
                    user_id: *user_id as u32, user_name: user_name.clone(),
                })
            }
            PluginEvent::RoomCreate { user_id, room_id } => {
                wit_events::PluginEvent::RoomCreate(wit_events::RoomUserEvent {
                    user_id: *user_id as u32, room_id: room_id.clone(),
                })
            }
            PluginEvent::RoomJoin { user_id, room_id, is_monitor } => {
                wit_events::PluginEvent::RoomJoin(wit_events::RoomJoinInfo {
                    user_id: *user_id as u32, room_id: room_id.clone(), is_monitor: *is_monitor,
                })
            }
            PluginEvent::RoomLeave { user_id, room_id } => {
                wit_events::PluginEvent::RoomLeave(wit_events::RoomUserEvent {
                    user_id: *user_id as u32, room_id: room_id.clone(),
                })
            }
            PluginEvent::RoomModify { user_id, room_id, data } => {
                wit_events::PluginEvent::RoomModify(wit_events::RoomModifyInfo {
                    user_id: *user_id as u32, room_id: room_id.clone(), data: data.clone(),
                })
            }
            PluginEvent::GameStart { user_id, room_id } => {
                wit_events::PluginEvent::GameStart(wit_events::RoomUserEvent {
                    user_id: *user_id as u32, room_id: room_id.clone(),
                })
            }
            PluginEvent::GameEnd { user_id, user_name, room_id, score, accuracy, perfect, good, bad, miss, max_combo, full_combo } => {
                wit_events::PluginEvent::GameEnd(wit_events::GameEndInfo {
                    user_id: *user_id as u32, room_id: room_id.clone(),
                    game_result: wit_events::GameEndRecord {
                        user_id: *user_id as u32, user_name: user_name.clone(),
                        score: *score as u32, accuracy: *accuracy,
                        perfect: *perfect as u32, good: *good as u32,
                        bad: *bad as u32, miss: *miss as u32,
                        max_combo: *max_combo as u32, full_combo: *full_combo,
                    },
                })
            }
            PluginEvent::PlayerTouches { user_id, room_id, data } => {
                let wit_points: Vec<wit_events::TouchEventPoint> = data.iter().map(|p| wit_events::TouchEventPoint {
                    time: p.time, finger: p.finger as u32, x: p.x, y: p.y,
                }).collect();
                wit_events::PluginEvent::PlayerTouches(wit_events::PlayerTouchesInfo {
                    user_id: *user_id as u32, room_id: room_id.clone(), data: wit_points,
                })
            }
            PluginEvent::PlayerJudges { user_id, room_id, data } => {
                let wit_items: Vec<wit_events::JudgeEventItem> = data.iter().map(|j| wit_events::JudgeEventItem {
                    time: j.time, line_id: j.line_id, note_id: j.note_id, judgement: j.judgement.clone(),
                }).collect();
                wit_events::PluginEvent::PlayerJudges(wit_events::PlayerJudgesInfo {
                    user_id: *user_id as u32, room_id: room_id.clone(), data: wit_items,
                })
            }
            PluginEvent::RoundComplete { room_id, chart_id, chart_name } => {
                wit_events::PluginEvent::RoundComplete(wit_events::RoundCompleteInfo {
                    room_id: room_id.clone(), chart_id: *chart_id as u32, chart_name: chart_name.clone(),
                })
            }
        };
        let result = self.component.call_on_event(&mut self.store, &wit_event)
            .map_err(|e| format!("component on_event: {e}"))?;
        match result {
            Ok(handled) => Ok(if handled { 1 } else { 0 }),
            Err(e) => Err(format!("component on_event returned error: {e}")),
        }
    }

    pub fn call_api(&mut self, method: &str, args: &[serde_json::Value]) -> Result<serde_json::Value, String> {
        use crate::plugin_abi::wit_abi::phira::plugin::phira_types as types;
        let wit_args: Vec<types::JsonValue> = args.iter().map(|v| crate::wit_host::json_value_to_wit(v)).collect();
        let result = self.component.call_on_api(&mut self.store, method, &wit_args)
            .map_err(|e| format!("component on_api: {e}"))?;
        match result {
            types::ApiResult::Ok(value) => Ok(crate::wit_host::wit_json_value_to_serde(&value)),
            types::ApiResult::Error(e) => Err(e),
        }
    }
}

#[cfg(feature = "wit-bindgen")]
impl Drop for WitPluginComponent {
    fn drop(&mut self) {
        self.call_cleanup();
    }
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wasm_host_helpers;

    #[test]
    fn default_capabilities_are_not_privileged() {
        let caps = wasm_host_helpers::default_capabilities();
        assert!(!caps.contains("admin"), "default must not include admin");
        assert!(!caps.contains("room.manage"), "default must not include room.manage");
    }

    #[test]
    fn identifiers_are_restricted() {
        assert!(wasm_host_helpers::validate_identifier("").is_err());
        assert!(wasm_host_helpers::validate_identifier("hello world").is_err());
        assert!(wasm_host_helpers::validate_identifier("valid-name.v2").is_ok());
    }

    // ── WASM component integration tests ──────────────────────────

    fn wasm_fixture_path() -> std::path::PathBuf {
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest.join("tests/test-plugin.component.wasm")
    }

    fn load_wasm_bytes() -> Vec<u8> {
        std::fs::read(wasm_fixture_path())
            .expect("test-plugin.component.wasm not found — run `make` in tests/test-plugin/")
    }

    fn mock_host_context() -> Arc<crate::wit_host::WitHostContext> {
        let state_query = phira_mp_plus_server_api::ServerStateQuery::new(|method: &str, _args: &[serde_json::Value]| {
            Err(format!("mock: no handler for {method}"))
        });
        let extensions = Arc::new(crate::extensions::ExtensionManager::new_in_memory());
        Arc::new(crate::wit_host::WitHostContext {
            state_query,
            extensions: Arc::clone(&extensions),
            room_commands: Arc::new(crate::room_actor::RoomCommandGateway::new()),
            ban_manager: Arc::new(crate::ban::BanManager::new(extensions)),
            simulation: Arc::new(crate::simulation::SimulationManager::new()),
            event_bus: Arc::new(crate::event_bus::EventBus::new(1024)),
            http_timeout_secs: 10,
            http_max_body: 1024 * 1024,
        })
    }

    /// Load the test WASM component.  Panics on failure (test infrastructure issue).
    fn load_component() -> WitPluginComponent {
        let bytes = load_wasm_bytes();
        let ctx = mock_host_context();
        WitPluginComponent::from_bytes_ctx(&bytes, "test-plugin".into(), ctx, WasmRuntimeConfig::default())
            .expect("WitPluginComponent::from_bytes_ctx should succeed")
    }

    mod wasm_tests {
        use super::*;

        #[test]
        fn load_succeeds() {
            load_component();
        }

        #[test]
        fn init_returns_ok() {
            let mut c = load_component();
            c.call_init().unwrap();
            assert!(c.initialized);
        }

        #[test]
        fn info_has_default_placeholder() {
            // The version is a placeholder until call_init() is called.
            // The plugin reports "0.1.0-test" via get_info() but the
            // component doesn't update its info field from that call.
            let c = load_component();
            assert_eq!(c.info.name, "test-plugin");
            assert_eq!(c.info.version, "0.1.0");
        }

        #[test]
        fn cleanup_does_not_panic() {
            let mut c = load_component();
            c.call_init().unwrap();
            c.call_cleanup();
            assert!(!c.initialized);
        }

        #[test]
        fn double_cleanup_is_safe() {
            let mut c = load_component();
            c.call_cleanup();
            c.call_cleanup();
        }

        #[test]
        fn on_event_returns_false() {
            let mut c = load_component();
            c.call_init().unwrap();
            let result = c.call_on_event(&PluginEvent::UserConnect {
                user_id: 1, user_name: "test".into(), user_ip: "127.0.0.1".into(),
            });
            assert_eq!(result, Ok(0), "unhandled event should return 0");
        }

        #[test]
        fn on_api_ping() {
            let mut c = load_component();
            c.call_init().unwrap();
            assert_eq!(c.call_api("ping", &[]), Ok(serde_json::json!(null)));
        }

        #[test]
        fn on_api_echo_roundtrip() {
            let mut c = load_component();
            c.call_init().unwrap();
            assert_eq!(c.call_api("echo", &[serde_json::json!("hello")]), Ok(serde_json::json!("hello")));
        }

        #[test]
        fn on_api_count_increments() {
            let mut c = load_component();
            c.call_init().unwrap();
            assert_eq!(c.call_api("count", &[]), Ok(serde_json::json!(0)));
            assert_eq!(c.call_api("count", &[]), Ok(serde_json::json!(1)));
            assert_eq!(c.call_api("count", &[]), Ok(serde_json::json!(2)));
        }

        #[test]
        fn on_api_unknown_method_returns_error() {
            let mut c = load_component();
            c.call_init().unwrap();
            assert!(c.call_api("nonexistent", &[]).is_err());
        }
    }
}

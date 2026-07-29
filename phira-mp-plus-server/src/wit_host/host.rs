//! Core host types and WIT host implementations.
//!
//! Decoupled from PlusServerState — the host depends only on
//! WitHostContext (an explicit bundle of the subsystems it needs)
//! and the generic ServerStateQuery from the api crate.

use phira_mp_plus_server_api as api;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Explicit dependency bundle for the WIT host.
///
/// Instead of grabbing the entire PlusServerState, WitPluginHost
/// only sees the subsystems it actually uses.  This makes the
/// dependency boundary visible and simplifies testing.
pub struct WitHostContext {
    /// Generic query dispatch (wraps server_state_query_for_host).
    pub state_query: api::ServerStateQuery,
    /// Extension manager for user/room extra data.
    pub extensions: Arc<crate::extensions::ExtensionManager>,
    /// Room command gateway.
    ///
    /// NOTE(Phase2-WorkD): Currently unused by the WIT host implementation.
    /// Room mutations are dispatched through `state_query` →
    /// `server_state_query_dispatch` → `s.room_commands.*` (gateway).  The
    /// `room_commands` field is included here for future direct use when the
    /// gateway is refactored to not require PlusServerState as a parameter.
    pub room_commands: Arc<crate::room_actor::RoomCommandGateway>,
    /// Ban manager.
    pub ban_manager: Arc<crate::ban::BanManager>,
    /// Event bus (for dispatching PluginEvents).
    pub event_bus: Arc<crate::event_bus::EventBus>,
    /// Immutable capability grant bound to this plugin instance.
    pub capabilities: Arc<HashSet<String>>,
    /// Node key pair for crypto operations (derived from HSN_SECRET_KEY).
    pub node_key: Arc<crate::crypto::NodeKey>,
    /// Shared timer registry (plugin_name → timer map).
    pub timers: Arc<Mutex<HashMap<String, HashMap<String, tokio::task::JoinHandle<()>>>>>,
    /// Host messaging callback. uid=0 means broadcast.
    pub send_chat: Option<Arc<dyn Fn(i32, String) + Send + Sync>>,
    /// Timer fire callback: calls the plugin's on_api("timer:fired", ...).
    pub timer_callback: Option<Arc<dyn Fn(String, String) + Send + Sync>>,
    /// HTTP sandbox timeout (seconds).
    pub http_timeout_secs: u64,
    /// HTTP sandbox max response body (bytes).
    pub http_max_body: usize,
    /// Whether plugin HTTP calls may target private/reserved addresses.
    pub http_allow_private_network: bool,
    /// TCP actor sender for plugin-initiated connections.
    pub tcp: Option<tokio::sync::mpsc::Sender<crate::plugin_tcp::PluginTcpCommand>>,
    /// TCP event callback: forwards tcp:accept/data/disconnect events to plugin's on-api.
    pub tcp_callback: Option<Arc<dyn Fn(String, serde_json::Value) + Send + Sync>>,
    /// Room state query access (for phira-room-state interface).
    pub room_state_query: Option<Arc<dyn Fn(String) -> Result<serde_json::Value, String> + Send + Sync>>,
    /// Plugin handler registry: method → owner + metadata.
    pub api_handlers: Arc<Mutex<HashMap<String, RegisteredHandler>>>,
    /// Shared handler registry (PluginManager/WasmPluginServices), same Arc as
    /// PluginManager.api_handlers.  Methods registered via register_handler are
    /// also inserted here so PluginManager can dispatch to them.
    pub services_handlers: Option<Arc<Mutex<HashMap<String, api::PluginApiHandler>>>>,
    /// Tracks method name ownership for the shared handler registry.
    /// Maps plugin_name -> list of handler methods owned by that plugin.
    /// Used by PluginManager::remove_plugin to clean up stale handlers.
    pub handler_owners: Option<Arc<Mutex<HashMap<String, Vec<String>>>>>,
    /// Dispatch function that forwards an API call to this plugin via
    /// PluginManager::call_plugin_api.  Set in build_context_from_services.
    pub api_forward: Option<Arc<dyn Fn(&str, &[serde_json::Value]) -> Result<serde_json::Value, String> + Send + Sync>>,
}

/// A registered plugin handler.
#[derive(Debug, Clone)]
pub struct RegisteredHandler {
    pub plugin_name: String,
    pub method: String,
    pub description: String,
    pub request_schema: Option<String>,
    pub response_schema: Option<String>,
}

/// Wraps server capabilities to implement WIT host traits.
pub struct WitPluginHost {
    pub(crate) ctx: Arc<WitHostContext>,
    pub(crate) plugin_name: String,
}

impl WitPluginHost {
    pub fn new(ctx: Arc<WitHostContext>, plugin_name: String) -> Self {
        Self { ctx, plugin_name }
    }

    pub fn name(&self) -> &str {
        &self.plugin_name
    }

    pub fn require_capability(&self, capability: &str) -> Result<(), String> {
        if self.ctx.capabilities.contains(capability) {
            Ok(())
        } else {
            Err(format!(
                "plugin '{}' lacks capability '{}'",
                self.plugin_name, capability
            ))
        }
    }

    #[cfg(feature = "wit-bindgen")]
    pub(crate) fn require_api_capability(
        &self,
        capability: &str,
    ) -> Result<(), crate::plugin_abi::wit_abi::phira::plugin::phira_types::ApiResult> {
        self.require_capability(capability)
            .map_err(crate::plugin_abi::wit_abi::phira::plugin::phira_types::ApiResult::Error)
    }

    /// Run an async fn synchronously with panic protection.
    ///
    /// Every WIT host method is sync, but most server operations are async.
    /// This helper takes an async closure that receives an `Arc<WitHostContext>`
    /// and blocks on it inside `catch_unwind`, so a panicking plugin call never
    /// takes down the host thread. Unlike the old `block_on_sync`, this does
    /// not require additional `futures::executor::block_on` inside the closure.
    pub(crate) fn block_on_async<T, F, Fut>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(Arc<WitHostContext>) -> Fut + Send,
        Fut: std::future::Future<Output = T> + Send,
        T: Send,
    {
        let ctx = Arc::clone(&self.ctx);
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => handle.block_on(f(ctx)),
                Err(_) => futures::executor::block_on(f(ctx)),
            }
        }))
        .map_err(|_| "WIT host operation panicked — plugin disabled".to_string())
    }
}

/// Convert a serde_json::Value to a WIT JsonValue. Only available with wit-bindgen.
#[cfg(feature = "wit-bindgen")]
pub fn json_value_to_wit(
    value: &serde_json::Value,
) -> crate::plugin_abi::wit_abi::phira::plugin::phira_types::JsonValue {
    use crate::plugin_abi::wit_abi::phira::plugin::phira_types::JsonValue;
    match value {
        serde_json::Value::Null => JsonValue::Null,
        serde_json::Value::Bool(b) => JsonValue::Flag(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                JsonValue::Integer(i)
            } else if let Some(f) = n.as_f64() {
                JsonValue::Float(f)
            } else {
                JsonValue::Text(n.to_string())
            }
        }
        serde_json::Value::String(s) => JsonValue::Text(s.clone()),
        serde_json::Value::Array(arr) => {
            JsonValue::Array(serde_json::to_string(arr).unwrap_or_default())
        }
        serde_json::Value::Object(obj) => {
            JsonValue::Object(serde_json::to_string(obj).unwrap_or_default())
        }
    }
}

/// Convert a WIT JsonValue back to serde_json::Value. Only available with wit-bindgen.
#[cfg(feature = "wit-bindgen")]
pub fn wit_json_value_to_serde(
    value: &crate::plugin_abi::wit_abi::phira::plugin::phira_types::JsonValue,
) -> serde_json::Value {
    use crate::plugin_abi::wit_abi::phira::plugin::phira_types::JsonValue;
    match value {
        JsonValue::Null => serde_json::Value::Null,
        JsonValue::Flag(b) => serde_json::Value::Bool(*b),
        JsonValue::Integer(i) => serde_json::json!(*i),
        JsonValue::Float(f) => serde_json::json!(*f),
        JsonValue::Text(s) => serde_json::Value::String(s.clone()),
        JsonValue::Array(s) | JsonValue::Object(s) => {
            serde_json::from_str(s).unwrap_or(serde_json::Value::String(s.clone()))
        }
    }
}

/// Normalize API arguments for plugin-scoped methods.
/// Only available with wit-bindgen.
#[cfg(feature = "wit-bindgen")]
pub(crate) fn normalize_plugin_scoped_api_args(
    method: &str,
    plugin_name: &str,
    args: Vec<serde_json::Value>,
) -> Result<Vec<serde_json::Value>, String> {
    if method != "http.register_route" {
        return Ok(args);
    }

    let path = match args.first() {
        Some(serde_json::Value::Object(config)) => {
            config.get("path").and_then(serde_json::Value::as_str)
        }
        Some(value) => value.as_str(),
        None => None,
    }
    .ok_or_else(|| "path required".to_string())?;

    Ok(vec![
        serde_json::Value::String(path.to_string()),
        serde_json::Value::String(plugin_name.to_string()),
    ])
}

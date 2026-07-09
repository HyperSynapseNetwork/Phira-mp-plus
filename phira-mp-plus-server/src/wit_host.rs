//! WIT/component-model host trait implementations.
//!
//! Decoupled from PlusServerState — the host depends only on
//! WitHostContext (an explicit bundle of the subsystems it needs)
//! and the generic ServerStateQuery from the api crate.
//!
//! The core `WitPluginHost` skeleton is available with `plugin-system`.
//! The generated trait impls require `wit-bindgen` (default feature).

use phira_mp_plus_server_api as api;
use std::sync::Arc;

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
    pub room_commands: Arc<crate::room_actor::RoomCommandGateway>,
    /// Ban manager.
    pub ban_manager: Arc<crate::ban::BanManager>,
    /// Simulation manager.
    pub simulation: Arc<crate::simulation::SimulationManager>,
    /// Event bus (for dispatching PluginEvents).
    pub event_bus: Arc<crate::event_bus::EventBus>,
    /// HTTP sandbox timeout (seconds).
    pub http_timeout_secs: u64,
    /// HTTP sandbox max response body (bytes).
    pub http_max_body: usize,
}

/// Wraps server capabilities to implement WIT host traits.
pub struct WitPluginHost {
    ctx: Arc<WitHostContext>,
    plugin_name: String,
}

impl WitPluginHost {
    pub fn new(ctx: Arc<WitHostContext>, plugin_name: String) -> Self {
        Self { ctx, plugin_name }
    }

    pub fn name(&self) -> &str {
        &self.plugin_name
    }

    /// Convenience: run an async fn synchronously with panic protection.
    ///
    /// Every WIT host method is sync, but most server operations are async.
    /// This helper uses `futures::executor::block_on` to bridge sync→async
    /// inside `catch_unwind`, so a panicking plugin call never takes down
    /// the host thread. The closure returns a `Result<T, String>` directly
    /// (synchronously), not a future — avoiding the lifetime issues with
    /// async closures borrowing from the `&WitHostContext` reference.
    fn block_on_sync<T, F>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&WitHostContext) -> T + Send,
        T: Send,
    {
        let ctx = Arc::clone(&self.ctx);
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => handle.block_on(async { f(&ctx) }),
                Err(_) => futures::executor::block_on(async { f(&ctx) }),
            }
        }))
        .map_err(|_| "WIT host operation panicked — plugin disabled".to_string())
    }
}

#[cfg(test)]
mod capability_tests {
    use crate::wasm_host_helpers;

    #[test]
    fn required_cap_maps_admin_methods() {
        assert_eq!(wasm_host_helpers::required_capability("admin.list"), Some("admin"));
        assert_eq!(wasm_host_helpers::required_capability("admin.add"), Some("admin"));
    }

    #[test]
    fn required_cap_maps_room_methods() {
        assert_eq!(wasm_host_helpers::required_capability("room.set_lock"), Some("room.manage"));
        assert_eq!(wasm_host_helpers::required_capability("room.kick"), Some("room.manage"));
    }

    #[test]
    fn required_cap_returns_none_for_unguarded_methods() {
        assert_eq!(wasm_host_helpers::required_capability("uuid.v4"), None);
        assert_eq!(wasm_host_helpers::required_capability("time.now"), None);
    }

    #[test]
    fn default_capabilities_dont_include_privileged() {
        let caps = wasm_host_helpers::default_capabilities();
        assert!(!caps.contains("admin"), "default must not include admin");
        assert!(!caps.contains("room.manage"), "default must not include room.manage");
        assert!(caps.contains("state.read"), "default must include state.read");
        assert!(caps.contains("config"), "default must include config");
    }

    #[test]
    fn persist_methods_require_state_read() {
        // persist.* methods require the "state.read" capability.
        let methods = ["persist.events", "persist.rooms", "persist.touches", "persist.judges"];
        for method in &methods {
            let cap = wasm_host_helpers::required_capability(method);
            assert_eq!(cap, Some("state.read"), "method {method} should require state.read");
        }
    }

    #[test]
    fn admin_methods_require_admin() {
        let methods = ["admin.list", "admin.add", "admin.remove", "admin.set"];
        for method in &methods {
            let cap = wasm_host_helpers::required_capability(method);
            assert_eq!(cap, Some("admin"), "method {method} should require admin");
        }
    }
}

// ══════════════════════════════════════════════════════════════════════
// Generated trait implementations — only with wit-bindgen feature.
// ══════════════════════════════════════════════════════════════════════

/// Convert a serde_json::Value to a WIT JsonValue. Only available with wit-bindgen.
#[cfg(feature = "wit-bindgen")]
pub fn json_value_to_wit(value: &serde_json::Value) -> crate::plugin_abi::wit_abi::phira::plugin::phira_types::JsonValue {
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
pub fn wit_json_value_to_serde(value: &crate::plugin_abi::wit_abi::phira::plugin::phira_types::JsonValue) -> serde_json::Value {
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

#[cfg(feature = "wit-bindgen")]
mod wit_trait_impls {
    use super::WitPluginHost;
    use crate::plugin_abi::wit_abi as wit;
    use wit::phira::plugin::phira_types as types;

    /// Helper: call ServerStateQuery and convert to ApiResult.
    fn query_api_result(host: &WitPluginHost, method: &str, args: &[serde_json::Value]) -> types::ApiResult {
        match host.ctx.state_query.call(method, args) {
            Ok(value) => types::ApiResult::Ok(json_to_wit_json(&value)),
            Err(e) => types::ApiResult::Error(e),
        }
    }

    // ── phira-types (data-only, no methods) ──
    impl types::Host for WitPluginHost {}

    // ── phira-events (data-only, no methods) ──
    impl wit::phira::plugin::phira_events::Host for WitPluginHost {}

    // ── phira-host ──
    impl wit::phira::plugin::phira_host::Host for WitPluginHost {
        fn log(&mut self, level: String, message: String) {
            match level.as_str() {
                "error" => tracing::error!("[plugin:{}] {message}", self.plugin_name),
                "warn" => tracing::warn!("[plugin:{}] {message}", self.plugin_name),
                "info" => tracing::info!("[plugin:{}] {message}", self.plugin_name),
                "debug" => tracing::debug!("[plugin:{}] {message}", self.plugin_name),
                "trace" => tracing::trace!("[plugin:{}] {message}", self.plugin_name),
                _ => tracing::info!("[plugin:{}] {message}", self.plugin_name),
            }
        }

        fn generate_uuid(&mut self) -> String {
            uuid::Uuid::new_v4().to_string()
        }

        fn current_time_ms(&mut self) -> u64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0)
        }

        fn api_call(&mut self, method: String, args: Vec<types::JsonValue>) -> types::ApiResult {
            let args_serde: Vec<serde_json::Value> = args.iter().map(|v| wit_json_to_json(v)).collect();
            match self.ctx.state_query.call(&method, &args_serde) {
                Ok(value) => types::ApiResult::Ok(json_to_wit_json(&value)),
                Err(e) => types::ApiResult::Error(e),
            }
        }

        fn send_chat(&mut self, user_id: u32, message: String) {
            tracing::info!(
                "[chat:plugin:{}] user={user_id}: {message}",
                self.plugin_name
            );
        }

        fn http_request(
            &mut self,
            url: String,
            method: String,
            headers: Vec<(String, String)>,
            body: Vec<u8>,
        ) -> Result<types::HttpResponse, String> {
            // SSRF validation — default to not allowing private networks
            crate::wasm_host_helpers::validate_http_url(&url, false)?;

            let timeout_secs = self.ctx.http_timeout_secs.max(5);
            let max_body = self.ctx.http_max_body.max(1);

            let client = reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(timeout_secs))
                .redirect(reqwest::redirect::Policy::limited(5))
                .build()
                .map_err(|e| format!("create HTTP client: {e}"))?;

            let req = match method.to_uppercase().as_str() {
                "GET" => client.get(&url),
                "POST" => client.post(&url),
                "PUT" => client.put(&url),
                "DELETE" => client.delete(&url),
                "PATCH" => client.patch(&url),
                "HEAD" => client.head(&url),
                other => return Err(format!("unsupported HTTP method: {other}")),
            };

            let req = headers.into_iter().fold(req, |r, (k, v)| r.header(&k, &v));
            let req = if !body.is_empty() { req.body(body) } else { req };

            let response = req
                .send()
                .map_err(|e| format!("HTTP request failed: {e}"))?;

            let status = response.status().as_u16();
            let resp_headers: Vec<(String, String)> = response
                .headers()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();

            let resp_body: Vec<u8> = response
                .bytes()
                .map_err(|e| format!("read response body: {e}"))?
                .into_iter()
                .take(max_body)
                .collect();

            Ok(types::HttpResponse {
                status,
                headers: resp_headers,
                body: resp_body,
            })
        }
    }

    // ── phira-query ──
    impl wit::phira::plugin::phira_query::Host for WitPluginHost {
        fn get_user(&mut self, user_id: u32) -> types::ApiResult {
            query_api_result(self, "user_name", &[serde_json::json!(user_id as i32)])
        }
        fn get_user_extra(&mut self, user_id: u32, key: String) -> types::ApiResult {
            match self.block_on_sync(|ctx|
                futures::executor::block_on(ctx.extensions.get_user_extra(user_id as i32, &key))
            ) {
                Ok(Some(value)) => types::ApiResult::Ok(types::JsonValue::Text(value)),
                Ok(None) => types::ApiResult::Ok(types::JsonValue::Null),
                Err(e) => types::ApiResult::Error(e),
            }
        }
        fn set_user_extra(&mut self, user_id: u32, key: String, value: String) -> types::ApiResult {
            match self.block_on_sync(|ctx|
                futures::executor::block_on(ctx.extensions.set_user_extra(user_id as i32, &key, value))
            ) {
                Ok(Ok(())) => types::ApiResult::Ok(types::JsonValue::Null),
                Ok(Err(e)) | Err(e) => types::ApiResult::Error(e),
            }
        }
        fn get_room(&mut self, room_id: String) -> types::ApiResult {
            query_api_result(self, "rooms.by_name", &[serde_json::json!(room_id)])
        }
        fn get_room_extra(&mut self, room_id: String, key: String) -> types::ApiResult {
            match self.block_on_sync(|ctx|
                futures::executor::block_on(ctx.extensions.get_room_extra(&room_id, &key))
            ) {
                Ok(Some(value)) => types::ApiResult::Ok(types::JsonValue::Text(value)),
                Ok(None) => types::ApiResult::Ok(types::JsonValue::Null),
                Err(e) => types::ApiResult::Error(e),
            }
        }
        fn list_rooms(&mut self) -> types::ApiResult {
            query_api_result(self, "rooms.list", &[])
        }
        fn list_online_users(&mut self) -> types::ApiResult {
            query_api_result(self, "rooms.list", &[])
        }
        fn is_user_online(&mut self, user_id: u32) -> bool {
            // Check if the user has an active session by looking up their name.
            // If the user exists and has a session, they are online.
            matches!(query_api_result(self, "user_name", &[serde_json::json!(user_id as i32)]), types::ApiResult::Ok(_))
        }
    }

    // ── phira-room-mgmt ──
    impl wit::phira::plugin::phira_room_mgmt::Host for WitPluginHost {
        fn create_empty_room(&mut self, room_id: String, endpoint: Option<String>) -> types::ApiResult {
            let mut args = vec![serde_json::json!(room_id)];
            if let Some(ep) = endpoint {
                args.push(serde_json::json!(ep));
            }
            query_api_result(self, "room.create_empty", &args)
        }
        fn kick_from_room(&mut self, room_id: String, target_id: u32) -> types::ApiResult {
            query_api_result(self, "room.kick", &[
                serde_json::json!(room_id),
                serde_json::json!(target_id),
            ])
        }
        fn transfer_host(&mut self, room_id: String, target_id: u32) -> types::ApiResult {
            query_api_result(self, "room.set_host", &[
                serde_json::json!(room_id),
                serde_json::json!(target_id),
            ])
        }
        fn set_host(&mut self, room_id: String, target_id: Option<u32>) -> types::ApiResult {
            query_api_result(self, "room.set_host", &[
                serde_json::json!(room_id),
                serde_json::json!(target_id.map(|id| id as i32)),
            ])
        }
        fn set_room_lock(&mut self, room_id: String, locked: bool) -> types::ApiResult {
            query_api_result(self, "room.set_lock", &[
                serde_json::json!(room_id),
                serde_json::json!(locked),
            ])
        }
        fn set_room_hidden(&mut self, _room_id: String, _hidden: bool) -> types::ApiResult {
            types::ApiResult::Error(
                "set_room_hidden requires capability 'room.manage' — no mailbox command for hidden".to_string()
            )
        }
        fn close_room(&mut self, room_id: String) -> types::ApiResult {
            query_api_result(self, "room.close", &[
                serde_json::json!(room_id),
            ])
        }
        fn set_room_phira_api_endpoint(&mut self, _room_id: String, _endpoint: Option<String>) -> types::ApiResult {
            types::ApiResult::Error(
                "set_room_phira_api_endpoint requires capability 'room.manage' — no mailbox command".to_string()
            )
        }
    }

    // ── phira-user-mgmt ──
    impl wit::phira::plugin::phira_user_mgmt::Host for WitPluginHost {
        fn kick_user(&mut self, user_id: u32, reason: String) -> types::ApiResult {
            query_api_result(self, "user.kick", &[
                serde_json::json!(user_id),
                serde_json::json!(reason),
            ])
        }
        fn ban_user(&mut self, user_id: u32, reason: String) -> types::ApiResult {
            query_api_result(self, "ban.add", &[
                serde_json::json!(user_id),
                serde_json::json!(reason),
            ])
        }
        fn unban_user(&mut self, user_id: u32) -> types::ApiResult {
            query_api_result(self, "ban.remove", &[
                serde_json::json!(user_id),
            ])
        }
        fn get_ban_list(&mut self) -> types::ApiResult {
            query_api_result(self, "ban.list", &[])
        }
        fn is_banned(&mut self, user_id: u32) -> bool {
            matches!(query_api_result(self, "ban.check", &[
                serde_json::json!(user_id),
            ]), types::ApiResult::Ok(_))
        }
    }

    // ── phira-messaging ──
    impl wit::phira::plugin::phira_messaging::Host for WitPluginHost {
        fn send_to_user(&mut self, user_id: u32, message: String) -> types::ApiResult {
            tracing::info!("[plugin:{}] send_to_user({user_id}): {message}", self.plugin_name);
            types::ApiResult::Ok(types::JsonValue::Null)
        }
        fn send_to_room(&mut self, _room_id: String, message: String) -> types::ApiResult {
            tracing::info!("[plugin:{}] send_to_room: {message}", self.plugin_name);
            types::ApiResult::Ok(types::JsonValue::Null)
        }
        fn send_to_all(&mut self, message: String) -> types::ApiResult {
            tracing::info!("[plugin:{}] send_to_all: {message}", self.plugin_name);
            types::ApiResult::Ok(types::JsonValue::Null)
        }
    }

    // ── phira-persistence ──
    impl wit::phira::plugin::phira_persistence::Host for WitPluginHost {
        fn query_events(&mut self, _since: u64, _limit: u32, _kind: Option<String>, _room: Option<String>, _user: Option<u32>) -> types::ApiResult {
            types::ApiResult::Error("query_events requires capability 'persist.read' — not yet wired to PersistenceWorker".to_string())
        }
        fn query_room_snapshots(&mut self, _since: u64, _limit: u32) -> types::ApiResult {
            types::ApiResult::Error("query_room_snapshots requires capability 'persist.read' — not yet wired to PersistenceWorker".to_string())
        }
        fn query_touches(&mut self, _since: u64, _limit: u32, _round: Option<String>, _player: Option<u32>) -> types::ApiResult {
            types::ApiResult::Error("query_touches requires capability 'persist.read' — not yet wired to RoundStore".to_string())
        }
        fn query_judges(&mut self, _since: u64, _limit: u32, _round: Option<String>, _player: Option<u32>) -> types::ApiResult {
            types::ApiResult::Error("query_judges requires capability 'persist.read' — not yet wired to RoundStore".to_string())
        }
        fn get_playtime(&mut self, _user_id: u32) -> types::ApiResult {
            types::ApiResult::Error("get_playtime requires capability 'persist.read' — no DbManager query path for playtime".to_string())
        }
        fn top_playtime(&mut self, _limit: u32) -> types::ApiResult {
            types::ApiResult::Error("top_playtime requires capability 'persist.read' — no DbManager query path for playtime".to_string())
        }
    }

    // ── phira-admin ──
    impl wit::phira::plugin::phira_admin::Host for WitPluginHost {
        fn list_admin_ids(&mut self) -> types::ApiResult {
            query_api_result(self, "admin.list", &[])
        }
        fn is_admin(&mut self, user_id: u32) -> bool {
            matches!(query_api_result(self, "admin.check", &[
                serde_json::json!(user_id),
            ]), types::ApiResult::Ok(types::JsonValue::Flag(true)))
        }
        fn add_admin_id(&mut self, user_id: u32) -> types::ApiResult {
            query_api_result(self, "admin.add", &[serde_json::json!(user_id)])
        }
        fn remove_admin_id(&mut self, user_id: u32) -> types::ApiResult {
            query_api_result(self, "admin.remove", &[serde_json::json!(user_id)])
        }
        fn set_admin_ids(&mut self, ids: Vec<u32>) -> types::ApiResult {
            query_api_result(self, "admin.set", &[serde_json::json!(ids)])
        }
    }

    // ── phira-config ──
    impl wit::phira::plugin::phira_config::Host for WitPluginHost {
        fn get_config(&mut self, key: String) -> types::ApiResult {
            let path = crate::wasm_host_helpers::config_path(&self.plugin_name);
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => return types::ApiResult::Ok(types::JsonValue::Null),
            };
            let root: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(e) => return types::ApiResult::Error(format!("parse config: {e}")),
            };
            // Navigate dot-separated key path (e.g. "api.timeout")
            let value = key.split('.').fold(Some(&root), |acc, part| {
                acc.and_then(|v| v.get(part))
            });
            match value {
                Some(v) => types::ApiResult::Ok(json_to_wit_json(v)),
                None => types::ApiResult::Ok(types::JsonValue::Null),
            }
        }
        fn set_config(&mut self, key: String, value: String) -> types::ApiResult {
            let path = crate::wasm_host_helpers::config_path(&self.plugin_name);
            let mut root: serde_json::Value = std::fs::read_to_string(&path)
                .ok()
                .and_then(|c| serde_json::from_str(&c).ok())
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            let parsed: serde_json::Value = match serde_json::from_str(&value) {
                Ok(v) => v,
                Err(_) => serde_json::Value::String(value),
            };
            // Navigate to the parent object and set the key
            let keys: Vec<&str> = key.split('.').collect();
            if keys.is_empty() {
                return types::ApiResult::Error("empty key".to_string());
            }
            if keys.len() == 1 {
                if let serde_json::Value::Object(ref mut map) = root {
                    map.insert(keys[0].to_string(), parsed);
                }
            } else {
                let mut current = &mut root;
                for &part in keys.iter().take(keys.len() - 1) {
                    current = match current.get_mut(part) {
                        Some(v @ serde_json::Value::Object(_)) => v,
                        Some(_) => return types::ApiResult::Error(format!("key '{part}' is not an object")),
                        None => return types::ApiResult::Error(format!("key '{part}' not found")),
                    };
                }
                if let serde_json::Value::Object(ref mut map) = current {
                    map.insert(keys[keys.len() - 1].to_string(), parsed);
                }
            }
            // Ensure parent dir exists
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::write(&path, serde_json::to_string_pretty(&root).unwrap_or_default()) {
                Ok(()) => types::ApiResult::Ok(types::JsonValue::Null),
                Err(e) => types::ApiResult::Error(format!("write config: {e}")),
            }
        }
        fn list_config(&mut self, prefix: String) -> types::ApiResult {
            let path = crate::wasm_host_helpers::config_path(&self.plugin_name);
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => return types::ApiResult::Ok(types::JsonValue::Array("[]".to_string())),
            };
            let root: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(e) => return types::ApiResult::Error(format!("parse config: {e}")),
            };
            // Collect keys that start with the given prefix
            fn collect_keys(value: &serde_json::Value, prefix: &str, current: &str) -> Vec<String> {
                match value {
                    serde_json::Value::Object(map) => {
                        let mut keys = Vec::new();
                        for (k, v) in map {
                            let path = if current.is_empty() { k.clone() } else { format!("{current}.{k}") };
                            if path.starts_with(prefix) {
                                keys.push(path.clone());
                            }
                            keys.extend(collect_keys(v, prefix, &path));
                        }
                        keys
                    }
                    _ => Vec::new(),
                }
            }
            let keys: Vec<String> = collect_keys(&root, &prefix, "")
                .into_iter()
                .filter(|k| k.starts_with(&prefix))
                .collect();
            types::ApiResult::Ok(json_to_wit_json(&serde_json::json!(keys)))
        }
        fn reload_config(&mut self) -> types::ApiResult {
            let path = crate::wasm_host_helpers::config_path(&self.plugin_name);
            match std::fs::read_to_string(&path) {
                Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
                    Ok(_) => types::ApiResult::Ok(types::JsonValue::Null),
                    Err(e) => types::ApiResult::Error(format!("reload config parse: {e}")),
                },
                Err(e) => types::ApiResult::Error(format!("reload config read: {e}")),
            }
        }
        fn poll_config_changes(&mut self, _since: u64) -> types::ApiResult {
            // Simple implementation: check if the config file exists and return its
            // modification time as a version indicator.
            let path = crate::wasm_host_helpers::config_path(&self.plugin_name);
            match std::fs::metadata(&path) {
                Ok(meta) => {
                    let version = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    types::ApiResult::Ok(types::JsonValue::Integer(version as i64))
                }
                Err(_) => types::ApiResult::Ok(types::JsonValue::Null),
            }
        }
    }

    // ── phira-simulation ──
    impl wit::phira::plugin::phira_simulation::Host for WitPluginHost {
        fn status(&mut self) -> types::ApiResult {
            let result = self.block_on_sync(|ctx|
                futures::executor::block_on(ctx.simulation.status())
            );
            match result {
                Ok(status) => {
                    let json = serde_json::to_value(&status).unwrap_or_default();
                    types::ApiResult::Ok(json_to_wit_json(&json))
                }
                Err(e) => types::ApiResult::Error(e),
            }
        }
        fn run(&mut self, preset: String, users: Option<u32>, rooms: Option<u32>, duration: Option<u32>) -> types::ApiResult {
            let result = self.block_on_sync(|ctx| {
                let mut config = crate::simulation::SimulationConfig::default();
                if let Some(p) = crate::simulation::SimulationPreset::parse(&preset) {
                    config = p.defaults(ctx.simulation.seed_hint());
                }
                if let Some(u) = users { config.users = u as usize; }
                if let Some(r) = rooms { config.rooms = r as usize; }
                if let Some(d) = duration { config.duration_secs = d as u64; }
                futures::executor::block_on(ctx.simulation.start(config))
            });
            match result {
                Ok(status) => {
                    let json = serde_json::to_value(&status).unwrap_or_default();
                    types::ApiResult::Ok(json_to_wit_json(&json))
                }
                Err(e) => types::ApiResult::Error(e),
            }
        }
        fn stop(&mut self) -> types::ApiResult {
            let result = self.block_on_sync(|ctx|
                futures::executor::block_on(ctx.simulation.stop("stopped via plugin API"))
            );
            match result {
                Ok(status) => {
                    let json = serde_json::to_value(&status).unwrap_or_default();
                    types::ApiResult::Ok(json_to_wit_json(&json))
                }
                Err(e) => types::ApiResult::Error(e),
            }
        }
        fn cleanup(&mut self) -> types::ApiResult {
            let result = self.block_on_sync(|ctx|
                futures::executor::block_on(ctx.simulation.cleanup())
            );
            match result {
                Ok(status) => {
                    let json = serde_json::to_value(&status).unwrap_or_default();
                    types::ApiResult::Ok(json_to_wit_json(&json))
                }
                Err(e) => types::ApiResult::Error(e),
            }
        }
    }

    // ── phira-runtime ──
    impl wit::phira::plugin::phira_runtime::Host for WitPluginHost {
        fn status(&mut self) -> types::ApiResult {
            query_api_result(self, "runtime.status", &[])
        }
        fn events(&mut self, limit: Option<u32>) -> types::ApiResult {
            query_api_result(self, "runtime.event_stats", &[
                serde_json::json!(limit.unwrap_or(50)),
            ])
        }
        fn commands(&mut self) -> types::ApiResult {
            query_api_result(self, "runtime.commands", &[])
        }
    }

    // ═══════════════════════════════════════════════════════════════
    // JSON conversion helpers
    // ═══════════════════════════════════════════════════════════════

    fn json_to_wit_json(value: &serde_json::Value) -> types::JsonValue {
        use types::JsonValue;
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

    fn wit_json_to_json(value: &types::JsonValue) -> serde_json::Value {
        use types::JsonValue;
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
}

#[cfg(test)]
mod tests {
    #[test]
    fn uuid_format_is_valid() {
        let s = uuid::Uuid::new_v4().to_string();
        assert_eq!(s.len(), 36);
        assert_eq!(s.chars().filter(|&c| c == '-').count(), 4);
    }
}

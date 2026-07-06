//! WIT/component-model host trait implementations.
//!
//! The core `WitPluginHost` skeleton is available with `plugin-system`.
//! The generated trait impls require `wit-bindgen` (default feature).

use crate::server::PlusServerState;
use std::sync::Arc;

/// Wraps server state to implement WIT host traits.
pub struct WitPluginHost {
    state: Arc<PlusServerState>,
    plugin_name: String,
}

impl WitPluginHost {
    pub fn new(state: Arc<PlusServerState>, plugin_name: String) -> Self {
        Self { state, plugin_name }
    }

    pub fn name(&self) -> &str {
        &self.plugin_name
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

    /// Helper: call server_state_query_for_host and convert to ApiResult.
    fn query_api_result(state: &std::sync::Arc<crate::server::PlusServerState>, method: &str, args: &[serde_json::Value]) -> types::ApiResult {
        match crate::server::server_state_query_for_host(state, method, args) {
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
            match crate::server::server_state_query_for_host(&self.state, &method, &args_serde) {
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
            _url: String,
            _method: String,
            _headers: Vec<(String, String)>,
            _body: Vec<u8>,
        ) -> Result<types::HttpResponse, String> {
            Err("http_request not yet implemented for WIT host".to_string())
        }
    }

    // ── phira-query ──
    impl wit::phira::plugin::phira_query::Host for WitPluginHost {
        fn get_user(&mut self, user_id: u32) -> types::ApiResult {
            query_api_result(&self.state, "user_name", &[serde_json::json!(user_id as i32)])
        }
        fn get_user_extra(&mut self, user_id: u32, key: String) -> types::ApiResult {
            let state = std::sync::Arc::clone(&self.state);
            match futures::executor::block_on(state.extensions.get_user_extra(user_id as i32, &key)) {
                Some(value) => types::ApiResult::Ok(types::JsonValue::Text(value)),
                None => types::ApiResult::Ok(types::JsonValue::Null),
            }
        }
        fn set_user_extra(&mut self, user_id: u32, key: String, value: String) -> types::ApiResult {
            let state = std::sync::Arc::clone(&self.state);
            match futures::executor::block_on(state.extensions.set_user_extra(user_id as i32, &key, value)) {
                Ok(()) => types::ApiResult::Ok(types::JsonValue::Null),
                Err(e) => types::ApiResult::Error(e),
            }
        }
        fn get_room(&mut self, room_id: String) -> types::ApiResult {
            query_api_result(&self.state, "rooms.by_name", &[serde_json::json!(room_id)])
        }
        fn get_room_extra(&mut self, room_id: String, key: String) -> types::ApiResult {
            let state = std::sync::Arc::clone(&self.state);
            match futures::executor::block_on(state.extensions.get_room_extra(&room_id, &key)) {
                Some(value) => types::ApiResult::Ok(types::JsonValue::Text(value)),
                None => types::ApiResult::Ok(types::JsonValue::Null),
            }
        }
        fn list_rooms(&mut self) -> types::ApiResult {
            query_api_result(&self.state, "rooms.list", &[])
        }
        fn list_online_users(&mut self) -> types::ApiResult {
            query_api_result(&self.state, "rooms.list", &[])
        }
        fn is_user_online(&mut self, user_id: u32) -> bool {
            // Check if the user has an active session by looking up their name.
            // If the user exists and has a session, they are online.
            matches!(query_api_result(&self.state, "user_name", &[serde_json::json!(user_id as i32)]), types::ApiResult::Ok(_))
        }
    }

    // ── phira-room-mgmt ──
    impl wit::phira::plugin::phira_room_mgmt::Host for WitPluginHost {
        fn create_empty_room(&mut self, _room_id: String, _endpoint: Option<String>) -> types::ApiResult {
            types::ApiResult::Error("create_empty_room not yet implemented — async write needs block_on".to_string())
        }
        fn kick_from_room(&mut self, room_id: String, target_id: u32) -> types::ApiResult {
            let state = std::sync::Arc::clone(&self.state);
            match futures::executor::block_on(
                state.room_commands.kick_user(&state, &room_id, target_id as i32)
            ) {
                Ok(v) => types::ApiResult::Ok(json_to_wit_json(&v)),
                Err(e) => types::ApiResult::Error(e),
            }
        }
        fn transfer_host(&mut self, room_id: String, target_id: u32) -> types::ApiResult {
            let state = std::sync::Arc::clone(&self.state);
            match futures::executor::block_on(
                state.room_commands.set_host(&state, &room_id, Some(target_id as i32))
            ) {
                Ok(v) => types::ApiResult::Ok(json_to_wit_json(&v)),
                Err(e) => types::ApiResult::Error(e),
            }
        }
        fn set_host(&mut self, room_id: String, target_id: Option<u32>) -> types::ApiResult {
            let state = std::sync::Arc::clone(&self.state);
            match futures::executor::block_on(
                state.room_commands.set_host(&state, &room_id, target_id.map(|id| id as i32))
            ) {
                Ok(v) => types::ApiResult::Ok(json_to_wit_json(&v)),
                Err(e) => types::ApiResult::Error(e),
            }
        }
        fn set_room_lock(&mut self, room_id: String, locked: bool) -> types::ApiResult {
            let state = std::sync::Arc::clone(&self.state);
            match futures::executor::block_on(
                state.room_commands.set_lock(&state, &room_id, locked)
            ) {
                Ok(v) => types::ApiResult::Ok(json_to_wit_json(&v)),
                Err(e) => types::ApiResult::Error(e),
            }
        }
        fn set_room_hidden(&mut self, _room_id: String, _hidden: bool) -> types::ApiResult {
            types::ApiResult::Error("set_room_hidden not yet implemented — no mailbox command for hidden".to_string())
        }
        fn close_room(&mut self, room_id: String) -> types::ApiResult {
            let state = std::sync::Arc::clone(&self.state);
            match futures::executor::block_on(
                state.room_commands.close_room(&state, &room_id)
            ) {
                Ok(v) => types::ApiResult::Ok(json_to_wit_json(&v)),
                Err(e) => types::ApiResult::Error(e),
            }
        }
        fn set_room_phira_api_endpoint(&mut self, _room_id: String, _endpoint: Option<String>) -> types::ApiResult {
            types::ApiResult::Error("set_room_phira_api_endpoint not yet implemented — no mailbox command".to_string())
        }
    }

    // ── phira-user-mgmt ──
    impl wit::phira::plugin::phira_user_mgmt::Host for WitPluginHost {
        fn kick_user(&mut self, _user_id: u32, _reason: String) -> types::ApiResult {
            types::ApiResult::Error("kick_user not yet implemented".to_string())
        }
        fn ban_user(&mut self, _user_id: u32, _reason: String) -> types::ApiResult {
            types::ApiResult::Error("ban_user not yet implemented".to_string())
        }
        fn unban_user(&mut self, _user_id: u32) -> types::ApiResult {
            types::ApiResult::Error("unban_user not yet implemented".to_string())
        }
        fn get_ban_list(&mut self) -> types::ApiResult {
            types::ApiResult::Error("get_ban_list not yet implemented".to_string())
        }
        fn is_banned(&mut self, _user_id: u32) -> bool { false }
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
            types::ApiResult::Error("query_events not yet implemented".to_string())
        }
        fn query_room_snapshots(&mut self, _since: u64, _limit: u32) -> types::ApiResult {
            types::ApiResult::Error("query_room_snapshots not yet implemented".to_string())
        }
        fn query_touches(&mut self, _since: u64, _limit: u32, _round: Option<String>, _player: Option<u32>) -> types::ApiResult {
            types::ApiResult::Error("query_touches not yet implemented".to_string())
        }
        fn query_judges(&mut self, _since: u64, _limit: u32, _round: Option<String>, _player: Option<u32>) -> types::ApiResult {
            types::ApiResult::Error("query_judges not yet implemented".to_string())
        }
        fn get_playtime(&mut self, _user_id: u32) -> types::ApiResult {
            types::ApiResult::Error("get_playtime not yet implemented".to_string())
        }
        fn top_playtime(&mut self, _limit: u32) -> types::ApiResult {
            types::ApiResult::Error("top_playtime not yet implemented".to_string())
        }
    }

    // ── phira-admin ──
    impl wit::phira::plugin::phira_admin::Host for WitPluginHost {
        fn list_admin_ids(&mut self) -> types::ApiResult {
            let ids = futures::executor::block_on(self.state.admin_ids.read());
            let list: Vec<u32> = ids.iter().copied().map(|id| id as u32).collect();
            types::ApiResult::Ok(json_to_wit_json(&serde_json::json!(list)))
        }
        fn is_admin(&mut self, user_id: u32) -> bool {
            let ids = futures::executor::block_on(self.state.admin_ids.read());
            ids.contains(&(user_id as i32))
        }
        fn add_admin_id(&mut self, user_id: u32) -> types::ApiResult {
            let mut ids = futures::executor::block_on(self.state.admin_ids.write());
            ids.insert(user_id as i32);
            types::ApiResult::Ok(types::JsonValue::Null)
        }
        fn remove_admin_id(&mut self, user_id: u32) -> types::ApiResult {
            let mut ids = futures::executor::block_on(self.state.admin_ids.write());
            ids.remove(&(user_id as i32));
            types::ApiResult::Ok(types::JsonValue::Null)
        }
        fn set_admin_ids(&mut self, ids: Vec<u32>) -> types::ApiResult {
            let mut current = futures::executor::block_on(self.state.admin_ids.write());
            current.clear();
            for id in ids {
                current.insert(id as i32);
            }
            types::ApiResult::Ok(types::JsonValue::Null)
        }
    }

    // ── phira-config ──
    impl wit::phira::plugin::phira_config::Host for WitPluginHost {
        fn get_config(&mut self, _key: String) -> types::ApiResult {
            types::ApiResult::Error("get_config not yet implemented".to_string())
        }
        fn set_config(&mut self, _key: String, _value: String) -> types::ApiResult {
            types::ApiResult::Error("set_config not yet implemented".to_string())
        }
        fn list_config(&mut self, _prefix: String) -> types::ApiResult {
            types::ApiResult::Error("list_config not yet implemented".to_string())
        }
        fn reload_config(&mut self) -> types::ApiResult {
            types::ApiResult::Error("reload_config not yet implemented".to_string())
        }
        fn poll_config_changes(&mut self, _since: u64) -> types::ApiResult {
            types::ApiResult::Error("poll_config_changes not yet implemented".to_string())
        }
    }

    // ── phira-simulation ──
    impl wit::phira::plugin::phira_simulation::Host for WitPluginHost {
        fn status(&mut self) -> types::ApiResult {
            let status = futures::executor::block_on(self.state.simulation.status());
            let json = serde_json::to_value(&status).unwrap_or_default();
            types::ApiResult::Ok(json_to_wit_json(&json))
        }
        fn run(&mut self, preset: String, users: Option<u32>, rooms: Option<u32>, duration: Option<u32>) -> types::ApiResult {
            let mut config = crate::simulation::SimulationConfig::default();
            if let Some(p) = crate::simulation::SimulationPreset::parse(&preset) {
                config = p.defaults(self.state.simulation.seed_hint());
            }
            if let Some(u) = users { config.users = u as usize; }
            if let Some(r) = rooms { config.rooms = r as usize; }
            if let Some(d) = duration { config.duration_secs = d as u64; }
            match futures::executor::block_on(self.state.simulation.start(config)) {
                Ok(status) => {
                    let json = serde_json::to_value(&status).unwrap_or_default();
                    types::ApiResult::Ok(json_to_wit_json(&json))
                }
                Err(e) => types::ApiResult::Error(e),
            }
        }
        fn stop(&mut self) -> types::ApiResult {
            let status = futures::executor::block_on(self.state.simulation.stop("stopped via plugin API"));
            let json = serde_json::to_value(&status).unwrap_or_default();
            types::ApiResult::Ok(json_to_wit_json(&json))
        }
        fn cleanup(&mut self) -> types::ApiResult {
            let status = futures::executor::block_on(self.state.simulation.cleanup());
            let json = serde_json::to_value(&status).unwrap_or_default();
            types::ApiResult::Ok(json_to_wit_json(&json))
        }
    }

    // ── phira-runtime ──
    impl wit::phira::plugin::phira_runtime::Host for WitPluginHost {
        fn status(&mut self) -> types::ApiResult {
            let snapshot = self.state.runtime_plan.snapshot();
            let json = serde_json::to_value(&snapshot).unwrap_or_default();
            types::ApiResult::Ok(json_to_wit_json(&json))
        }
        fn events(&mut self, _limit: Option<u32>) -> types::ApiResult {
            let stats = self.state.event_bus.stats(50);
            let json = serde_json::to_value(&stats).unwrap_or_default();
            types::ApiResult::Ok(json_to_wit_json(&json))
        }
        fn commands(&mut self) -> types::ApiResult {
            let names: Vec<&str> = self.state.command_registry.iter().map(|s| s.name.as_str()).collect();
            types::ApiResult::Ok(json_to_wit_json(&serde_json::json!(names)))
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

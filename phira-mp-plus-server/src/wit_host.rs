//! WIT/component-model host trait implementations.
//!
//! The core `WitPluginHost` skeleton is available with `plugin-system`.
//! The generated trait impls (PhiraHost, etc.) require `wit-bindgen`.

use crate::server::PlusServerState;
use std::sync::Arc;

/// Wraps server state to implement WIT host traits.
pub struct WitPluginHost {
    #[allow(dead_code)]
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

// Generated trait implementations — only with wit-bindgen feature.
#[cfg(feature = "wit-bindgen")]
mod wit_trait_impls {
    use super::WitPluginHost;
    use crate::plugin_abi::wit_abi as wit;
    use wit::phira::plugin::phira_host as host_iface;
    use wit::phira::plugin::phira_types as types;

    impl host_iface::Host for WitPluginHost {
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
            let args_serde: Vec<serde_json::Value> = args.iter().map(from_wit_json).collect();
            match crate::server::server_state_query_for_host(&self.state, &method, &args_serde) {
                Ok(value) => types::ApiResult::Ok(to_wit_json(&value)),
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

#[allow(dead_code)]
    fn to_wit_json(value: &serde_json::Value) -> types::JsonValue {
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

#[allow(dead_code)]
    fn from_wit_json(value: &types::JsonValue) -> serde_json::Value {
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

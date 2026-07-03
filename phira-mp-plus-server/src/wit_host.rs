//! WIT/component-model host trait implementations.
//!
//! Implements the generated host traits from `crate::plugin_abi::wit_abi`
//! (phira-host, phira-query, phira-room-mgmt, etc.) so WASM plugins
//! loaded via the component model can use them instead of the JSON bridge.

use std::sync::{Arc, Mutex};

use crate::plugin_abi::wit_abi;
use crate::server::PlusServerState;

/// Wraps server state to implement the generated WIT host traits.
pub struct WitPluginHost {
    state: Arc<PlusServerState>,
    plugin_name: String,
    started: std::time::Instant,
}

impl WitPluginHost {
    pub fn new(state: Arc<PlusServerState>, plugin_name: String) -> Self {
        Self { state, plugin_name, started: std::time::Instant::now() }
    }

    fn log(&self, level: &str, message: &str) {
        match level {
            "error" => tracing::error!("[plugin:{}] {message}", self.plugin_name),
            "warn" => tracing::warn!("[plugin:{}] {message}", self.plugin_name),
            "info" => tracing::info!("[plugin:{}] {message}", self.plugin_name),
            "debug" => tracing::debug!("[plugin:{}] {message}", self.plugin_name),
            "trace" => tracing::trace!("[plugin:{}] {message}", self.plugin_name),
            _ => tracing::info!("[plugin:{}] {message}", self.plugin_name),
        }
    }
}

// ── Generated trait implementations ──

impl wit_abi::PhiraHost for WitPluginHost {
    fn log(&mut self, level: String, message: String) {
        self.log(&level, &message);
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
    fn api_call(&mut self, method: String, args: Vec<wit_abi::JsonValue>) -> Result<wit_abi::JsonValue, String> {
        let args_serde: Vec<serde_json::Value> = args.iter().map(from_wit_json).collect();
        let result = crate::server_query::server_state_query(&self.state, &method, &args_serde)
            .map_err(|e| e.to_string())?;
        Ok(to_wit_json(&result))
    }
    fn send_chat(&mut self, user_id: u32, message: String) {
        self.state.broadcast_system_message(&format!("[plugin:{}] {}", self.plugin_name, message));
    }
    fn http_request(
        &mut self,
        url: String,
        method: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Result<wit_abi::HttpResponse, String> {
        let response = crate::plugin_http::plugin_http_request(&self.state, &url, &method, &headers, &body)
            .map_err(|e| format!("HTTP request failed: {e}"))?;
        Ok(wit_abi::HttpResponse {
            status: response.status,
            headers: response.headers,
            body: response.body,
        })
    }
}

// ── JSON conversion helpers ──

fn to_wit_json(value: &serde_json::Value) -> wit_abi::JsonValue {
    use wit_abi::JsonValue;
    match value {
        serde_json::Value::Null => JsonValue::Null,
        serde_json::Value::Bool(b) => JsonValue::Flag(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() { JsonValue::Integer(i) }
            else if let Some(f) = n.as_f64() { JsonValue::Float(f) }
            else { JsonValue::Text(n.to_string()) }
        }
        serde_json::Value::String(s) => JsonValue::Text(s.clone()),
        serde_json::Value::Array(arr) => JsonValue::Array(serde_json::to_string(arr).unwrap_or_default()),
        serde_json::Value::Object(obj) => JsonValue::Object(serde_json::to_string(obj).unwrap_or_default()),
    }
}

fn from_wit_json(value: &wit_abi::JsonValue) -> serde_json::Value {
    use wit_abi::JsonValue;
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

/// Convert a wit_abi::JsonValue to serde_json::Value.
pub fn from_wit_json(value: &crate::plugin_abi::wit_abi::JsonValue) -> serde_json::Value {
    use crate::plugin_abi::wit_abi::JsonValue;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uuid_format() {
        let s = uuid::Uuid::new_v4().to_string();
        assert_eq!(s.len(), 36);
        assert_eq!(s.chars().filter(|&c| c == '-').count(), 4);
    }

    #[test]
    fn test_json_conversion_roundtrip() {
        let original = serde_json::json!({
            "name": "test",
            "count": 42,
            "active": true,
            "data": null,
            "score": 3.14,
        });
        let wit = to_wit_json(&original);
        let back = from_wit_json(&wit);
        assert_eq!(original, back);
    }
}

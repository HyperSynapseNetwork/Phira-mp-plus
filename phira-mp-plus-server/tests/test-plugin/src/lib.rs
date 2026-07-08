//! Minimal test WASM plugin for integration tests.
//!
//! Implements the phira-plugin-v2 world with known, deterministic behavior
//! that integration tests verify against.

// Use wit-bindgen directly instead of the SDK macro because the test
// plugin lives deeper in the tree (tests/test-plugin/) and the SDK's
// hardcoded "../wit/phira-plugin.wit" path doesn't resolve from here.
wit_bindgen::generate!({
    path: "../../../wit/phira-plugin.wit",
    world: "phira-plugin-v2",
});
export!(TestPlugin);

use crate::phira::plugin::phira_host;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicI32, Ordering};

static COUNTER: AtomicI32 = AtomicI32::new(0);

struct TestPlugin;

impl Guest for TestPlugin {
    fn init() -> Result<(), String> {
        COUNTER.store(0, Ordering::SeqCst);
        Ok(())
    }

    fn get_info() -> PluginInfo {
        PluginInfo {
            name: "test-plugin".to_string(),
            version: "0.1.0-test".to_string(),
            author: "phira-mp-plus".to_string(),
            description: "Integration test WASM plugin".to_string(),
        }
    }

    fn cleanup() {
        COUNTER.store(0, Ordering::SeqCst);
    }

    fn on_event(_event: PluginEvent) -> Result<bool, String> {
        Ok(false)
    }

    fn on_api(method: String, args: Vec<JsonValue>) -> ApiResult {
        let result = match method.as_str() {
            "ping" => json!(null),
            "echo" => {
                if args.is_empty() {
                    json!(null)
                } else {
                    wit_json_to_serde(&args[0])
                }
            }
            "count" => {
                let n = COUNTER.fetch_add(1, Ordering::SeqCst);
                json!(n)
            }
            "log" => {
                let level = args.get(0).and_then(|v| match v { JsonValue::Text(s) => Some(s.clone()), _ => None }).unwrap_or_default();
                let msg = args.get(1).and_then(|v| match v { JsonValue::Text(s) => Some(s.clone()), _ => None }).unwrap_or_default();
                phira_host::log(&level, &msg);
                json!(null)
            }
            "host.api_call" => {
                let method_name = args.get(0).and_then(|v| match v { JsonValue::Text(s) => Some(s.clone()), _ => None }).unwrap_or_default();
                let call_args: Vec<JsonValue> = args.iter().skip(1).cloned().collect();
                match phira_host::api_call(&method_name, &call_args) {
                    ApiResult::Ok(value) => wit_json_to_serde(&value),
                    ApiResult::Error(e) => json!({"error": e}),
                }
            }
            "host.http_request" => {
                let url = args.get(0).and_then(|v| match v { JsonValue::Text(s) => Some(s.clone()), _ => None }).unwrap_or_default();
                match phira_host::http_request(&url, "GET", &[], &[]) {
                    Ok(resp) => json!({"status": resp.status, "body_len": resp.body.len()}),
                    Err(e) => json!({"error": e}),
                }
            }
            _ => return ApiResult::Error(format!("unknown method: {method}")),
        };
        ApiResult::Ok(json_value_to_wit(&result))
    }
}

fn json_value_to_wit(value: &Value) -> JsonValue {
    match value {
        Value::Null => JsonValue::Null,
        Value::Bool(b) => JsonValue::Flag(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() { JsonValue::Integer(i) }
            else if let Some(f) = n.as_f64() { JsonValue::Float(f) }
            else { JsonValue::Text(n.to_string()) }
        }
        Value::String(s) => JsonValue::Text(s.clone()),
        Value::Array(arr) => JsonValue::Array(serde_json::to_string(arr).unwrap_or_default()),
        Value::Object(obj) => JsonValue::Object(serde_json::to_string(obj).unwrap_or_default()),
    }
}

fn wit_json_to_serde(value: &JsonValue) -> Value {
    match value {
        JsonValue::Null => Value::Null,
        JsonValue::Flag(b) => Value::Bool(*b),
        JsonValue::Integer(i) => json!(*i),
        JsonValue::Float(f) => json!(*f),
        JsonValue::Text(s) => Value::String(s.clone()),
        JsonValue::Array(s) | JsonValue::Object(s) => {
            serde_json::from_str(s).unwrap_or(Value::String(s.clone()))
        }
    }
}

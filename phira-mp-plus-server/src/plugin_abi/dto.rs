//! Typed DTOs for the plugin JSON bridge ABI.

use serde::{Deserialize, Serialize};

/// Typed DTO for a plugin API call.
///
/// This is the typed boundary for the JSON bridge.  New host code should prefer
/// constructing this over raw `Vec<serde_json::Value>` when calling plugins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginApiCall {
    pub method: String,
    pub args: Vec<serde_json::Value>,
}

impl PluginApiCall {
    pub fn new(method: impl Into<String>, args: Vec<serde_json::Value>) -> Self {
        Self { method: method.into(), args }
    }
}

/// Convert a typed API call to the JSON bridge wire format.
pub fn encode_typed_api_call(call: &PluginApiCall) -> Result<Vec<u8>, String> {
    let args = std::iter::once(serde_json::Value::String(call.method.clone()))
        .chain(call.args.iter().cloned())
        .collect::<Vec<_>>();
    serde_json::to_vec(&args).map_err(|e| format!("encode plugin API call: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_api_call_encodes_to_wire_format() {
        let call = PluginApiCall::new("admin.get_info", vec![serde_json::json!({"user_id": 42})]);
        let bytes = encode_typed_api_call(&call).unwrap();
        let decoded: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0], "admin.get_info");
        assert_eq!(decoded[1]["user_id"], 42);
    }
}

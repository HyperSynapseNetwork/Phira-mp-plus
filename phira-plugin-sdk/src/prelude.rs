//! Plugin SDK prelude — import with `use phira_plugin_sdk::prelude::*;`.

pub use serde_json::{json, Value};

/// Plugin metadata returned by `phira_get_info`.
pub struct PluginInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub author: &'static str,
    pub description: &'static str,
}

/// Log a message at the given level through the host logging system.
///
/// The host exports `phira_host_log` which accepts level and message
/// as (pointer, length) pairs in plugin linear memory.
pub fn host_log(level: &str, message: &str) {
    unsafe {
        extern "C" {
            fn phira_host_log(level_ptr: i32, level_len: i32, msg_ptr: i32, msg_len: i32);
        }
        let level_bytes = level.as_bytes();
        let msg_bytes = message.as_bytes();
        phira_host_log(
            level_bytes.as_ptr() as i32,
            level_bytes.len() as i32,
            msg_bytes.as_ptr() as i32,
            msg_bytes.len() as i32,
        );
    }
}

/// Call a host API method and return the JSON result.
///
/// The host exports `phira_host_api_call` which accepts method and args
/// as (pointer, length) pairs and writes the result to an output buffer.
pub fn host_api_call(method: &str, args_json: &str) -> Result<Value, String> {
    let _ = method;
    let _ = args_json;
    Err("host_api_call via SDK requires wasm32 target".to_string())
}

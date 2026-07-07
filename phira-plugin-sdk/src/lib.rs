//! Phira-mp+ Plugin SDK
//!
//! This crate provides the `wit_bindgen!` macro for developing WIT/component-model
//! WASM plugins for Phira-mp+. The server has removed the JSON-memory bridge and
//! requires WIT components.
//!
//! # Usage
//!
//! ```ignore
//! phira_plugin_sdk::wit_bindgen!("phira-plugin-v2");
//!
//! struct MyPlugin;
//! impl PhiraPluginV2 for MyPlugin {
//!     fn init(&mut self) -> Result<(), String> { Ok(()) }
//!     fn get_info(&mut self) -> PluginInfo { unimplemented!() }
//!     fn cleanup(&mut self) {}
//!     fn on_event(&mut self, _event: PluginEvent) -> Result<bool, String> { Ok(false) }
//!     fn on_api(&mut self, _method: String, _args: Vec<JsonValue>) -> ApiResult {
//!         ApiResult::Ok(JsonValue::Null)
//!     }
//! }
//! ```
//!
//! Requires `wit-bindgen` in Cargo.toml:
//! ```toml
//! [dependencies]
//! wit-bindgen = "0.30"
//! ```

/// Generate WASM plugin entry points (JSON ABI bridge).
///
/// Usage:
/// ```ignore
/// phira_plugin_sdk::plugin_entry!(init, get_info, cleanup, on_event, on_api);
/// ```
#[macro_export]
macro_rules! plugin_entry {
    ($init:ident, $get_info:ident, $cleanup:ident, $on_event:ident, $on_api:ident) => {
        #[no_mangle]
        pub extern "C" fn phira_init() -> i32 { $init() }
        #[no_mangle]
        pub extern "C" fn phira_get_info() { $get_info() }
        #[no_mangle]
        pub extern "C" fn phira_cleanup() { $cleanup() }
        #[no_mangle]
        pub extern "C" fn phira_on_event(ptr: i32, len: i32) -> i32 { $on_event(ptr, len) }
        #[no_mangle]
        pub extern "C" fn phira_on_api(method_ptr: i32, method_len: i32, args_ptr: i32, args_len: i32) -> i64 { $on_api(method_ptr, method_len, args_ptr, args_len) }
    };
}

/// Generate WIT bindings for a plugin.
///
/// This macro wraps `wit_bindgen::generate!` with the canonical WIT file path.
/// It generates typed guest stubs that call host functions through the
/// component model.
///
/// Usage:
/// ```ignore
/// phira_plugin_sdk::wit_bindgen!("phira-plugin-v2");
/// ```
#[macro_export]
macro_rules! wit_bindgen {
    ($world:expr) => {
        wit_bindgen::generate!({
            path: "../../wit/phira-plugin.wit",
            world: $world,
        });
    };
}

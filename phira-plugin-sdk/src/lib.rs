//! Phira-mp+ Plugin SDK
//!
//! This crate provides types and helpers for developing WASM plugins
//! for Phira-mp+.
//!
//! # WIT ABI (current)
//! All new plugins MUST use the typed WIT/component-model ABI. The server has
//! removed the JSON-memory bridge and requires WIT components.
//!
//! ```ignore
//! phira_plugin_sdk::wit_bindgen!("phira-plugin-v2");
//!
//! struct MyPlugin;
//! impl PhiraPluginV2 for MyPlugin {
//!     fn init(&mut self) -> Result<(), String> { Ok(()) }
//!     fn get_info(&mut self) -> phira_plugin_sdk::PluginInfo {
//!         phira_plugin_sdk::PluginInfo { name: "my-plugin", .. }
//!     }
//!     fn cleanup(&mut self) {}
//!     fn on_event(&mut self, _event: phira_plugin_sdk::PluginEvent) -> Result<bool, String> { Ok(false) }
//!     fn on_api(&mut self, _method: String, _args: Vec<phira_plugin_sdk::JsonValue>) -> phira_plugin_sdk::ApiResult {
//!         phira_plugin_sdk::ApiResult::Ok(phira_plugin_sdk::JsonValue::Null)
//!     }
//! }
//! ```
//!
//! # JSON ABI (legacy — server no longer accepts)
//! The old JSON-memory bridge has been removed from the server. This SDK retains
//! the `plugin_entry!` macro only as a read-only reference for migration. Do NOT
//! use it for new plugins.

pub mod prelude;

/// Generate the plugin entry points required by the Phira-mp+ host.
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
        pub extern "C" fn phira_on_api(ptr: i32, len: i32) -> i32 { $on_api(ptr, len) }
    };
}

/// Generate WIT bindings for a plugin.
///
/// This macro wraps `wit_bindgen::generate!` with the canonical WIT file path.
/// It generates typed guest stubs that call host functions through the
/// component model, instead of the raw JSON memory bridge.
///
/// Usage in a plugin crate:
/// ```ignore
/// phira_plugin_sdk::wit_bindgen!("phira-plugin-v2");
///
/// // Then implement the generated trait:
/// struct MyPlugin;
/// impl PhiraHost for MyPlugin { ... }
/// ```
///
/// Requires `wit-bindgen` in Cargo.toml:
/// ```toml
/// [dependencies]
/// wit-bindgen = "0.30"
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

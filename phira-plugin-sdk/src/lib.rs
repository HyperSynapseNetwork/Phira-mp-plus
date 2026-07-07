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
//! wit-bindgen = "0.58"
//! ```

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

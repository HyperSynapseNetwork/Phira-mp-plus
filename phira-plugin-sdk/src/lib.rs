//! Phira-mp+ Plugin SDK
//!
//! This crate provides types and helpers for developing WASM plugins
//! for Phira-mp+.
//!
//! # JSON ABI (current)
//! Plugins communicate with the host via JSON strings through guest memory.
//!
//! ```ignore
//! use phira_plugin_sdk::prelude::*;
//! phira_plugin_sdk::plugin_entry!(init, get_info, cleanup, on_event, on_api);
//!
//! fn init() -> i32 { 0 }
//! fn get_info() {
//!     let info = PluginInfo { name: "my-plugin", version: "0.1", author: "me", description: "" };
//!     // write info to memory offset 0
//! }
//! # fn cleanup() {}
//! # fn on_event(_ptr: i32, _len: i32) -> i32 { 0 }
//! # fn on_api(_ptr: i32, _len: i32) -> i32 { 0 }
//! ```
//!
//! # WIT ABI (phase 2)
//! Once the server is in dual-ABI mode, plugins can use typed WIT-generated bindings:
//!
//! ```ignore
//! phira_plugin_sdk::wit_bindgen!(world = "phira-plugin-v2");
//! ```

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

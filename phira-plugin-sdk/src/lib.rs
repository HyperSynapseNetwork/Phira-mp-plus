//! Phira-mp+ Plugin SDK
//!
//! This crate provides types and helpers for developing WASM plugins
//! for Phira-mp+.
//!
//! # JSON ABI (current)
//! Plugins communicate with the host via JSON strings through guest memory.
//! The host provides import functions (`phira_host_api_call`, etc.)
//! and expects the plugin to export lifecycle functions (`phira_init`, etc.).
//!
//! # WIT ABI (planned)
//! Once the WIT/component-model migration reaches phase 2, this SDK will
//! generate typed guest bindings from `wit/phira-plugin.wit`.

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

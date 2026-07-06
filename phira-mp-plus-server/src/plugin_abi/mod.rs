//! Plugin ABI boundary.
//!
//! Runtime v2 now treats WIT/component-model ABI v2 as the current target.
//! The JSON helpers remain only as internal compatibility and contract-test
//! utilities while WIT lifecycle dispatch is completed.
//!
//! # ABI versions
//! - `abi-json-v1`: Legacy JSON-memory bridge
//! - `abi-wit-v2`: Current typed WIT/component ABI target

mod dto;
mod json;
mod plan;

/// Generated WIT/component-model bindings for the phira-plugin-v2 world.
/// Only available when the `wit-bindgen` feature is enabled (not in defaults).
#[cfg(feature = "wit-bindgen")]
pub(crate) mod wit_abi {
    wasmtime::component::bindgen!("phira-plugin-v2" in "../wit/phira-plugin.wit");
}

pub use dto::{encode_typed_api_call, PluginApiCall};
pub use json::{
    decode_plugin_api_result_json, encode_plugin_api_args_json, encode_plugin_event_json,
};
pub use plan::wit;
pub use plan::{
    is_abi_version_supported, plugin_abi_plan, supported_abi_versions, PluginAbiPlan,
    PluginAbiTransport,
};

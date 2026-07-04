//! Plugin ABI boundary.
//!
//! The current WASM guest ABI still moves event/API payloads through JSON bytes
//! in guest memory. Runtime v2 centralises that JSON bridge in one small module
//! so the rest of the host no longer treats ad-hoc JSON strings as the plugin ABI.
//! The target state is a typed WIT/component-model ABI; until then this module
//! is the only place where the JSON transport should be encoded/decoded.
//!
//! # ABI versions
//! - `abi-json-v1`: Current JSON bridge (temporary)
//! - `abi-wit-v2`: Target typed WIT/component ABI

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

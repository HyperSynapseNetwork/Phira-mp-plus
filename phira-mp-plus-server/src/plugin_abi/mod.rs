//! Plugin ABI boundary.
//!
//! WIT/component-model ABI v2 is the only ABI. JSON-memory bridge (abi-json-v1)
//! has been removed. MIGRATION_PHASE=3.

mod plan;

/// Generated WIT/component-model bindings for the phira-plugin-v2 world.
/// Only available when the `wit-bindgen` feature is enabled (not in defaults).
#[cfg(feature = "wit-bindgen")]
pub(crate) mod wit_abi {
    wasmtime::component::bindgen!("phira-plugin-v2" in "../wit/phira-plugin.wit");
}

pub use plan::wit;
pub use plan::{
    is_abi_version_supported, plugin_abi_plan, supported_abi_versions, PluginAbiPlan,
    PluginAbiTransport,
};

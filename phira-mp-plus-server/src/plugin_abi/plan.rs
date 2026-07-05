//! Plugin ABI plan, transport enum, version metadata and WIT constants.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PluginAbiTransport {
    JsonMemoryV1,
    WitTypedV2,
}

impl PluginAbiTransport {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::JsonMemoryV1 => "json_memory_v1",
            Self::WitTypedV2 => "wit_typed_v2",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginAbiPlan {
    pub current_transport: PluginAbiTransport,
    pub target_transport: PluginAbiTransport,
    pub current_version: &'static str,
    pub target_version: &'static str,
    pub risks: Vec<&'static str>,
    pub next_steps: Vec<&'static str>,
}

pub fn plugin_abi_plan() -> PluginAbiPlan {
    PluginAbiPlan {
        current_transport: PluginAbiTransport::WitTypedV2,
        target_transport: PluginAbiTransport::WitTypedV2,
        current_version: "abi-wit-v2",
        target_version: "abi-wit-v2",
        risks: vec![
            "WIT call_on_event / call_api still stubs — plugin lifecycle incomplete",
            "Component model adapters increase binary size ~14MB",
            "All .wasm plugins must be compiled as WIT components, not modules",
        ],
        next_steps: vec![
            "centralize every JSON ABI encode/decode call in plugin_abi.rs",
            "add contract tests for every host event and host API method exposed to plugins",
            "write abi-wit-v2 WIT definitions before changing guest-facing behavior",
            "switch WASM host exports from JSON-memory bridge to typed WIT/component bindings after tests cover v1 parity",
        ],
    }
}

/// WIT ABI v2 metadata.
pub mod wit {
    pub const WIT_FILE: &str = "wit/phira-plugin.wit";
    pub const WIT_WORLD: &str = "phira-plugin-v2";
    pub const WIT_VERSION: &str = "abi-wit-v2";
    /// 0 = JSON bridge active.
    /// 1 = Host WIT bindings generated.
    /// 2 = JSON bridge removed, WIT ABI only (current).
    pub const MIGRATION_PHASE: u8 = 2;
}

pub fn supported_abi_versions() -> Vec<&'static str> {
    vec!["abi-wit-v2"]
}

pub fn is_abi_version_supported(version: &str) -> bool {
    matches!(version, "abi-wit-v2")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_plan_tracks_wit_as_current() {
        let plan = plugin_abi_plan();
        assert_eq!(plan.current_transport, PluginAbiTransport::WitTypedV2);
        assert_eq!(plan.target_transport, PluginAbiTransport::WitTypedV2);
        assert!(plan.risks.iter().any(|r| r.contains("call_on_event")));
    }

    #[test]
    fn abi_version_supported_checks_work() {
        assert!(!is_abi_version_supported("abi-json-v1"));
        assert!(is_abi_version_supported("abi-wit-v2"));
        assert!(!is_abi_version_supported(""));
    }

    #[test]
    fn supported_abi_versions_includes_wit() {
        let versions = supported_abi_versions();
        assert!(!versions.contains(&"abi-json-v1"));
        assert!(versions.contains(&"abi-wit-v2"));
        assert_eq!(versions.len(), 1);
    }
}

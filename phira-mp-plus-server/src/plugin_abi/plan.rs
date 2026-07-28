use serde::Serialize;

/// Current plugin ABI transport — WIT component model v3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PluginAbiTransport {
    WitTypedV3,
}

impl PluginAbiTransport {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WitTypedV3 => "wit_typed_v3",
        }
    }
}

/// WIT ABI v3 metadata.
pub mod wit {
    pub const WIT_FILE: &str = "wit/phira-plugin.wit";
    pub const WIT_WORLD: &str = "phira-plugin-v3";
    pub const WIT_VERSION: &str = "abi-wit-v3";
    /// Stable ABI state: JSON bridge removed, WIT-only component ABI (no longer a migration).
    pub const MIGRATION_PHASE: usize = 3;
}

pub fn plugin_abi_plan() -> PluginAbiPlan {
    PluginAbiPlan {
        current_transport: PluginAbiTransport::WitTypedV3,
        target_transport: PluginAbiTransport::WitTypedV3,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginAbiPlan {
    pub current_transport: PluginAbiTransport,
    pub target_transport: PluginAbiTransport,
}

pub fn supported_abi_versions() -> Vec<&'static str> {
    vec!["abi-wit-v3"]
}

pub fn is_abi_version_supported(version: &str) -> bool {
    matches!(version, "abi-wit-v3")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_plan_tracks_wit_as_current() {
        let plan = plugin_abi_plan();
        assert_eq!(plan.current_transport, PluginAbiTransport::WitTypedV3);
        assert_eq!(plan.target_transport, PluginAbiTransport::WitTypedV3);
    }

    #[test]
    fn abi_version_supported_checks_work() {
        assert!(!is_abi_version_supported("abi-json-v1"));
        assert!(is_abi_version_supported("abi-wit-v3"));
        assert!(!is_abi_version_supported(""));
    }

    #[test]
    fn supported_abi_versions_includes_wit() {
        let versions = supported_abi_versions();
        assert!(!versions.contains(&"abi-json-v1"));
        assert!(versions.contains(&"abi-wit-v3"));
        assert_eq!(versions.len(), 1);
    }
}

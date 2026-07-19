//! Role-Based Access Control for PMP administration.
//!
//! Migrating from flat `admin_phira_ids` to fine-grained roles:
//!
//!   viewer       — read-only diagnostics and status
//!   moderator    — kick, lock, cycle rooms; manage chat
//!   operator     — moderator + plugin reload/disable, room close
//!   plugin-admin — operator + plugin install/upgrade/remove/purge
//!   system-admin — full access including RBAC management
//!
//! Phase 1: introduce roles alongside existing admin_phira_ids.
//! The config maps user IDs to roles; unknown users default to no role.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Administrative role with increasing authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerRole {
    /// Read-only access: status, diagnostics, room info.
    Viewer,
    /// Can kick/lock/cycle rooms, manage chat.
    Moderator,
    /// Moderator + plugin reload/disable, room close.
    Operator,
    /// Operator + plugin install/upgrade/remove.
    PluginAdmin,
    /// Full access including RBAC management.
    SystemAdmin,
}

impl ServerRole {
    /// Returns true if this role has at least the permissions of `required`.
    pub fn grants(&self, required: ServerRole) -> bool {
        *self >= required
    }
}

/// Role assignments stored in the server config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RbacConfig {
    /// Map of user_id → role for explicitly assigned roles.
    #[serde(default)]
    pub roles: HashMap<i32, ServerRole>,
    /// Fallback role for authenticated users not in the roles map.
    /// The default (none) means no admin access at all.
    #[serde(default)]
    pub default_role: Option<ServerRole>,
}

impl RbacConfig {
    /// Look up the role for a given user ID.
    pub fn role_for(&self, user_id: i32) -> Option<ServerRole> {
        self.roles
            .get(&user_id)
            .copied()
            .or(self.default_role)
    }

    /// Check whether a user has at least the given role.
    pub fn user_has_role(&self, user_id: i32, required: ServerRole) -> bool {
        self.role_for(user_id)
            .map(|role| role.grants(required))
            .unwrap_or(false)
    }
}

/// Simple action types for permission checks.
/// Maps high-level operations to minimum role requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminAction {
    ViewDiagnostics,
    KickUser,
    LockRoom,
    CycleRoom,
    CloseRoom,
    ReloadPlugin,
    DisablePlugin,
    InstallPlugin,
    RemovePlugin,
    PurgePluginData,
    ManageRbac,
}

impl AdminAction {
    /// The minimum role required to perform this action.
    pub fn minimum_role(self) -> ServerRole {
        match self {
            Self::ViewDiagnostics => ServerRole::Viewer,
            Self::KickUser | Self::LockRoom | Self::CycleRoom => ServerRole::Moderator,
            Self::CloseRoom | Self::ReloadPlugin | Self::DisablePlugin => ServerRole::Operator,
            Self::InstallPlugin | Self::RemovePlugin => ServerRole::PluginAdmin,
            Self::PurgePluginData | Self::ManageRbac => ServerRole::SystemAdmin,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_admin_grants_all() {
        let action = AdminAction::ManageRbac;
        assert!(ServerRole::SystemAdmin.grants(action.minimum_role()));
    }

    #[test]
    fn viewer_cannot_kick() {
        assert!(!ServerRole::Viewer.grants(AdminAction::KickUser.minimum_role()));
    }

    #[test]
    fn moderator_can_kick() {
        assert!(ServerRole::Moderator.grants(AdminAction::KickUser.minimum_role()));
    }

    #[test]
    fn operator_cannot_install_plugins() {
        assert!(!ServerRole::Operator.grants(AdminAction::InstallPlugin.minimum_role()));
    }

    #[test]
    fn role_lookup_from_config() {
        let mut rbac = RbacConfig::default();
        rbac.roles.insert(1, ServerRole::SystemAdmin);
        rbac.roles.insert(2, ServerRole::Viewer);

        assert!(rbac.user_has_role(1, ServerRole::SystemAdmin));
        assert!(!rbac.user_has_role(2, ServerRole::Moderator));
        assert!(!rbac.user_has_role(99, ServerRole::Viewer));
    }

    #[test]
    fn default_role_fallback() {
        let mut rbac = RbacConfig::default();
        rbac.default_role = Some(ServerRole::Viewer);
        assert!(rbac.user_has_role(999, ServerRole::Viewer));
        assert!(!rbac.user_has_role(999, ServerRole::Moderator));
    }
}

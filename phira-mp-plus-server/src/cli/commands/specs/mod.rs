//! Command specification modules for the Runtime CommandRegistry.
//!
//! Each sub-module returns its command specs so `runtime_registry()` can
//! collect them into a single `CommandRegistry`.  This keeps the registry
//! definitional — it only maps names to metadata — while the actual command
//! handlers live alongside their group's module.

pub mod benchmark;
pub mod core;
pub mod ops;
pub mod plugin;
pub mod room;
pub mod runtime;
pub mod security;
pub mod user;

/// Concatenate all command specs from every group.
pub fn all_specs() -> Vec<crate::command_registry::CommandSpec> {
    let mut out = Vec::new();
    out.extend(core::specs());
    out.extend(runtime::specs());
    out.extend(benchmark::specs());
    out.extend(user::specs());
    out.extend(room::specs());
    out.extend(plugin::specs());
    out.extend(security::specs());
    out.extend(ops::specs());
    out
}

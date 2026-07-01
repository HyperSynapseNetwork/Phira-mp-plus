//! Command Registry diagnostics.

use super::super::super::*;

impl CliHandler {
    pub(in crate::cli) fn print_runtime_commands(&self) {
        self.out(format!("  {} Command Registry", c::green("◆")));
        self.out(format!("  {} groups: {}", c::dim("│"), self.state.command_registry.groups().join(", ")));
        self.out(format!("  {} specs:  {}", c::dim("│"), self.state.command_registry.iter().count()));
        self.out(format!("  {} roots:  {}", c::dim("│"), self.state.command_registry.root_commands().join(", ")));
    }
}

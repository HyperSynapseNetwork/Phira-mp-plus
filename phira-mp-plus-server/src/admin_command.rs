//! Unified `AdminCommand` trait and dispatcher.
//!
//! Every administrative command implements this trait, providing metadata,
//! parameter definitions, help text and an async `execute` method.  CLI,
//! TUI and Web API are merely different Dispatcher implementations that
//! route user input to the same command implementations.
//!
//! Help text and parameter validation live here, not in the dispatcher.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Serialize;

/// Categories for grouping commands in help output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandCategory {
    Admin,
    Room,
    Plugin,
    Benchmark,
    Simulation,
    Broadcast,
    Runtime,
    System,
}

impl CommandCategory {
    pub fn label(&self, lang: &str) -> &'static str {
        match self {
            Self::Admin => "管理",
            Self::Room => "房间",
            Self::Plugin => "插件",
            Self::Benchmark => "基准测试",
            Self::Simulation => "模拟",
            Self::Broadcast => "广播",
            Self::Runtime => "运行时",
            Self::System => "系统",
        }
    }
}

/// Parameter type for command arguments.
#[derive(Debug, Clone)]
pub enum ParamType {
    String,
    Integer,
    Boolean,
    Choice(&'static [&'static str]),
}

/// A single parameter definition.
#[derive(Debug, Clone)]
pub struct ParamDef {
    pub name: &'static str,
    pub description: &'static str,
    pub param_type: ParamType,
    pub required: bool,
    pub default_value: Option<&'static str>,
}

/// Result of executing a command.
#[derive(Debug, Clone, Serialize)]
pub struct CommandResult {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Unified admin command trait.
///
/// All commands (admin, room, benchmark, etc.) implement this.
/// Dispatchers (CLI, TUI, Web) call `execute` after parsing args.
pub trait AdminCommand: Send + Sync {
    /// Primary command name (e.g. `"ban"`, `"room"`, `"benchmark"`).
    fn name(&self) -> &'static str;

    /// Alternative spellings (e.g. `["kick"]` for `"ban"`).
    fn aliases(&self) -> &[&'static str] {
        &[]
    }

    /// One-line help string.
    fn help(&self) -> &'static str;

    /// Detailed help (shown for `help <command>`).
    fn detailed_help(&self) -> &'static str {
        self.help()
    }

    /// Category for grouping in `help` output.
    fn category(&self) -> CommandCategory;

    /// Parameter definitions for argument parsing and `help` output.
    fn params(&self) -> &[ParamDef] {
        &[]
    }

    /// Execute the command with parsed arguments.
    fn execute(
        &self,
        args: &[String],
        state: &crate::server::PlusServerState,
        out: &dyn Fn(&str),
    ) -> Pin<Box<dyn Future<Output = CommandResult> + Send + '_>>;
}

// ── Registry ──

/// A registry of all admin commands, indexed by name and alias.
#[derive(Default)]
pub struct CommandRegistry {
    commands: Vec<Box<dyn AdminCommand>>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, cmd: Box<dyn AdminCommand>) {
        self.commands.push(cmd);
    }

    pub fn find(&self, name: &str) -> Option<&dyn AdminCommand> {
        self.commands
            .iter()
            .find(|c| c.name() == name || c.aliases().contains(&name))
            .map(|c| c.as_ref())
    }

    pub fn all(&self) -> &[Box<dyn AdminCommand>] {
        &self.commands
    }

    pub fn by_category(&self) -> Vec<(CommandCategory, Vec<&dyn AdminCommand>)> {
        let mut map: Vec<(CommandCategory, Vec<&dyn AdminCommand>)> = Vec::new();
        for cmd in &self.commands {
            let cat = cmd.category();
            if let Some(entry) = map.iter_mut().find(|(c, _)| *c == cat) {
                entry.1.push(cmd.as_ref());
            } else {
                map.push((cat, vec![cmd.as_ref()]));
            }
        }
        map
    }
}

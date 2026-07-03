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
    pub fn label(&self) -> &'static str {
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
pub trait AdminCommand: Send + Sync {
    fn name(&self) -> &'static str;
    fn aliases(&self) -> &[&'static str] { &[] }
    fn help(&self) -> &'static str;
    fn detailed_help(&self) -> &'static str { self.help() }
    fn category(&self) -> CommandCategory;
    fn params(&self) -> &[ParamDef] { &[] }
    fn execute(
        &self,
        args: &[String],
        state: &crate::server::PlusServerState,
    ) -> Pin<Box<dyn Future<Output = CommandResult> + Send + '_>>;
}

// ── Registry ──

#[derive(Default)]
pub struct CommandRegistry {
    commands: Vec<Box<dyn AdminCommand>>,
}

impl CommandRegistry {
    pub fn new() -> Self { Self::default() }
    pub fn register(&mut self, cmd: Box<dyn AdminCommand>) { self.commands.push(cmd); }
    pub fn find(&self, name: &str) -> Option<&dyn AdminCommand> {
        self.commands.iter().find(|c| c.name() == name || c.aliases().contains(&name)).map(|c| c.as_ref())
    }
    pub fn all(&self) -> &[Box<dyn AdminCommand>] { &self.commands }
}

// ── Built-in commands ──

pub struct HelpCommand;

impl AdminCommand for HelpCommand {
    fn name(&self) -> &'static str { "help" }
    fn aliases(&self) -> &[&'static str] { &["?"] }
    fn help(&self) -> &'static str { "显示命令帮助" }
    fn category(&self) -> CommandCategory { CommandCategory::System }
    fn params(&self) -> &[ParamDef] {
        &[ParamDef {
            name: "command",
            description: "要查看帮助的命令名",
            param_type: ParamType::String,
            required: false,
            default_value: None,
        }]
    }

    fn execute(
        &self,
        args: &[String],
        state: &crate::server::PlusServerState,
    ) -> Pin<Box<dyn Future<Output = CommandResult> + Send + '_>> {
        let target = args.first().map(|s| s.as_str()).unwrap_or("");
        Box::pin(async move {
            let registry = &state.admin_commands;
            if target.is_empty() {
                // Show all commands by category
                let mut msg = String::new();
                let mut cats: Vec<(CommandCategory, Vec<&dyn AdminCommand>)> = Vec::new();
                for cmd in registry.all() {
                    let cat = cmd.category();
                    if let Some(entry) = cats.iter_mut().find(|(c, _)| *c == cat) {
                        entry.1.push(cmd.as_ref());
                    } else {
                        cats.push((cat, vec![cmd.as_ref()]));
                    }
                }
                for (cat, cmds) in &cats {
                    msg.push_str(&format!("\n  {}:", cat.label()));
                    for cmd in cmds {
                        let alias_str = if cmd.aliases().is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", cmd.aliases().join(", "))
                        };
                        msg.push_str(&format!("\n    {:<15}{}{}", cmd.name(), alias_str, cmd.help()));
                    }
                }
                CommandResult { success: true, message: msg, data: None }
            } else {
                match registry.find(target) {
                    Some(cmd) => {
                        let mut msg = format!("{} — {}\n", cmd.name(), cmd.help());
                        if !cmd.params().is_empty() {
                            msg.push_str("\n参数:\n");
                            for p in cmd.params() {
                                let req = if p.required { " (必填)" } else { "" };
                                msg.push_str(&format!("  {}{}\n", p.name, req));
                            }
                        }
                        CommandResult { success: true, message: msg, data: None }
                    }
                    None => CommandResult {
                        success: false,
                        message: format!("未知命令: {target}"),
                        data: None,
                    },
                }
            }
        })
    }
}

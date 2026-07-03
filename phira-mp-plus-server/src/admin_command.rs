//! Unified `AdminCommand` trait and dispatcher.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandCategory {
    Admin, Room, Plugin, Benchmark, Simulation, Broadcast, Runtime, System,
}

impl CommandCategory {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Admin => "管理", Self::Room => "房间", Self::Plugin => "插件",
            Self::Benchmark => "基准测试", Self::Simulation => "模拟",
            Self::Broadcast => "广播", Self::Runtime => "运行时", Self::System => "系统",
        }
    }
}

#[derive(Debug, Clone)]
pub enum ParamType { String, Integer, Boolean, Choice(&'static [&'static str]) }

#[derive(Debug, Clone)]
pub struct ParamDef {
    pub name: &'static str,
    pub description: &'static str,
    pub param_type: ParamType,
    pub required: bool,
    pub default_value: Option<&'static str>,
}

#[derive(Debug, Clone, serde::Serialize)]
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
    fn category(&self) -> CommandCategory;
    fn params(&self) -> &[ParamDef] { &[] }
    fn execute(
        &self,
        args: Vec<String>,
        state: Arc<crate::server::PlusServerState>,
    ) -> Pin<Box<dyn Future<Output = CommandResult> + Send>>;
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
    /// Snapshot of command metadata (name, aliases, help) — owned, no lifetime issues.
    pub fn snapshot(&self) -> Vec<CmdInfo> {
        self.commands.iter().map(|c| CmdInfo {
            name: c.name().to_string(),
            aliases: c.aliases().iter().map(|a| a.to_string()).collect(),
            help: c.help().to_string(),
            category: c.category(),
        }).collect()
    }
}

#[derive(Debug, Clone)]
pub struct CmdInfo {
    pub name: String,
    pub aliases: Vec<String>,
    pub help: String,
    pub category: CommandCategory,
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
            name: "command", description: "要查看帮助的命令名",
            param_type: ParamType::String, required: false, default_value: None,
        }]
    }

    fn execute(
        &self,
        args: Vec<String>,
        state: Arc<crate::server::PlusServerState>,
    ) -> Pin<Box<dyn Future<Output = CommandResult> + Send>> {
        let target = args.first().cloned().unwrap_or_default();
        let cmds = state.admin_commands.snapshot();
        drop(state); // release the Arc before entering async
        Box::pin(async move {
            if target.is_empty() {
                let mut msg = String::new();
                for cmd in &cmds {
                    let alias_str = if cmd.aliases.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", cmd.aliases.join(", "))
                    };
                    msg.push_str(&format!("\n  {:<15}{}{}", cmd.name, alias_str, cmd.help));
                }
                CommandResult { success: true, message: msg, data: None }
            } else if let Some(cmd) = cmds.iter().find(|c| c.name == target || c.aliases.contains(&target)) {
                let msg = format!("{} — {}\n", cmd.name, cmd.help);
                CommandResult { success: true, message: msg, data: None }
            } else {
                CommandResult { success: false, message: format!("未知命令: {target}"), data: None }
            }
        })
    }
}

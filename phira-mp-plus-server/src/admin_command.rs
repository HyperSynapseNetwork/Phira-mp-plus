//! Unified `AdminCommand` trait and dispatcher.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

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

use std::net::IpAddr;

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

// ── Exit ──

pub struct ExitCommand;

impl AdminCommand for ExitCommand {
    fn name(&self) -> &'static str { "exit" }
    fn help(&self) -> &'static str { "关闭服务器" }
    fn category(&self) -> CommandCategory { CommandCategory::System }
    fn execute(
        &self,
        _args: Vec<String>,
        state: Arc<crate::server::PlusServerState>,
    ) -> Pin<Box<dyn Future<Output = CommandResult> + Send>> {
        Box::pin(async move {
            state.shutdown.notify_one();
            CommandResult { success: true, message: "正在关闭服务器...".to_string(), data: None }
        })
    }
}

// ── Status ──

pub struct StatusCommand;

impl AdminCommand for StatusCommand {
    fn name(&self) -> &'static str { "status" }
    fn help(&self) -> &'static str { "显示服务器状态" }
    fn category(&self) -> CommandCategory { CommandCategory::System }
    fn execute(
        &self,
        _args: Vec<String>,
        state: Arc<crate::server::PlusServerState>,
    ) -> Pin<Box<dyn Future<Output = CommandResult> + Send>> {
        Box::pin(async move {
            let users_online = state.users.read().await.len();
            let rooms_active = state.rooms.read().await.len();
            let msg = format!("用户在线: {users_online} | 活跃房间: {rooms_active}");
            CommandResult { success: true, message: msg, data: None }
        })
    }
}

// ── Ban ──

pub struct BanCommand;

impl AdminCommand for BanCommand {
    fn name(&self) -> &'static str { "ban" }
    fn help(&self) -> &'static str { "封禁用户: ban <用户ID> [原因]" }
    fn category(&self) -> CommandCategory { CommandCategory::Admin }
    fn execute(
        &self,
        args: Vec<String>,
        state: Arc<crate::server::PlusServerState>,
    ) -> Pin<Box<dyn Future<Output = CommandResult> + Send>> {
        Box::pin(async move {
            if args.is_empty() {
                return CommandResult { success: false, message: "用法: ban <用户ID> [原因]".into(), data: None };
            }
            let user_id: i32 = match args[0].parse() {
                Ok(id) => id,
                Err(_) => return CommandResult { success: false, message: format!("无效用户ID: {}", args[0]), data: None },
            };
            let reason = if args.len() > 1 { args[1..].join(" ") } else { String::new() };
            match state.ban_manager.ban_user(user_id, &reason).await {
                Ok(r) => CommandResult { success: true, message: format!("用户 {user_id} 已封禁 (原因: {r})"), data: None },
                Err(e) => CommandResult { success: false, message: format!("封禁失败: {e}"), data: None },
            }
        })
    }
}

pub struct UnbanCommand;

impl AdminCommand for UnbanCommand {
    fn name(&self) -> &'static str { "unban" }
    fn help(&self) -> &'static str { "解封用户: unban <用户ID>" }
    fn category(&self) -> CommandCategory { CommandCategory::Admin }
    fn execute(
        &self,
        args: Vec<String>,
        state: Arc<crate::server::PlusServerState>,
    ) -> Pin<Box<dyn Future<Output = CommandResult> + Send>> {
        Box::pin(async move {
            if args.is_empty() {
                return CommandResult { success: false, message: "用法: unban <用户ID>".into(), data: None };
            }
            let user_id: i32 = match args[0].parse() {
                Ok(id) => id,
                Err(_) => return CommandResult { success: false, message: format!("无效用户ID: {}", args[0]), data: None },
            };
            match state.ban_manager.unban_user(user_id).await {
                Ok(()) => CommandResult { success: true, message: format!("用户 {user_id} 已解封"), data: None },
                Err(e) => CommandResult { success: false, message: format!("解封失败: {e}"), data: None },
            }
        })
    }
}

pub struct BanlistCommand;

impl AdminCommand for BanlistCommand {
    fn name(&self) -> &'static str { "banlist" }
    fn help(&self) -> &'static str { "列出封禁列表" }
    fn category(&self) -> CommandCategory { CommandCategory::Admin }
    fn execute(
        &self,
        _args: Vec<String>,
        state: Arc<crate::server::PlusServerState>,
    ) -> Pin<Box<dyn Future<Output = CommandResult> + Send>> {
        Box::pin(async move {
            let list = state.ban_manager.list_banned().await;
            if list.is_empty() {
                return CommandResult { success: true, message: "封禁列表为空".into(), data: None };
            }
            let mut msg = String::from("封禁列表:\n");
            for entry in &list {
                msg.push_str(&format!("  用户 {} — {}\n", entry.user_id, entry.reason));
            }
            CommandResult { success: true, message: msg.trim().to_string(), data: None }
        })
    }
}

pub struct BanIpCommand;

impl AdminCommand for BanIpCommand {
    fn name(&self) -> &'static str { "ban ip" }
    fn aliases(&self) -> &[&'static str] { &["banip"] }
    fn help(&self) -> &'static str { "IP 封禁: ban ip <地址> [原因]" }
    fn category(&self) -> CommandCategory { CommandCategory::Admin }
    fn execute(
        &self,
        args: Vec<String>,
        state: Arc<crate::server::PlusServerState>,
    ) -> Pin<Box<dyn Future<Output = CommandResult> + Send>> {
        Box::pin(async move {
            if args.is_empty() {
                return CommandResult { success: false, message: "用法: ban ip <地址> [原因]".into(), data: None };
            }
            let ip: IpAddr = match args[0].parse() {
                Ok(ip) => ip,
                Err(_) => return CommandResult { success: false, message: format!("无效IP地址: {}", args[0]), data: None },
            };
            let reason = if args.len() > 1 { args[1..].join(" ") } else { String::new() };
            match state.ban_manager.ban_ip(ip, &reason).await {
                Ok(()) => CommandResult { success: true, message: format!("IP {ip} 已封禁"), data: None },
                Err(e) => CommandResult { success: false, message: format!("IP封禁失败: {e}"), data: None },
            }
        })
    }
}

pub struct UnbanIpCommand;

impl AdminCommand for UnbanIpCommand {
    fn name(&self) -> &'static str { "unban ip" }
    fn aliases(&self) -> &[&'static str] { &["unbanip"] }
    fn help(&self) -> &'static str { "IP 解封: unban ip <地址>" }
    fn category(&self) -> CommandCategory { CommandCategory::Admin }
    fn execute(
        &self,
        args: Vec<String>,
        state: Arc<crate::server::PlusServerState>,
    ) -> Pin<Box<dyn Future<Output = CommandResult> + Send>> {
        Box::pin(async move {
            if args.is_empty() {
                return CommandResult { success: false, message: "用法: unban ip <地址>".into(), data: None };
            }
            let ip: IpAddr = match args[0].parse() {
                Ok(ip) => ip,
                Err(_) => return CommandResult { success: false, message: format!("无效IP地址: {}", args[0]), data: None },
            };
            match state.ban_manager.unban_ip(ip).await {
                Ok(()) => CommandResult { success: true, message: format!("IP {ip} 已解封"), data: None },
                Err(e) => CommandResult { success: false, message: format!("IP解封失败: {e}"), data: None },
            }
        })
    }
}

pub struct BanlistIpCommand;

impl AdminCommand for BanlistIpCommand {
    fn name(&self) -> &'static str { "banlist ip" }
    fn aliases(&self) -> &[&'static str] { &["banlistip"] }
    fn help(&self) -> &'static str { "列出 IP 封禁列表" }
    fn category(&self) -> CommandCategory { CommandCategory::Admin }
    fn execute(
        &self,
        _args: Vec<String>,
        state: Arc<crate::server::PlusServerState>,
    ) -> Pin<Box<dyn Future<Output = CommandResult> + Send>> {
        Box::pin(async move {
            let list = state.ban_manager.list_ip_bans().await;
            if list.is_empty() {
                return CommandResult { success: true, message: "IP封禁列表为空".into(), data: None };
            }
            let mut msg = String::from("IP封禁列表:\n");
            for entry in &list {
                msg.push_str(&format!("  {} — {}\n", entry.ip, entry.reason));
            }
            CommandResult { success: true, message: msg.trim().to_string(), data: None }
        })
    }
}

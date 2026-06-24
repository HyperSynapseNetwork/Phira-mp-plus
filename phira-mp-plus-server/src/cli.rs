//! Phira-mp+ CLI 交互式管理控制台
//!
//! 提供管理员可通过命令行执行的管理操作：
//! - 插件管理（列表、启用、禁用、重载）
//! - 用户管理（列表、踢出）
//! - 房间管理（列表、踢出、关闭）
//! - 服务器状态与关闭
//! - 消息广播
//! - 插件扩展命令
//!
//! 本模块不直接读写终端，而是通过 mpsc 通道与 TUI 层通信：
//! - 从 `cmd_rx` 接收命令字符串
//! - 向 `out_tx` 发送输出文本

use crate::plugin::PluginEvent;
use crate::server::PlusServerState;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::info;

// ── ANSI 颜色辅助 ──
mod c {
    pub fn green(s: &str) -> String { format!("\x1b[32m{}\x1b[0m", s) }
    pub fn cyan(s: &str) -> String { format!("\x1b[36m{}\x1b[0m", s) }
    pub fn yellow(s: &str) -> String { format!("\x1b[33m{}\x1b[0m", s) }
    pub fn red(s: &str) -> String { format!("\x1b[31m{}\x1b[0m", s) }
    pub fn bold(s: &str) -> String { format!("\x1b[1m{}\x1b[0m", s) }
    pub fn dim(s: &str) -> String { format!("\x1b[2m{}\x1b[0m", s) }
    pub fn magenta(s: &str) -> String { format!("\x1b[35m{}\x1b[0m", s) }
}

/// CLI 命令处理器
pub struct CliHandler {
    state: Arc<PlusServerState>,
    running: Arc<RwLock<bool>>,
    out_tx: mpsc::UnboundedSender<String>,
}

impl CliHandler {
    pub fn new(state: Arc<PlusServerState>, out_tx: mpsc::UnboundedSender<String>) -> Self {
        Self {
            state,
            running: Arc::new(RwLock::new(true)),
            out_tx,
        }
    }

    pub fn is_running(&self) -> Arc<RwLock<bool>> {
        Arc::clone(&self.running)
    }

    /// 发送一行输出到 TUI
    fn out(&self, msg: impl Into<String>) {
        let _ = self.out_tx.send(msg.into());
    }

    /// 启动 CLI 处理器（运行在 tokio 任务中）
    pub async fn start(&self, mut cmd_rx: mpsc::UnboundedReceiver<String>) {
        info!("CLI management console started");

        self.out(String::new());
        self.out(format!("  {} Phira-mp+ v{} 管理控制台", c::bold("◆"), env!("CARGO_PKG_VERSION")));
        self.out(format!("  {} 输入 {} 查看命令帮助，{} 关闭服务器",
            c::dim("▸"), c::cyan("help"), c::red("exit")));
        self.out(String::new());

        while let Some(line) = cmd_rx.recv().await {
            // 如果未运行（exit 后），直接退出
            if !*self.running.read().await {
                break;
            }

            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            let mut parts = line.split_whitespace();
            let command = parts.next().unwrap_or("");
            let args: Vec<&str> = parts.collect();

            match command {
                "exit" | "quit" | "q" => {
                    self.out(format!("  {} 正在关闭服务器...", c::yellow("⟳")));
                    *self.running.write().await = false;
                    self.state.shutdown.notify_one();
                    self.out(format!("  {} 已发送关闭信号", c::green("✓")));
                    break;
                }
                "help" | "h" | "?" => self.print_help().await,
                "plugins" | "pl" => self.list_plugins().await,
                "plug-enable" | "pe" => {
                    if args.is_empty() {
                        self.out(format!("  {} {} <插件名>", c::yellow("?"), c::bold("plug-enable")));
                    } else {
                        self.enable_plugin(args[0]).await;
                    }
                }
                "plug-disable" | "pd" => {
                    if args.is_empty() {
                        self.out(format!("  {} {} <插件名>", c::yellow("?"), c::bold("plug-disable")));
                    } else {
                        self.disable_plugin(args[0]).await;
                    }
                }
                "plug-reload" | "pr" => self.reload_plugins().await,
                "plugin" => {
                    let sub = args.first().copied().unwrap_or("");
                    match sub {
                        "list" | "ls" | "" => self.list_plugins().await,
                        "enable" | "on" => {
                            if args.len() < 2 {
                                self.out(format!("  {} {} plugin enable <插件名>", c::yellow("?"), c::bold("用法")));
                            } else {
                                self.enable_plugin(args[1]).await;
                            }
                        }
                        "disable" | "off" => {
                            if args.len() < 2 {
                                self.out(format!("  {} {} plugin disable <插件名>", c::yellow("?"), c::bold("用法")));
                            } else {
                                self.disable_plugin(args[1]).await;
                            }
                        }
                        "reload" | "r" => self.reload_plugins().await,
                        "info" => {
                            if args.len() < 2 {
                                self.out(format!("  {} {} plugin info <插件名>", c::yellow("?"), c::bold("用法")));
                            } else {
                                self.plugin_info(args[1]).await;
                            }
                        }
                        _ => {
                            self.out(format!("  {} 未知子命令: {}  ", c::red("✗"), c::yellow(sub)));
                            self.out(format!("  {} 可用: plugin list | enable | disable | reload | info", c::dim("▸")));
                        }
                    }
                }
                "users" | "u" => self.list_users().await,
                "rooms" | "r" => self.list_rooms().await,
                // ── 统一 room 子命令 ──
                "room" => {
                    let sub = args.first().copied().unwrap_or("");
                    match sub {
                        "list" | "ls" | "" => self.list_rooms().await,
                        "info" | "i" => {
                            if args.len() < 2 { self.out(format!("  {} {} room info <房间ID>", c::yellow("?"), c::bold("用法"))); }
                            else { self.room_info(args[1]).await; }
                        }
                        "start" | "s" => {
                            if args.len() < 2 { self.out(format!("  {} {} room start <房间ID>", c::yellow("?"), c::bold("用法"))); }
                            else { self.room_start(args[1]).await; }
                        }
                        "cancel" | "c" => {
                            if args.len() < 2 { self.out(format!("  {} {} room cancel <房间ID>", c::yellow("?"), c::bold("用法"))); }
                            else { self.room_cancel(args[1]).await; }
                        }
                        "kick" | "k" => {
                            if args.len() < 3 { self.out(format!("  {} {} room kick <房间ID> <用户ID>", c::yellow("?"), c::bold("用法"))); }
                            else { self.kick_from_room(args[1], args[2]).await; }
                        }
                        "transfer" | "t" => {
                            if args.len() < 3 { self.out(format!("  {} {} room transfer <房间ID> <用户ID>", c::yellow("?"), c::bold("用法"))); }
                            else { let uid: i32 = match args[2].parse() { Ok(id) => id, Err(_) => { self.out(format!("  {} 无效的用户ID", c::red("✗"))); return; } };
                                self.room_transfer(args[1], uid).await; }
                        }
                        "close" | "cl" => {
                            if args.len() < 2 { self.out(format!("  {} {} room close <房间ID>", c::yellow("?"), c::bold("用法"))); }
                            else { self.close_room(args[1]).await; }
                        }
                        "set" => {
                            if args.len() < 4 { self.out(format!("  {} {} room set <房间ID> <字段> <值>", c::yellow("?"), c::bold("用法"))); }
                            else { self.room_set(args[1], args[2], &args[3..].join(" ")).await; }
                        }
                        "history" | "h" => {
                            if args.len() < 2 { self.out(format!("  {} {} room history <房间ID>", c::yellow("?"), c::bold("用法"))); }
                            else { self.room_history(args[1]).await; }
                        }
                        "ban" | "b" => {
                            if args.len() < 3 { self.out(format!("  {} {} room ban <房间ID> <用户ID>", c::yellow("?"), c::bold("用法"))); }
                            else { let uid: i32 = match args[2].parse() { Ok(id) => id, Err(_) => { self.out(format!("  {} 无效的用户ID", c::red("✗"))); return; } };
                                match self.state.ban_manager.room_ban_user(args[1], uid).await {
                                    Ok(_) => self.out(format!("  {} 用户 {} 已加入房间 {} 的黑名单", c::green("✓"), uid, args[1])),
                                    Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                                }}
                        }
                        "unban" | "ub" => {
                            if args.len() < 3 { self.out(format!("  {} {} room unban <房间ID> <用户ID>", c::yellow("?"), c::bold("用法"))); }
                            else { let uid: i32 = match args[2].parse() { Ok(id) => id, Err(_) => { self.out(format!("  {} 无效的用户ID", c::red("✗"))); return; } };
                                match self.state.ban_manager.room_unban_user(args[1], uid).await {
                                    Ok(_) => self.out(format!("  {} 用户 {} 已移出房间 {} 的黑名单", c::green("✓"), uid, args[1])),
                                    Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                                }}
                        }
                        "banlist" | "bl" => {
                            if args.len() < 2 { self.out(format!("  {} {} room banlist <房间ID>", c::yellow("?"), c::bold("用法"))); }
                            else { self.room_ban_list(args[1]).await; }
                        }
                        _ => {
                            self.out(format!("  {} 未知子命令: {}  ", c::red("✗"), c::yellow(sub)));
                            self.out(format!("  {} 可用: room list|info|start|cancel|kick|transfer|close|set|history|ban|unban|banlist", c::dim("▸")));
                        }
                    }
                }
                "kick" | "k" => {
                    if args.len() >= 2 {
                        self.kick_from_room(args[0], args[1]).await;
                    } else if args.len() == 1 {
                        self.kick_user(args[0]).await;
                    } else {
                        self.out(format!("  {} {} <房间ID> <用户ID>  从房间踢出用户", c::yellow("?"), c::bold("kick")));
                        self.out(format!("  {} {} <用户ID>          从服务器踢出用户", c::dim("│"), c::bold("kick")));
                    }
                }
                "broadcast" | "bc" => {
                    if args.is_empty() {
                        self.out(format!("  {} {} all <消息>          广播给所有用户", c::yellow("?"), c::bold("broadcast")));
                        self.out(format!("  {} {} room <房间ID> <消息>  广播给指定房间", c::dim("▸"), c::bold("broadcast")));
                        self.out(format!("  {} {} user <用户ID> <消息>  发送给指定用户", c::dim("▸"), c::bold("broadcast")));
                    } else {
                        let scope = args[0];
                        let rest: Vec<&str> = args[1..].iter().copied().collect();
                        match scope {
                            "all" | "a" => {
                                self.broadcast_all(&rest.join(" ")).await;
                            }
                            "room" | "r" => {
                                if rest.len() < 2 {
                                    self.out(format!("  {} {} room <房间ID> <消息>", c::yellow("?"), c::bold("broadcast")));
                                } else {
                                    self.broadcast_room(rest[0], &rest[1..].join(" ")).await;
                                }
                            }
                            "user" | "u" => {
                                if rest.len() < 2 {
                                    self.out(format!("  {} {} user <用户ID> <消息>", c::yellow("?"), c::bold("broadcast")));
                                } else {
                                    let uid: i32 = match rest[0].parse() { Ok(id) => id, Err(_) => { self.out(format!("  {} 无效的用户ID", c::red("✗"))); return; } };
                                    self.broadcast_user(uid, &rest[1..].join(" ")).await;
                                }
                            }
                            _ => {
                                // 兼容旧语法：直接广播
                                let msg = args.join(" ");
                                self.broadcast_all(&msg).await;
                            }
                        }
                    }
                }
                "status" | "st" => self.status().await,
                "ext-list" | "el" => self.list_extensions().await,
                "ext-get" | "eg" => {
                    if args.len() < 2 {
                        self.out(format!("  {} {} <用户ID|房间ID> <key>", c::yellow("?"), c::bold("ext-get")));
                    } else {
                        self.get_extension(args[0], args[1]).await;
                    }
                }
                "ban" => {
                    if args.len() < 1 {
                        self.out(format!("  {} {} <用户ID> [原因]", c::yellow("?"), c::bold("ban")));
                    } else {
                        let reason = if args.len() >= 2 { args[1..].join(" ") } else { "违规行为".to_string() };
                        self.ban_user(args[0], &reason).await;
                    }
                }
                "unban" => {
                    if args.is_empty() {
                        self.out(format!("  {} {} <用户ID>", c::yellow("?"), c::bold("unban")));
                    } else {
                        self.unban_user(args[0]).await;
                    }
                }
                "banlist" | "bl" => self.ban_list().await,
                "room-ban" | "rb" => {
                    if args.len() < 2 {
                        self.out(format!("  {} {} <房间ID> <用户ID>", c::yellow("?"), c::bold("room-ban")));
                    } else {
                        let uid: i32 = match args[1].parse() {
                            Ok(id) => id,
                            Err(_) => { self.out(format!("  {} 无效的用户ID", c::red("✗"))); return; }
                        };
                        match self.state.ban_manager.room_ban_user(args[0], uid).await {
                            Ok(_) => self.out(format!("  {} 用户 {} 已加入房间 {} 的黑名单", c::green("✓"), uid, args[0])),
                            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                        }
                    }
                }
                "room-unban" | "ru" => {
                    if args.len() < 2 {
                        self.out(format!("  {} {} <房间ID> <用户ID>", c::yellow("?"), c::bold("room-unban")));
                    } else {
                        let uid: i32 = match args[1].parse() {
                            Ok(id) => id,
                            Err(_) => { self.out(format!("  {} 无效的用户ID", c::red("✗"))); return; }
                        };
                        match self.state.ban_manager.room_unban_user(args[0], uid).await {
                            Ok(_) => self.out(format!("  {} 用户 {} 已移出房间 {} 的黑名单", c::green("✓"), uid, args[0])),
                            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
                        }
                    }
                }
                "room-banlist" | "rbl" => {
                    if args.is_empty() {
                        self.out(format!("  {} {} <房间ID>", c::yellow("?"), c::bold("room-banlist")));
                    } else {
                        self.room_ban_list(args[0]).await;
                    }
                }
                "close-room" | "cr" => {
                    if args.is_empty() {
                        self.out(format!("  {} {} <房间ID>", c::yellow("?"), c::bold("close-room")));
                    } else {
                        self.close_room(args[0]).await;
                    }
                }
                "room-start" | "rs" => {
                    if args.is_empty() {
                        self.out(format!("  {} {} <房间ID>", c::yellow("?"), c::bold("room-start")));
                    } else {
                        self.room_start(args[0]).await;
                    }
                }
                "room-cancel" | "rc" => {
                    if args.is_empty() {
                        self.out(format!("  {} {} <房间ID>", c::yellow("?"), c::bold("room-cancel")));
                    } else {
                        self.room_cancel(args[0]).await;
                    }
                }
                "room-info" | "ri" => {
                    if args.is_empty() {
                        self.out(format!("  {} {} <房间ID>", c::yellow("?"), c::bold("room-info")));
                    } else {
                        self.room_info(args[0]).await;
                    }
                }
                "room-transfer" | "rt" => {
                    if args.len() < 2 {
                        self.out(format!("  {} {} <房间ID> <用户ID>", c::yellow("?"), c::bold("room-transfer")));
                    } else {
                        let uid: i32 = match args[1].parse() {
                            Ok(id) => id,
                            Err(_) => { self.out(format!("  {} 无效的用户ID", c::red("✗"))); return; }
                        };
                        self.room_transfer(args[0], uid).await;
                    }
                }
                "room-set" => {
                    if args.len() < 3 {
                        self.out(format!("  {} {} <房间ID> <字段> <值>", c::yellow("?"), c::bold("room-set")));
                        self.out(format!("  {} 字段: lock (true/false) | cycle (true/false) | chart-id (<谱面ID>)",
                            c::dim("▸")));
                    } else {
                        self.room_set(args[0], args[1], &args[2..].join(" ")).await;
                    }
                }
                "room-history" | "rh" => {
                    if args.is_empty() {
                        self.out(format!("  {} {} <房间ID>", c::yellow("?"), c::bold("room-history")));
                    } else {
                        self.room_history(args[0]).await;
                    }
                }
                _ => {
                    // 尝试插件命令
                    if !self.try_plugin_command(command, &args).await {
                        self.out(format!("  {} 未知命令: {}   输入 {} 查看帮助",
                            c::red("✗"), c::yellow(command), c::cyan("help")));
                    }
                }
            }
        }

        info!("CLI session ended");
    }

    async fn print_help(&self) {
        self.out(format!("  {} Phira-mp+ 管理命令", c::bold("◆")));
        self.out(format!("  {} ─────────────────────────────────────────────", c::dim("")));
        self.out(format!(""));
        self.out(format!("  {} 通用", c::cyan("▸")));
        self.out(format!("    {:<22} {}", c::dim("help"), "显示此帮助"));
        self.out(format!("    {:<22} {}", c::dim("exit"), "关闭服务器"));
        self.out(format!("    {:<22} {}", c::dim("status"), "服务器状态"));
        self.out(format!(""));
        self.out(format!("  {} WASM 插件", c::cyan("▸")));
        self.out(format!("    {:<22} {}", c::dim("plugin list"), "列出所有 WASM 插件"));
        self.out(format!("    {:<22} {}", c::dim("plugin enable <名>"), "启用插件"));
        self.out(format!("    {:<22} {}", c::dim("plugin disable <名>"), "禁用插件"));
        self.out(format!("    {:<22} {}", c::dim("plugin info <名>"), "插件详情"));
        self.out(format!("    {:<22} {}", c::dim("plugin reload"), "重载所有插件"));
        self.out(format!(""));
        self.out(format!("  {} 用户", c::cyan("▸")));
        self.out(format!("    {:<22} {}", c::dim("users"), "在线用户"));
        self.out(format!("    {:<22} {}", c::dim("kick <用户ID>"), "踢出用户"));
        self.out(format!("    {:<22} {}", c::dim("broadcast <消息>"), "广播消息"));
        self.out(format!(""));
        self.out(format!("  {} 房间 (room <子命令>)", c::cyan("▸")));
        self.out(format!("    {:<22} {}", c::dim("room list"), "活跃房间"));
        self.out(format!("    {:<22} {}", c::dim("room info <ID>"), "房间详情"));
        self.out(format!("    {:<22} {}", c::dim("room kick <ID> <用户>"), "踢出"));
        self.out(format!("    {:<22} {}", c::dim("room start|cancel <ID>"), "开始/取消"));
        self.out(format!("    {:<22} {}", c::dim("room close <ID>"), "解散"));
        self.out(format!("    {:<22} {}", c::dim("room transfer <ID> <用户>"), "转移房主"));
        self.out(format!("    {:<22} {}", c::dim("room set <ID> <字段> <值>"), "修改设置"));
        self.out(format!("    {:<22} {}", c::dim("room history <ID>"), "游玩记录"));
        self.out(format!("    {:<22} {}", c::dim("room ban|unban|banlist"), "房间黑名单"));
        self.out(format!(""));
        self.out(format!("  {} 黑名单", c::cyan("▸")));
        self.out(format!("    {:<22} {}", c::dim("ban <用户ID> [原因]"), "封禁"));
        self.out(format!("    {:<22} {}", c::dim("unban <用户ID>"), "解封"));
        self.out(format!("    {:<22} {}", c::dim("banlist"), "封禁列表"));
        self.out(format!(""));
        self.out(format!("  {} 扩展", c::cyan("▸")));
        self.out(format!("    {:<22} {}", c::dim("ext-list"), "扩展字段列表"));
        self.out(format!("    {:<22} {}", c::dim("ext-get <ID> <key>"), "查看扩展数据"));

        let plugin_cmds = self.state.plugin_manager.list_cli_commands().await;
        if !plugin_cmds.is_empty() {
            self.out(format!(""));
            self.out(format!("  {} WASM 插件扩展", c::magenta("▸")));
            for cmd in &plugin_cmds {
                self.out(format!("    {:<22} {}", c::dim(&cmd.name), cmd.description));
            }
        }
        self.out(format!(""));
        self.out(format!("  {} ─────────────────────────────────────────────", c::dim("")));
    }

    // ── 插件管理 ──

    async fn list_plugins(&self) {
        let plugins = self.state.plugin_manager.list_plugins().await;
        if plugins.is_empty() {
            self.out(format!("  {} 无已加载的插件", c::dim("·")));
            return;
        }
        self.out(format!("  {} 已加载插件 ({})", c::green("◆"), plugins.len()));
        self.out(format!("  {}", c::dim("  ────────────────────────────────────────────")));
        for p in &plugins {
            let state_str = match p.state {
                crate::plugin::PluginState::Enabled => c::green("启用"),
                crate::plugin::PluginState::Disabled => c::yellow("禁用"),
                crate::plugin::PluginState::Loaded => c::cyan("已加载"),
                crate::plugin::PluginState::Error(_) => c::red("错误"),
            };
            self.out(format!("  {} {:<20} {} {}",
                c::dim("│"), p.info.name, c::dim(p.info.version.as_str()), state_str));
        }
    }

    async fn enable_plugin(&self, name: &str) {
        match self.state.plugin_manager.enable_plugin(name).await {
            Ok(_) => self.out(format!("  {} 插件 {} 已启用", c::green("✓"), c::bold(name))),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    async fn disable_plugin(&self, name: &str) {
        match self.state.plugin_manager.disable_plugin(name).await {
            Ok(_) => self.out(format!("  {} 插件 {} 已禁用", c::green("✓"), c::bold(name))),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    async fn reload_plugins(&self) {
        self.out(format!("  {} 正在重载所有插件...", c::yellow("⟳")));
        match self.state.plugin_manager.reload_plugins().await {
            Ok(count) => self.out(format!("  {} 已重载 {} 个插件", c::green("✓"), count)),
            Err(e) => self.out(format!("  {} 重载失败: {}", c::red("✗"), e)),
        }
    }

    async fn plugin_info(&self, name: &str) {
        let plugins = self.state.plugin_manager.list_plugins().await;
        if let Some(p) = plugins.into_iter().find(|p| p.info.name == name) {
            let state_str = match p.state {
                crate::plugin::PluginState::Enabled => c::green("启用"),
                crate::plugin::PluginState::Disabled => c::yellow("禁用"),
                crate::plugin::PluginState::Loaded => c::cyan("已加载"),
                crate::plugin::PluginState::Error(ref e) => c::red(&format!("错误: {}", e)),
            };
            self.out(format!("  {} 插件详情: {}", c::green("◆"), c::bold(&p.info.name)));
            self.out(format!("  {} 版本:     {}", c::dim("│"), p.info.version));
            self.out(format!("  {} 作者:     {}", c::dim("│"), p.info.author));
            self.out(format!("  {} 描述:     {}", c::dim("│"), p.info.description));
            self.out(format!("  {} 状态:     {}", c::dim("│"), state_str));
            self.out(format!("  {} 路径:     {}", c::dim("│"), c::dim(&p.path)));
        } else {
            self.out(format!("  {} 未找到插件: {}", c::yellow("!"), name));
        }
    }

    // ── 用户管理 ──

    async fn list_users(&self) {
        let users = self.state.users.read().await;
        if users.is_empty() {
            self.out(format!("  {} 当前无在线用户", c::dim("·")));
        } else {
            self.out(format!("  {} 在线用户 ({})", c::green("◆"), users.len()));
            self.out(format!("  {}", c::dim("  ────────────────────────────────────────────")));
            for user in users.values() {
                let monitor = if user.monitor.load(std::sync::atomic::Ordering::SeqCst) {
                    c::yellow(" [观]")
                } else {
                    String::new()
                };
                let in_room = {
                    let room_guard = user.room.read().await;
                    room_guard.as_ref()
                        .map(|r| format!(" {} 房间 {}", c::dim("·"), c::cyan(&r.id.to_string())))
                        .unwrap_or_default()
                };
                self.out(format!("  {} {:<6} {}{}{}",
                    c::dim("│"), user.id, c::bold(&user.name), monitor, in_room));
            }
        }
        drop(users);

        // 显示房间概要
        let rooms = self.state.rooms.read().await;
        if !rooms.is_empty() {
            self.out(String::new());
            self.out(format!("  {} 活跃房间 ({})", c::green("◆"), rooms.len()));
            for room in rooms.values() {
                let users_count = room.users().await.len();
                let monitors_count = room.monitors().await.len();
                let state_str = match &*room.state.read().await {
                    crate::room::InternalRoomState::SelectChart => c::cyan("选曲中"),
                    crate::room::InternalRoomState::WaitForReady { .. } => c::yellow("等待准备"),
                    crate::room::InternalRoomState::Playing { .. } => c::magenta("游戏中"),
                };
                self.out(format!("  {} {:<15}  {}  {}+{} 人",
                    c::dim("│"), room.id.to_string(), state_str, users_count, monitors_count));
            }
        }
    }

    async fn list_rooms(&self) {
        let rooms = self.state.rooms.read().await;
        if rooms.is_empty() {
            self.out(format!("  {} 当前无活跃房间", c::dim("·")));
            drop(rooms);
            return;
        }

        self.out(format!("  {} 活跃房间 ({})", c::green("◆"), rooms.len()));
        self.out(format!("  {}", c::dim("  ────────────────────────────────────────────")));
        for room in rooms.values() {
            let users_in_room = room.users().await;
            let monitors_in_room = room.monitors().await;

            let state_str = match &*room.state.read().await {
                crate::room::InternalRoomState::SelectChart => "SelectChart",
                crate::room::InternalRoomState::WaitForReady { .. } => "WaitForReady",
                crate::room::InternalRoomState::Playing { .. } => "Playing",
            };
            let locked = if room.locked.load(std::sync::atomic::Ordering::SeqCst) {
                c::yellow("锁定")
            } else {
                c::dim("未锁定")
            };
            let cycling = if room.cycle.load(std::sync::atomic::Ordering::SeqCst) {
                c::cyan("轮换")
            } else {
                c::dim("不轮换")
            };

            self.out(format!("  {} {}", c::dim("┏"), c::bold(&room.id.to_string())));
            self.out(format!("  {} 状态: {}  {}  {}  {}", c::dim("┃"), state_str, locked, cycling,
                if users_in_room.len() + monitors_in_room.len() > 0 {
                    c::cyan(&format!("{} 人在线", users_in_room.len() + monitors_in_room.len()))
                } else {
                    c::dim("空闲")
                }
            ));
            if !users_in_room.is_empty() {
                self.out(format!("  {} 玩家: {}", c::dim("┃"),
                    users_in_room.iter()
                        .map(|u| format!("{}({})", c::bold(&u.name), u.id))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if !monitors_in_room.is_empty() {
                self.out(format!("  {} 旁观: {}", c::dim("┃"),
                    monitors_in_room.iter()
                        .map(|u| format!("{}({})", c::bold(&u.name), u.id))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            self.out(format!("  {}", c::dim("  ─ ─ ─ ─ ─ ─")));
        }

        drop(rooms);

        // 在线用户概要
        let users = self.state.users.read().await;
        if !users.is_empty() {
            self.out(String::new());
            self.out(format!("  {} 在线用户 ({})", c::green("◆"), users.len()));
            for user in users.values() {
                let in_room = if user.room.read().await.is_some() {
                    c::cyan(" 在房间中")
                } else {
                    c::dim(" 大厅")
                };
                self.out(format!("  {} {:<6}  {}{}", c::dim("│"), user.id, c::bold(&user.name), in_room));
            }
        }
    }

    async fn kick_from_room(&self, room_id: &str, target_id: &str) {
        let target: i32 = match target_id.parse() {
            Ok(id) => id,
            Err(_) => {
                self.out(format!("  {} 无效的用户ID: {}", c::red("✗"), target_id));
                return;
            }
        };

        let room = {
            let rooms = self.state.rooms.read().await;
            let rid: phira_mp_common::RoomId = match room_id.to_string().try_into() {
                Ok(id) => id,
                Err(_) => {
                    self.out(format!("  {} 无效的房间ID", c::red("✗")));
                    return;
                }
            };
            rooms.get(&rid).map(Arc::clone)
        };

        if let Some(room) = room {
            let user_to_kick = {
                let users_in_room = room.users().await;
                let monitors_in_room = room.monitors().await;
                users_in_room.into_iter()
                    .chain(monitors_in_room.into_iter())
                    .find(|u| u.id == target)
            };

            if let Some(user) = user_to_kick {
                // 向房间内其他用户发送通知
                room.send(phira_mp_common::Message::Chat {
                    user: 0,
                    content: format!("用户 {} 已被管理员踢出房间", user.name),
                }).await;
                let _ = room.on_user_leave(&user).await;
                info!(user = target, room = room_id, "kicked from room by admin");
                self.state.plugin_manager
                    .trigger(&PluginEvent::RoomModify {
                        user_id: target,
                        room_id: room_id.to_string(),
                        data: r#"{"action":"kicked"}"#.to_string(),
                    }).await;
                self.out(format!("  {} 用户 {} 已从房间 {} 踢出", c::green("✓"), target, room_id));
            } else {
                self.out(format!("  {} 在房间 {} 中未找到用户 {}", c::yellow("!"), room_id, target));
            }
        } else {
            self.out(format!("  {} 未找到房间 {}", c::red("✗"), room_id));
        }
    }

    async fn kick_user(&self, target_id: &str) {
        let target: i32 = match target_id.parse() {
            Ok(id) => id,
            Err(_) => {
                self.out(format!("  {} 无效的用户ID: {}", c::red("✗"), target_id));
                return;
            }
        };

        let user = {
            let users = self.state.users.read().await;
            users.get(&target).map(Arc::clone)
        };

        if let Some(user) = user {
            // 从房间移除（如果在房间中）
            {
                let room_clone = {
                    let room_guard = user.room.read().await;
                    room_guard.as_ref().map(Arc::clone)
                };
                if let Some(room) = room_clone {
                    let room_id = room.id.to_string();
                    if room.on_user_leave(&user).await {
                        self.state.rooms.write().await.remove(&room.id);
                    }
                    self.state.plugin_manager
                        .trigger(&PluginEvent::RoomLeave { user_id: target, room_id }).await;
                }
            }

            // 发送踢出消息
            {
                let sessions = self.state.sessions.read().await;
                for session in sessions.values() {
                    if session.user.id == target {
                        let _ = session.stream.send(
                            phira_mp_common::ServerCommand::Message(
                                phira_mp_common::Message::Chat {
                                    user: 0,
                                    content: "你已被管理员踢出服务器".to_string(),
                                },
                            )
                        ).await;
                        break;
                    }
                }
            }

            // 从用户列表移除
            self.state.users.write().await.remove(&target);
            info!(user = target, "kicked from server by admin");
            self.state.plugin_manager
                .trigger(&PluginEvent::UserDisconnect {
                    user_id: target,
                    user_name: user.name.clone(),
                }).await;

            self.out(format!("  {} 用户 {} ({}) 已从服务器踢出", c::green("✓"), c::bold(&user.name), target));
        } else {
            self.out(format!("  {} 未找到用户 {}", c::red("✗"), target));
        }
    }

    async fn close_room(&self, room_id: &str) {
        let rid: phira_mp_common::RoomId = match room_id.to_string().try_into() {
            Ok(id) => id,
            Err(_) => {
                self.out(format!("  {} 无效的房间ID", c::red("✗")));
                return;
            }
        };

        let room_opt = {
            let rooms = self.state.rooms.read().await;
            rooms.get(&rid).map(Arc::clone)
        };

        if let Some(room) = room_opt {
            let room_id_str = room.id.to_string();

            room.send(phira_mp_common::Message::Chat {
                user: 0,
                content: "房间已被管理员关闭".to_string(),
            }).await;

            for user in room.users().await {
                *user.room.write().await = None;
            }
            for user in room.monitors().await {
                *user.room.write().await = None;
            }

            self.state.rooms.write().await.remove(&rid);
            info!(room = room_id_str, "room closed by admin");
            self.state.plugin_manager
                .trigger(&PluginEvent::RoomModify {
                    user_id: 0,
                    room_id: room_id_str,
                    data: r#"{"action":"closed"}"#.to_string(),
                }).await;

            self.out(format!("  {} 房间 {} 已解散", c::green("✓"), c::bold(room_id)));
        } else {
            self.out(format!("  {} 未找到房间 {}", c::red("✗"), room_id));
        }
    }

    // ── 房间管理增强 ──

    /// 从字符串查找房间
    async fn find_room(&self, room_id: &str) -> Option<Arc<crate::room::Room>> {
        let rid: phira_mp_common::RoomId = room_id.to_string().try_into().ok()?;
        self.state.rooms.read().await.get(&rid).map(Arc::clone)
    }

    async fn room_info(&self, room_id: &str) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => { self.out(format!("  {} 未找到房间 {}", c::red("✗"), room_id)); return; }
        };
        let users = room.users().await;
        let monitors = room.monitors().await;
        let state_str = match &*room.state.read().await {
            crate::room::InternalRoomState::SelectChart => "SelectChart",
            crate::room::InternalRoomState::WaitForReady { .. } => "WaitForReady",
            crate::room::InternalRoomState::Playing { .. } => "Playing",
        };
        let locked = if room.locked.load(std::sync::atomic::Ordering::SeqCst) { c::yellow("锁定") } else { c::dim("未锁定") };
        let cycling = if room.cycle.load(std::sync::atomic::Ordering::SeqCst) { c::cyan("轮换") } else { c::dim("不轮换") };
        let chart_info = match room.chart.read().await.as_ref() {
            Some(c) => format!("{} (id={})", c.name, c.id),
            None => "未选择".to_string(),
        };
        let host_name = room.host_id().await
            .and_then(|hid| users.iter().find(|u| u.id == hid).map(|u| u.name.clone()))
            .unwrap_or_else(|| "未知".to_string());

        self.out(format!("  {} 房间: {}", c::green("◆"), c::bold(room_id)));
        self.out(format!("  {} 状态: {} | {} | {}", c::dim("│"), state_str, locked, cycling));
        self.out(format!("  {} 房主: {}", c::dim("│"), host_name));
        self.out(format!("  {} 谱面: {}", c::dim("│"), chart_info));
        self.out(format!("  {} 玩家: {}", c::dim("│"),
            users.iter().map(|u| format!("{}({})", u.name, u.id)).collect::<Vec<_>>().join(", ")));
        if !monitors.is_empty() {
            self.out(format!("  {} 旁观: {}", c::dim("│"),
                monitors.iter().map(|u| format!("{}({})", u.name, u.id)).collect::<Vec<_>>().join(", ")));
        }
        // 历史记录统计
        let history = room.play_history.read().await;
        if !history.is_empty() {
            self.out(format!("  {} 历史对局: {} 轮", c::dim("│"), history.len()));
        }
    }

    /// 强制开始游戏（管理员操作）
    async fn room_start(&self, room_id: &str) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => { self.out(format!("  {} 未找到房间 {}", c::red("✗"), room_id)); return; }
        };
        // 检查状态
        {
            let guard = room.state.read().await;
            match &*guard {
                crate::room::InternalRoomState::Playing { .. } => {
                    self.out(format!("  {} 游戏已在进行中", c::yellow("!")));
                    return;
                }
                _ => {}
            }
        }
        // 检查谱面
        if room.chart.read().await.is_none() {
            self.out(format!("  {} 尚未选择谱面", c::yellow("!")));
            return;
        }
        // 将所有用户标记为已准备
        let users = room.users().await;
        let user_ids: std::collections::HashSet<i32> = users.iter().map(|u| u.id).collect();
        room.reset_game_time().await;
        room.send(phira_mp_common::Message::GameStart { user: 0 }).await;
        *room.state.write().await = crate::room::InternalRoomState::WaitForReady {
            started: user_ids,
        };
        room.on_state_change().await;
        room.check_all_ready().await;
        // 触发插件事件
        if let Some(pm) = &room.plugin_manager {
            pm.trigger(&PluginEvent::GameStart {
                user_id: 0, room_id: room_id.to_string(),
            }).await;
        }
        self.out(format!("  {} 游戏已开始", c::green("✓")));
    }

    /// 取消准备状态（管理员操作）
    async fn room_cancel(&self, room_id: &str) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => { self.out(format!("  {} 未找到房间 {}", c::red("✗"), room_id)); return; }
        };
        let canceled = {
            let mut guard = room.state.write().await;
            if let crate::room::InternalRoomState::WaitForReady { .. } = &*guard {
                room.send(phira_mp_common::Message::CancelGame { user: 0 }).await;
                *guard = crate::room::InternalRoomState::SelectChart;
                room.on_state_change().await;
                true
            } else {
                false
            }
        };
        if canceled {
            self.out(format!("  {} 已取消准备状态", c::green("✓")));
        } else {
            self.out(format!("  {} 当前状态不需要取消", c::yellow("!")));
        }
    }

    async fn room_transfer(&self, room_id: &str, user_id: i32) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => { self.out(format!("  {} 未找到房间 {}", c::red("✗"), room_id)); return; }
        };
        // 获取用户名
        let user_name = room.users().await.iter()
            .find(|u| u.id == user_id)
            .map(|u| u.name.clone())
            .unwrap_or_else(|| format!("{}", user_id));
        // 发送房间通知
        room.send(phira_mp_common::Message::Chat {
            user: 0,
            content: format!("房主已转移给 {}", user_name),
        }).await;
        match room.transfer_host(user_id).await {
            Ok(_) => self.out(format!("  {} 房主已转移给用户 {} ({})", c::green("✓"), user_name, user_id)),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    async fn room_set(&self, room_id: &str, field: &str, value: &str) {
        use std::sync::atomic::Ordering;
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => { self.out(format!("  {} 未找到房间 {}", c::red("✗"), room_id)); return; }
        };
        match field {
            "lock" | "locked" => {
                let v = value == "true" || value == "1" || value == "锁定";
                room.locked.store(v, Ordering::SeqCst);
                room.send(phira_mp_common::Message::LockRoom { lock: v }).await;
                room.on_state_change().await;
                // 触发插件事件
                self.state.plugin_manager
                    .trigger(&PluginEvent::RoomModify {
                        user_id: 0,
                        room_id: room_id.to_string(),
                        data: format!(r#"{{"action":"lock","value":{v}}}"#),
                    }).await;
                self.out(format!("  {} 房间 {} 已{}锁定", c::green("✓"), room_id, if v { "" } else { "解除" }));
            }
            "cycle" | "cycling" => {
                let v = value == "true" || value == "1" || value == "轮换";
                room.cycle.store(v, Ordering::SeqCst);
                room.send(phira_mp_common::Message::CycleRoom { cycle: v }).await;
                room.on_state_change().await;
                // 触发插件事件
                self.state.plugin_manager
                    .trigger(&PluginEvent::RoomModify {
                        user_id: 0,
                        room_id: room_id.to_string(),
                        data: format!(r#"{{"action":"cycle","value":{v}}}"#),
                    }).await;
                self.out(format!("  {} 房间 {} 已{}轮换", c::green("✓"), room_id, if v { "开启" } else { "关闭" }));
            }
            "chart-id" | "chart" => {
                let cid: i32 = match value.parse() {
                    Ok(id) => id,
                    Err(_) => { self.out(format!("  {} 无效的谱面ID", c::red("✗"))); return; }
                };
                // 从 API 获取谱面信息（模拟玩家选曲）
                let chart = match reqwest::get(format!("https://phira.5wyxi.com/chart/{cid}")).await {
                    Ok(resp) => match resp.error_for_status() {
                        Ok(resp) => resp.json::<crate::server::Chart>().await.ok(),
                        Err(_) => None,
                    },
                    Err(_) => None,
                }.unwrap_or(crate::server::Chart {
                    id: cid,
                    name: format!("chart_{cid}"),
                });
                room.send(phira_mp_common::Message::SelectChart {
                    user: 0,
                    name: chart.name.clone(),
                    id: chart.id,
                }).await;
                room.chart.write().await.replace(chart);
                room.on_state_change().await;
                // 触发插件事件
                self.state.plugin_manager
                    .trigger(&PluginEvent::RoomModify {
                        user_id: 0,
                        room_id: room_id.to_string(),
                        data: format!(r#"{{"action":"select-chart","chart-id":{cid}}}"#),
                    }).await;
                self.out(format!("  {} 谱面已切换为 ID {}", c::green("✓"), cid));
            }
            _ => {
                self.out(format!("  {} 未知字段: {}", c::red("✗"), field));
                self.out(format!("  {} 支持: lock, cycle, chart-id", c::dim("▸")));
            }
        }
    }

    async fn room_history(&self, room_id: &str) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => { self.out(format!("  {} 未找到房间 {}", c::red("✗"), room_id)); return; }
        };
        let history = room.play_history.read().await;
        if history.is_empty() {
            self.out(format!("  {} 该房间暂无游玩记录", c::dim("·")));
            return;
        }
        self.out(format!("  {} 房间 {} 游玩记录 ({} 轮)", c::green("◆"), room_id, history.len()));
        self.out(format!("  {}", c::dim("  ────────────────────────────────────────────")));
        for (i, round) in history.iter().enumerate() {
            let round_num = i + 1;
            self.out(format!("  {} 第{}轮: {} (id={})", c::dim("┏"), round_num, c::bold(&round.chart_name), round.chart_id));
            for r in &round.results {
                let status = if r.aborted { c::yellow(" (放弃)") } else { String::new() };
                self.out(format!("  {} {:<6}  得分:{:<8} 准确率:{:<6.2}%  FC:{}{}",
                    c::dim("┃"), format!("{}({})", r.user_name, r.user_id),
                    r.score, r.accuracy * 100.0,
                    if r.full_combo { c::green("✓") } else { c::dim("✗") },
                    status,
                ));
            }
            self.out(format!("  {}", c::dim("  ─ ─ ─ ─ ─ ─")));
        }
    }

    /// 广播给所有用户
    async fn broadcast_all(&self, message: &str) {
        let users = self.state.users.read().await;
        let content = format!("[系统广播] {}", message);
        let msg = phira_mp_common::ServerCommand::Message(
            phira_mp_common::Message::Chat { user: 0, content },
        );
        let mut sent = 0usize;
        for user in users.values() {
            user.try_send(msg.clone()).await;
            sent += 1;
        }
        drop(users);
        info!(sent, message = %message, "broadcast to all");
        self.state.plugin_manager
            .trigger(&PluginEvent::RoomModify {
                user_id: 0,
                room_id: "*broadcast*".to_string(),
                data: format!(r#"{{"action":"broadcast","scope":"all","message":"{}"}}"#, message),
            }).await;
        self.out(format!("  {} 已广播给 {} 个用户", c::green("✓"), sent));
    }

    /// 广播给指定房间
    async fn broadcast_room(&self, room_id: &str, message: &str) {
        let room = match self.find_room(room_id).await {
            Some(r) => r,
            None => { self.out(format!("  {} 未找到房间 {}", c::red("✗"), room_id)); return; }
        };
        let content = format!("[房间广播] {}", message);
        room.send(phira_mp_common::Message::Chat { user: 0, content }).await;
        let users = room.users().await.len();
        info!(room = room_id, message = %message, "broadcast to room");
        self.state.plugin_manager
            .trigger(&PluginEvent::RoomModify {
                user_id: 0,
                room_id: room_id.to_string(),
                data: format!(r#"{{"action":"broadcast","scope":"room","message":"{}"}}"#, message),
            }).await;
        self.out(format!("  {} 已发送房间广播 ({} 人)", c::green("✓"), users));
    }

    /// 发送给指定用户
    async fn broadcast_user(&self, user_id: i32, message: &str) {
        let users = self.state.users.read().await;
        if let Some(user) = users.get(&user_id) {
            let content = format!("[管理员消息] {}", message);
            user.try_send(phira_mp_common::ServerCommand::Message(
                phira_mp_common::Message::Chat { user: 0, content },
            )).await;
            info!(user = user_id, message = %message, "message to user");
            self.out(format!("  {} 已发送给用户 {}", c::green("✓"), user_id));
        } else {
            self.out(format!("  {} 未找到用户 {}", c::red("✗"), user_id));
        }
    }

    async fn status(&self) {
        let users = self.state.users.read().await.len();
        let rooms = self.state.rooms.read().await.len();
        let sessions = self.state.sessions.read().await.len();
        let plugins = self.state.plugin_manager.list_plugins().await.len();
        self.out(format!("  {} Phira-mp+ v{}  │ 端口 {}  │ 用户 {} 会话 {} 房间 {} 插件 {}",
            c::bold("◆"), env!("CARGO_PKG_VERSION"),
            self.state.config.port, users, sessions, rooms, plugins));
    }

    // ── 黑名单管理 ──

    async fn ban_user(&self, target: &str, reason: &str) {
        let uid: i32 = match target.parse() {
            Ok(id) => id,
            Err(_) => {
                self.out(format!("  {} 无效的用户ID: {}", c::red("✗"), target));
                return;
            }
        };
        match self.state.ban_manager.ban_user(uid, reason).await {
            Ok(_) => self.out(format!("  {} 用户 {} 已封禁\n    {}", c::green("✓"), uid, c::yellow(reason))),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    async fn unban_user(&self, target: &str) {
        let uid: i32 = match target.parse() {
            Ok(id) => id,
            Err(_) => {
                self.out(format!("  {} 无效的用户ID: {}", c::red("✗"), target));
                return;
            }
        };
        match self.state.ban_manager.unban_user(uid).await {
            Ok(_) => self.out(format!("  {} 用户 {} 已解封", c::green("✓"), uid)),
            Err(e) => self.out(format!("  {} {}", c::red("✗"), e)),
        }
    }

    async fn ban_list(&self) {
        let list = self.state.ban_manager.list_banned().await;
        if list.is_empty() {
            self.out(format!("  {} 黑名单为空", c::dim("·")));
            return;
        }
        self.out(format!("  {} 封禁用户 ({})", c::green("◆"), list.len()));
        self.out(format!("  {}", c::dim("  ────────────────────────────────────────────")));
        for entry in &list {
            self.out(format!("  {} {:<6}  {}", c::dim("│"),
                entry.user_id,
                c::dim(&entry.reason),
            ));
        }
    }

    async fn room_ban_list(&self, room_id: &str) {
        let list = self.state.ban_manager.list_room_bans(room_id).await;
        if list.is_empty() {
            self.out(format!("  {} 房间 {} 的黑名单为空", c::dim("·"), room_id));
            return;
        }
        self.out(format!("  {} 房间 {} 黑名单: {:?}", c::green("◆"), room_id, list));
    }

    // ── 扩展数据 ──

    async fn list_extensions(&self) {
        let user_fields = self.state.extensions.list_user_fields().await;
        let room_fields = self.state.extensions.list_room_fields().await;

        self.out(format!("  {} 用户扩展字段:", c::cyan("◆")));
        if user_fields.is_empty() {
            self.out(format!("    {} 无", c::dim("-")));
        } else {
            for f in &user_fields {
                self.out(format!("    {} {}", c::dim("·"), f));
            }
        }

        self.out(format!("  {} 房间扩展字段:", c::cyan("◆")));
        if room_fields.is_empty() {
            self.out(format!("    {} 无", c::dim("-")));
        } else {
            for f in &room_fields {
                self.out(format!("    {} {}", c::dim("·"), f));
            }
        }
    }

    async fn get_extension(&self, id: &str, key: &str) {
        if let Ok(uid) = id.parse::<i32>() {
            if let Some(val) = self.state.extensions.get_user_extra(uid, key).await {
                self.out(format!("  {} 用户 {} 的 {} = {}", c::green("◆"), uid, c::cyan(key), val));
                return;
            }
        }
        if let Some(val) = self.state.extensions.get_room_extra(id, key).await {
            self.out(format!("  {} 房间 {} 的 {} = {}", c::green("◆"), id, c::cyan(key), val));
            return;
        }
        self.out(format!("  {} 未找到扩展数据: id={}, key={}", c::yellow("!"), id, key));
    }

    /// 尝试将命令分发给插件注册的 CLI 命令
    async fn try_plugin_command(&self, command: &str, args: &[&str]) -> bool {
        let result = self.state.plugin_manager.execute_cli_command(command, args).await;
        match result {
            Some(output_lines) => {
                for line in output_lines {
                    self.out(format!("  {} {}", c::magenta("◈"), line));
                }
                true
            }
            None => false,
        }
    }
}

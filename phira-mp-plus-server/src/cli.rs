//! Phira-mp+ CLI 交互式管理控制台
//!
//! 提供管理员可通过命令行执行的管理操作：
//! - 插件管理（列表、启用、禁用、重载）
//! - 用户管理（列表、踢出）
//! - 房间管理（列表、踢出、关闭）
//! - 服务器状态与关闭
//! - 消息广播
//! - 插件扩展命令

use crate::plugin::PluginEvent;
use crate::server::PlusServerState;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::RwLock;
use tracing::info;

// ── ANSI 颜色辅助 ──
mod c {
    #![allow(dead_code)]
    pub const RESET: &str = "\x1b[0m";
    pub const BOLD: &str = "\x1b[1m";
    pub const DIM: &str = "\x1b[2m";
    pub const GREEN: &str = "\x1b[32m";
    pub const CYAN: &str = "\x1b[36m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const RED: &str = "\x1b[31m";
    pub const MAGENTA: &str = "\x1b[35m";
    pub const BLUE: &str = "\x1b[34m";
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
}

impl CliHandler {
    pub fn new(state: Arc<PlusServerState>) -> Self {
        Self {
            state,
            running: Arc::new(RwLock::new(true)),
        }
    }

    pub fn is_running(&self) -> Arc<RwLock<bool>> {
        Arc::clone(&self.running)
    }

    /// 启动交互式 CLI 监听（运行在独立任务中）
    pub async fn start(&self) {
        let stdin = tokio::io::stdin();
        let reader = BufReader::new(stdin);
        let mut lines = reader.lines();

        info!("CLI management console started");
        println!();
        println!("  {} Phira-mp+ v{} 管理控制台", c::bold("◆"), env!("CARGO_PKG_VERSION"));
        println!("  {} 输入 {} 查看命令帮助，{} 关闭服务器",
            c::dim("▸"), c::cyan("help"), c::red("exit"));
        println!();

        while let Ok(Some(line)) = lines.next_line().await {
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
                    println!("  {} 正在关闭服务器...", c::yellow("⟳"));
                    *self.running.write().await = false;
                    self.state.shutdown.notify_one();
                    println!("  {} 已发送关闭信号", c::green("✓"));
                    break;
                }
                "help" | "h" | "?" => self.print_help().await,
                "plugins" | "pl" => self.list_plugins().await,
                "plug-enable" | "pe" => {
                    if args.is_empty() {
                        println!("  {} {} <插件名>", c::yellow("?"), c::bold("plug-enable"));
                    } else {
                        self.enable_plugin(args[0]).await;
                    }
                }
                "plug-disable" | "pd" => {
                    if args.is_empty() {
                        println!("  {} {} <插件名>", c::yellow("?"), c::bold("plug-disable"));
                    } else {
                        self.disable_plugin(args[0]).await;
                    }
                }
                "plug-reload" | "pr" => self.reload_plugins().await,
                "users" | "u" => self.list_users().await,
                "rooms" | "r" => self.list_rooms().await,
                "kick" | "k" => {
                    if args.len() < 2 {
                        println!("  {} {} <房间ID> <用户ID>  从房间踢出用户", c::yellow("?"), c::bold("kick"));
                        println!("  {} {} <用户ID>          从服务器踢出用户", c::dim("│"), c::bold("kick"));
                    } else if args.len() == 2 {
                        self.kick_from_room(args[0], args[1]).await;
                    } else if args.len() == 1 {
                        self.kick_user(args[0]).await;
                    } else {
                        self.kick_user(args[0]).await;
                    }
                }
                "broadcast" | "bc" => {
                    if args.is_empty() {
                        println!("  {} {} <消息>", c::yellow("?"), c::bold("broadcast"));
                    } else {
                        let msg = args.join(" ");
                        self.broadcast(&msg).await;
                    }
                }
                "status" | "st" => self.status().await,
                "ext-list" | "el" => self.list_extensions().await,
                "ext-get" | "eg" => {
                    if args.len() < 2 {
                        println!("  {} {} <用户ID|房间ID> <key>", c::yellow("?"), c::bold("ext-get"));
                    } else {
                        self.get_extension(args[0], args[1]).await;
                    }
                }
                "close-room" | "cr" => {
                    if args.is_empty() {
                        println!("  {} {} <房间ID>", c::yellow("?"), c::bold("close-room"));
                    } else {
                        self.close_room(args[0]).await;
                    }
                }
                _ => {
                    // 尝试插件命令
                    if !self.try_plugin_command(command, &args).await {
                        println!("  {} 未知命令: {}   输入 {} 查看帮助",
                            c::red("✗"), c::yellow(command), c::cyan("help"));
                    }
                }
            }
        }

        info!("CLI session ended");
    }

    async fn print_help(&self) {
        println!();
        println!("  {} {}", c::bold("⋆"), c::bold("Phira-mp+ 管理命令"));
        println!("  {}", c::dim("──────────────────────────────────────────────────"));
        println!();
        println!("  {} 通用", c::cyan("▸"));
        println!("    {} {:<20} {}", c::dim("│"), "help (h, ?)", "显示此帮助");
        println!("    {} {:<20} {}", c::dim("│"), "exit (quit, q)", "关闭服务器");
        println!("    {} {:<20} {}", c::dim("│"), "status (st)", "服务器状态");
        println!();
        println!("  {} 插件管理", c::cyan("▸"));
        println!("    {} {:<20} {}", c::dim("│"), "plugins (pl)", "列出所有插件");
        println!("    {} {:<20} {}", c::dim("│"), "plug-enable (pe)", "启用插件");
        println!("    {} {:<20} {}", c::dim("│"), "plug-disable (pd)", "禁用插件");
        println!("    {} {:<20} {}", c::dim("│"), "plug-reload (pr)", "重载所有插件");
        println!();
        println!("  {} 用户 / 房间", c::cyan("▸"));
        println!("    {} {:<20} {}", c::dim("│"), "users (u)", "在线用户");
        println!("    {} {:<20} {}", c::dim("│"), "rooms (r)", "活跃房间");
        println!("    {} {:<20} {}", c::dim("│"), "kick (k)", "踢出用户");
        println!("    {} {:<20} {}", c::dim("│"), "close-room (cr)", "解散房间");
        println!("    {} {:<20} {}", c::dim("│"), "broadcast (bc)", "广播消息");
        println!();
        println!("  {} 扩展数据", c::cyan("▸"));
        println!("    {} {:<20} {}", c::dim("│"), "ext-list (el)", "列出扩展字段");
        println!("    {} {:<20} {}", c::dim("│"), "ext-get (eg)", "查看扩展数据");

        // 列出插件注册的命令
        let plugin_cmds = self.state.plugin_manager.list_cli_commands().await;
        if !plugin_cmds.is_empty() {
            println!();
            println!("  {} 插件扩展", c::magenta("▸"));
            for cmd in &plugin_cmds {
                println!("    {} {:<20} {}", c::dim("│"), cmd.name, cmd.description);
            }
        }
        println!();
        println!("  {}", c::dim("──────────────────────────────────────────────────"));
        println!();
    }

    // ── 插件管理 ──

    async fn list_plugins(&self) {
        let plugins = self.state.plugin_manager.list_plugins().await;
        if plugins.is_empty() {
            println!("  {} 无已加载的插件", c::dim("·"));
            return;
        }
        println!("  {} 已加载插件 ({})", c::green("◆"), plugins.len());
        println!("  {}", c::dim("  ────────────────────────────────────────────"));
        for p in &plugins {
            let state_str = match p.state {
                crate::plugin::PluginState::Enabled => c::green("启用"),
                crate::plugin::PluginState::Disabled => c::yellow("禁用"),
                crate::plugin::PluginState::Loaded => c::cyan("已加载"),
                crate::plugin::PluginState::Error(_) => c::red("错误"),
            };
            println!("  {} {:<20} {} {}",
                c::dim("│"), p.info.name, c::dim(p.info.version.as_str()), state_str);
        }
    }

    async fn enable_plugin(&self, name: &str) {
        match self.state.plugin_manager.enable_plugin(name).await {
            Ok(_) => println!("  {} 插件 {} 已启用", c::green("✓"), c::bold(name)),
            Err(e) => println!("  {} {}", c::red("✗"), e),
        }
    }

    async fn disable_plugin(&self, name: &str) {
        match self.state.plugin_manager.disable_plugin(name).await {
            Ok(_) => println!("  {} 插件 {} 已禁用", c::green("✓"), c::bold(name)),
            Err(e) => println!("  {} {}", c::red("✗"), e),
        }
    }

    async fn reload_plugins(&self) {
        println!("  {} 正在重载所有插件...", c::yellow("⟳"));
        match self.state.plugin_manager.reload_plugins().await {
            Ok(count) => println!("  {} 已重载 {} 个插件", c::green("✓"), count),
            Err(e) => println!("  {} 重载失败: {}", c::red("✗"), e),
        }
    }

    // ── 用户管理 ──

    async fn list_users(&self) {
        let users = self.state.users.read().await;
        if users.is_empty() {
            println!("  {} 当前无在线用户", c::dim("·"));
        } else {
            println!("  {} 在线用户 ({})", c::green("◆"), users.len());
            println!("  {}", c::dim("  ────────────────────────────────────────────"));
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
                println!("  {} {:<6} {}{}{}",
                    c::dim("│"), user.id, c::bold(&user.name), monitor, in_room);
            }
        }
        drop(users);

        // 显示房间概要
        let rooms = self.state.rooms.read().await;
        if !rooms.is_empty() {
            println!();
            println!("  {} 活跃房间 ({})", c::green("◆"), rooms.len());
            for room in rooms.values() {
                let users_count = room.users().await.len();
                let monitors_count = room.monitors().await.len();
                let state_str = match &*room.state.read().await {
                    crate::room::InternalRoomState::SelectChart => c::cyan("选曲中"),
                    crate::room::InternalRoomState::WaitForReady { .. } => c::yellow("等待准备"),
                    crate::room::InternalRoomState::Playing { .. } => c::magenta("游戏中"),
                };
                println!("  {} {:<15}  {}  {}+{} 人",
                    c::dim("│"), room.id.to_string(), state_str, users_count, monitors_count);
            }
        }
    }

    async fn list_rooms(&self) {
        let rooms = self.state.rooms.read().await;
        if rooms.is_empty() {
            println!("  {} 当前无活跃房间", c::dim("·"));
            drop(rooms);
            return;
        }

        println!("  {} 活跃房间 ({})", c::green("◆"), rooms.len());
        println!("  {}", c::dim("  ────────────────────────────────────────────"));
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

            println!("  {} {}", c::dim("┏"), c::bold(&room.id.to_string()));
            println!("  {} 状态: {}  {}  {}  {}", c::dim("┃"), state_str, locked, cycling,
                if users_in_room.len() + monitors_in_room.len() > 0 {
                    c::cyan(&format!("{} 人在线", users_in_room.len() + monitors_in_room.len()))
                } else {
                    c::dim("空闲")
                }
            );
            if !users_in_room.is_empty() {
                println!("  {} 玩家: {}", c::dim("┃"),
                    users_in_room.iter()
                        .map(|u| format!("{}({})", c::bold(&u.name), u.id))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if !monitors_in_room.is_empty() {
                println!("  {} 旁观: {}", c::dim("┃"),
                    monitors_in_room.iter()
                        .map(|u| format!("{}({})", c::bold(&u.name), u.id))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            println!("  {}", c::dim("  ─ ─ ─ ─ ─ ─"));
        }

        drop(rooms);

        // 在线用户概要
        let users = self.state.users.read().await;
        if !users.is_empty() {
            println!();
            println!("  {} 在线用户 ({})", c::green("◆"), users.len());
            for user in users.values() {
                let in_room = if user.room.read().await.is_some() {
                    c::cyan(" 在房间中")
                } else {
                    c::dim(" 大厅")
                };
                println!("  {} {:<6}  {}{}", c::dim("│"), user.id, c::bold(&user.name), in_room);
            }
        }
    }

    async fn kick_from_room(&self, room_id: &str, target_id: &str) {
        let target: i32 = match target_id.parse() {
            Ok(id) => id,
            Err(_) => {
                println!("  {} 无效的用户ID: {}", c::red("✗"), target_id);
                return;
            }
        };

        let room = {
            let rooms = self.state.rooms.read().await;
            let rid: phira_mp_common::RoomId = match room_id.to_string().try_into() {
                Ok(id) => id,
                Err(_) => {
                    println!("  {} 无效的房间ID", c::red("✗"));
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
                let _ = room.on_user_leave(&user).await;
                info!(user = target, room = room_id, "kicked from room by admin");
                self.state.plugin_manager
                    .trigger(&PluginEvent::RoomModify {
                        user_id: target,
                        room_id: room_id.to_string(),
                        data: r#"{"action":"kicked"}"#.to_string(),
                    }).await;
                println!("  {} 用户 {} 已从房间 {} 踢出", c::green("✓"), target, room_id);
            } else {
                println!("  {} 在房间 {} 中未找到用户 {}", c::yellow("!"), room_id, target);
            }
        } else {
            println!("  {} 未找到房间 {}", c::red("✗"), room_id);
        }
    }

    async fn kick_user(&self, target_id: &str) {
        let target: i32 = match target_id.parse() {
            Ok(id) => id,
            Err(_) => {
                println!("  {} 无效的用户ID: {}", c::red("✗"), target_id);
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

            println!("  {} 用户 {} ({}) 已从服务器踢出", c::green("✓"), c::bold(&user.name), target);
        } else {
            println!("  {} 未找到用户 {}", c::red("✗"), target);
        }
    }

    async fn close_room(&self, room_id: &str) {
        let rid: phira_mp_common::RoomId = match room_id.to_string().try_into() {
            Ok(id) => id,
            Err(_) => {
                println!("  {} 无效的房间ID", c::red("✗"));
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

            println!("  {} 房间 {} 已解散", c::green("✓"), c::bold(room_id));
        } else {
            println!("  {} 未找到房间 {}", c::red("✗"), room_id);
        }
    }

    async fn broadcast(&self, message: &str) {
        let users = self.state.users.read().await;
        let msg = phira_mp_common::ServerCommand::Message(
            phira_mp_common::Message::Chat {
                user: 0,
                content: format!("[系统广播] {}", message),
            }
        );

        let mut sent = 0usize;
        for user in users.values() {
            user.try_send(msg.clone()).await;
            sent += 1;
        }
        drop(users);

        info!(sent, message = %message, "broadcast message");
        self.state.plugin_manager
            .trigger(&PluginEvent::RoomModify {
                user_id: 0,
                room_id: "*broadcast*".to_string(),
                data: format!(r#"{{"action":"broadcast","message":"{}"}}"#, message),
            }).await;

        println!("  {} 已广播给 {} 个用户", c::green("✓"), sent);
    }

    async fn status(&self) {
        let stats = {
            let users = self.state.users.read().await.len();
            let rooms = self.state.rooms.read().await.len();
            let sessions = self.state.sessions.read().await.len();
            let plugins = self.state.plugin_manager.list_plugins().await.len();
            (users, rooms, sessions, plugins)
        };
        println!();
        println!("  {} {}", c::bold("⋆"), c::bold("Phira-mp+ 服务器状态"));
        println!("  {}", c::dim("  ─────────────────────────────────────"));
        println!("  {} 版本        {}", c::dim("│"), c::cyan(env!("CARGO_PKG_VERSION")));
        println!("  {} 端口        {}", c::dim("│"), self.state.config.port);
        println!("  {} 在线用户    {}", c::dim("│"), stats.0);
        println!("  {} 活跃会话    {}", c::dim("│"), stats.2);
        println!("  {} 活跃房间    {}", c::dim("│"), stats.1);
        println!("  {} 已加载插件  {}", c::dim("│"), stats.3);
        println!("  {}", c::dim("  ─────────────────────────────────────"));
        println!();
    }

    // ── 扩展数据 ──

    async fn list_extensions(&self) {
        let user_fields = self.state.extensions.list_user_fields().await;
        let room_fields = self.state.extensions.list_room_fields().await;

        println!("  {} 用户扩展字段:", c::cyan("◆"));
        if user_fields.is_empty() {
            println!("    {} 无", c::dim("-"));
        } else {
            for f in &user_fields {
                println!("    {} {}", c::dim("·"), f);
            }
        }

        println!("  {} 房间扩展字段:", c::cyan("◆"));
        if room_fields.is_empty() {
            println!("    {} 无", c::dim("-"));
        } else {
            for f in &room_fields {
                println!("    {} {}", c::dim("·"), f);
            }
        }
    }

    async fn get_extension(&self, id: &str, key: &str) {
        if let Ok(uid) = id.parse::<i32>() {
            if let Some(val) = self.state.extensions.get_user_extra(uid, key).await {
                println!("  {} 用户 {} 的 {} = {}", c::green("◆"), uid, c::cyan(key), val);
                return;
            }
        }
        if let Some(val) = self.state.extensions.get_room_extra(id, key).await {
            println!("  {} 房间 {} 的 {} = {}", c::green("◆"), id, c::cyan(key), val);
            return;
        }
        println!("  {} 未找到扩展数据: id={}, key={}", c::yellow("!"), id, key);
    }

    /// 尝试将命令分发给插件注册的 CLI 命令
    async fn try_plugin_command(&self, command: &str, args: &[&str]) -> bool {
        let result = self.state.plugin_manager.execute_cli_command(command, args).await;
        match result {
            Some(output_lines) => {
                for line in output_lines {
                    println!("  {} {}", c::magenta("◈"), line);
                }
                true
            }
            None => false,
        }
    }
}

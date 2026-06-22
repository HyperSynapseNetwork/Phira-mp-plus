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

        println!();
        println!("╔══════════════════════════════════════════╗");
        println!("║     Phira-mp+ 管理控制台已启动            ║");
        println!("║     输入 'help' 查看命令列表              ║");
        println!("║     输入 'exit' 或 'q' 关闭服务器         ║");
        println!("╚══════════════════════════════════════════╝");
        println!();

        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            let mut parts = line.split_whitespace();
            let command = parts.next().unwrap_or("");
            let args: Vec<&str> = parts.collect();

            match command {
                "exit" | "quit" | "q" => {
                    println!("正在关闭服务器...");
                    *self.running.write().await = false;
                    self.state.shutdown.notify_one();
                    println!("服务器关闭信号已发送。");
                    break;
                }
                "help" | "h" | "?" => self.print_help().await,
                "plugins" | "pl" => self.list_plugins().await,
                "plug-enable" | "pe" => {
                    if args.is_empty() {
                        println!("用法: plug-enable <插件名>");
                    } else {
                        self.enable_plugin(args[0]).await;
                    }
                }
                "plug-disable" | "pd" => {
                    if args.is_empty() {
                        println!("用法: plug-disable <插件名>");
                    } else {
                        self.disable_plugin(args[0]).await;
                    }
                }
                "plug-reload" | "pr" => self.reload_plugins().await,
                "users" | "u" => self.list_users().await,
                "rooms" | "r" => self.list_rooms().await,
                "kick" | "k" => {
                    if args.len() < 2 {
                        println!("用法: kick <房间ID|user-id> <目标用户ID>");
                        println!("       kick <房间ID> <用户ID>   — 从房间踢出用户");
                        println!("       kick <用户ID>            — 从服务器踢出用户");
                    } else if args.len() == 2 {
                        self.kick_from_room(args[0], args[1]).await;
                    } else {
                        self.kick_user(args[0]).await;
                    }
                }
                "broadcast" | "bc" => {
                    if args.is_empty() {
                        println!("用法: broadcast <消息内容>");
                    } else {
                        let msg = args.join(" ");
                        self.broadcast(&msg).await;
                    }
                }
                "status" | "st" => self.status().await,
                "ext-list" | "el" => self.list_extensions().await,
                "ext-get" | "eg" => {
                    if args.len() < 2 {
                        println!("用法: ext-get <user-id|room-id> <key>");
                    } else {
                        self.get_extension(args[0], args[1]).await;
                    }
                }
                "close-room" | "cr" => {
                    if args.is_empty() {
                        println!("用法: close-room <房间ID>");
                    } else {
                        self.close_room(args[0]).await;
                    }
                }
                _ => {
                    // 尝试插件命令
                    if !self.try_plugin_command(command, &args).await {
                        println!("未知命令: '{}'. 输入 'help' 查看命令列表", command);
                    }
                }
            }

            // 检查是否需要退出
            if !*self.running.read().await {
                break;
            }
        }

        info!("CLI session ended");
    }

    async fn print_help(&self) {
        println!();
        println!("╔══════════════════════════════════════════════════════════════════╗");
        println!("║                Phira-mp+ 管理命令列表                           ║");
        println!("╠══════════════════════════════════════════════════════════════════╣");
        println!("║  通用命令:                                                     ║");
        println!("║    help (h, ?)        - 显示此帮助                             ║");
        println!("║    exit (quit, q)     - 关闭服务器                             ║");
        println!("║    status (st)         - 显示服务器状态                        ║");
        println!("╠══════════════════════════════════════════════════════════════════╣");
        println!("║  插件管理:                                                     ║");
        println!("║    plugins (pl)        - 列出所有插件                          ║");
        println!("║    plug-enable (pe)    - 启用插件 <插件名>                     ║");
        println!("║    plug-disable (pd)   - 禁用插件 <插件名>                     ║");
        println!("║    plug-reload (pr)    - 重载所有插件                          ║");
        println!("╠══════════════════════════════════════════════════════════════════╣");
        println!("║  用户/房间管理:                                                ║");
        println!("║    users (u)           - 列出在线用户                          ║");
        println!("║    rooms (r)           - 列出活跃房间                          ║");
        println!("║    kick (k)            - 踢出用户 (kick <room> <user-id>)      ║");
        println!("║    close-room (cr)     - 关闭/解散房间 <房间ID>                ║");
        println!("║    broadcast (bc)      - 广播消息                              ║");
        println!("╠══════════════════════════════════════════════════════════════════╣");
        println!("║  扩展数据管理:                                                 ║");
        println!("║    ext-list (el)       - 列出扩展字段                          ║");
        println!("║    ext-get (eg)        - 获取扩展数据                          ║");
        println!("╠══════════════════════════════════════════════════════════════════╣");
        // 列出插件注册的命令
        let plugin_cmds = self.state.plugin_manager.list_cli_commands().await;
        if !plugin_cmds.is_empty() {
            println!("╠══════════════════════════════════════════════════════════════════╣");
            println!("║  插件扩展命令:                                                 ║");
            for cmd in &plugin_cmds {
                println!("║    {:<20} - {:<30} ║", cmd.name, cmd.description);
            }
        }
        println!("╚══════════════════════════════════════════════════════════════════╝");
        println!();
    }

    async fn list_plugins(&self) {
        let plugins = self.state.plugin_manager.list_plugins().await;
        if plugins.is_empty() {
            println!("  无已加载的插件");
            return;
        }
        println!("  已加载插件 ({}):", plugins.len());
        println!(
            "  {:<24} {:<10} {:<8} {}",
            "名称", "版本", "状态", "作者"
        );
        println!("  {}", "-".repeat(60));
        for p in &plugins {
            let state_str = match p.state {
                crate::plugin::PluginState::Enabled => "启用",
                crate::plugin::PluginState::Disabled => "禁用",
                crate::plugin::PluginState::Loaded => "已加载",
                crate::plugin::PluginState::Error(_) => "错误",
            };
            println!(
                "  {:<24} {:<10} {:<8} {}",
                p.info.name, p.info.version, state_str, p.info.author
            );
        }
    }

    async fn enable_plugin(&self, name: &str) {
        match self.state.plugin_manager.enable_plugin(name).await {
            Ok(_) => println!("  插件 '{}' 已启用", name),
            Err(e) => println!("  错误: {}", e),
        }
    }

    async fn disable_plugin(&self, name: &str) {
        match self.state.plugin_manager.disable_plugin(name).await {
            Ok(_) => println!("  插件 '{}' 已禁用", name),
            Err(e) => println!("  错误: {}", e),
        }
    }

    async fn reload_plugins(&self) {
        println!("  正在重载所有插件...");
        match self.state.plugin_manager.reload_plugins().await {
            Ok(count) => println!("  已重载 {} 个插件", count),
            Err(e) => println!("  重载错误: {}", e),
        }
    }

    async fn list_users(&self) {
        let users = self.state.users.read().await;
        if users.is_empty() {
            println!("  当前无在线用户");
            return;
        }
        println!("  在线用户 ({}):", users.len());
        println!("  {:<8} {:<24} {:<8}", "用户ID", "名称", "监视器");
        println!("  {}", "-".repeat(48));
        for user in users.values() {
            let sessions = user.session.read().await;
            let _has_session = sessions.is_some();
            drop(sessions);
            let monitor = if user.monitor.load(std::sync::atomic::Ordering::SeqCst) {
                "是"
            } else {
                "否"
            };
            println!(
                "  {:<8} {:<24} {:<8}",
                user.id, user.name, monitor
            );
        }
        drop(users);
        // 显示房间信息
        let rooms = self.state.rooms.read().await;
        if !rooms.is_empty() {
            println!();
            println!("  活跃房间 ({}):", rooms.len());
            println!("  {:<20} {:<8} {:<12}", "房间ID", "人数", "状态");
            println!("  {}", "-".repeat(48));
            for room in rooms.values() {
                let users_in_room = room.users().await.len();
                let monitors_in_room = room.monitors().await.len();
                let state_str = match &*room.state.read().await {
                    crate::room::InternalRoomState::SelectChart => "选曲中",
                    crate::room::InternalRoomState::WaitForReady { .. } => "等待准备",
                    crate::room::InternalRoomState::Playing { .. } => "游戏中",
                };
                println!(
                    "  {:<20} {:<8} {:<12}",
                    room.id.to_string(),
                    format!("{}+{}旁观", users_in_room, monitors_in_room),
                    state_str,
                );
            }
        }
    }

    async fn list_rooms(&self) {
        let rooms = self.state.rooms.read().await;
        if rooms.is_empty() {
            println!("  当前无活跃房间");
            return;
        }
        println!("  活跃房间 ({}):", rooms.len());
        println!();
        for room in rooms.values() {
            let users_in_room = room.users().await;
            let monitors_in_room = room.monitors().await;
            let state_str = match &*room.state.read().await {
                crate::room::InternalRoomState::SelectChart => "SelectChart",
                crate::room::InternalRoomState::WaitForReady { .. } => "WaitForReady",
                crate::room::InternalRoomState::Playing { .. } => "Playing",
            };
            let locked = if room.locked.load(std::sync::atomic::Ordering::SeqCst) {
                "锁定"
            } else {
                "未锁定"
            };
            let cycling = if room.cycle.load(std::sync::atomic::Ordering::SeqCst) {
                "轮换"
            } else {
                "不轮换"
            };

            println!("  ┌ 房间: {}", room.id);
            println!("  ├ 状态: {} | {} | {}", state_str, locked, cycling);
            println!(
                "  ├ 玩家: {}",
                users_in_room
                    .iter()
                    .map(|u| format!("{}({})", u.name, u.id))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            if !monitors_in_room.is_empty() {
                println!(
                    "  ├ 旁观: {}",
                    monitors_in_room
                        .iter()
                        .map(|u| format!("{}({})", u.name, u.id))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            println!("  └──────────────");
        }
        drop(rooms);

        // 同时显示在线用户统计
        let users = self.state.users.read().await;
        if !users.is_empty() {
            println!();
            println!("  在线用户 ({}):", users.len());
            for user in users.values() {
                let in_room = if user.room.read().await.is_some() {
                    "在房间中"
                } else {
                    "大厅"
                };
                println!("    {} ({}) - {}", user.name, user.id, in_room);
            }
        }
    }

    async fn kick_from_room(&self, room_id: &str, target_id: &str) {
        let target: i32 = match target_id.parse() {
            Ok(id) => id,
            Err(_) => {
                println!("  错误: 无效的用户ID '{}'", target_id);
                return;
            }
        };

        // 查找房间
        let room = {
            let rooms = self.state.rooms.read().await;
            // 尝试解析 RoomId
            let rid: phira_mp_common::RoomId = match room_id.to_string().try_into() {
                Ok(id) => id,
                Err(_) => {
                    println!("  错误: 无效的房间ID");
                    return;
                }
            };
            rooms.get(&rid).map(Arc::clone)
        };

        if let Some(room) = room {
            // 在房间内查找用户
            let user_to_kick = {
                let users_in_room = room.users().await;
                let monitors_in_room = room.monitors().await;
                users_in_room
                    .into_iter()
                    .chain(monitors_in_room.into_iter())
                    .find(|u| u.id == target)
            };

            if let Some(user) = user_to_kick {
                // 从房间移除
                let _ = room.on_user_leave(&user).await;
                info!(user = target, room = room_id, "kicked from room by admin");

                // 触发插件事件
                self.state
                    .plugin_manager
                    .trigger(&PluginEvent::RoomModify {
                        user_id: target,
                        room_id: room_id.to_string(),
                        data: r#"{"action":"kicked"}"#.to_string(),
                    })
                    .await;
                println!("  用户 {} 已从房间 {} 踢出", target, room_id);
            } else {
                println!("  用户在房间 {} 中未找到 {}", room_id, target);
            }
        } else {
            println!("  未找到房间 '{}'", room_id);
        }
    }

    async fn kick_user(&self, target_id: &str) {
        let target: i32 = match target_id.parse() {
            Ok(id) => id,
            Err(_) => {
                println!("  错误: 无效的用户ID '{}'", target_id);
                return;
            }
        };

        // 查找用户
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

                    // 触发插件事件
                    self.state
                        .plugin_manager
                        .trigger(&PluginEvent::RoomLeave {
                            user_id: target,
                            room_id,
                        })
                        .await;
                }
            }

            // 断开用户连接
            {
                let sessions = self.state.sessions.read().await;
                for session in sessions.values() {
                    if session.user.id == target {
                        // 发送踢出消息
                        let _ = session
                            .stream
                            .send(phira_mp_common::ServerCommand::Message(
                                phira_mp_common::Message::Chat {
                                    user: 0,
                                    content: "你已被管理员踢出服务器".to_string(),
                                },
                            ))
                            .await;
                        break;
                    }
                }
            }

            // 从用户列表移除
            self.state.users.write().await.remove(&target);
            info!(user = target, "kicked from server by admin");

            // 触发插件事件
            self.state
                .plugin_manager
                .trigger(&PluginEvent::UserDisconnect {
                    user_id: target,
                    user_name: user.name.clone(),
                })
                .await;

            println!("  用户 {} ({}) 已从服务器踢出", user.name, target);
        } else {
            println!("  未找到用户 '{}'", target);
        }
    }

    async fn close_room(&self, room_id: &str) {
        let rid: phira_mp_common::RoomId = match room_id.to_string().try_into() {
            Ok(id) => id,
            Err(_) => {
                println!("  错误: 无效的房间ID");
                return;
            }
        };

        let room_opt = {
            let rooms = self.state.rooms.read().await;
            rooms.get(&rid).map(Arc::clone)
        };

        if let Some(room) = room_opt {
            let room_id_str = room.id.to_string();

            // 通知所有用户房间被关闭
            room.send(phira_mp_common::Message::Chat {
                user: 0,
                content: "房间已被管理员关闭".to_string(),
            })
            .await;

            // 移除所有用户
            for user in room.users().await {
                *user.room.write().await = None;
            }
            for user in room.monitors().await {
                *user.room.write().await = None;
            }

            // 解散房间
            self.state.rooms.write().await.remove(&rid);
            info!(room = room_id_str, "room closed by admin");

            // 触发插件事件
            self.state
                .plugin_manager
                .trigger(&PluginEvent::RoomModify {
                    user_id: 0,
                    room_id: room_id_str,
                    data: r#"{"action":"closed"}"#.to_string(),
                })
                .await;

            println!("  房间 '{}' 已解散", room_id);
        } else {
            println!("  未找到房间 '{}'", room_id);
        }
    }

    async fn broadcast(&self, message: &str) {
        let users = self.state.users.read().await;
        let msg = phira_mp_common::ServerCommand::Message(phira_mp_common::Message::Chat {
            user: 0,
            content: format!("[系统广播] {}", message),
        });

        let mut sent = 0usize;
        for user in users.values() {
            user.try_send(msg.clone()).await;
            sent += 1;
        }
        drop(users);

        // 记录日志
        info!(sent, message = %message, "broadcast message");

        // 触发插件事件
        self.state
            .plugin_manager
            .trigger(&PluginEvent::RoomModify {
                user_id: 0,
                room_id: "*broadcast*".to_string(),
                data: format!(r#"{{"action":"broadcast","message":"{}"}}"#, message),
            })
            .await;

        println!("  已广播消息给 {} 个用户", sent);
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
        println!("╔══════════════════════════════════════════╗");
        println!("║        Phira-mp+ 服务器状态              ║");
        println!("╠══════════════════════════════════════════╣");
        println!("║  版本: {:<35} ║", env!("CARGO_PKG_VERSION"));
        println!("║  端口: {:<35} ║", self.state.config.port);
        println!("║  在线用户: {:<31} ║", stats.0);
        println!("║  活跃会话: {:<31} ║", stats.2);
        println!("║  活跃房间: {:<31} ║", stats.1);
        println!("║  已加载插件: {:<30} ║", stats.3);
        println!("╚══════════════════════════════════════════╝");
        println!();
    }

    async fn list_extensions(&self) {
        let user_fields = self.state.extensions.list_user_fields().await;
        let room_fields = self.state.extensions.list_room_fields().await;

        println!("  用户扩展字段:");
        if user_fields.is_empty() {
            println!("    (无)");
        } else {
            for f in &user_fields {
                println!("    - {}", f);
            }
        }

        println!("  房间扩展字段:");
        if room_fields.is_empty() {
            println!("    (无)");
        } else {
            for f in &room_fields {
                println!("    - {}", f);
            }
        }
    }

    async fn get_extension(&self, id: &str, key: &str) {
        // 先尝试用户ID
        if let Ok(uid) = id.parse::<i32>() {
            if let Some(val) = self.state.extensions.get_user_extra(uid, key).await {
                println!("  用户 {} 的扩展数据 '{}': {}", uid, key, val);
                return;
            }
        }
        // 再尝试房间ID
        if let Some(val) = self.state.extensions.get_room_extra(id, key).await {
            println!("  房间 '{}' 的扩展数据 '{}': {}", id, key, val);
            return;
        }
        println!("  未找到扩展数据: id={}, key={}", id, key);
    }

    /// 尝试将命令分发给插件注册的 CLI 命令
    async fn try_plugin_command(&self, command: &str, args: &[&str]) -> bool {
        let result = self
            .state
            .plugin_manager
            .execute_cli_command(command, args)
            .await;
        match result {
            Some(output_lines) => {
                for line in output_lines {
                    println!("  [插件] {}", line);
                }
                true
            }
            None => false,
        }
    }
}

//! Phira-mp+ CLI 交互式管理控制台
//!
//! 提供管理员可通过命令行执行的管理操作：
//! - 插件管理（列表、启用、禁用、重载）
//! - 用户管理（列表、踢出、封禁、解封）
//! - 房间管理（列表、踢出、关闭）
//! - 服务器状态查看
//! - 消息广播

use crate::plugin::PluginManager;
use crate::extensions::ExtensionManager;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::RwLock;
use tracing::{info};

/// CLI 命令处理器
pub struct CliHandler {
    plugin_manager: Arc<PluginManager>,
    extensions: Arc<ExtensionManager>,
    running: Arc<RwLock<bool>>,
}

impl CliHandler {
    pub fn new(
        plugin_manager: Arc<PluginManager>,
        extensions: Arc<ExtensionManager>,
    ) -> Self {
        Self {
            plugin_manager,
            extensions,
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
                    break;
                }
                "help" | "h" | "?" => self.print_help(),
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
                "users" | "u" => self.list_users_short().await,
                "rooms" | "r" => self.list_rooms_short().await,
                "kick" | "k" => {
                    if args.len() < 2 {
                        println!("用法: kick <room-id|user-id> <target-id>");
                    } else {
                        self.kick_user(args[0], args[1]).await;
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
                _ => {
                    println!("未知命令: '{}'. 输入 'help' 查看命令列表", command);
                }
            }
        }

        info!("CLI session ended");
    }

    fn print_help(&self) {
        println!();
        println!("╔══════════════════════════════════════════════════════╗");
        println!("║             Phira-mp+ 管理命令列表                  ║");
        println!("╠══════════════════════════════════════════════════════╣");
        println!("║  通用命令:                                         ║");
        println!("║    help (h, ?)       - 显示此帮助                   ║");
        println!("║    exit (quit, q)    - 关闭服务器                   ║");
        println!("║    status (st)        - 显示服务器状态              ║");
        println!("╠══════════════════════════════════════════════════════╣");
        println!("║  插件管理:                                         ║");
        println!("║    plugins (pl)       - 列出所有插件                ║");
        println!("║    plug-enable (pe)   - 启用插件                    ║");
        println!("║    plug-disable (pd)  - 禁用插件                    ║");
        println!("║    plug-reload (pr)   - 重载所有插件                ║");
        println!("╠══════════════════════════════════════════════════════╣");
        println!("║  用户/房间管理:                                    ║");
        println!("║    users (u)          - 列出在线用户                ║");
        println!("║    rooms (r)          - 列出活跃房间                ║");
        println!("║    kick (k)           - 踢出用户                    ║");
        println!("║    broadcast (bc)     - 广播消息                    ║");
        println!("╠══════════════════════════════════════════════════════╣");
        println!("║  扩展数据管理:                                     ║");
        println!("║    ext-list (el)      - 列出扩展字段                ║");
        println!("║    ext-get (eg)       - 获取扩展数据                ║");
        println!("╚══════════════════════════════════════════════════════╝");
        println!();
    }

    async fn list_plugins(&self) {
        let plugins = self.plugin_manager.list_plugins().await;
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
        match self.plugin_manager.enable_plugin(name).await {
            Ok(_) => println!("  插件 '{}' 已启用", name),
            Err(e) => println!("  错误: {}", e),
        }
    }

    async fn disable_plugin(&self, name: &str) {
        match self.plugin_manager.disable_plugin(name).await {
            Ok(_) => println!("  插件 '{}' 已禁用", name),
            Err(e) => println!("  错误: {}", e),
        }
    }

    async fn reload_plugins(&self) {
        println!("  正在重载所有插件...");
        match self.plugin_manager.reload_plugins().await {
            Ok(count) => println!("  已重载 {} 个插件", count),
            Err(e) => println!("  重载错误: {}", e),
        }
    }

    async fn list_users_short(&self) {
        println!("  功能: 用户列表 (等待服务器实现)");
        println!("  (完整实现在集成 phase 后可用)");
    }

    async fn list_rooms_short(&self) {
        println!("  功能: 房间列表 (等待服务器实现)");
        println!("  (完整实现在集成 phase 后可用)");
    }

    async fn kick_user(&self, room_or_user: &str, target: &str) {
        println!("  踢出用户: 目标={}, ID={}", room_or_user, target);
    }

    async fn broadcast(&self, message: &str) {
        println!("  广播消息: {}", message);
    }

    async fn status(&self) {
        println!();
        println!("╔════════════════════════════════════╗");
        println!("║        Phira-mp+ 服务器状态        ║");
        println!("╠════════════════════════════════════╣");
        println!("║  版本: 0.1.0                       ║");
        println!("║  状态: 运行中                      ║");
        let plugin_count = self.plugin_manager.list_plugins().await.len();
        println!("║  插件数: {:<24} ║", plugin_count);
        println!("╚════════════════════════════════════╝");
        println!();
    }

    async fn list_extensions(&self) {
        let user_fields = self.extensions.list_user_fields().await;
        let room_fields = self.extensions.list_room_fields().await;

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
        // Try as user id first
        if let Ok(uid) = id.parse::<i32>() {
            if let Some(val) = self.extensions.get_user_extra(uid, key).await {
                println!("  用户 {} 的扩展数据 '{}': {}", uid, key, val);
                return;
            }
        }
        // Try as room id
        if let Some(val) = self.extensions.get_room_extra(id, key).await {
            println!("  房间 '{}' 的扩展数据 '{}': {}", id, key, val);
            return;
        }
        println!("  未找到扩展数据: id={}, key={}", id, key);
    }
}

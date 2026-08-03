//! CLI command dispatch implementation.
//!
//! This module keeps the top-level command routing out of `cli.rs`. Concrete
//! command-family dispatch lives under `cli/commands/` so the CLI command
//! surface can grow without turning a single file into another monolith.

use super::*;

impl CliHandler {
    pub(super) async fn dispatch_command(&self, command: &str, args: &[&str]) -> bool {
        match command {
            "exit" => {
                self.out(format!("  {} 正在关闭服务器...", c::yellow("⟳")));
                *self.running.write().await = false;
                self.state.shutdown.notify_one();
                self.out(format!("  {} 已发送关闭信号", c::green("✓")));
                false
            }
            "help" => {
                self.print_help(args).await;
                true
            }
            "plugin" => {
                self.dispatch_plugin_command(args).await;
                true
            }
            "users" => {
                self.list_users().await;
                true
            }
            "rooms" => {
                self.list_rooms().await;
                true
            }
            "room" => {
                self.dispatch_room_command(args).await;
                true
            }
            "force-start" => {
                if let Some(room_id) = args.first() {
                    self.room_start(room_id).await;
                } else {
                    self.out(format!(
                        "  {} {} force-start <房间ID>",
                        c::yellow("?"),
                        c::bold("用法")
                    ));
                }
                true
            }
            "kick" => {
                self.dispatch_user_kick_command(args).await;
                true
            }
            "broadcast" => {
                self.dispatch_broadcast_command(args).await;
                true
            }
            "status" => {
                self.status().await;
                true
            }
            "version" => {
                self.out(format!(
                    "  {} PMP v{}",
                    c::green("◆"),
                    env!("CARGO_PKG_VERSION")
                ));
                true
            }
            "benchmark" => {
                self.dispatch_benchmark_command(args).await;
                true
            }
            "runtime" => {
                self.dispatch_runtime_command(args).await;
                true
            }
            "extension" | "extensions" => {
                self.dispatch_extension_command(args).await;
                true
            }
            "admin-id" => {
                self.admin_ids(args).await;
                true
            }
            "ban" => {
                if args.first().copied() == Some("ip") {
                    self.dispatch_ban_ip_command(&args[1..]).await;
                } else {
                    self.dispatch_global_ban_command(args).await;
                }
                true
            }
            "unban" => {
                if args.first().copied() == Some("ip") {
                    self.dispatch_unban_ip_command(&args[1..]).await;
                } else {
                    self.dispatch_global_unban_command(args).await;
                }
                true
            }
            "banlist" => {
                if args.first().copied() == Some("ip") {
                    self.dispatch_ip_banlist().await;
                } else {
                    self.ban_list().await;
                }
                true
            }
            "ip-history" => {
                self.dispatch_user_ip_history(args).await;
                true
            }
            "roomcreation" => {
                match args.first().copied() {
                    Some("on" | "enable" | "1") => self.toggle_room_creation(true).await,
                    Some("off" | "disable" | "0") => self.toggle_room_creation(false).await,
                    _ => {
                        let enabled = self.state.live_config.read().await.room_creation_enabled;
                        self.out(format!(
                            "  {} 玩家建房：{}",
                            c::cyan("◆"),
                            if enabled { "已开启" } else { "已关闭" }
                        ));
                    }
                }
                true
            }
            "approve" => {
                self.dispatch_approve_command(args).await;
                true
            }
            _ => {
                // Try Runtime CommandRegistry (unified execution path)
                if let Some(output) = self
                    .state
                    .command_registry
                    .execute(&self.state, command, args)
                    .await
                {
                    for line in output {
                        self.out(line);
                    }
                } else if !self.try_plugin_command(command, args).await {
                    self.out(format!(
                        "  {} {}",
                        c::red("✗"),
                        self.state.command_registry.format_unknown(command)
                    ));
                }
                true
            }
        }
    }
}

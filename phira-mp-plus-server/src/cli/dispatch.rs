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
            "benchmark" => {
                self.dispatch_benchmark_command(args).await;
                true
            }
            "simulation" => {
                self.dispatch_simulation_command(args).await;
                true
            }
            "runtime" => {
                self.dispatch_runtime_command(args).await;
                true
            }
            "benchmark-bind" => {
                self.out(format!(
                    "  {} benchmark-bind 已废弃。请使用 benchmark token bind <token>。",
                    c::yellow("!")
                ));
                true
            }
            "benchmark-cleanup" => {
                self.out(format!(
                    "  {} benchmark-cleanup 已废弃。请使用 simulation cleanup。",
                    c::yellow("!")
                ));
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
            "ext-list" => {
                self.out(format!(
                    "  {} ext-list 已废弃。请使用 extension list。",
                    c::yellow("!")
                ));
                true
            }
            "ext-get" => {
                self.out(format!(
                    "  {} ext-get 已废弃。请使用 extension get <target> <key>。",
                    c::yellow("!")
                ));
                true
            }
            "ban" => {
                self.dispatch_global_ban_command(args).await;
                true
            }
            "unban" => {
                self.dispatch_global_unban_command(args).await;
                true
            }
            "banlist" => {
                self.ban_list().await;
                true
            }
            _ => {
                // Try CommandRegistry execute first (Runtime v2 unified execution path)
                if let Some(output) =
                    self.state
                        .command_registry
                        .execute(&self.state, command, args)
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

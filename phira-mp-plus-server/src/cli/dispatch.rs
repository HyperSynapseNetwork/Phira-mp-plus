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
                // Delegate to AdminCommand registry (typed, shared across CLI/TUI/Web)
                if let Some(cmd) = self.state.admin_commands.find("exit") {
                    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
                    let result = cmd.execute(args, Arc::clone(&self.state)).await;
                    for line in result.message.lines() {
                        self.out(format!("  {line}"));
                    }
                }
                *self.running.write().await = false;
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
            "extension" | "extensions" => {
                self.dispatch_extension_command(args).await;
                true
            }
            "admin-id" => {
                self.admin_ids(args).await;
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
                // Try AdminCommand registry first (typed, shared across CLI/TUI/Web)
                if let Some(cmd) = self.state.admin_commands.find(command) {
                    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
                    let result = cmd.execute(args, Arc::clone(&self.state)).await;
                    for line in result.message.lines() {
                        self.out(format!("  {}", line));
                    }
                // Fall back to Runtime v2 CommandRegistry (legacy commands)
                } else if let Some(output) =
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

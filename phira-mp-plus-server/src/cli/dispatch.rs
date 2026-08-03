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
            _ => {
                // 统一执行路径：registry handler → plugin CLI → unknown
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

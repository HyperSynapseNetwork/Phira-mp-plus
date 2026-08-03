//! Runtime CLI diagnostics and control commands.
//!
//! This module is intentionally split by diagnostic domain so `runtime` does
//! not become the next CLI junk drawer after `cli.rs` was reduced.

mod commands;
mod events;
mod latency;
mod persistence;
mod phira;
mod schema;
mod status;

use super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn dispatch_runtime_command(&self, args: &[&str]) {
        self.runtime_command(args).await;
    }

    async fn runtime_command(&self, args: &[&str]) {
        let sub = args.first().copied().unwrap_or("status");
        match sub {
            "status" | "" => self.print_runtime_status().await,
            "phira" => self.print_runtime_phira(),
            "commands" => self.print_runtime_commands(),
            "events" => self.print_runtime_events(),
            "schema" => self.print_runtime_schema().await,
            "persistence" => self.print_runtime_persistence().await,
            "latency" => self.print_runtime_latency().await,
            _ => {
                self.out(format!(
                    "  {} 未知 runtime 子命令: {}",
                    c::red("✗"),
                    c::yellow(sub)
                ));
                self.out(format!(
                    "  {} 可用: runtime status | phira | commands | events | persistence | schema | latency",
                    c::dim("▸")
                ));
            }
        }
    }
}

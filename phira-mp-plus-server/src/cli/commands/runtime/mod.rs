//! Runtime v2 CLI diagnostics and control commands.
//!
//! This module is intentionally split by diagnostic domain so `runtime` does
//! not become the next CLI junk drawer after `cli.rs` was reduced.

mod actors;
mod budget;
mod commands;
mod cutover;
mod events;
mod persistence;
mod phira;
mod roadmap;
mod rooms;
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
            "roadmap" => self.print_runtime_roadmap(),
            "budget" => self.print_runtime_budget(),
            "phira" => self.print_runtime_phira(),
            "commands" => self.print_runtime_commands(),
            "events" => self.print_runtime_events(),
            "rooms" => self.print_runtime_rooms(),
            "actors" => self.print_runtime_actors().await,
            "cutover" => self.runtime_cutover(args).await,
            "schema" => self.print_runtime_schema().await,
            "persistence" => self.print_runtime_persistence().await,
            _ => {
                self.out(format!("  {} 未知 runtime 子命令: {}", c::red("✗"), c::yellow(sub)));
                self.out(format!("  {} 可用: runtime status | roadmap | budget | phira | commands | events | persistence | schema | cutover | actors | rooms", c::dim("▸")));
            }
        }
    }
}

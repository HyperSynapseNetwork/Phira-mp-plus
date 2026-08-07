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
    /// D3 收敛：`runtime` 单命令一次打印全部诊断分区。
    pub(in crate::cli) async fn print_runtime_all(&self) {
        self.print_runtime_status().await;
        self.print_runtime_commands();
        self.print_runtime_phira();
        self.print_runtime_events();
        self.print_runtime_schema().await;
        self.print_runtime_persistence().await;
        self.print_runtime_latency().await;
    }
}

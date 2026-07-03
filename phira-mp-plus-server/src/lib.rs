//! Phira-mp+ - 增强版 Phira 多人游戏服务端
//!
//! 基于 Phira-mp 二次开发，通过受控的 WASM JSON ABI、管理控制台和扩展 API
//! 提供可部署、可观察、可扩展的多人游戏服务。

pub mod actor_runtime;
pub mod ban;
pub mod benchmark_report;
pub mod benchmark_snapshot;
pub mod cli;
pub mod cli_tui;
pub mod command_registry;
pub mod db;
pub mod event_bus;
pub mod extensions;
pub mod internal_hooks;
pub mod l10n;
pub mod logging;
pub mod persistence;
pub mod persistence_worker;
pub mod phira_client;
pub mod plugin;
pub mod plugin_abi;
pub mod plugin_http;
pub mod rate_limiter;
pub mod room;
pub mod room_actor;
pub mod round_store;
pub mod runtime_diagnostics;
pub mod runtime_plan;
pub mod server;
pub mod server_query;
pub mod session;
mod session_auth;
pub mod session_dispatch;
pub mod session_room;
mod session_telemetry;
pub mod simulation;
pub mod simulation_realistic;
pub mod telemetry;
pub mod telemetry_batcher;
pub use session_room::decode_admin_room_command;
pub mod terminal;
#[cfg(feature = "plugin-system")]
pub mod wasm_host;

pub use l10n::*;
pub use room::*;
pub use server::*;
pub use session::*;

//! Phira-mp+ - 增强版 Phira 多人游戏服务端
//!
//! 基于 Phira-mp 二次开发，通过受控的 WIT WASM 插件 ABI、管理控制台和扩展 API
//! 提供可部署、可观察、可扩展的多人游戏服务。

// Clippy allows — kept only for unavoidable architectural reasons.
// 函数签名参数过多（回调/Handler 聚合），重构成本高于收益。
#![allow(clippy::too_many_arguments)]
// 复杂类型别名（回调嵌套/泛型约束），拆分后反而降低可读性。
#![allow(clippy::type_complexity)]
// 枚举变体大小悬殊（WASM blob 等大数据结构），分离引入额外间接层。
#![allow(clippy::large_enum_variant)]
// `#[cfg(test)]` 模块位于文件中间，受模块声明顺序约束无法移动。
#![allow(clippy::items_after_test_module)]

// backup module not part of server runtime — see src/bin/pmp-admin.rs
pub mod ban;
pub mod benchmark;
pub mod benchmark_report;
pub mod benchmark_snapshot;
pub mod cli;
pub mod cli_tui;
pub mod crypto;
pub mod command_registry;
pub mod db;
pub mod error;
pub mod event_bus;
pub mod extensions;
pub mod plugin_tcp;
pub mod internal_hooks;
pub mod l10n;
pub mod logging;
pub mod persistence;
pub mod phira_client;
pub mod play_history;
pub mod plugin;
pub mod plugin_abi;
pub mod plugin_http;
pub mod trusted_forwarded_http;
pub mod rate_limiter;
pub mod room;
pub mod room_actor;
pub mod round_store;
pub mod runtime_diagnostics;
pub mod server;
pub mod server_query;
pub mod session;
pub(crate) mod session_actor;
mod session_auth;
pub mod session_dispatch;
pub mod session_permissions;
pub mod session_room;
mod session_lifecycle;
mod session_telemetry;
pub mod simulation;
pub mod supervisor_actor;
pub use session_room::decode_admin_room_command;
pub mod terminal;
#[cfg(feature = "plugin-system")]
pub mod wasm_host;
pub mod wasm_host_helpers;
#[cfg(feature = "plugin-system")]
pub mod wit_host;

pub use l10n::*;
pub use room::*;
pub use server::*;
pub use session::*;

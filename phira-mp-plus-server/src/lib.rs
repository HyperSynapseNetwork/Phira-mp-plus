//! Phira-mp+ - 增强版 Phira 多人游戏服务端
//!
//! 基于 Phira-mp 二次开发，通过受控的 WIT WASM 插件 ABI、管理控制台和扩展 API
//! 提供可部署、可观察、可扩展的多人游戏服务。

// Clippy allows — kept only for unavoidable architectural reasons.
// 函数签名参数过多（回调/Handler 聚合），重构成本高于收益。
#![allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::large_enum_variant,
    clippy::items_after_test_module,
    // 以下 lint 已有大量违规，暂全局允许待分批修复
    clippy::derivable_impls,
    clippy::collapsible_match,
    clippy::redundant_closure,
    clippy::field_reassign_with_default,
    clippy::unnecessary_sort_by,
    clippy::manual_checked_ops,
    clippy::while_let_on_iterator,
    clippy::unnecessary_map_or,
    clippy::vec_init_then_push,
    clippy::redundant_async_block,
    clippy::explicit_auto_deref,
    clippy::useless_conversion,
    clippy::clone_on_copy,
    clippy::collapsible_if,
    clippy::iter_kv_map,
    clippy::get_first,
    clippy::assertions_on_constants,
    clippy::manual_ok_err,
    clippy::manual_map,
    clippy::manual_is_multiple_of,
    clippy::manual_try_fold,
    clippy::useless_vec,
)]

// backup module not part of server runtime — see src/bin/pmp-admin.rs
pub mod auto_update;
pub mod ban;
pub mod benchmark;
pub mod cli;
pub(crate) mod official_client_compat;
pub mod cli_tui;
pub mod cli_status;
pub mod crypto;
pub mod command_registry;
pub mod db;
pub mod error;
pub mod event_bus;
pub mod extensions;
pub mod plugin_tcp;
pub mod internal_hooks;
pub mod history;
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
pub mod server_instance;
pub mod server_query;
pub mod session;
pub(crate) mod session_actor;
mod session_auth;
pub mod session_dispatch;
pub mod session_permissions;
pub mod session_room;
#[cfg(unix)]
pub mod openuds;
mod session_lifecycle;
mod session_telemetry;
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

//! Phira-mp+ - 增强版 Phira 多人游戏服务端
//!
//! 基于 Phira-mp 二次开发，通过受控的 WIT WASM 插件 ABI、管理控制台和扩展 API
//! 提供可部署、可观察、可扩展的多人游戏服务。

// Clippy allows — each group lists a rationale.
// REGENERATED from old lib.rs (D agent removed without finishing cleanup).
// TODO: remove each allow one by one after fixing the underlying code.
#![allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::large_enum_variant,
    clippy::items_after_test_module,
    clippy::new_without_default,
    clippy::vec_init_then_push,
    clippy::assertions_on_constants,
    clippy::derivable_impls,
    clippy::redundant_closure,
    clippy::useless_format,
    clippy::clone_on_copy,
    clippy::unnecessary_sort_by,
    clippy::field_reassign_with_default,
    clippy::explicit_auto_deref,
    clippy::get_first,
    clippy::unnecessary_map_or,
    clippy::io_other_error,
    clippy::manual_ok_err,
    clippy::collapsible_match,
    clippy::manual_map,
    clippy::collapsible_str_replace,
    clippy::redundant_async_block,
    clippy::manual_try_fold,
    clippy::while_let_on_iterator,
)]

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

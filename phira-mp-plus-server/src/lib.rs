//! Phira-mp+ - 增强版 Phira 多人游戏服务端
//!
//! 基于 Phira-mp 二次开发，通过受控的 WIT WASM 插件 ABI、管理控制台和扩展 API
//! 提供可部署、可观察、可扩展的多人游戏服务。
// Clippy allows — each group lists a rationale.
#![allow(
    clippy::too_many_arguments,        // structural in game protocol handlers
    clippy::type_complexity,           // plugin/actor trait signatures
    clippy::large_enum_variant,        // WAL/worker message enums (wired correctly)
    clippy::items_after_test_module,   // test-adjacent helper functions
    clippy::new_without_default,       // builder-style constructors
    clippy::vec_init_then_push,        // readability preference
    clippy::assertions_on_constants,   // runtime guard, not logic error
    clippy::derivable_impls,           // explicit default intent
    clippy::redundant_closure,         // minor readability, deprecated in modern Rust
    clippy::useless_format,            // style preference
    clippy::clone_on_copy,             // Copy types, zero cost
    clippy::unnecessary_sort_by,       // sort_by(Reverse) vs sort_by_key
    clippy::field_reassign_with_default, // test builder pattern
    clippy::explicit_auto_deref,       // deref visibility
    clippy::get_first,                 // args.get(0) pattern
    clippy::unnecessary_map_or,        // map_or vs is_none_or
    clippy::io_other_error,            // stable compat shim
    clippy::manual_ok_err,             // ok()/map() preference
    clippy::collapsible_match,         // match nesting
    clippy::manual_map,                // if let Some → map
    clippy::collapsible_str_replace,   // char array vs chain
    clippy::redundant_async_block,     // Box::pin wrapper
    clippy::manual_try_fold,           // fold on Try types
    clippy::while_let_on_iterator,     // while let vs for loop
    clippy::useless_conversion,        // into_iter on IntoIterator
    clippy::manual_checked_ops,        // if-guarded division
    clippy::useless_vec,               // vec![] vs array
    clippy::manual_is_multiple_of,     // % vs is_multiple_of
    clippy::iter_kv_map,               // .iter().map() vs .values().map()
)]

pub mod actor_runtime;
pub mod backup;
pub mod ban;
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
pub mod federation;
pub(crate) mod idle;
pub mod internal_hooks;
pub mod l10n;
pub mod logging;
pub mod persistence;
pub mod persistence_worker;
pub mod phira_client;
pub mod play_history;
pub mod plugin;
pub mod plugin_abi;
pub mod plugin_http;
pub mod proxy_protocol;
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
mod session_telemetry;
pub mod simulation;
pub mod simulation_realistic;
pub mod supervisor_actor;
pub mod telemetry;
pub mod telemetry_batcher;
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

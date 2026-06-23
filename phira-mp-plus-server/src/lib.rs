//! Phira-mp+ - 增强版 Phira 多人游戏服务端
//!
//! 基于 Phira-mp 二次开发，通过完善的 WASM 插件系统与基于 WIT 实现的 API 系统
//! 使其获得了强大的拓展性，同时得益于 WASM 和 Rust，兼具高性能与高稳定性。

pub mod ban;
pub mod cli;
pub mod cli_tui;
pub mod extensions;
pub mod l10n;
pub mod plugin;
pub mod plugin_http;
pub mod rate_limiter;
pub mod round_store;
#[cfg(feature = "plugin-system")]
pub mod wasm_host;
pub mod room;
pub mod server;
pub mod session;

pub use l10n::*;
pub use room::*;
pub use server::*;
pub use session::*;

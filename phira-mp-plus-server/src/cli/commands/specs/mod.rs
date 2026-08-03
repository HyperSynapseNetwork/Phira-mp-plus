//! Command specification modules for the Runtime CommandRegistry.
//!
//! Each sub-module returns its command specs so `runtime_registry()` can
//! collect them into a single `CommandRegistry`.  This keeps the registry
//! definitional — it only maps names to metadata — while the actual command
//! handlers live alongside their group's module.

use crate::cli::CliHandler;
use crate::command_registry::CommandHandler;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub mod benchmark;
pub mod core;
pub mod extensions;
pub mod ops;
pub mod plugin;
pub mod room;
pub mod runtime;
pub mod security;
pub mod user;

/// Wrap a no-argument `CliHandler` method as a `CommandHandler`.
///
/// The closure is called with the temporary `CliHandler` and its `out` lines
/// are collected into the returned output.
pub fn no_arg<F>(f: F) -> CommandHandler
where
    F: for<'a> Fn(&'a CliHandler) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(move |state, _args| {
        let state = Arc::clone(state);
        Box::pin(async move { crate::cli::with_cli(&state, |h| f(h)).await })
    })
}

/// Wrap an argument-taking `CliHandler` method as a `CommandHandler`.
///
/// The closure receives the command arguments as owned `Vec<String>` so the
/// returned future can move them without borrowing the outer handler closure.
pub fn with_args<F>(f: F) -> CommandHandler
where
    F: for<'a> Fn(&'a CliHandler, Vec<String>) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>
        + Send
        + Sync
        + 'static,
{
    Arc::new(move |state, args| {
        let state = Arc::clone(state);
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let f_ref = &f;
        Box::pin(async move {
            crate::cli::with_cli(&state, move |h| f_ref(h, args)).await
        })
    })
}

/// Concatenate all command specs from every group.
pub fn all_specs() -> Vec<crate::command_registry::CommandSpec> {
    let mut out = Vec::new();
    out.extend(core::specs());
    out.extend(runtime::specs());
    out.extend(benchmark::specs());
    out.extend(user::specs());
    out.extend(room::specs());
    out.extend(plugin::specs());
    out.extend(security::specs());
    out.extend(ops::specs());
    out.extend(extensions::specs());
    out
}

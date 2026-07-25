//! Unified error types for Phira-mp+.
//!
//! Every fallible operation should produce a typed [`AppError`] so callers can
//! discriminate error sources without parsing string messages.

use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    // ── Network / I/O ─────────────────────────────────────────────
    #[error("network error: {0}")]
    Network(String),
    #[error("connection reset: {0}")]
    ConnectionReset(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    // ── Database / Persistence ─────────────────────────────────────
    #[error("database error: {0}")]
    Database(String),
    #[error("database not configured")]
    DatabaseNotConfigured,
    #[error("round store error: {0}")]
    RoundStore(String),

    // ── Plugin / WASM ──────────────────────────────────────────────
    #[error("plugin error: {0}")]
    Plugin(String),
    #[error("plugin not found: {0}")]
    PluginNotFound(String),
    #[error("plugin crashed: {0}")]
    PluginCrashed(String),

    // ── Room / Session ─────────────────────────────────────────────
    #[error("room error: {0}")]
    Room(String),
    #[error("room not found: {0}")]
    RoomNotFound(String),
    #[error("session error: {0}")]
    Session(String),

    // ── Configuration ──────────────────────────────────────────────
    #[error("config error: {0}")]
    Config(String),
    #[error("config validation failed: {0}")]
    ConfigValidation(String),
    #[error("config file not found: {0}")]
    ConfigFileNotFound(String),

    // ── Internal ───────────────────────────────────────────────────
    #[error("internal error: {0}")]
    Internal(String),
    #[error("not implemented: {0}")]
    NotImplemented(String),
    #[error("unexpected: {0}")]
    Unexpected(String),
}

impl AppError {
    /// Categorise for logging / metrics.
    pub fn category(&self) -> &'static str {
        match self {
            Self::Network(_) | Self::ConnectionReset(_) => "network",
            Self::Io(_) => "io",
            Self::Database(_) | Self::DatabaseNotConfigured | Self::RoundStore(_) => "database",
            Self::Plugin(_) | Self::PluginNotFound(_) | Self::PluginCrashed(_) => "plugin",
            Self::Room(_) | Self::RoomNotFound(_) => "room",
            Self::Session(_) => "session",
            Self::Config(_) | Self::ConfigValidation(_) | Self::ConfigFileNotFound(_) => "config",
            Self::Internal(_) | Self::NotImplemented(_) | Self::Unexpected(_) => "internal",
        }
    }
}

// Convenience conversions for common patterns.

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        Self::Internal(format!("JSON error: {e}"))
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<AppError> for String {
    fn from(e: AppError) -> Self {
        e.to_string()
    }
}

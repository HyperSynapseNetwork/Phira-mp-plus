//! Server configuration types.
//!
//! Extracted from the original `server.rs` — see the parent module for
//! the re-exported type list.

use crate::error::AppError;
use crate::phira_client::PhiraHttpPolicyConfig;
use crate::plugin::WasmRuntimeConfig;
use crate::telemetry_batcher::{TelemetryBatcherPolicy, TelemetryCutoverMode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Chart information from the Phira API
#[derive(Debug, Deserialize, Clone)]
pub struct Chart {
    pub id: i32,
    pub name: String,
}

/// Record information from the Phira API
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Record {
    pub id: i32,
    pub player: i32,
    pub score: i32,
    pub perfect: i32,
    pub good: i32,
    pub bad: i32,
    pub miss: i32,
    pub max_combo: i32,
    pub accuracy: f32,
    pub full_combo: bool,
    pub std: f32,
    pub std_score: f32,
}

/// Convenience alias for a tokio RwLock-wrapped HashMap.
pub type SafeMap<K, V> = RwLock<HashMap<K, V>>;

/// Convenience alias for a UUID-keyed SafeMap.
pub type IdMap<V> = SafeMap<Uuid, V>;

pub(crate) fn normalize_phira_api_endpoint(value: &str) -> Result<String, String> {
    let endpoint = value.trim().trim_end_matches('/').to_string();
    if endpoint.is_empty() {
        return Err("phira_api_endpoint cannot be empty".to_string());
    }
    let url =
        reqwest::Url::parse(&endpoint).map_err(|e| format!("invalid phira_api_endpoint: {e}"))?;
    match url.scheme() {
        "http" | "https" => Ok(endpoint),
        other => Err(format!("unsupported phira_api_endpoint scheme: {other}")),
    }
}

pub(crate) fn parse_room_endpoint_value(value: &str) -> Result<Option<String>, String> {
    let raw = value.trim();
    if raw.is_empty()
        || raw.eq_ignore_ascii_case("default")
        || raw.eq_ignore_ascii_case("global")
        || raw.eq_ignore_ascii_case("none")
        || raw.eq_ignore_ascii_case("null")
        || raw.eq_ignore_ascii_case("clear")
        || raw == "全局"
        || raw == "默认"
        || raw == "清除"
    {
        Ok(None)
    } else {
        normalize_phira_api_endpoint(raw).map(Some)
    }
}

// ── Configuration types ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeV2Config {
    /// Bounded queue size for Runtime v2 PersistenceWorker.
    #[serde(default = "default_runtime_persistence_queue_capacity")]
    pub persistence_queue_capacity: usize,
    /// Local JSONL journal for events that still fail after the database retry
    /// budget. Set to `null` only when an external supervisor captures the same
    /// failed payloads; otherwise disabling it reintroduces silent loss.
    #[serde(default = "default_persistence_dead_letter_path")]
    pub persistence_dead_letter_path: Option<String>,
    /// Enqueue-before write-ahead log used for crash recovery and startup replay.
    #[serde(default = "default_persistence_wal_path")]
    pub persistence_wal_path: String,
    /// Batcher policy for production Touch/Judge telemetry.
    #[serde(default)]
    pub telemetry_batcher: TelemetryBatcherPolicy,
    /// Startup cutover mode for production Touch/Judge persistence.
    #[serde(default)]
    pub telemetry_cutover_mode: TelemetryCutoverMode,
    /// Unified Phira HTTP retry/timeout/circuit-breaker policy.
    #[serde(default)]
    pub phira_http: PhiraHttpPolicyConfig,
}

impl Default for RuntimeV2Config {
    fn default() -> Self {
        Self {
            persistence_queue_capacity: default_runtime_persistence_queue_capacity(),
            persistence_dead_letter_path: default_persistence_dead_letter_path(),
            persistence_wal_path: default_persistence_wal_path(),
            telemetry_batcher: TelemetryBatcherPolicy::default(),
            telemetry_cutover_mode: TelemetryCutoverMode::default(),
            phira_http: PhiraHttpPolicyConfig::default(),
        }
    }
}

/// Hot-reloadable runtime configuration subset.
///
/// Fields that can be safely changed at runtime without restarting the server.
/// Updated via the `config reload` CLI command or file watcher.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LiveConfig {
    /// Chat feature toggle.
    #[serde(default)]
    pub chat_enabled: bool,
    /// Server display name.
    #[serde(default)]
    pub server_name: Option<String>,
    /// Allowed monitor user IDs.
    #[serde(default)]
    pub monitors: Vec<i32>,
    /// Game admin Phira IDs.
    #[serde(default)]
    pub admin_phira_ids: Vec<i32>,
    /// Benchmark auth tokens.
    #[serde(default)]
    pub benchmark_phira_tokens: Vec<String>,
    /// Connection rate limit (per window).
    #[serde(default = "default_rate_limit")]
    pub connection_rate_limit: u32,
    /// Rate limit window in seconds.
    #[serde(default = "default_rate_window")]
    pub connection_rate_window: u32,
    /// Runtime v2 internal policy.
    #[serde(default)]
    pub runtime_v2: RuntimeV2Config,
}

impl LiveConfig {
    /// Extract hot-reloadable fields from a full config.
    pub fn from_full(config: &PlusConfig) -> Self {
        Self {
            chat_enabled: config.chat_enabled,
            server_name: config.server_name.clone(),
            monitors: config.monitors.clone(),
            admin_phira_ids: config.admin_phira_ids.clone(),
            benchmark_phira_tokens: config.benchmark_phira_tokens.clone(),
            connection_rate_limit: config.connection_rate_limit,
            connection_rate_window: config.connection_rate_window,
            runtime_v2: config.runtime_v2.clone(),
        }
    }
}

/// Operating profile that controls safe defaults.
/// Production profile enforces additional validation (finite limits, loopback bind, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigProfile {
    Development,
    Staging,
    Production,
}

impl Default for ConfigProfile {
    fn default() -> Self {
        Self::Development
    }
}

/// Phira-mp+ 增强配置（支持 YAML 文件、环境变量、CLI 参数三层覆盖）
///
/// Note: deny_unknown_fields was removed to allow graceful deprecation of
/// the rbac section. If a new field needs strict validation, add it back.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PlusConfig {
    /// Schema version for config file forward-compatibility.
    /// Current version: 1. Increment when making backward-incompatible changes.
    #[serde(default = "default_config_version")]
    pub config_version: u8,
    #[serde(default)]
    pub profile: ConfigProfile,
    pub port: u16,
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    /// HTTP/SSE listener bind address. Defaults to loopback for production
    /// safety; change to "0.0.0.0" only when PPB or a reverse proxy requires
    /// it and the network boundary is explicitly controlled.
    #[serde(default = "default_http_bind_address")]
    pub http_bind_address: String,
    #[serde(default = "default_monitors")]
    pub monitors: Vec<i32>,
    /// Explicit CLI monitor override retained across `config reload`.
    #[serde(skip)]
    pub cli_monitors_override: Option<Vec<i32>>,
    #[serde(default = "default_plugins_dir")]
    pub plugins_dir: String,
    pub extensions_file: Option<String>,
    /// Source YAML path used by `config reload`.
    #[serde(skip, default = "default_config_path")]
    pub config_path: String,
    #[serde(default = "default_true")]
    pub cli_enabled: bool,
    #[serde(default)]
    pub max_rooms: Option<usize>,
    #[serde(default)]
    pub max_users_per_room: Option<usize>,
    /// Maximum number of authenticated/registered sessions.
    #[serde(default = "default_max_sessions")]
    pub max_sessions: usize,
    /// Maximum number of concurrent pre-authentication handshakes.
    #[serde(default = "default_max_pending_auth")]
    pub max_pending_auth: usize,
    /// Total deadline used by the ordered shutdown sequence.
    #[serde(default = "default_graceful_shutdown_timeout_secs")]
    pub graceful_shutdown_timeout_secs: u64,
    /// Port for the optional forwarded-header compatibility listener.
    #[serde(default = "default_proxy_protocol_port")]
    pub proxy_protocol_port: u16,
    #[serde(default = "default_rate_limit")]
    pub connection_rate_limit: u32,
    #[serde(default = "default_rate_window")]
    pub connection_rate_window: u32,
    #[serde(default)]
    pub server_name: Option<String>,
    #[serde(default)]
    pub admin_token: Option<String>,
    #[serde(default = "default_phira_api")]
    pub phira_api_endpoint: String,
    #[serde(default = "default_true")]
    pub chat_enabled: bool,
    #[serde(default = "default_retention_days")]
    pub round_data_retention_days: u32,
    #[serde(default)]
    pub database_url: Option<String>,
    /// When set, database initialization failure is tolerated even when
    /// `database_url` is explicitly configured. The server will start without
    /// structured persistence (readiness reports degraded).
    /// This is intended for development/staging only.
    #[serde(default)]
    pub allow_database_degraded_mode: bool,
    #[serde(default = "default_persistence_retention_days")]
    pub persistence_retention_days: u32,
    #[serde(default)]
    pub touch_judge_retention_days: Option<u32>,
    #[serde(default)]
    pub admin_phira_ids: Vec<i32>,
    #[serde(default)]
    pub benchmark_phira_tokens: Vec<String>,
    #[serde(default)]
    pub wasm_runtime: WasmRuntimeConfig,
    #[serde(default)]
    pub runtime_v2: RuntimeV2Config,
    /// Idle mode configuration.
    #[serde(default)]
    pub idle: crate::idle::IdleConfig,
}

impl Default for PlusConfig {
    fn default() -> Self {
        Self {
            port: 12346,
            config_version: 1,
            profile: ConfigProfile::Development,
            http_port: 12347,
            http_bind_address: default_http_bind_address(),
            monitors: default_monitors(),
            cli_monitors_override: None,
            plugins_dir: "plugins".to_string(),
            extensions_file: Some("data/extensions.json".to_string()),
            config_path: default_config_path(),
            cli_enabled: true,
            max_rooms: None,
            max_users_per_room: None,
            max_sessions: default_max_sessions(),
            max_pending_auth: default_max_pending_auth(),
            graceful_shutdown_timeout_secs: default_graceful_shutdown_timeout_secs(),
            connection_rate_limit: 30,
            connection_rate_window: 10,
            server_name: None,
            admin_token: None,
            phira_api_endpoint: "https://phira.5wyxi.com".to_string(),
            chat_enabled: true,
            round_data_retention_days: 7,
            database_url: None,
            allow_database_degraded_mode: false,
            persistence_retention_days: 30,
            touch_judge_retention_days: None,
            admin_phira_ids: Vec::new(),
            benchmark_phira_tokens: Vec::new(),
            wasm_runtime: WasmRuntimeConfig::default(),
            runtime_v2: RuntimeV2Config::default(),
            idle: crate::idle::IdleConfig::default(),
            proxy_protocol_port: 0,
        }
    }
}

impl PlusConfig {
    /// Normalize values that are accepted in a user-friendly form but must be
    /// canonical before any subsystem stores them.
    pub fn normalize(&mut self) -> Result<(), AppError> {
        // Environment variable overrides for secrets (highest priority).
        // PM_DATABASE_URL / PM_DATABASE_URL_FILE overrides database_url.
        if let Ok(val) = std::env::var("PM_DATABASE_URL") {
            if !val.trim().is_empty() {
                self.database_url = Some(val.trim().to_string());
            }
        } else if let Ok(path) = std::env::var("PM_DATABASE_URL_FILE") {
            if let Ok(val) = std::fs::read_to_string(&path) {
                let trimmed = val.trim().to_string();
                if !trimmed.is_empty() {
                    self.database_url = Some(trimmed);
                }
            }
        }
        // PM_ADMIN_TOKEN overrides admin_token.
        if let Ok(val) = std::env::var("PM_ADMIN_TOKEN") {
            if !val.trim().is_empty() {
                self.admin_token = Some(val.trim().to_string());
            }
        } else if let Ok(path) = std::env::var("PM_ADMIN_TOKEN_FILE") {
            if let Ok(val) = std::fs::read_to_string(&path) {
                let trimmed = val.trim().to_string();
                if !trimmed.is_empty() {
                    self.admin_token = Some(trimmed);
                }
            }
        }
        self.phira_api_endpoint = normalize_phira_api_endpoint(&self.phira_api_endpoint)
            .map_err(AppError::ConfigValidation)?;
        Ok(())
    }

    /// Return a YAML representation of the config with secret fields masked.
    pub fn redacted_string(&self) -> String {
        let mut value = serde_json::to_value(self).unwrap_or_default();
        // Mask known secret fields
        if let Some(obj) = value.as_object_mut() {
            for field in &["database_url", "admin_token", "benchmark_phira_tokens"] {
                if let Some(val) = obj.get_mut(*field) {
                    if !val.is_null() && !val.as_str().map_or(true, |s| s.is_empty()) {
                        *val = serde_json::json!("****");
                    }
                }
            }
        }
        // Convert to YAML-like format (serde_json→serde_yaml)
        serde_yaml::to_string(&value).unwrap_or_else(|_| "<redacted config error>".to_string())
    }

    /// 从 YAML 文件加载配置
    pub fn from_yaml(path: &str) -> Result<Self, anyhow::Error> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read config '{path}': {e}"))?;
        let config: Self = serde_yaml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("failed to parse config '{path}': {e}"))?;
        Ok(config)
    }

    /// 启动时校验配置合法性。
    pub fn validate(&self) -> Result<(), AppError> {
        if self.port == 0 {
            return Err(AppError::ConfigValidation(format!(
                "端口 {} 超出范围 (1-65535)",
                self.port
            )));
        }
        if self.http_port > 0 && self.port == self.http_port {
            return Err(AppError::ConfigValidation(
                "TCP 端口和 HTTP 端口不能相同".into(),
            ));
        }
        if self.proxy_protocol_port > 0 && self.http_port == 0 {
            return Err(AppError::ConfigValidation(
                "启用 proxy_protocol_port 时必须同时启用 http_port".into(),
            ));
        }
        if self.proxy_protocol_port > 0
            && (self.proxy_protocol_port == self.port || self.proxy_protocol_port == self.http_port)
        {
            return Err(AppError::ConfigValidation(
                "PROXY protocol 端口不能与 TCP/HTTP 端口相同".into(),
            ));
        }
        if self.http_port > 0 && self.http_bind_address.trim().is_empty() {
            return Err(AppError::ConfigValidation(
                "http_bind_address 不能为空".into(),
            ));
        }
        if self.http_port > 0 && self.http_bind_address != "0.0.0.0" {
            // Validate that the address can be parsed, but don't force a specific format.
            let addr = format!("{}:{}", self.http_bind_address, self.http_port);
            if addr.parse::<std::net::SocketAddr>().is_err() {
                return Err(AppError::ConfigValidation(format!(
                    "http_bind_address \"{}\" 无法解析为有效的 IP 地址",
                    self.http_bind_address
                )));
            }
        }
        if self.max_rooms == Some(0) {
            return Err(AppError::ConfigValidation("max_rooms 必须大于 0".into()));
        }
        if self.max_rooms.is_none() {
            if self.profile == ConfigProfile::Production {
                return Err(AppError::ConfigValidation(
                    "production profile requires finite max_rooms (set max_rooms to a positive integer)"
                        .into(),
                ));
            }
            tracing::warn!(
                "max_rooms is null (unlimited). Set a finite limit for production safety."
            );
        }
        if self.profile == ConfigProfile::Production {
            if self.http_bind_address != "127.0.0.1" {
                return Err(AppError::ConfigValidation(
                    "production profile requires http_bind_address=\"127.0.0.1\"; "
                        .to_string()
                        + "set profile=development to bind externally",
                ));
            }
            if self.allow_database_degraded_mode {
                return Err(AppError::ConfigValidation(
                    "allow_database_degraded_mode is incompatible with production profile".into(),
                ));
            }
            if self.database_url.is_none() {
                return Err(AppError::ConfigValidation(
                    "production profile requires database_url".into(),
                ));
            }
        }
        if self.max_users_per_room == Some(0) {
            return Err(AppError::ConfigValidation(
                "max_users_per_room 必须大于 0".into(),
            ));
        }
        if self
            .max_users_per_room
            .is_some_and(|max_users| max_users > self.max_sessions)
        {
            return Err(AppError::ConfigValidation(
                "max_users_per_room 不能超过 max_sessions".into(),
            ));
        }
        if self.max_sessions == 0 || self.max_sessions > 1_000_000 {
            return Err(AppError::ConfigValidation(
                "max_sessions 必须在 1..=1000000 范围内".into(),
            ));
        }
        if self.max_pending_auth == 0 || self.max_pending_auth > self.max_sessions {
            return Err(AppError::ConfigValidation(
                "max_pending_auth 必须大于 0 且不能超过 max_sessions".into(),
            ));
        }
        if !(1..=300).contains(&self.graceful_shutdown_timeout_secs) {
            return Err(AppError::ConfigValidation(
                "graceful_shutdown_timeout_secs 必须在 1..=300 范围内".into(),
            ));
        }
        if self.plugins_dir.trim().is_empty() {
            return Err(AppError::ConfigValidation("plugins_dir 不能为空".into()));
        }
        if !(1..=4096).contains(&self.wasm_runtime.max_memory_mb) {
            return Err(AppError::ConfigValidation(
                "wasm_runtime.max_memory_mb 必须在 1..=4096 范围内".into(),
            ));
        }
        if self.wasm_runtime.fuel_per_call == 0 {
            return Err(AppError::ConfigValidation(
                "wasm_runtime.fuel_per_call 必须大于 0；PMP 不允许无计量插件执行".into(),
            ));
        }
        if !((64 * 1024)..=(64 * 1024 * 1024)).contains(&self.wasm_runtime.max_stack_bytes) {
            return Err(AppError::ConfigValidation(
                "wasm_runtime.max_stack_bytes 必须在 65536..=67108864 范围内".into(),
            ));
        }
        if !(1..=300).contains(&self.wasm_runtime.http_timeout_secs)
            || !(1..=(128 * 1024 * 1024)).contains(&self.wasm_runtime.max_http_response_bytes)
            || !(1..=(128 * 1024 * 1024)).contains(&self.wasm_runtime.max_file_bytes)
            || !(1..=256).contains(&self.wasm_runtime.max_event_concurrency)
            || !(16..=1_000_000).contains(&self.wasm_runtime.event_queue_capacity)
            || !(1..=300_000).contains(&self.wasm_runtime.call_timeout_ms)
        {
            return Err(AppError::ConfigValidation(
                "wasm_runtime 的超时、大小、并发或队列限制超出安全范围".into(),
            ));
        }
        let runtime = &self.runtime_v2;
        if !(16..=1_000_000).contains(&runtime.persistence_queue_capacity) {
            return Err(AppError::ConfigValidation(
                "runtime_v2.persistence_queue_capacity 必须在 16..=1000000 范围内".into(),
            ));
        }
        if runtime
            .persistence_dead_letter_path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(AppError::ConfigValidation(
                "runtime_v2.persistence_dead_letter_path 不能是空字符串；使用 null 可显式禁用"
                    .into(),
            ));
        }
        if runtime.persistence_wal_path.trim().is_empty() {
            return Err(AppError::ConfigValidation(
                "runtime_v2.persistence_wal_path 不能为空".into(),
            ));
        }
        // Validate WAL and dead-letter paths do not conflict.
        if let Err(e) = crate::persistence::wal::PersistenceWal::validate_paths_not_equal(
            runtime.persistence_dead_letter_path.as_deref().map(std::path::Path::new),
            std::path::Path::new(&runtime.persistence_wal_path),
        ) {
            return Err(AppError::ConfigValidation(format!(
                "runtime_v2 WAL/dead-letter 路径冲突: {e}"
            )));
        }
        let telemetry = &runtime.telemetry_batcher;
        if !(16..=1_000_000).contains(&telemetry.queue_capacity)
            || telemetry.max_items_per_batch == 0
            || telemetry.max_items_per_batch > telemetry.queue_capacity
            || !(50..=60_000).contains(&telemetry.flush_interval_ms)
        {
            return Err(AppError::ConfigValidation(
                "runtime_v2.telemetry_batcher 的队列、批量或刷新间隔无效".into(),
            ));
        }
        if matches!(
            runtime.telemetry_cutover_mode,
            TelemetryCutoverMode::WorkerAuthoritative
        ) {
            if !telemetry.enabled || telemetry.dry_run {
                return Err(AppError::ConfigValidation(
                    "worker_authoritative 要求 telemetry_batcher.enabled=true 且 dry_run=false"
                        .into(),
                ));
            }
            if self
                .database_url
                .as_deref()
                .map_or(true, |database_url| database_url.trim().is_empty())
            {
                return Err(AppError::ConfigValidation(
                    "worker_authoritative 要求配置非空 database_url".into(),
                ));
            }
        }
        if self
            .database_url
            .as_deref()
            .is_some_and(|database_url| database_url.trim().is_empty())
        {
            return Err(AppError::ConfigValidation(
                "database_url 不能是空字符串；无数据库时应省略该字段".into(),
            ));
        }
        if self.connection_rate_limit == 0 {
            return Err(AppError::ConfigValidation(
                "connection_rate_limit 必须大于 0".into(),
            ));
        }
        if self.connection_rate_window == 0 {
            return Err(AppError::ConfigValidation(
                "connection_rate_window 必须大于 0".into(),
            ));
        }
        if self.idle.check_interval_secs == 0 {
            return Err(AppError::ConfigValidation(
                "idle.check_interval_secs 必须大于 0".into(),
            ));
        }
        Ok(())
    }

    /// 合并 CLI 参数覆盖。只有显式提供的参数才覆盖 YAML。
    pub fn merge_cli(mut self, cli: PlusConfigCli) -> Self {
        if let Some(port) = cli.port {
            self.port = port;
        }
        if let Some(http_port) = cli.http_port {
            self.http_port = http_port;
        }
        if !cli.monitors.is_empty() {
            self.cli_monitors_override = Some(cli.monitors.clone());
            self.monitors = cli.monitors;
        }
        if let Some(plugins_dir) = cli.plugins_dir {
            self.plugins_dir = plugins_dir;
        }
        if let Some(ext) = cli.extensions_file {
            self.extensions_file = Some(ext);
        }
        if let Some(proxy_protocol_port) = cli.proxy_protocol_port {
            self.proxy_protocol_port = proxy_protocol_port;
        }
        if cli.disable_cli {
            self.cli_enabled = false;
        }
        self
    }
}

/// CLI 覆盖配置（只有用户显式提供的参数才覆盖 YAML）
pub struct PlusConfigCli {
    pub port: Option<u16>,
    pub http_port: Option<u16>,
    pub proxy_protocol_port: Option<u16>,
    pub monitors: Vec<i32>,
    pub plugins_dir: Option<String>,
    pub extensions_file: Option<String>,
    pub disable_cli: bool,
}

// ── Default-value helpers ──────────────────────────────────────────

fn default_config_version() -> u8 {
    1
}

fn default_http_port() -> u16 {
    12347
}

fn default_http_bind_address() -> String {
    "127.0.0.1".to_string()
}
fn default_config_path() -> String {
    "server_config.yml".to_string()
}
fn default_plugins_dir() -> String {
    "plugins".to_string()
}
fn default_monitors() -> Vec<i32> {
    vec![2]
}
fn default_true() -> bool {
    true
}
fn default_rate_limit() -> u32 {
    30
}
fn default_rate_window() -> u32 {
    10
}
fn default_phira_api() -> String {
    "https://phira.5wyxi.com".to_string()
}
fn default_proxy_protocol_port() -> u16 {
    0
}
fn default_retention_days() -> u32 {
    7
}
fn default_persistence_retention_days() -> u32 {
    30
}
fn default_runtime_persistence_queue_capacity() -> usize {
    2048
}
fn default_persistence_dead_letter_path() -> Option<String> {
    Some("data/persistence-dead-letter.jsonl".to_string())
}
fn default_persistence_wal_path() -> String {
    "data/persistence-worker.wal.jsonl".to_string()
}
fn default_max_sessions() -> usize {
    4096
}
fn default_max_pending_auth() -> usize {
    256
}
fn default_graceful_shutdown_timeout_secs() -> u64 {
    15
}

#[cfg(test)]
mod tests {
    use super::{PlusConfig, PlusConfigCli, TelemetryCutoverMode};

    #[test]
    fn partial_yaml_uses_runtime_defaults() {
        let config: PlusConfig = serde_yaml::from_str(
            "chat_enabled: false
",
        )
        .unwrap();
        assert_eq!(config.port, 12346);
        assert_eq!(config.http_port, 12347);
        assert_eq!(config.monitors, vec![2]);
        assert_eq!(
            config.extensions_file.as_deref(),
            Some("data/extensions.json")
        );
        assert!(!config.chat_enabled);
    }

    #[test]
    fn normalization_canonicalizes_phira_endpoint() {
        let mut config = PlusConfig {
            phira_api_endpoint: " https://example.com/api/ ".to_string(),
            ..PlusConfig::default()
        };
        config.normalize().unwrap();
        assert_eq!(config.phira_api_endpoint, "https://example.com/api");
    }

    #[test]
    fn unknown_top_level_field_is_rejected() {
        let err = serde_yaml::from_str::<PlusConfig>("chat_enabld: false\n")
            .expect_err("misspelled config keys must not be ignored");
        assert!(err.to_string().contains("unknown field"));
    }

    #[test]
    fn explicit_empty_monitor_list_is_preserved() {
        let config: PlusConfig = serde_yaml::from_str(
            "monitors: []
",
        )
        .unwrap();
        assert!(config.monitors.is_empty());
    }

    #[test]
    fn only_explicit_cli_values_override_yaml() {
        let config = PlusConfig {
            port: 23456,
            http_port: 23457,
            plugins_dir: "custom-plugins".to_string(),
            ..PlusConfig::default()
        }
        .merge_cli(PlusConfigCli {
            port: None,
            http_port: None,
            proxy_protocol_port: None,
            monitors: Vec::new(),
            plugins_dir: None,
            extensions_file: None,
            disable_cli: true,
        });
        assert_eq!(config.port, 23456);
        assert_eq!(config.http_port, 23457);
        assert_eq!(config.plugins_dir, "custom-plugins");
        assert!(config.cli_monitors_override.is_none());
        assert!(!config.cli_enabled);
    }

    #[test]
    fn explicit_monitor_cli_override_is_recorded() {
        let config = PlusConfig::default().merge_cli(PlusConfigCli {
            port: None,
            http_port: None,
            proxy_protocol_port: None,
            monitors: vec![7, 9],
            plugins_dir: None,
            extensions_file: None,
            disable_cli: false,
        });
        assert_eq!(config.monitors, vec![7, 9]);
        assert_eq!(config.cli_monitors_override, Some(vec![7, 9]));
    }
    #[test]
    fn rejects_invalid_capacity_and_shutdown_limits() {
        let mut config = PlusConfig::default();
        config.max_sessions = 0;
        assert!(config.validate().is_err());

        let mut config = PlusConfig::default();
        config.max_pending_auth = config.max_sessions + 1;
        assert!(config.validate().is_err());

        let mut config = PlusConfig::default();
        config.graceful_shutdown_timeout_secs = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_unmetered_or_unbounded_plugin_runtime() {
        let mut config = PlusConfig::default();
        config.wasm_runtime.fuel_per_call = 0;
        assert!(config.validate().is_err());

        let mut config = PlusConfig::default();
        config.wasm_runtime.event_queue_capacity = 0;
        assert!(config.validate().is_err());

        let mut config = PlusConfig::default();
        config.wasm_runtime.call_timeout_ms = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_invalid_runtime_v2_batching_contract() {
        let mut config = PlusConfig::default();
        config.runtime_v2.persistence_queue_capacity = 0;
        assert!(config.validate().is_err());

        let mut config = PlusConfig::default();
        config.runtime_v2.telemetry_batcher.max_items_per_batch =
            config.runtime_v2.telemetry_batcher.queue_capacity + 1;
        assert!(config.validate().is_err());

        let mut config = PlusConfig::default();
        config.runtime_v2.telemetry_batcher.flush_interval_ms = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn default_runtime_v2_enables_dead_letter_journal() {
        let config = PlusConfig::default();
        assert_eq!(
            config.runtime_v2.persistence_dead_letter_path.as_deref(),
            Some("data/persistence-dead-letter.jsonl")
        );
    }

    #[test]
    fn rejects_empty_dead_letter_path_but_allows_explicit_disable() {
        let mut config = PlusConfig::default();
        config.runtime_v2.persistence_dead_letter_path = Some("   ".to_string());
        assert!(config.validate().is_err());

        config.runtime_v2.persistence_dead_letter_path = None;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn worker_authoritative_requires_real_database_and_active_batcher() {
        let mut config = PlusConfig::default();
        config.runtime_v2.telemetry_cutover_mode = TelemetryCutoverMode::WorkerAuthoritative;
        assert!(config.validate().is_err());

        config.database_url = Some("postgres://localhost/phira".to_string());
        assert!(config.validate().is_ok());

        config.runtime_v2.telemetry_batcher.dry_run = true;
        assert!(config.validate().is_err());
    }

    #[test]
    fn redacted_config_hides_secrets() {
        let mut config = PlusConfig::default();
        config.database_url = Some("postgres://user:secret@localhost/db".to_string());
        config.admin_token = Some("my-secret-token".to_string());
        let redacted = config.redacted_string();
        assert!(!redacted.contains("secret"), "redacted config should not contain secret: {redacted}");
        assert!(redacted.contains("****"), "redacted config should mask values");
    }
}

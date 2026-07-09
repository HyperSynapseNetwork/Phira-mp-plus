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

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeV2Config {
    /// Bounded queue size for Runtime v2 PersistenceWorker.
    #[serde(default = "default_runtime_persistence_queue_capacity")]
    pub persistence_queue_capacity: usize,
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

/// Phira-mp+ 增强配置（支持 YAML 文件、环境变量、CLI 参数三层覆盖）
#[derive(Debug, Clone, Deserialize)]
pub struct PlusConfig {
    pub port: u16,
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    #[serde(default)]
    pub monitors: Vec<i32>,
    #[serde(default = "default_plugins_dir")]
    pub plugins_dir: String,
    pub extensions_file: Option<String>,
    #[serde(default = "default_true")]
    pub cli_enabled: bool,
    #[serde(default)]
    pub max_rooms: Option<usize>,
    #[serde(default)]
    pub max_users_per_room: Option<usize>,
    /// Port for PROXY protocol (v1/v2) connections from reverse proxies.
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
    #[serde(default)]
    pub chat_enabled: bool,
    #[serde(default = "default_retention_days")]
    pub round_data_retention_days: u32,
    #[serde(default)]
    pub database_url: Option<String>,
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
            http_port: 12347,
            monitors: vec![2],
            plugins_dir: "plugins".to_string(),
            extensions_file: None,
            cli_enabled: true,
            max_rooms: None,
            max_users_per_room: None,
            connection_rate_limit: 30,
            connection_rate_window: 10,
            server_name: None,
            admin_token: None,
            phira_api_endpoint: "https://phira.5wyxi.com".to_string(),
            chat_enabled: true,
            round_data_retention_days: 7,
            database_url: None,
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
        if !std::path::Path::new(&self.plugins_dir).exists() {
            return Err(AppError::ConfigValidation(format!(
                "插件目录不存在: {}",
                self.plugins_dir
            )));
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
        Ok(())
    }

    /// 合并 CLI 参数覆盖
    pub fn merge_cli(mut self, cli: PlusConfigCli) -> Self {
        if cli.port != 12346 {
            self.port = cli.port;
        }
        if cli.http_port != 12347 {
            self.http_port = cli.http_port;
        }
        if !cli.monitors.is_empty() {
            self.monitors = cli.monitors;
        }
        if cli.plugins_dir != "plugins" {
            self.plugins_dir = cli.plugins_dir;
        }
        if let Some(ext) = cli.extensions_file {
            self.extensions_file = Some(ext);
        }
        if cli.proxy_protocol_port > 0 {
            self.proxy_protocol_port = cli.proxy_protocol_port;
        }
        self
    }
}

/// CLI 覆盖配置（来自命令行参数）
pub struct PlusConfigCli {
    pub port: u16,
    pub http_port: u16,
    pub proxy_protocol_port: u16,
    pub monitors: Vec<i32>,
    pub plugins_dir: String,
    pub extensions_file: Option<String>,
    pub log_file: String,
}

// ── Default-value helpers ──────────────────────────────────────────

fn default_http_port() -> u16 { 12347 }
fn default_plugins_dir() -> String { "plugins".to_string() }
fn default_true() -> bool { true }
fn default_rate_limit() -> u32 { 30 }
fn default_rate_window() -> u32 { 10 }
fn default_phira_api() -> String { "https://phira.5wyxi.com".to_string() }
fn default_proxy_protocol_port() -> u16 { 0 }
fn default_retention_days() -> u32 { 7 }
fn default_persistence_retention_days() -> u32 { 30 }
fn default_runtime_persistence_queue_capacity() -> usize { 2048 }

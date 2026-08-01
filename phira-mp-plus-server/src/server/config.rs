//! Server configuration types.
//!
//! Extracted from the original `server.rs` — see the parent module for
//! the re-exported type list.

use crate::error::AppError;
use crate::persistence::high_frequency::HighFrequencyConfig;
use crate::phira_client::PhiraHttpPolicyConfig;
use crate::plugin::WasmRuntimeConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::RwLock;
use uuid::Uuid;

/// Chart information from the Phira API
#[derive(Debug, Deserialize, Clone)]
pub struct Chart {
    pub id: i32,
    pub name: String,
    /// Download URL for the .phira chart file (zip).
    #[serde(default)]
    pub file: Option<String>,
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
pub struct RuntimeConfig {
    /// Bounded queue size for Runtime PersistenceWorker.
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
    /// Unified Phira HTTP retry/timeout/circuit-breaker policy.
    #[serde(default)]
    pub phira_http: PhiraHttpPolicyConfig,
    /// High-frequency telemetry writer (Touch/Judge) configuration.
    #[serde(default)]
    pub high_frequency: HighFrequencyConfig,
    /// When true, a failure to restore a configured persistent room aborts
    /// startup recovery (fail-closed).  Default false: individual room
    /// restoration failures are logged but do not block startup.
    #[serde(default)]
    pub persistent_rooms_required: bool,
    /// Maximum time to wait for the persistence WAL to replay + drain during
    /// startup recovery.  Must comfortably exceed the persistence pipeline's
    /// own retry budget so a transient DB fault can self-heal instead of
    /// failing startup (PMP37 P0-C).
    #[serde(default = "default_startup_recovery_timeout_secs")]
    pub startup_recovery_timeout_secs: u64,
}

fn default_startup_recovery_timeout_secs() -> u64 {
    30
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            persistence_queue_capacity: default_runtime_persistence_queue_capacity(),
            persistence_dead_letter_path: default_persistence_dead_letter_path(),
            persistence_wal_path: default_persistence_wal_path(),
            phira_http: PhiraHttpPolicyConfig::default(),
            high_frequency: HighFrequencyConfig::default(),
            persistent_rooms_required: false,
            startup_recovery_timeout_secs: default_startup_recovery_timeout_secs(),
        }
    }
}

/// Hot-reloadable runtime configuration subset.
///
/// Fields that can be safely changed at runtime without restarting the server.
/// Updated via the `config reload` CLI command or file watcher.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LiveConfig {
    /// Hot-reloadable Phira API endpoint override (used by Mock Phira in benchmarks).
    /// When empty, consumers fall back to `PlusConfig::phira_api_endpoint`.
    #[serde(default)]
    pub phira_api_endpoint: String,
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
    /// Connection rate limit (per window).
    #[serde(default = "default_rate_limit")]
    pub connection_rate_limit: u32,
    /// Rate limit window in seconds.
    #[serde(default = "default_rate_window")]
    pub connection_rate_window: u32,
    /// Runtime internal policy.
    #[serde(default)]
    pub runtime: RuntimeConfig,
    /// 是否允许玩家建房（CLI 可动态切换）。
    #[serde(default = "default_room_creation_enabled")]
    pub room_creation_enabled: bool,
}

impl LiveConfig {
    /// Extract hot-reloadable fields from a full config.
    pub fn from_full(config: &PlusConfig) -> Self {
        Self {
            phira_api_endpoint: config.phira_api_endpoint.clone(),
            chat_enabled: config.chat_enabled,
            server_name: config.server_name.clone(),
            monitors: config.monitors.clone(),
            admin_phira_ids: config.admin_phira_ids.clone(),
            connection_rate_limit: config.connection_rate_limit,
            connection_rate_window: config.connection_rate_window,
            runtime: config.runtime.clone(),
            room_creation_enabled: config.room_creation_enabled,
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

/// 官方 Phira 客户端兼容参数（PMP42 P0-B/P0-C）。
///
/// 官方 Phira 客户端是不可修改的兼容目标。PMP 必须复现官方 `phira-mp` 的
/// 可观察响应时序：
/// - 响应不能早于客户端安装回调（send→install-callback）；
/// - 响应不能晚于客户端约 7 秒的固定 deadline。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatibilityConfig {
    /// 是否针对未修改的官方客户端启用兼容延迟。设为 `false` 可做差分/压测。
    #[serde(default = "default_official_phira_client")]
    pub official_phira_client: bool,
    /// 请求型命令响应的最低服务端延迟（毫秒）。从收到命令开始计时。
    #[serde(default = "default_minimum_response_latency_ms")]
    pub minimum_response_latency_ms: u64,
    /// 单条普通客户端命令的总业务 deadline（毫秒），覆盖 mailbox 发送与
    /// reply 两个阶段。必须明显小于官方客户端约 7 秒的固定等待。
    #[serde(default = "default_session_command_deadline_ms")]
    pub session_command_deadline_ms: u64,
    /// PMP45 P0-I: 权威提交后保留的响应/发送/flush 预算（毫秒）。总
    /// `session_command_deadline_ms` 拆分为 commit budget（= 总预算 - 本值）
    /// 与 response budget（= 本值）。authoritative 提交（RoomActor 提交 /
    /// user.room 变更 / WAL 写入 / 官方广播）必须在 commit deadline 前完成，
    /// 留下至少本值的时间用于最小响应时延与响应 flush——避免「服务端已提交、
    /// 客户端已超时」（audit §17/P0-I）。默认 1000ms，范围 200..=2500ms，
    /// 且必须严格小于 `session_command_deadline_ms`。
    #[serde(default = "default_commit_response_reserve_ms")]
    pub commit_response_reserve_ms: u64,
    /// 总认证绝对预算（毫秒）。认证流程（Phira API + 重试 + 退避 + WAL admission +
    /// 最低响应时延 + 响应 flush）共用该预算，必须早于官方客户端约 7 秒 deadline。
    /// 默认 5000ms，范围 1000..=6500ms。
    #[serde(default = "default_auth_deadline_ms")]
    pub auth_deadline_ms: u64,
    /// 出站认证屏障（SessionOutboundGate）缓冲事件数量上限。认证握手期间房间
    /// 广播进入该缓冲；超过上限则直接丢弃高频事件（或按分类策略处理），防止
    /// 慢认证连接造成无界内存增长。默认 256。
    #[serde(default = "default_gate_max_pending_events")]
    pub gate_max_pending_events: usize,
    /// 缓冲字节上限（粗估：每 ServerCommand 近似估算）。默认 1 MiB。
    #[serde(default = "default_gate_max_pending_bytes")]
    pub gate_max_pending_bytes: usize,
    /// 认证屏障最大持续时间（毫秒）。超过后强制关闭认证（fail-closed）。默认 8000。
    #[serde(default = "default_gate_max_auth_duration_ms")]
    pub gate_max_auth_duration_ms: u64,
    /// ProtocolHack 补偿消息延迟（毫秒）。`None` 时回退到
    /// `minimum_response_latency_ms`（默认 10ms）；设为 `Some(0)` 可做差分测试
    /// （与官方/无补偿时序对比）。补偿消息在官方响应 flush 之后调度，
    /// 不阻塞 Room Actor（PMP42 P1 ProtocolHack）。
    #[serde(default)]
    pub protocol_hack_delay_ms: Option<u64>,
}

impl Default for CompatibilityConfig {
    fn default() -> Self {
        Self {
            official_phira_client: default_official_phira_client(),
            minimum_response_latency_ms: default_minimum_response_latency_ms(),
            session_command_deadline_ms: default_session_command_deadline_ms(),
            commit_response_reserve_ms: default_commit_response_reserve_ms(),
            auth_deadline_ms: default_auth_deadline_ms(),
            gate_max_pending_events: default_gate_max_pending_events(),
            gate_max_pending_bytes: default_gate_max_pending_bytes(),
            gate_max_auth_duration_ms: default_gate_max_auth_duration_ms(),
            protocol_hack_delay_ms: None,
        }
    }
}

fn default_official_phira_client() -> bool {
    true
}

fn default_minimum_response_latency_ms() -> u64 {
    10
}

fn default_session_command_deadline_ms() -> u64 {
    4500
}

fn default_commit_response_reserve_ms() -> u64 {
    1000
}

fn default_auth_deadline_ms() -> u64 {
    5000
}

fn default_gate_max_pending_events() -> usize {
    256
}

fn default_gate_max_pending_bytes() -> usize {
    1_048_576
}

fn default_gate_max_auth_duration_ms() -> u64 {
    8000
}

impl Default for ConfigProfile {
    fn default() -> Self {
        Self::Development
    }
}

/// Phira-mp+ 增强配置（支持 YAML 文件、环境变量、CLI 参数三层覆盖）
///
/// deny_unknown_fields ensures config typos are caught at startup.
/// If a field is removed (like `rbac`), users must remove it from their
/// config file — serde will produce a clear error message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlusConfig {
    #[serde(default)]
    pub profile: ConfigProfile,
    pub port: u16,
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    /// HTTP/SSE listener bind address. Defaults to loopback for production
    /// safety; change to "0.0.0.0" only when a reverse proxy requires
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
    /// Sentry error monitoring DSN. Set to a valid Sentry DSN to enable
    /// automatic error and panic capture. Leave empty or omit to disable.
    #[serde(default)]
    pub sentry_dsn: Option<String>,
    #[serde(default)]
    pub max_rooms: Option<usize>,
    #[serde(default)]
    pub max_users_per_room: Option<usize>,
    /// 准备倒计时（秒）。房主/管理员发起游戏后，未在此时长内准备的玩家自动弃权。
    #[serde(default = "default_ready_countdown_secs")]
    pub ready_countdown_secs: u64,
    /// 对局超时偏移量（秒）。谱面时长 + 此偏移 = 最大对局时间。
    /// 首个完成者出现后，偏移量会重新计算（给其他玩家追赶时间）。
    /// 设为 0 表示不启用对局超时。
    #[serde(default = "default_playing_timeout_offset_secs")]
    pub playing_timeout_offset_secs: u64,
    #[serde(default = "default_room_creation_enabled")]
    pub room_creation_enabled: bool,
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
    #[serde(default = "default_trusted_forwarded_http_port")]
    pub trusted_forwarded_http_port: u16,
    /// Comma-separated CIDR allowlist for HAProxy PROXY protocol v1/v2.
    ///
    /// When set, connections whose peer IP matches an entry in this list
    /// are inspected for a PROXY protocol header.  The real client address
    /// from that header replaces the socket peer address for authentication
    /// and rate-limiting.
    ///
    /// Example: `"10.0.0.0/8,192.168.0.0/16"`
    #[serde(default)]
    pub proxy_allow_cidr: Option<String>,
    #[serde(default = "default_rate_limit")]
    pub connection_rate_limit: u32,
    #[serde(default = "default_rate_window")]
    pub connection_rate_window: u32,
    #[serde(default)]
    pub server_name: Option<String>,
    #[serde(default = "default_phira_api")]
    pub phira_api_endpoint: String,
    #[serde(default = "default_true")]
    pub chat_enabled: bool,
    #[serde(default = "default_retention_days")]
    pub round_data_retention_days: u32,
    #[serde(default)]
    pub database_url: String,
    #[serde(default = "default_persistence_retention_days")]
    pub persistence_retention_days: u32,
    #[serde(default)]
    pub touch_judge_retention_days: Option<u32>,
    #[serde(default)]
    pub admin_phira_ids: Vec<i32>,
    #[serde(default)]
    pub wasm_runtime: WasmRuntimeConfig,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    /// Idle mode configuration.
    #[serde(default)]
    pub idle: IdleConfig,
    /// OpenUDS (Unix Domain Socket) API configuration.
    #[serde(default)]
    pub openuds: OpenUdsConfig,
    /// 官方 Phira 客户端兼容参数（PMP42 P0-B/P0-C）。
    #[serde(default)]
    pub compatibility: CompatibilityConfig,
}

impl Default for PlusConfig {
    fn default() -> Self {
        Self {
            port: 12346,
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
            phira_api_endpoint: "https://phira.5wyxi.com".to_string(),
            chat_enabled: true,
            round_data_retention_days: 7,
            database_url: String::new(),
            persistence_retention_days: 30,
            touch_judge_retention_days: None,
            admin_phira_ids: Vec::new(),
            wasm_runtime: WasmRuntimeConfig::default(),
            runtime: RuntimeConfig::default(),
            idle: IdleConfig::default(),
            openuds: OpenUdsConfig::default(),
            compatibility: CompatibilityConfig::default(),
            trusted_forwarded_http_port: 0,
            proxy_allow_cidr: None,
            ready_countdown_secs: default_ready_countdown_secs(),
            playing_timeout_offset_secs: default_playing_timeout_offset_secs(),
            room_creation_enabled: default_room_creation_enabled(),
            sentry_dsn: None,
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
                self.database_url = val.trim().to_string();
            }
        } else if let Ok(path) = std::env::var("PM_DATABASE_URL_FILE") {
            if let Ok(val) = std::fs::read_to_string(&path) {
                let trimmed = val.trim().to_string();
                if !trimmed.is_empty() {
                    self.database_url = trimmed;
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
            for field in &["database_url"] {
                if let Some(val) = obj.get_mut(*field) {
                    let is_non_empty_string = val.as_str().is_some_and(|s| !s.is_empty());
                    if is_non_empty_string {
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
        if self.trusted_forwarded_http_port > 0 && self.http_port == 0 {
            return Err(AppError::ConfigValidation(
                "启用 trusted_forwarded_http_port 时必须同时启用 http_port".into(),
            ));
        }
        if self.trusted_forwarded_http_port > 0
            && (self.trusted_forwarded_http_port == self.port || self.trusted_forwarded_http_port == self.http_port)
        {
            return Err(AppError::ConfigValidation(
                "可信转发 HTTP 端口不能与 TCP/HTTP 端口相同".into(),
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
            if self.http_bind_address == "0.0.0.0" {
                tracing::warn!(
                    "http_bind_address is 0.0.0.0 — the HTTP/SSE port will be exposed on all \
                     interfaces. Ensure a firewall or reverse proxy restricts access."
                );
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
        let runtime = &self.runtime;
        if !(16..=1_000_000).contains(&runtime.persistence_queue_capacity) {
            return Err(AppError::ConfigValidation(
                "runtime.persistence_queue_capacity 必须在 16..=1000000 范围内".into(),
            ));
        }
        if runtime
            .persistence_dead_letter_path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err(AppError::ConfigValidation(
                "runtime.persistence_dead_letter_path 不能是空字符串；使用 null 可显式禁用"
                    .into(),
            ));
        }
        if runtime.persistence_wal_path.trim().is_empty() {
            return Err(AppError::ConfigValidation(
                "runtime.persistence_wal_path 不能为空".into(),
            ));
        }
        // Validate WAL and dead-letter paths do not conflict.
        if let Err(e) = crate::persistence::wal::PersistenceWal::validate_paths_not_equal(
            runtime.persistence_dead_letter_path.as_deref().map(std::path::Path::new),
            std::path::Path::new(&runtime.persistence_wal_path),
        ) {
            return Err(AppError::ConfigValidation(format!(
                "runtime WAL/dead-letter 路径冲突: {e}"
            )));
        }
        if self.database_url.trim().is_empty() {
            return Err(AppError::ConfigValidation(
                "database_url 不能为空；必须指定 PostgreSQL 连接地址".into(),
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
        if let Some(ref cidr) = self.proxy_allow_cidr {
            if let Err(e) = super::proxy_protocol::validate_cidr_list(cidr) {
                return Err(AppError::ConfigValidation(format!(
                    "proxy_allow_cidr 无效: {e}"
                )));
            }
        }
        // 官方客户端兼容参数校验：总 deadline 必须明显小于客户端约 7 秒超时。
        if !(100..=6000).contains(&self.compatibility.session_command_deadline_ms) {
            return Err(AppError::ConfigValidation(
                "compatibility.session_command_deadline_ms 必须在 100..=6000ms 范围内（需小于官方客户端约 7 秒 deadline）"
                    .into(),
            ));
        }
        // PMP45 P0-I: 提交后响应预算范围校验。该值把总命令 deadline 拆分为
        // commit budget 与 response budget，必须保证权威提交后仍留有足够
        // 响应/flush 时间（audit §17）。
        if !(200..=2500).contains(&self.compatibility.commit_response_reserve_ms) {
            return Err(AppError::ConfigValidation(
                "compatibility.commit_response_reserve_ms 必须在 200..=2500ms 范围内".into(),
            ));
        }
        // PMP45 P0-I 跨字段校验：reserve 必须严格小于总命令 deadline。否则
        // commit deadline 落在接收点之前，命令永远无法提交（P0-I 拆分失效）。
        if self.compatibility.commit_response_reserve_ms
            >= self.compatibility.session_command_deadline_ms
        {
            return Err(AppError::ConfigValidation(
                "compatibility.commit_response_reserve_ms 必须严格小于 session_command_deadline_ms：否则 commit deadline 落在接收点之前，命令永远无法提交"
                    .into(),
            ));
        }
        // 认证绝对预算必须早于官方客户端约 7 秒 deadline，同时给 Phira API
        // 重试/退避留出合理空间（PMP44 P0-D）。
        if !(1000..=6500).contains(&self.compatibility.auth_deadline_ms) {
            return Err(AppError::ConfigValidation(
                "compatibility.auth_deadline_ms 必须在 1000..=6500ms 范围内（需早于官方客户端约 7 秒 deadline）"
                    .into(),
            ));
        }
        if self.compatibility.minimum_response_latency_ms > 1000 {
            return Err(AppError::ConfigValidation(
                "compatibility.minimum_response_latency_ms 不能超过 1000ms".into(),
            ));
        }
        if self.compatibility.protocol_hack_delay_ms.is_some_and(|ms| ms > 1000) {
            return Err(AppError::ConfigValidation(
                "compatibility.protocol_hack_delay_ms 不能超过 1000ms（None 回退到 minimum_response_latency_ms）"
                    .into(),
            ));
        }
        // PMP44 P1 §33: 官方兼容模式下最低响应时延不能误设 0。官方客户端在
        // send 之后才安装 Authenticate 回调，0ms 最低时延会让响应先于回调安装、
        // 与回调安装竞态；必须设置正数最低时延或关闭官方兼容。例外：显式设置
        // protocol_hack_delay_ms = Some(0) 用于差分测试（与官方/无补偿时序对比）
        // 时用户有意为零延迟，放行。
        if self.compatibility.official_phira_client
            && self.compatibility.minimum_response_latency_ms == 0
            && self.compatibility.protocol_hack_delay_ms != Some(0)
        {
            return Err(AppError::ConfigValidation(
                "compatibility.official_phira_client=true 时 minimum_response_latency_ms 不能为 0：官方客户端在 send 之后才安装 Authenticate 回调，0ms 最低时延会与回调安装竞态。请设置正数最低时延或关闭官方兼容；差分测试可显式设置 protocol_hack_delay_ms=0"
                    .into(),
            ));
        }
        // PMP44 P0-G: 出站认证屏障缓冲与超时上限校验。数量/字节上限用于
        // 防止慢认证连接造成无界内存增长；持续时间上限用于 fail-closed
        // 强制关闭认证。
        if !(64..=4096).contains(&self.compatibility.gate_max_pending_events) {
            return Err(AppError::ConfigValidation(
                "compatibility.gate_max_pending_events 必须在 64..=4096 范围内".into(),
            ));
        }
        if !((64 * 1024)..=(8 * 1024 * 1024)).contains(&self.compatibility.gate_max_pending_bytes) {
            return Err(AppError::ConfigValidation(
                "compatibility.gate_max_pending_bytes 必须在 64KiB..=8MiB 范围内".into(),
            ));
        }
        if !(1000..=15000).contains(&self.compatibility.gate_max_auth_duration_ms) {
            return Err(AppError::ConfigValidation(
                "compatibility.gate_max_auth_duration_ms 必须在 1000..=15000ms 范围内".into(),
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
        if let Some(trusted_forwarded_http_port) = cli.trusted_forwarded_http_port {
            self.trusted_forwarded_http_port = trusted_forwarded_http_port;
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
    pub trusted_forwarded_http_port: Option<u16>,
    pub monitors: Vec<i32>,
    pub plugins_dir: Option<String>,
    pub extensions_file: Option<String>,
    pub disable_cli: bool,
}

// ── Default-value helpers ──────────────────────────────────────────

fn default_http_port() -> u16 {
    12347
}

fn default_http_bind_address() -> String {
    "0.0.0.0".to_string()
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleConfig {
    #[serde(default = "default_heartbeat_timeout")]
    pub heartbeat_timeout_secs: u64,
    #[serde(default = "default_auth_timeout")]
    pub auth_timeout_secs: u64,
    /// 断线重连宽限时间（秒）。玩家断线后在此时长内重连可恢复。
    #[serde(default = "default_dangle_grace_secs")]
    pub dangle_grace_secs: u64,
    /// Playing 状态断线重连宽限时间（秒）。
    /// Playing 中断线不立即踢出房间，保留成员资格等待重连。
    /// 默认为 15 秒，设为 0 表示立即踢出（旧行为）。
    #[serde(default = "default_playing_reconnect_grace_secs")]
    pub playing_reconnect_grace_secs: u64,
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self {
            heartbeat_timeout_secs: default_heartbeat_timeout(),
            auth_timeout_secs: default_auth_timeout(),
            dangle_grace_secs: default_dangle_grace_secs(),
            playing_reconnect_grace_secs: default_playing_reconnect_grace_secs(),
        }
    }
}

fn default_heartbeat_timeout() -> u64 { 15 }
fn default_auth_timeout() -> u64 { 15 }
fn default_dangle_grace_secs() -> u64 { 10 }
fn default_playing_reconnect_grace_secs() -> u64 { 15 }

// ── OpenUDS Config ──────────────────────────────────────────

/// OpenUDS (Unix Domain Socket) API configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenUdsConfig {
    /// Whether to enable the OpenUDS API.
    #[serde(default)]
    pub enabled: bool,
    /// Unix Domain Socket path.
    #[serde(default = "default_openuds_socket_path")]
    pub socket_path: String,
    /// Auth token for automatic authentication.
    /// Empty string = CLI approve mode.
    #[serde(default)]
    pub auth_token: String,
    /// Maximum concurrent UDS connections.
    #[serde(default = "default_openuds_max_connections")]
    pub max_connections: u32,
    /// Event buffer size per connection.
    #[serde(default = "default_openuds_event_buffer_size")]
    pub event_buffer_size: u32,
    /// Heartbeat interval in seconds.
    #[serde(default = "default_openuds_heartbeat_interval_secs")]
    pub heartbeat_interval_secs: u64,
}

impl Default for OpenUdsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            socket_path: default_openuds_socket_path(),
            auth_token: String::new(),
            max_connections: default_openuds_max_connections(),
            event_buffer_size: default_openuds_event_buffer_size(),
            heartbeat_interval_secs: default_openuds_heartbeat_interval_secs(),
        }
    }
}

fn default_openuds_socket_path() -> String {
    "/var/run/pmp-openuds.sock".to_string()
}

fn default_openuds_max_connections() -> u32 {
    4
}

fn default_openuds_event_buffer_size() -> u32 {
    1024
}

fn default_openuds_heartbeat_interval_secs() -> u64 {
    60
}

fn default_phira_api() -> String {
    "https://phira.5wyxi.com".to_string()
}
fn default_trusted_forwarded_http_port() -> u16 {
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
fn default_ready_countdown_secs() -> u64 { 60 }
fn default_room_creation_enabled() -> bool { true }
fn default_playing_timeout_offset_secs() -> u64 { 60 }
fn default_graceful_shutdown_timeout_secs() -> u64 {
    15
}

#[cfg(test)]
mod tests {
    use super::{PlusConfig, PlusConfigCli};
    use crate::plugin::WasmRuntimeConfig;
    use crate::RuntimeConfig;

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
            trusted_forwarded_http_port: None,
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
            trusted_forwarded_http_port: None,
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
        assert!(PlusConfig { max_sessions: 0, ..Default::default() }.validate().is_err());

        assert!(PlusConfig { max_pending_auth: PlusConfig::default().max_sessions + 1, ..Default::default() }.validate().is_err());

        assert!(PlusConfig { graceful_shutdown_timeout_secs: 0, ..Default::default() }.validate().is_err());
    }

    #[test]
    fn rejects_unmetered_or_unbounded_plugin_runtime() {
        assert!(PlusConfig { wasm_runtime: WasmRuntimeConfig { fuel_per_call: 0, ..Default::default() }, ..Default::default() }.validate().is_err());

        assert!(PlusConfig { wasm_runtime: WasmRuntimeConfig { event_queue_capacity: 0, ..Default::default() }, ..Default::default() }.validate().is_err());

        assert!(PlusConfig { wasm_runtime: WasmRuntimeConfig { call_timeout_ms: 0, ..Default::default() }, ..Default::default() }.validate().is_err());
    }

    #[test]
    fn rejects_invalid_runtime_batching_contract() {
        assert!(PlusConfig { runtime: RuntimeConfig { persistence_queue_capacity: 0, ..Default::default() }, ..Default::default() }.validate().is_err());
    }

    #[test]
    fn default_runtime_enables_dead_letter_journal() {
        let config = PlusConfig::default();
        assert_eq!(
            config.runtime.persistence_dead_letter_path.as_deref(),
            Some("data/persistence-dead-letter.jsonl")
        );
    }

    #[test]
    fn rejects_empty_dead_letter_path_but_allows_explicit_disable() {
        let mut config = PlusConfig::default();
        config.database_url = "postgres://localhost:5432/pmp_test".to_string();
        config.runtime.persistence_dead_letter_path = Some("   ".to_string());
        assert!(config.validate().is_err());

        config.runtime.persistence_dead_letter_path = None;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn redacted_config_hides_secrets() {
        let mut config = PlusConfig::default();
        config.database_url = "postgres://user:secret@localhost/db".to_string();
        let redacted = config.redacted_string();
        assert!(!redacted.contains("secret"), "redacted config should not contain secret: {redacted}");
        assert!(redacted.contains("****"), "redacted config should mask values");
    }

    #[test]
    fn compatibility_defaults_target_official_client() {
        let config = PlusConfig::default();
        assert!(config.compatibility.official_phira_client);
        assert_eq!(config.compatibility.minimum_response_latency_ms, 10);
        assert_eq!(config.compatibility.session_command_deadline_ms, 4500);
        // PMP45 P0-I: 提交后响应预算默认 1000ms（总 deadline 拆分为 3500ms
        // commit budget + 1000ms response budget）。
        assert_eq!(config.compatibility.commit_response_reserve_ms, 1000);
        // PMP44 P0-D: 认证绝对预算默认 5000ms，必须早于官方客户端约 7 秒 deadline。
        assert_eq!(config.compatibility.auth_deadline_ms, 5000);
        // ProtocolHack 默认回退到 minimum_response_latency_ms（10ms），
        // 可显式设为 0 做差分测试。
        assert_eq!(config.compatibility.protocol_hack_delay_ms, None);
        // PMP44 P0-G: 认证屏障默认上限——256 事件 / 1 MiB / 8000ms。
        assert_eq!(config.compatibility.gate_max_pending_events, 256);
        assert_eq!(config.compatibility.gate_max_pending_bytes, 1_048_576);
        assert_eq!(config.compatibility.gate_max_auth_duration_ms, 8000);
    }

    #[test]
    fn compatibility_validates_deadline_bounds() {
        let config = PlusConfig {
            compatibility: crate::CompatibilityConfig {
                session_command_deadline_ms: 7000, // at or above client ~7s
                ..Default::default()
            },
            database_url: "postgres://localhost/db".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_err());

        let config = PlusConfig {
            compatibility: crate::CompatibilityConfig {
                minimum_response_latency_ms: 2000,
                ..Default::default()
            },
            database_url: "postgres://localhost/db".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn commit_response_reserve_validates_bounds_and_cross_field() {
        // 低于下限（200ms）必须校验失败。
        let config = PlusConfig {
            compatibility: crate::CompatibilityConfig {
                commit_response_reserve_ms: 199,
                ..Default::default()
            },
            database_url: "postgres://localhost/db".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
        // 高于上限（2500ms）必须校验失败。
        let config = PlusConfig {
            compatibility: crate::CompatibilityConfig {
                commit_response_reserve_ms: 2501,
                ..Default::default()
            },
            database_url: "postgres://localhost/db".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
        // PMP45 P0-I 跨字段：reserve 大于等于总命令 deadline 时 commit deadline
        // 落在接收点之前，命令永远无法提交——必须校验失败。
        let config = PlusConfig {
            compatibility: crate::CompatibilityConfig {
                session_command_deadline_ms: 1000,
                commit_response_reserve_ms: 1000,
                ..Default::default()
            },
            database_url: "postgres://localhost/db".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
        // 默认值必须在合法范围内（补全 database_url 以满足 validate 前置条件）。
        let config = PlusConfig {
            database_url: "postgres://localhost/db".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn auth_deadline_default_is_5000() {
        let config = PlusConfig::default();
        assert_eq!(config.compatibility.auth_deadline_ms, 5000);
    }

    #[test]
    fn auth_deadline_rejects_out_of_range() {
        // 低于下限（1000ms）必须校验失败。
        let config = PlusConfig {
            compatibility: crate::CompatibilityConfig {
                auth_deadline_ms: 999,
                ..Default::default()
            },
            database_url: "postgres://localhost/db".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
        // 达到/超过官方客户端约 7 秒 deadline 必须校验失败。
        let config = PlusConfig {
            compatibility: crate::CompatibilityConfig {
                auth_deadline_ms: 6501,
                ..Default::default()
            },
            database_url: "postgres://localhost/db".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn gate_config_rejects_out_of_range() {
        // 事件数量低于下限（64）必须校验失败。
        let config = PlusConfig {
            compatibility: crate::CompatibilityConfig {
                gate_max_pending_events: 16,
                ..Default::default()
            },
            database_url: "postgres://localhost/db".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
        // 字节上限低于下限（64KiB）必须校验失败。
        let config = PlusConfig {
            compatibility: crate::CompatibilityConfig {
                gate_max_pending_bytes: 1024,
                ..Default::default()
            },
            database_url: "postgres://localhost/db".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
        // 认证屏障持续时间超出上限（15000ms）必须校验失败。
        let config = PlusConfig {
            compatibility: crate::CompatibilityConfig {
                gate_max_auth_duration_ms: 16000,
                ..Default::default()
            },
            database_url: "postgres://localhost/db".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
        // 默认值必须在合法范围内（补全 database_url 以满足 validate 前置条件）。
        let config = PlusConfig {
            database_url: "postgres://localhost/db".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn official_compat_rejects_zero_min_latency_unless_hack_zero() {
        // PMP44 P1 §33: 默认配置（补全 database_url 以满足前置校验）必须通过。
        let config = PlusConfig {
            database_url: "postgres://localhost/db".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_ok());

        // official_phira_client=true 且 minimum_response_latency_ms=0 且
        // protocol_hack_delay_ms=None（回退到 0ms 地板）必须校验失败。
        let config = PlusConfig {
            compatibility: crate::CompatibilityConfig {
                official_phira_client: true,
                minimum_response_latency_ms: 0,
                protocol_hack_delay_ms: None,
                ..Default::default()
            },
            database_url: "postgres://localhost/db".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_err());

        // 显式 protocol_hack_delay_ms=Some(0)（差分测试）表示用户有意零延迟，放行。
        let config = PlusConfig {
            compatibility: crate::CompatibilityConfig {
                official_phira_client: true,
                minimum_response_latency_ms: 0,
                protocol_hack_delay_ms: Some(0),
                ..Default::default()
            },
            database_url: "postgres://localhost/db".to_string(),
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }
}

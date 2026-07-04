//! Server configuration and runtime state.

use crate::ban::BanManager;
use crate::benchmark_report::{BenchmarkMode, BenchmarkReport};
use crate::benchmark_snapshot::BenchmarkReportStore;
use crate::event_bus::MpEvent;
use crate::extensions::ExtensionManager;
use crate::plugin::{PluginEvent, PluginManager, WasmRuntimeConfig};
use crate::plugin_http::{PluginHttpServer, SseHub};
use anyhow::Result;
use phira_mp_common::{generate_secret_key, RoomEvent, RoomId, ServerCommand};
use phira_mp_plus_server_api as api;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::{atomic::Ordering, Arc, Weak},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, Notify, RwLock},
};
use tracing::{info, trace, warn};
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

pub type SafeMap<K, V> = RwLock<HashMap<K, V>>;

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

/// 自旋获取 tokio RwLock 读锁（同步上下文使用，如 Web API 的 try_read 路径）
macro_rules! read_lock {
    ($lock:expr) => {{
        loop {
            match $lock.try_read() {
                Ok(g) => break g,
                Err(_) => std::thread::yield_now(),
            }
        }
    }};
}
pub type IdMap<V> = SafeMap<Uuid, V>;

/// Phira-mp+ 增强配置（支持 YAML 文件、环境变量、CLI 参数三层覆盖）
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
    /// Set to 0 (default) to disable.  Typical value: 12344
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
    /// 轮次 Touches/Judges 数据保留天数（0 = 不保留）
    #[serde(default = "default_retention_days")]
    pub round_data_retention_days: u32,
    /// PostgreSQL 数据库连接 URL（如 postgres://user:pass@localhost/dbname）
    /// 未设置时保留旧 JSON/文件回退，但统一结构化持久化需要配置 PostgreSQL。
    #[serde(default)]
    pub database_url: Option<String>,
    /// 统一持久化数据保留天数（0 = 不自动清理 PostgreSQL 历史数据）。
    #[serde(default = "default_persistence_retention_days")]
    pub persistence_retention_days: u32,
    /// Touches/Judges 高频遥测在 PostgreSQL 中的独立保留天数。
    /// 未设置时遵循 `persistence_retention_days`；设置为 0 表示不自动清理遥测。
    #[serde(default)]
    pub touch_judge_retention_days: Option<u32>,
    /// 拥有游戏内 `_命令` 入口和管理 WIT/API 的 Phira 用户 ID 列表。
    #[serde(default)]
    pub admin_phira_ids: Vec<i32>,
    /// 压测使用的 Phira token 列表。可在配置文件中直接填写，或通过 CLI 写入 data/benchmark-auth.json。
    #[serde(default)]
    pub benchmark_phira_tokens: Vec<String>,
    /// WASM sandbox/resource limits.
    #[serde(default)]
    pub wasm_runtime: WasmRuntimeConfig,
    /// Runtime v2 internal policy. This is intentionally config-driven so the
    /// test-stage server can change persistence/telemetry behavior without
    /// adding more CLI surface area.
    #[serde(default)]
    pub runtime_v2: RuntimeV2Config,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeV2Config {
    /// Bounded queue size for Runtime v2 PersistenceWorker.
    #[serde(default = "default_runtime_persistence_queue_capacity")]
    pub persistence_queue_capacity: usize,
    /// Batcher policy for production Touch/Judge telemetry.
    #[serde(default)]
    pub telemetry_batcher: crate::telemetry_batcher::TelemetryBatcherPolicy,
    /// Startup cutover mode for production Touch/Judge persistence.
    #[serde(default)]
    pub telemetry_cutover_mode: crate::telemetry_batcher::TelemetryCutoverMode,
    /// Unified Phira HTTP retry/timeout/circuit-breaker policy.
    #[serde(default)]
    pub phira_http: crate::phira_client::PhiraHttpPolicyConfig,
}

impl Default for RuntimeV2Config {
    fn default() -> Self {
        Self {
            persistence_queue_capacity: default_runtime_persistence_queue_capacity(),
            telemetry_batcher: crate::telemetry_batcher::TelemetryBatcherPolicy::default(),
            telemetry_cutover_mode: crate::telemetry_batcher::TelemetryCutoverMode::default(),
            phira_http: crate::phira_client::PhiraHttpPolicyConfig::default(),
        }
    }
}

fn default_http_port() -> u16 {
    12347
}
fn default_plugins_dir() -> String {
    "plugins".to_string()
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
    4096
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
            proxy_protocol_port: 0,
        }
    }
}

impl PlusConfig {
    /// 从 YAML 文件加载配置
    pub fn from_yaml(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read config '{path}': {e}"))?;
        let config: Self = serde_yaml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("failed to parse config '{path}': {e}"))?;
        Ok(config)
    }

    /// 启动时校验配置合法性。
    pub fn validate(&self) -> Result<(), crate::error::AppError> {
        use crate::error::AppError;
        // Port range
        if self.port == 0 {
            return Err(AppError::ConfigValidation(format!(
                "端口 {} 超出范围 (1-65535)",
                self.port
            )));
        }
        if self.http_port == 0 {
            return Err(AppError::ConfigValidation(format!(
                "HTTP 端口 {} 超出范围 (1-65535)",
                self.http_port
            )));
        }
        if self.port == self.http_port {
            return Err(AppError::ConfigValidation(
                "TCP 端口和 HTTP 端口不能相同".into(),
            ));
        }
        // Plugin directory
        if !std::path::Path::new(&self.plugins_dir).exists() {
            return Err(AppError::ConfigValidation(format!(
                "插件目录不存在: {}",
                self.plugins_dir
            )));
        }
        // Rate limiter
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
        // Retention
        if self.round_data_retention_days == 0 && self.persistence_retention_days == 0 {
            // 允许 0 = 不清理，不需要报错
        }
        Ok(())
    }

    /// 合并 CLI 参数覆盖（非默认值的 CLI 参数覆盖 YAML 配置）
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
        if cli.no_cli {
            self.cli_enabled = false;
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
    pub no_cli: bool,
    pub log_file: String,
}

/// Runtime v2 benchmark request.
///
/// Simulation remains the default benchmark path and is handled by
/// [`crate::simulation::SimulationManager`]. This queue is only for explicit
/// benchmark modes: real network tests and hybrid Phira probes.
pub struct BenchRequest {
    pub kind: BenchRequestKind,
    pub result_tx: std::sync::mpsc::Sender<String>,
}

impl BenchRequest {
    pub fn real(
        duration_secs: u64,
        target_rooms: usize,
        result_tx: std::sync::mpsc::Sender<String>,
    ) -> Self {
        Self {
            kind: BenchRequestKind::Real {
                duration_secs,
                target_rooms,
            },
            result_tx,
        }
    }

    pub fn hybrid(
        config: HybridBenchmarkConfig,
        result_tx: std::sync::mpsc::Sender<String>,
    ) -> Self {
        Self {
            kind: BenchRequestKind::Hybrid(config),
            result_tx,
        }
    }

    pub fn timeout_secs(&self) -> u64 {
        match &self.kind {
            BenchRequestKind::Real { duration_secs, .. } => duration_secs.saturating_add(120),
            BenchRequestKind::Hybrid(config) => config.timeout_secs(),
        }
    }
}

pub enum BenchRequestKind {
    Real {
        duration_secs: u64,
        target_rooms: usize,
    },
    Hybrid(HybridBenchmarkConfig),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridBenchmarkConfig {
    /// Compatibility window used for CLI timeout/reporting. This is not a
    /// load duration until hybrid grows a sustained runner.
    pub duration_secs: u64,
    /// Optional `/me` probe. Requires at least one benchmark token.
    pub authenticate: bool,
    /// Optional `/chart/<id>` probe.
    pub chart_lookup: Option<i32>,
    /// Optional `/record/<id>` probe.
    pub record_lookup: Option<i32>,
    /// Reserved write-path switch. It is parsed and reported, but intentionally
    /// blocked until upload semantics are made explicit.
    pub upload_record: bool,
    /// Optional Phira endpoint override for hybrid probes only.
    pub endpoint_override: Option<String>,
}

impl Default for HybridBenchmarkConfig {
    fn default() -> Self {
        Self {
            duration_secs: 30,
            authenticate: false,
            chart_lookup: None,
            record_lookup: None,
            upload_record: false,
            endpoint_override: None,
        }
    }
}

impl HybridBenchmarkConfig {
    pub fn timeout_secs(&self) -> u64 {
        self.duration_secs.clamp(5, 300).saturating_add(120)
    }

    pub fn touches_phira(&self) -> bool {
        self.authenticate
            || self.chart_lookup.is_some()
            || self.record_lookup.is_some()
            || self.upload_record
    }

    pub fn enabled_switches(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.authenticate {
            out.push("authenticate".to_string());
        }
        if let Some(id) = self.chart_lookup {
            out.push(format!("chart_lookup={id}"));
        }
        if let Some(id) = self.record_lookup {
            out.push(format!("record_lookup={id}"));
        }
        if self.upload_record {
            out.push("upload_record".to_string());
        }
        out
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(5..=300).contains(&self.duration_secs) {
            return Err("hybrid duration must be between 5 and 300 seconds".to_string());
        }
        if let Some(id) = self.chart_lookup {
            if id <= 0 {
                return Err("hybrid chart_lookup id must be positive".to_string());
            }
        }
        if let Some(id) = self.record_lookup {
            if id <= 0 {
                return Err("hybrid record_lookup id must be positive".to_string());
            }
        }
        if let Some(endpoint) = &self.endpoint_override {
            normalize_phira_api_endpoint(endpoint)?;
        }
        Ok(())
    }
}

const BENCH_AUTH_FILE: &str = "data/benchmark-auth.json";

#[derive(Debug, Default, Deserialize, Serialize)]
struct BenchmarkAuthFile {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    tokens: Vec<String>,
}

fn sanitize_benchmark_tokens<I>(items: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut out: Vec<String> = Vec::new();
    for item in items {
        for token in item.split(|ch: char| ch == ',' || ch == ';' || ch.is_whitespace()) {
            let token = token.trim();
            if token.is_empty() || token.len() > 32 {
                continue;
            }
            if !out.iter().any(|existing| existing.as_str() == token) {
                out.push(token.to_string());
            }
        }
    }
    out
}

fn load_benchmark_tokens(config: &PlusConfig) -> Vec<String> {
    let configured = sanitize_benchmark_tokens(config.benchmark_phira_tokens.clone());
    if !configured.is_empty() {
        return configured;
    }

    let Ok(content) = std::fs::read_to_string(BENCH_AUTH_FILE) else {
        return Vec::new();
    };
    match serde_json::from_str::<BenchmarkAuthFile>(&content) {
        Ok(file) => {
            let mut tokens = file.tokens;
            if let Some(token) = file.token {
                tokens.push(token);
            }
            sanitize_benchmark_tokens(tokens)
        }
        Err(err) => {
            warn!(
                path = BENCH_AUTH_FILE,
                "failed to parse benchmark auth file: {err}"
            );
            Vec::new()
        }
    }
}

fn save_benchmark_tokens(tokens: &[String]) -> Result<(), String> {
    std::fs::create_dir_all("data").map_err(|e| format!("create data directory: {e}"))?;
    let file = BenchmarkAuthFile {
        token: None,
        tokens: tokens.to_vec(),
    };
    let payload = serde_json::to_string_pretty(&file)
        .map_err(|e| format!("serialize benchmark auth: {e}"))?;
    std::fs::write(BENCH_AUTH_FILE, payload).map_err(|e| format!("write {BENCH_AUTH_FILE}: {e}"))
}

#[derive(Debug, Deserialize)]
struct RemotePhiraUserInfo {
    id: i32,
    name: String,
}

async fn fetch_phira_user_name(endpoint: &str, token: &str) -> Option<(i32, String)> {
    let endpoint = endpoint.trim_end_matches('/');
    let url = format!("{endpoint}/me");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()?;
    let response = client
        .get(url)
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let info = response.json::<RemotePhiraUserInfo>().await.ok()?;
    Some((info.id, info.name))
}

async fn fetch_phira_chart(endpoint: &str, chart_id: i32) -> Option<Chart> {
    let endpoint = endpoint.trim_end_matches('/');
    let url = format!("{endpoint}/chart/{chart_id}");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()?;
    client
        .get(url)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json::<Chart>()
        .await
        .ok()
}

/// Phira-mp+ 服务器状态
pub struct PlusServerState {
    pub config: PlusConfig,
    /// Hot-reloadable runtime config.
    pub live_config: Arc<RwLock<LiveConfig>>,
    pub sessions: IdMap<Arc<super::session::Session>>,
    pub users: SafeMap<i32, Arc<super::session::User>>,
    pub rooms: SafeMap<RoomId, Arc<super::room::Room>>,
    pub lost_con_tx: mpsc::Sender<Uuid>,
    pub plugin_manager: Arc<PluginManager>,
    pub extensions: Arc<ExtensionManager>,
    pub ban_manager: Arc<BanManager>,
    pub shutdown: Notify,
    /// 连接速率限制器（按 IP）
    pub connection_limiter: super::rate_limiter::ConnectionRateLimiter,
    /// 轮次数据持久化存储（Touches/Judges 按轮次写入磁盘）
    pub round_store: Arc<super::round_store::RoundStore>,
    /// 用户房间访问历史: user_id → (room_id, room_uuid, join_timestamp_ms)
    pub user_room_history: SafeMap<i32, Vec<(String, String, i64)>>,
    /// 压测请求发送端（背景 tokio 任务消费）
    pub bench_tx: tokio::sync::mpsc::UnboundedSender<BenchRequest>,
    /// Runtime v2 命令元数据注册表。Step 1 仅用于 help/补全/未来统一入口，不改变现有执行逻辑。
    pub command_registry: Arc<crate::command_registry::CommandRegistry>,
    /// Runtime v2 事件总线。当前记录新增 Runtime v2 事件和诊断统计，旧路径仍逐步迁移。
    pub event_bus: Arc<crate::event_bus::EventBus>,
    /// Runtime v2 benchmark 报告只读快照。CLI/TUI/Web 诊断读这里，不解析 EventBus 字符串。
    pub benchmark_reports: Arc<BenchmarkReportStore>,
    /// Runtime v2 Simulation 状态管理器。当前只创建隔离 shadow world，不污染真实 rooms/users。
    pub simulation: Arc<crate::simulation::SimulationManager>,
    /// Runtime v2 持久化 Worker 骨架。现有 db.rs 写入路径暂不迁移。
    pub persistence_worker: Arc<crate::persistence_worker::PersistenceWorker>,
    /// Runtime v2 Actor 模型迁移蓝图。当前是诊断/路线层，不替换真实协议热路径。
    pub actor_runtime: Arc<crate::actor_runtime::ActorRuntime>,
    /// Runtime v2 Room command gateway. Admin/StateQuery room writes route through this facade while the gateway gradually moves commands into per-room mailboxes.
    pub room_commands: Arc<crate::room_actor::RoomCommandGateway>,
    /// Runtime v2 Phira HTTP client. Authentication/chart/record paths should converge here before hybrid/real benchmark expansion.
    pub phira_client: Arc<crate::phira_client::PhiraRetryClient>,
    /// Runtime v2 master workboard. Keeps the original long-term targets visible in CLI/TUI/API diagnostics.
    pub runtime_plan: Arc<crate::runtime_plan::RuntimePlan>,
    /// 压测用 Phira token 列表（来自配置或 CLI 命令）。
    pub bench_tokens: RwLock<Vec<String>>,
    /// 管理员 Phira ID 集合。可由配置、PostgreSQL 设置、CLI/WIT 动态修改。
    pub admin_ids: RwLock<HashSet<i32>>,
    /// 房间 monitor key（与 phira-web-monitor 共享密钥）
    pub room_monitor_key: Vec<u8>,
    pub events: Arc<SseHub>,
    /// 房间 monitor 会话（唯一）
    pub room_monitor: RwLock<Option<Weak<super::session::Session>>>,
    /// 游戏 monitor 会话（按用户 ID）
    pub game_monitors: SafeMap<i32, Weak<super::session::Session>>,
    /// PostgreSQL 数据库管理器。
    pub db_manager: super::db::DbManager,
}

/// Phira-mp+ 服务器
pub struct PlusServer {
    pub state: Arc<PlusServerState>,
    listener: TcpListener,
    _lost_con_handle: tokio::task::JoinHandle<()>,
}

fn spawn_runtime_event_observer(event_bus: Arc<crate::event_bus::EventBus>) {
    let mut rx = event_bus.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    trace!(kind = event.kind(), summary = %event.summary(), "runtime event observed");
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "runtime event observer lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Subscribe to EventBus events and drive real side effects.
///
/// This is the Runtime v2 event-driven subscriber that gradually replaces direct
/// calls in session.rs / runner.rs. Old paths remain intact (dual-write) until
/// the EventBus path is proven stable.
fn spawn_event_subscribers(state: &Arc<PlusServerState>) {
    let mut rx = state.event_bus.subscribe();
    let state_clone = Arc::clone(state);
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    match &event {
                        MpEvent::SimulationStarted { .. } => {
                            state_clone
                                .broadcast_system_message(
                                    "服务器正在进行性能测试，期间可能出现短暂卡顿。",
                                )
                                .await;
                        }
                        MpEvent::SimulationStopped { .. } => {
                            state_clone
                                .broadcast_system_message("性能测试已结束。")
                                .await;
                        }
                        MpEvent::BenchmarkCompleted { report } => {
                            use crate::persistence::message::PersistenceEvent;
                            let persistence_event = PersistenceEvent::BenchmarkReport {
                                report: report.clone(),
                            };
                            let _ = state_clone
                                .persistence_worker
                                .enqueue(persistence_event)
                                .await;
                        }
                        MpEvent::UserConnected {
                            user_id,
                            user_name,
                            user_ip,
                            user_language,
                        } => {
                            // Plugin dispatch now handled by EventBus plugin subscriber.
                            state_clone.event_bus.publish(
                                crate::event_bus::MpEvent::PluginEventDispatched(
                                    std::sync::Arc::new(PluginEvent::UserConnect {
                                        user_id: *user_id,
                                        user_name: user_name.clone(),
                                        user_ip: user_ip.clone(),
                                    }),
                                ),
                            );

                            // 2. Record in DB if available
                            if let Some(db) = crate::internal_hooks::DB.get() {
                                db.record_user_seen_sync(
                                    *user_id,
                                    user_name,
                                    user_language,
                                    Some(user_ip.clone()),
                                );
                            }

                            // 3. Track player and playtime
                            crate::internal_hooks::track_player(*user_id, user_name);
                            crate::internal_hooks::playtime_connect(*user_id);

                            // 4. Welcome message
                            let online = {
                                let rooms = state_clone.rooms.read().await;
                                let mut in_room = std::collections::HashSet::new();
                                for (_, room) in rooms.iter() {
                                    for u in room.users().await {
                                        in_room.insert(u.id);
                                    }
                                }
                                in_room.len()
                            };
                            crate::internal_hooks::send_welcome(
                                *user_id,
                                user_name,
                                online,
                                &state_clone,
                            );
                        }
                        _ => {}
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "event subscriber lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

/// Subscribe to EventBus PluginEventDispatched events and dispatch to plugins.
///
/// This brings plugin dispatch into the EventBus pipeline, so new event sources
/// only need to publish to the bus instead of calling pm.trigger() directly.
/// The subscriber checks has_plugins() to avoid unnecessary work when no
/// plugins are loaded, and spawns a separate task per event so slow plugin
/// WASM code never blocks the subscriber loop.
fn spawn_plugin_subscriber(state: &Arc<PlusServerState>) {
    let mut rx = state.event_bus.subscribe();
    let pm = Arc::clone(&state.plugin_manager);
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if let crate::event_bus::MpEvent::PluginEventDispatched(plugin_event) = &event {
                        if !pm.has_plugins().await {
                            continue;
                        }
                        // Spawn independently so slow plugin code doesn't block
                        // the subscriber from processing subsequent events.
                        let pm = Arc::clone(&pm);
                        let ev = Arc::clone(plugin_event);
                        tokio::spawn(async move {
                            pm.trigger(&ev).await;
                        });
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "plugin subscriber lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

impl PlusServer {
    /// 创建新的 Phira-mp+ 服务器
    pub async fn new(config: PlusConfig) -> Result<Self> {
        let addrs: &[std::net::SocketAddr] = &[std::net::SocketAddr::new(
            std::net::Ipv6Addr::UNSPECIFIED.into(),
            config.port,
        )];

        let listener = TcpListener::bind(addrs).await?;
        for addr in addrs {
            info!("Phira-mp+ Local Address: http://{}", addr);
        }

        let (lost_con_tx, mut lost_con_rx) = mpsc::channel(16);

        // 初始化扩展管理器
        let extensions = Arc::new(ExtensionManager::new(config.extensions_file.clone()));

        // 初始化插件管理器
        let plugin_manager = Arc::new(PluginManager::new(
            &config.plugins_dir,
            Arc::clone(&extensions),
            config.wasm_runtime.clone(),
        ));

        // 初始化黑名单管理器
        let ban_manager = Arc::new(BanManager::new(Arc::clone(&extensions)));

        let http_port = config.http_port;
        let rate_limit = config.connection_rate_limit;
        let rate_window = config.connection_rate_window;
        let retention_days = config.round_data_retention_days;
        let bench_tokens = load_benchmark_tokens(&config);
        let mut admin_ids: HashSet<i32> = config.admin_phira_ids.iter().copied().collect();
        if admin_ids.is_empty() {
            if let Ok(raw) = std::fs::read_to_string("data/admin-phira-ids.json") {
                if let Ok(ids) = serde_json::from_str::<Vec<i32>>(&raw) {
                    admin_ids.extend(ids.into_iter().filter(|id| *id > 0));
                }
            }
        }
        let (bench_tx, bench_rx) = tokio::sync::mpsc::unbounded_channel::<BenchRequest>();

        let runtime_v2 = config.runtime_v2.clone();
        let command_registry = Arc::new(crate::command_registry::runtime_v2_registry());
        let event_bus = Arc::new(crate::event_bus::EventBus::new_with_trace(
            crate::runtime_diagnostics::EVENT_BUS_CHANNEL_CAPACITY,
            crate::runtime_diagnostics::EVENT_TRACE_WINDOW,
        ));
        spawn_runtime_event_observer(Arc::clone(&event_bus));
        let benchmark_reports = Arc::new(BenchmarkReportStore::new(
            crate::runtime_diagnostics::BENCHMARK_REPORT_HISTORY,
        ));
        let simulation = Arc::new(crate::simulation::SimulationManager::new());
        let persistence_worker = crate::persistence_worker::PersistenceWorker::spawn_with_policy(
            runtime_v2.persistence_queue_capacity,
            runtime_v2.telemetry_batcher.clone(),
            runtime_v2.telemetry_cutover_mode,
        );
        crate::persistence_worker::spawn_event_bus_mirror(
            Arc::clone(&event_bus),
            Arc::clone(&persistence_worker),
        );
        let actor_runtime = Arc::new(crate::actor_runtime::ActorRuntime::new_blueprint());
        let room_commands = Arc::new(crate::room_actor::RoomCommandGateway::new());
        let phira_client = Arc::new(crate::phira_client::PhiraRetryClient::new(
            runtime_v2.phira_http.clone().into_policy(),
        )?);
        let runtime_plan = Arc::new(crate::runtime_plan::RuntimePlan::master_plan());
        let events = Arc::new(SseHub::new());
        // Capture config fields before config is consumed by state
        let proxy_protocol_port = config.proxy_protocol_port;
        // Initialize database connection early so it's available throughout
        let db_manager = super::db::DbManager::new(config.database_url.as_deref()).await;
        let live_config = Arc::new(RwLock::new(LiveConfig::from_full(&config)));
        let state = Arc::new(PlusServerState {
            config,
            live_config,
            sessions: IdMap::default(),
            users: SafeMap::default(),
            rooms: SafeMap::default(),
            lost_con_tx,
            plugin_manager,
            extensions,
            ban_manager,
            shutdown: Notify::new(),
            connection_limiter: super::rate_limiter::ConnectionRateLimiter::new(
                rate_limit,
                rate_window,
            ),
            round_store: Arc::new(super::round_store::RoundStore::new("data", retention_days)),
            user_room_history: SafeMap::default(),
            bench_tx: bench_tx.clone(),
            command_registry,
            event_bus,
            benchmark_reports,
            simulation,
            persistence_worker,
            actor_runtime,
            room_commands,
            phira_client,
            runtime_plan,
            bench_tokens: RwLock::new(bench_tokens),
            admin_ids: RwLock::new(admin_ids),
            room_monitor_key: generate_secret_key("room_monitor", 64).unwrap_or_default(),
            room_monitor: RwLock::new(None),
            game_monitors: SafeMap::default(),
            events,
            db_manager,
        });
        spawn_event_subscribers(&state);
        spawn_plugin_subscriber(&state);
        state.room_commands.start_mailbox(Arc::clone(&state), 1024);
        state
            .actor_runtime
            .mark_status(
                "room-actor",
                crate::actor_runtime::ActorBoundaryStatus::WriteRouted,
                "set_lock/set_cycle/set_host/close/kick/start/cancel cross a per-room mailbox registry; gateway internals are now split for typed command migration",
            )
            .await;
        let bench_state = Arc::clone(&state);
        tokio::spawn(async move {
            let mut bench_rx = bench_rx;
            while let Some(request) = bench_rx.recv().await {
                let bs = Arc::clone(&bench_state);
                let output = match request.kind {
                    BenchRequestKind::Real {
                        duration_secs,
                        target_rooms,
                    } => bs.run_benchmark_network(duration_secs, target_rooms).await,
                    BenchRequestKind::Hybrid(config) => bs.run_benchmark_hybrid(config).await,
                };
                let _ = request.result_tx.send(output);
            }
        });

        let lost_con_state = Arc::clone(&state);
        let lost_con_handle = tokio::spawn(async move {
            while let Some(id) = lost_con_rx.recv().await {
                warn!("lost connection with {id}");
                let session_opt = lost_con_state.sessions.write().await.remove(&id);
                if let Some(session) = session_opt {
                    let user_ref = {
                        let session_guard = session.user.session.read().await;
                        session_guard
                            .as_ref()
                            .is_some_and(|it| it.ptr_eq(&Arc::downgrade(&session)))
                    };
                    if user_ref {
                        Arc::clone(&session.user).dangle().await;
                    }
                }
            }
        });

        // 初始化黑名单扩展字段
        state.ban_manager.register_fields().await;

        // 设置发送聊天消息能力（供插件使用）
        let s = Arc::clone(&state);
        state
            .plugin_manager
            .set_send_chat(Arc::new(move |uid, msg| {
                let s = Arc::clone(&s);
                tokio::spawn(async move {
                    let cmd =
                        phira_mp_common::ServerCommand::Message(phira_mp_common::Message::Chat {
                            user: 0,
                            content: msg,
                        });

                    // WASM `send.to_all` uses uid = 0.  Older code only looked up a
                    // concrete user id, so `send.to_all` could silently send to no
                    // one.  Clone user Arcs before awaiting to avoid holding the
                    // global users lock across network sends.
                    if uid == 0 {
                        let recipients = {
                            let users = s.users.read().await;
                            users.values().cloned().collect::<Vec<_>>()
                        };
                        for user in recipients {
                            user.try_send(cmd.clone()).await;
                        }
                        return;
                    }

                    let user = {
                        let users = s.users.read().await;
                        users.get(&uid).cloned()
                    };
                    if let Some(user) = user {
                        user.try_send(cmd).await;
                    }
                });
            }))
            .await;

        // 设置默认状态查询（所有插件可用 state.query host API）
        let state_query_all = api::ServerStateQuery::new({
            let s = Arc::clone(&state);
            move |method: &str, args: &[Value]| -> Result<Value, String> {
                server_state_query_inner(&s, method, args)
            }
        });
        state
            .plugin_manager
            .set_default_state(state_query_all)
            .await;

        let http_server = Arc::new(PluginHttpServer::new(
            http_port,
            proxy_protocol_port,
            Arc::clone(&state.events),
        ));
        let http_handle = api::HttpHandle::new(crate::plugin_http::HttpHandleBridge(Arc::clone(
            &http_server,
        )));
        state.plugin_manager.set_http_handle(http_handle).await;

        // 加载插件
        let plugin_count = state.plugin_manager.load_plugins().await.unwrap_or(0);
        info!("loaded {} plugin(s)", plugin_count);

        let state_for_webapi = Arc::clone(&state);
        let webapi_state_query = api::ServerStateQuery::new(move |method: &str, args: &[Value]| {
            server_state_query(&state_for_webapi, method, args)
        });
        let http_for_webapi = Arc::clone(&http_server);
        let state_for_webapi2 = Arc::clone(&state);
        let sq = webapi_state_query.clone();
        let sq_rooms = sq.clone();
        let state_rooms = Arc::clone(&state_for_webapi2);
        http_for_webapi.register_route_sync(
            "/api/rooms",
            Arc::new(move |_, _| {
                let rooms = sq_rooms.call("rooms.list", &[]).map_err(|e| (500u16, e))?;
                let online_count = state_rooms.users.try_read().map(|g| g.len()).unwrap_or(0);
                Ok(serde_json::json!({
                    "rooms": rooms,
                    "player_count": online_count,
                    "total_players": crate::internal_hooks::player_count(),
                }))
            }),
        );
        let runtime_state = Arc::clone(&state_for_webapi2);
        http_for_webapi.register_route_sync(
            "/api/runtime",
            Arc::new(move |_, _| {
                server_state_query_inner(&runtime_state, "runtime.status", &[])
                    .map_err(|e| (500u16, e))
            }),
        );
        let simulation_state = Arc::clone(&state_for_webapi2);
        http_for_webapi.register_route_sync(
            "/api/simulation",
            Arc::new(move |_, _| {
                server_state_query_inner(&simulation_state, "simulation.status", &[])
                    .map_err(|e| (500u16, e))
            }),
        );
        let simulation_world_state = Arc::clone(&state_for_webapi2);
        http_for_webapi.register_route_sync(
            "/api/simulation/world",
            Arc::new(move |_, _| {
                server_state_query_inner(
                    &simulation_world_state,
                    "simulation.world",
                    &[serde_json::json!(20)],
                )
                .map_err(|e| (500u16, e))
            }),
        );
        let benchmark_report_state = Arc::clone(&state_for_webapi2);
        http_for_webapi.register_route_sync(
            "/api/benchmark/reports",
            Arc::new(move |_, _| {
                server_state_query_inner(&benchmark_report_state, "benchmark.reports", &[])
                    .map_err(|e| (500u16, e))
            }),
        );
        let benchmark_history_state = Arc::clone(&state_for_webapi2);
        let bhs2 = Arc::clone(&state_for_webapi2);
        http_for_webapi.register_route_sync(
            "/api/benchmark/reports/history",
            Arc::new(move |_, params| {
                let mode = params
                    .get(0)
                    .map(|v| serde_json::Value::String(v.clone()));
                let limit = params
                    .get(1)
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(|v| serde_json::json!(v));
                let args: Vec<Value> = mode.into_iter().chain(limit.into_iter()).collect();
                server_state_query_inner(&benchmark_history_state, "benchmark.history", &args)
                    .map_err(|e| (500u16, e))
            }),
        );
        http_for_webapi.register_route_sync(
            "/api/benchmark/reports/history/<mode>",
            Arc::new(move |_, params| {
                let mode = params.get(0).cloned();
                let args: Vec<Value> = mode
                    .map(|m| serde_json::json!(m))
                    .into_iter()
                    .collect();
                server_state_query_inner(&bhs2, "benchmark.history", &args)
                    .map_err(|e| (500u16, e))
            }),
        );
        // GET /api/players/all — 所有连接过服务器的玩家
        http_for_webapi.register_route_sync(
            "/api/players/all",
            Arc::new(move |_, _| {
                let players: Vec<i32> = crate::internal_hooks::all_players()
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect();
                Ok(serde_json::json!({"total": players.len(), "players": players}))
            }),
        );
        let s2 = Arc::clone(&state_for_webapi2);
        http_for_webapi.register_route_sync(
            "/api/rooms/<name>",
            Arc::new(move |_, params| {
                let name = params.first().cloned().unwrap_or_default();
                server_state_query_inner(&s2, "rooms.by_name", &[serde_json::json!(name)])
                    .map_err(|e| (500u16, e))
            }),
        );
        let sq = webapi_state_query.clone();
        http_for_webapi.register_route_sync(
            "/api/user_name/<id>",
            Arc::new(move |_, params| {
                let uid: i32 = params.first().and_then(|p| p.parse().ok()).unwrap_or(0);
                sq.call("user_name", &[serde_json::json!(uid)])
                    .map_err(|e| (500u16, e))
            }),
        );
        // 内置 benchmark/benchmark-bind 已由 CLI 核心直接处理；
        // test.* WIT/host API 仍保留给插件和自动化调用。

        // 初始化内置功能（欢迎语/追踪/排行等）
        crate::internal_hooks::init_internal_hooks(&state, &http_server, &state.plugin_manager)
            .await;

        // 启动中央 HTTP 服务器（所有路由已注册完毕）
        let http_state = Arc::clone(&state);
        tokio::spawn(async move {
            http_server.start(http_state).await;
        });

        // 定期持久化 auth 缓存（避免每次认证都写盘）
        let persist_state = Arc::clone(&state);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                if let Err(e) = persist_state.extensions.persist().await {
                    warn!("auth cache persist: {e}");
                }
            }
        });

        // 轮次文件与统一 PostgreSQL 持久化定期清理（每小时检查一次）
        let telemetry_retention_days = state
            .config
            .touch_judge_retention_days
            .unwrap_or(state.config.persistence_retention_days);
        if retention_days > 0
            || state.config.persistence_retention_days > 0
            || telemetry_retention_days > 0
        {
            let cleanup_state = Arc::clone(&state);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                    cleanup_state.round_store.cleanup_expired().await;
                    if let Some(db) = crate::internal_hooks::DB.get() {
                        let telemetry_retention_days = cleanup_state
                            .config
                            .touch_judge_retention_days
                            .unwrap_or(cleanup_state.config.persistence_retention_days);
                        db.cleanup_expired(
                            cleanup_state.config.persistence_retention_days,
                            telemetry_retention_days,
                        )
                        .await;
                    }
                }
            });
        }

        Ok(Self {
            state,
            listener,
            _lost_con_handle: lost_con_handle,
        })
    }

    /// 接受新连接
    pub async fn accept(&self) -> Result<()> {
        let (stream, addr) = self.listener.accept().await?;
        let ip = addr.ip().to_string();

        // 连接速率限制检查
        if !self.state.connection_limiter.check(&ip).await {
            // 限流时直接静默丢弃，不产生日志
            return Ok(());
        }

        // 最大会话数快速检查（try_read 避免阻塞）
        if let Ok(guard) = self.state.sessions.try_read() {
            if guard.len() > 4096 {
                return Ok(());
            }
        }

        let id = Uuid::new_v4();
        let session = match tokio::time::timeout(
            std::time::Duration::from_secs(15),
            super::session::Session::new(id, addr, stream, Arc::clone(&self.state)),
        )
        .await
        {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                warn!("failed to create session for {ip}: {e:?}");
                return Ok(());
            }
            Err(_) => {
                warn!("session creation timed out for {ip}");
                return Ok(());
            }
        };

        // 写锁窗口最小化：仅插入 session
        if let Ok(mut guard) = self.state.sessions.try_write() {
            guard.insert(id, session);
        } else {
            self.state.sessions.write().await.insert(id, session);
        }

        trace!("connection from {ip} accepted, session {id}");
        Ok(())
    }

    /// 触发插件事件
    pub async fn trigger_event(&self, event: &PluginEvent) {
        self.state
            .event_bus
            .publish(crate::event_bus::MpEvent::PluginEventDispatched(
                std::sync::Arc::new(event.clone()),
            ));
    }

    /// 获取服务器统计信息
    pub async fn stats(&self) -> ServerStats {
        let user_count = self
            .state
            .users
            .read()
            .await
            .values()
            .filter(|user| user.id > 0)
            .count();
        let room_count = self.state.rooms.read().await.len();
        let session_count = self.state.sessions.read().await.len();
        let plugin_count = self.state.plugin_manager.list_plugins().await.len();

        ServerStats {
            users_online: user_count,
            active_rooms: room_count,
            active_sessions: session_count,
            loaded_plugins: plugin_count,
            port: self.state.config.port,
        }
    }
}

/// 服务器统计信息
pub struct ServerStats {
    pub users_online: usize,
    pub active_rooms: usize,
    pub active_sessions: usize,
    pub loaded_plugins: usize,
    pub port: u16,
}

impl PlusServerState {
    /// Publish a Runtime v2 event without changing the current side-effect path.
    ///
    /// Step 4 uses this as an observation-only mirror: plugins, room monitor,
    /// SSE and PostgreSQL direct writes continue to run exactly as before.
    pub fn publish_runtime_event(&self, event: crate::event_bus::MpEvent) -> usize {
        if let crate::event_bus::MpEvent::BenchmarkCompleted { report } = &event {
            self.benchmark_reports.record(report.clone());
        }
        self.event_bus.publish(event)
    }

    pub fn publish_benchmark_completed(&self, report: &BenchmarkReport) -> usize {
        self.publish_runtime_event(crate::event_bus::MpEvent::BenchmarkCompleted {
            report: report.clone(),
        })
    }

    fn append_benchmark_report(&self, out: &mut String, report: BenchmarkReport) {
        out.push_str(&report.render_text());
        self.publish_benchmark_completed(&report);
    }

    /// Broadcast a system chat message to every currently connected normal user.
    ///
    /// This is intentionally small and side-effect-only. Runtime v2 background
    /// tasks use it for simulation lifecycle notices without reaching into the
    /// CLI handler. User Arcs are cloned before awaiting so the global users lock
    /// is never held across network sends.
    pub async fn broadcast_system_message(&self, message: &str) -> usize {
        let recipients = {
            let users = self.users.read().await;
            users.values().cloned().collect::<Vec<_>>()
        };
        let cmd = ServerCommand::Message(phira_mp_common::Message::Chat {
            user: 0,
            content: format!("[系统广播] {message}"),
        });
        let mut sent = 0usize;
        for user in recipients {
            user.try_send(cmd.clone()).await;
            sent += 1;
        }
        sent
    }

    async fn mirror_room_event_to_runtime_bus(&self, event: &RoomEvent) {
        use crate::event_bus::MpEvent;

        match event {
            RoomEvent::CreateRoom { room, .. } => {
                let room_uuid = self
                    .rooms
                    .read()
                    .await
                    .get(room)
                    .map(|room| room.uuid)
                    .unwrap_or_else(Uuid::nil);
                self.publish_runtime_event(MpEvent::RoomCreated {
                    room_id: room.clone(),
                    room_uuid,
                });
                self.publish_runtime_event(MpEvent::RoomUpdated {
                    room_id: room.clone(),
                });
            }
            RoomEvent::UpdateRoom { room, data } => {
                self.publish_runtime_event(MpEvent::RoomUpdated {
                    room_id: room.clone(),
                });
                if let Some(host) = data.host {
                    self.publish_runtime_event(MpEvent::HostChanged {
                        room_id: room.clone(),
                        host: Some(host),
                    });
                }
                if let Some(lock) = data.lock {
                    self.publish_runtime_event(MpEvent::RoomLocked {
                        room_id: room.clone(),
                        locked: lock,
                    });
                }
                if let Some(cycle) = data.cycle {
                    self.publish_runtime_event(MpEvent::RoomCycled {
                        room_id: room.clone(),
                        cycle,
                    });
                }
                if let Some(chart_id) = data.chart {
                    self.publish_runtime_event(MpEvent::ChartSelected {
                        room_id: room.clone(),
                        chart_id,
                    });
                }
                if let Some(state) = data.state {
                    self.publish_runtime_event(MpEvent::RoomStateChanged {
                        room_id: room.clone(),
                        state: format!("{state:?}"),
                    });
                }
            }
            RoomEvent::JoinRoom { room, user } => {
                self.publish_runtime_event(MpEvent::RoomJoined {
                    room_id: room.clone(),
                    user_id: *user,
                });
            }
            RoomEvent::LeaveRoom { room, user } => {
                self.publish_runtime_event(MpEvent::RoomLeft {
                    room_id: room.clone(),
                    user_id: *user,
                });
            }
            RoomEvent::NewRound { room, .. } => {
                let room_ref = {
                    let rooms = self.rooms.read().await;
                    rooms.get(room).map(Arc::clone)
                };
                let round_id = if let Some(room_ref) = room_ref {
                    room_ref
                        .current_round_id
                        .read()
                        .await
                        .map(|round_id| round_id.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                } else {
                    "unknown".to_string()
                };
                self.publish_runtime_event(MpEvent::RoundCompleted {
                    room_id: room.clone(),
                    round_id,
                });
            }
        }
    }

    /// 获取房间 monitor 会话
    pub async fn get_room_monitor(&self) -> Option<Arc<super::session::Session>> {
        self.room_monitor
            .read()
            .await
            .as_ref()
            .and_then(Weak::upgrade)
    }
    /// 设置房间 monitor 会话
    pub async fn set_room_monitor(&self, session: Weak<super::session::Session>) {
        *self.room_monitor.write().await = Some(session);
    }
    /// 获取游戏 monitor 会话
    pub async fn get_game_monitor(&self, player_id: i32) -> Option<Arc<super::session::Session>> {
        self.game_monitors
            .read()
            .await
            .get(&player_id)
            .and_then(Weak::upgrade)
    }
    /// 设置游戏 monitor 会话
    pub async fn set_game_monitor(&self, player_id: i32, session: Weak<super::session::Session>) {
        self.game_monitors.write().await.insert(player_id, session);
    }

    pub async fn publish_room_event(&self, event: RoomEvent) {
        self.mirror_room_event_to_runtime_bus(&event).await;
        if let Some(db) = crate::internal_hooks::DB.get() {
            let (room_id, user_id) = match &event {
                RoomEvent::CreateRoom { room, .. }
                | RoomEvent::UpdateRoom { room, .. }
                | RoomEvent::NewRound { room, .. } => (Some(room.to_string()), None),
                RoomEvent::JoinRoom { room, user } | RoomEvent::LeaveRoom { room, user } => {
                    (Some(room.to_string()), Some(*user))
                }
            };
            db.record_room_event_sync(
                event.event_type(),
                room_id.clone(),
                user_id,
                event.clone().inner(),
            );
            if let Some(room_id) = room_id {
                if let Ok(rid) = room_id.clone().try_into() {
                    if let Some(room) = self.rooms.read().await.get(&rid).map(Arc::clone) {
                        self.persist_room_snapshot_background(room);
                    }
                }
            }
        }
        self.events.publish_room_event(event.clone());
        if let Some(monitor) = self.get_room_monitor().await {
            monitor.try_send(ServerCommand::RoomEvent(event)).await;
        }
    }

    /// 创建无人持久空房间。该房间没有初始房主，首个加入的普通玩家会静默成为房主。
    pub async fn create_empty_room(
        self: &Arc<Self>,
        room_id: &str,
        endpoint: Option<String>,
        persistent_empty: bool,
    ) -> Result<Value, String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let endpoint = match endpoint {
            Some(value) => Some(normalize_phira_api_endpoint(&value)?),
            None => None,
        };
        let max_users = self.config.max_users_per_room.unwrap_or(8);
        let room = Arc::new(crate::room::Room::new_empty(
            rid.clone(),
            Some(Arc::clone(&self.plugin_manager)),
            Arc::downgrade(self),
            max_users,
            Some(Arc::clone(&self.round_store)),
        ));
        room.set_persistent_empty(persistent_empty);
        if let Some(endpoint) = endpoint.clone() {
            room.set_phira_api_endpoint_override(Some(endpoint)).await;
        }
        {
            let mut rooms = self.rooms.write().await;
            if rooms.contains_key(&rid) {
                return Err("room already exists".to_string());
            }
            rooms.insert(rid.clone(), Arc::clone(&room));
        }
        self.publish_room_event(RoomEvent::CreateRoom {
            room: rid.clone(),
            data: crate::room::Room::into_data(&room).await,
        })
        .await;
        self.event_bus
            .publish(crate::event_bus::MpEvent::PluginEventDispatched(
                std::sync::Arc::new(PluginEvent::RoomCreate {
                    user_id: 0,
                    room_id: rid.to_string(),
                }),
            ));
        Ok(serde_json::json!({
            "ok": true,
            "room_id": rid.to_string(),
            "uuid": room.uuid.to_string(),
            "persistent_empty": room.is_persistent_empty(),
            "phira_api_endpoint": room.effective_phira_api_endpoint(self).await,
            "phira_api_endpoint_override": room.phira_api_endpoint_override().await,
        }))
    }

    pub async fn set_room_persistent_empty(
        &self,
        room_id: &str,
        persistent: bool,
    ) -> Result<Value, String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let room = {
            let rooms = self.rooms.read().await;
            rooms.get(&rid).map(Arc::clone).ok_or("room not found")?
        };
        room.set_persistent_empty(persistent);
        self.event_bus
            .publish(crate::event_bus::MpEvent::PluginEventDispatched(
                std::sync::Arc::new(PluginEvent::RoomModify {
                    user_id: 0,
                    room_id: rid.to_string(),
                    data: serde_json::json!({"action":"persistent_empty","value": persistent})
                        .to_string(),
                }),
            ));
        Ok(
            serde_json::json!({"ok": true, "room_id": rid.to_string(), "persistent_empty": persistent}),
        )
    }

    /// 如果房间没有真实房主或系统 `?` 房主，让指定普通玩家成为房主。
    /// `announce=false` 用于无人空房间首个玩家加入：只更新服务器状态与 host 标记，
    /// 不广播 `NewHost`，避免客户端在还没收到 JoinRoom 用户表前显示 `? 成为房主`。
    pub async fn assign_room_host_if_missing(
        &self,
        room: &Arc<crate::room::Room>,
        user: &Arc<super::session::User>,
        monitor: bool,
        announce: bool,
    ) -> bool {
        if monitor || room.has_host().await {
            return false;
        }
        room.set_host(Some(user.id), announce).await.is_ok()
    }

    fn persist_room_snapshot_background(&self, room: Arc<crate::room::Room>) {
        let fallback_endpoint = self.config.phira_api_endpoint.clone();
        tokio::spawn(async move {
            let Some(db) = crate::internal_hooks::DB.get() else {
                return;
            };
            if !db.is_active() {
                return;
            }
            let users = room.users().await;
            let monitors = room.monitors().await;
            let host_id = room
                .host_id()
                .await
                .or_else(|| room.is_system_host().then_some(-1));
            let chart = room.chart.read().await.clone();
            let state = match &*room.state.read().await {
                crate::room::InternalRoomState::SelectChart => {
                    serde_json::json!({"kind":"select_chart"})
                }
                crate::room::InternalRoomState::WaitForReady { started, .. } => {
                    let mut ready: Vec<i32> = started.iter().copied().collect();
                    ready.sort_unstable();
                    serde_json::json!({"kind":"wait_for_ready", "ready_users": ready})
                }
                crate::room::InternalRoomState::Playing { results, aborted } => {
                    let mut finished: Vec<i32> = results.keys().copied().collect();
                    finished.sort_unstable();
                    let mut aborted_users: Vec<i32> = aborted.iter().copied().collect();
                    aborted_users.sort_unstable();
                    serde_json::json!({"kind":"playing", "finished_users": finished, "aborted_users": aborted_users})
                }
            };
            let mut user_values = Vec::new();
            for u in &users {
                user_values.push(serde_json::json!({"id": u.id, "name": room.display_name(u).await, "monitor": false}));
            }
            let mut monitor_values = Vec::new();
            for u in &monitors {
                monitor_values.push(serde_json::json!({"id": u.id, "name": room.display_name(u).await, "monitor": true}));
            }
            let endpoint_override = room.phira_api_endpoint_override().await;
            let endpoint = endpoint_override.clone().unwrap_or(fallback_endpoint);
            let payload = serde_json::json!({
                "id": room.id.to_string(),
                "uuid": room.uuid.to_string(),
                "created_at": room.created_at,
                "updated_at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0),
                "host": host_id,
                "host_is_system": room.is_system_host(),
                "users": user_values,
                "monitors": monitor_values,
                "locked": room.is_locked(),
                "cycling": room.is_cycle(),
                "hidden": room.is_hidden(),
                "persistent_empty": room.is_persistent_empty(),
                "live": room.is_live(),
                "max_users": room.max_users_count(),
                "chart": chart.as_ref().map(|c| serde_json::json!({"id": c.id, "name": c.name.clone()})),
                "state": state,
                "current_round_id": room.current_round_id.read().await.as_ref().map(|id| id.to_string()),
                "phira_api_endpoint": endpoint,
                "phira_api_endpoint_override": endpoint_override,
            });
            db.record_room_snapshot_sync(room.id.to_string(), room.uuid.to_string(), payload);
        });
    }

    /// 刷新房间内展示用用户名与谱面名。只影响服务端 TUI/Web/欢迎语/历史展示；不改客户端本机 Phira API。
    pub async fn refresh_room_display_metadata(&self, room: &Arc<crate::room::Room>) {
        let endpoint = room.effective_phira_api_endpoint(self).await;
        Self::refresh_room_display_metadata_with_endpoint(room, endpoint).await;
    }

    async fn refresh_room_display_metadata_with_endpoint(
        room: &Arc<crate::room::Room>,
        endpoint: String,
    ) {
        let people = room
            .users()
            .await
            .into_iter()
            .chain(room.monitors().await.into_iter())
            .collect::<Vec<_>>();
        for user in people {
            let mut display = user.name.clone();
            if let Some(token) = user.auth_token().await {
                if let Some((remote_id, remote_name)) =
                    fetch_phira_user_name(&endpoint, &token).await
                {
                    if remote_id == user.id || user.id < 0 {
                        display = remote_name;
                    }
                }
            }
            room.set_display_name(user.id, display).await;
        }
        let chart_id = room.chart.read().await.as_ref().map(|chart| chart.id);
        if let Some(chart_id) = chart_id {
            if let Some(chart) = fetch_phira_chart(&endpoint, chart_id).await {
                *room.chart.write().await = Some(chart);
                room.publish_update(phira_mp_common::PartialRoomData {
                    chart: Some(chart_id),
                    ..Default::default()
                })
                .await;
            }
        }
    }

    /// 后台刷新房间展示元数据。
    ///
    /// 这个流程会访问 Phira `/me` 和 `/chart/<id>`，自定义 endpoint 慢、不可达或 502 时可能
    /// 等到 reqwest 超时。加入房间、强制迁移、设置 endpoint 等协议关键路径不能等待它，
    /// 否则客户端会先看到 timeout，随后重连才发现服务端其实已经把用户放进房间。
    pub fn refresh_room_display_metadata_background(&self, room: &Arc<crate::room::Room>) {
        let room = Arc::clone(room);
        let fallback_endpoint = self.config.phira_api_endpoint.clone();
        tokio::spawn(async move {
            let endpoint = room
                .phira_api_endpoint_override()
                .await
                .unwrap_or(fallback_endpoint);
            PlusServerState::refresh_room_display_metadata_with_endpoint(&room, endpoint).await;
        });
    }
}

impl PlusServerState {
    /// 绑定真实 Phira 账号 token 作为网络压测客户端。
    pub async fn bind_benchmark_tokens(&self, raw_tokens: Vec<String>) -> Result<usize, String> {
        let tokens = sanitize_benchmark_tokens(raw_tokens);
        if tokens.is_empty() {
            return Err(
                "未提供有效 token；可传入空格/逗号分隔的 1 个或多个 Phira token".to_string(),
            );
        }
        save_benchmark_tokens(&tokens)?;
        let count = tokens.len();
        *self.bench_tokens.write().await = tokens;
        Ok(count)
    }

    /// Runtime v2 hybrid benchmark probe.
    ///
    /// Hybrid is explicit and switch-driven. With all switches disabled it is a
    /// dry-run contract check and does not contact Phira. Read probes go through
    /// the unified PhiraRetryClient; write probes are intentionally blocked until
    /// upload-record semantics and safety limits are specified.
    pub async fn run_benchmark_hybrid(&self, config: HybridBenchmarkConfig) -> String {
        let mut out = String::new();
        macro_rules! o { ($($t:tt)*) => { out.push_str(&format!($($t)*)); out.push('\n'); } }

        o!("  ◆ Phira-mp+ Hybrid benchmark probe");
        o!(
            "  │ duration={}s  endpoint={}",
            config.duration_secs,
            config
                .endpoint_override
                .as_deref()
                .unwrap_or("<global phira_api_endpoint>"),
        );
        let switches = config.enabled_switches();
        let mut report = BenchmarkReport::new(
            BenchmarkMode::Hybrid,
            "hybrid Phira probe",
            config.duration_secs,
        );
        if switches.is_empty() {
            report.probes.record_skipped();
            report.add_note(
                "dry-run: no Phira request was sent because all hybrid switches are disabled",
            );
            report.add_note("simulation remains the default pressure path; real Phira probes require explicit switches");
            o!("  │ switches: none");
            o!("  │");
            o!("  ✓ hybrid dry-run complete: no Phira request was sent");
            o!("  │ simulation remains the default pressure path; real Phira probes require explicit switches");
            self.append_benchmark_report(&mut out, report);
            return out;
        }
        o!("  │ switches: {}", switches.join(", "));
        o!("  │");

        if let Err(err) = config.validate() {
            report.add_failure_sample("config", err.clone());
            o!("  ✗ invalid hybrid config: {err}");
            self.append_benchmark_report(&mut out, report);
            return out;
        }

        let endpoint_override = config.endpoint_override.as_deref();
        let tokens = self.bench_tokens.read().await.clone();
        let mut ok = 0usize;
        let mut failed = 0usize;

        if config.authenticate {
            o!("  ├─ authenticate /me");
            if let Some(token) = tokens.first() {
                match self
                    .phira_client
                    .get_json::<RemotePhiraUserInfo>(
                        &self.config.phira_api_endpoint,
                        endpoint_override,
                        "/me",
                        Some(token),
                        crate::phira_client::PhiraRetryNoticeTarget::Silent,
                    )
                    .await
                {
                    Ok(user) => {
                        ok += 1;
                        report.probes.record_success();
                        o!("  │ ✓ authenticated as {} ({})", user.name, user.id);
                    }
                    Err(err) => {
                        failed += 1;
                        report.probes.record_failure();
                        report.add_failure_sample("authenticate", err.to_string());
                        o!("  │ ✗ authenticate failed: {err}");
                    }
                }
            } else {
                failed += 1;
                report.probes.record_skipped();
                report.add_failure_sample("authenticate", "no benchmark token configured");
                o!("  │ ✗ skipped: no benchmark token configured");
                o!("  │   set benchmark_phira_tokens in server_config.yml or run benchmark-auth <token>");
            }
        }

        if let Some(chart_id) = config.chart_lookup {
            o!("  ├─ chart_lookup /chart/{chart_id}");
            match self
                .phira_client
                .get_json::<Chart>(
                    &self.config.phira_api_endpoint,
                    endpoint_override,
                    &format!("/chart/{chart_id}"),
                    None,
                    crate::phira_client::PhiraRetryNoticeTarget::Silent,
                )
                .await
            {
                Ok(chart) => {
                    ok += 1;
                    report.probes.record_success();
                    o!("  │ ✓ chart {}: {}", chart.id, chart.name);
                }
                Err(err) => {
                    failed += 1;
                    report.probes.record_failure();
                    report.add_failure_sample("chart_lookup", err.to_string());
                    o!("  │ ✗ chart lookup failed: {err}");
                }
            }
        }

        if let Some(record_id) = config.record_lookup {
            o!("  ├─ record_lookup /record/{record_id}");
            match self
                .phira_client
                .get_json::<Record>(
                    &self.config.phira_api_endpoint,
                    endpoint_override,
                    &format!("/record/{record_id}"),
                    None,
                    crate::phira_client::PhiraRetryNoticeTarget::Silent,
                )
                .await
            {
                Ok(record) => {
                    ok += 1;
                    report.probes.record_success();
                    o!(
                        "  │ ✓ record {}: player={} score={} acc={:.4}",
                        record.id,
                        record.player,
                        record.score,
                        record.accuracy
                    );
                }
                Err(err) => {
                    failed += 1;
                    report.probes.record_failure();
                    report.add_failure_sample("record_lookup", err.to_string());
                    o!("  │ ✗ record lookup failed: {err}");
                }
            }
        }

        if config.upload_record {
            failed += 1;
            report.probes.record_blocked();
            report.add_failure_sample("upload_record", "hybrid write probes are intentionally disabled until upload semantics are specified");
            o!("  ├─ upload_record");
            o!("  │ ✗ blocked: hybrid write probes are intentionally disabled until upload semantics are specified");
        }

        let stats = self.phira_client.stats();
        o!("  │");
        o!("  └─ hybrid complete: ok={ok} failed={failed}");
        o!("  │ phira_http: requests={} successes={} failures={} retry_attempts={} circuit_open={}",
            stats.requests, stats.successes, stats.failures, stats.retry_attempts, stats.circuit_open_rejections);
        report.add_note(format!(
            "phira_http requests={} successes={} failures={} retry_attempts={} circuit_open={}",
            stats.requests,
            stats.successes,
            stats.failures,
            stats.retry_attempts,
            stats.circuit_open_rejections,
        ));
        self.append_benchmark_report(&mut out, report);
        out
    }

    /// 通过真实 TCP 协议连接本服务端执行压测；不再直接篡改内存状态。
    pub async fn run_benchmark_network(&self, duration_secs: u64, target_rooms: usize) -> String {
        use std::time::Instant;

        struct BenchClient {
            stream: tokio::net::TcpStream,
            room_id: String,
        }

        let tokens = self.bench_tokens.read().await.clone();
        let mut out = String::new();
        macro_rules! o { ($($t:tt)*) => { out.push_str(&format!($($t)*)); out.push('\n'); } }

        o!("  ◆ Phira-mp+ 真实网络压测");
        o!("  │ 目标房间: {target_rooms}  测试时长: {duration_secs}s");
        o!("  │");
        if tokens.is_empty() {
            o!("  ✗ 未配置 Phira 压测账号");
            o!("  │  请配置 benchmark_phira_tokens 或执行 benchmark-auth <token>");
            o!(r#"  │  或直接修改 server_config.yml: benchmark_phira_tokens: ["..."]"#);
            o!(r#"  │  也可以写入 {BENCH_AUTH_FILE}: {{"tokens":["..."]}}"#);
            let mut report = BenchmarkReport::new(
                BenchmarkMode::Real,
                "real TCP compatibility benchmark",
                duration_secs,
            )
            .with_target_rooms(target_rooms);
            report.add_failure_sample("config", "no benchmark Phira tokens configured");
            report.add_note("real benchmark is explicit and requires local benchmark tokens; simulation remains the default pressure path");
            self.append_benchmark_report(&mut out, report);
            return out;
        }

        let room_count = target_rooms.max(1);
        let token_slots = tokens.len().max(1);
        if tokens.len() < target_rooms {
            o!(
                "  │ 账号不足：将复用 {} 个 token 分批创建/重建 {} 间房间",
                tokens.len(),
                target_rooms
            );
            o!(
                "  │ 最终只保持最多 {} 个真实客户端在线；创建吞吐仍覆盖目标房间数",
                tokens.len()
            );
            o!("  │");
        }

        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], self.config.port));
        let started_at = Instant::now();
        let mut clients_by_slot: Vec<Option<BenchClient>> =
            (0..token_slots).map(|_| None).collect();
        let mut created = 0usize;
        let mut rebuilt = 0usize;
        let mut joined = 0usize;
        let mut failures: Vec<String> = Vec::new();

        o!("  ├─ [阶段1] 真实 TCP 连接 + 认证 + 创建/重建房间");
        let phase1 = Instant::now();
        for i in 0..room_count {
            let slot = i % tokens.len();
            if let Some(mut old) = clients_by_slot[slot].take() {
                let _ = bench_leave_room(&mut old.stream).await;
                rebuilt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            let token = &tokens[slot];
            let room_id = format!("bench-{i}");
            match bench_connect_auth(addr, token).await {
                Ok(mut stream) => match bench_create_room(&mut stream, &room_id).await {
                    Ok(()) => {
                        clients_by_slot[slot] = Some(BenchClient { stream, room_id });
                        created += 1;
                    }
                    Err(err) => failures.push(format!("create {room_id}: {err}")),
                },
                Err(err) => failures.push(format!("auth host#{i}: {err}")),
            }
        }
        let mut clients: Vec<BenchClient> = clients_by_slot.into_iter().flatten().collect();
        o!(
            "  │ ✓ 创建/重建 {created} 间, 重建 {rebuilt} 次, 当前保持 {} 个客户端, 耗时 {:.1}s",
            clients.len(),
            phase1.elapsed().as_secs_f64()
        );

        if created == 0 {
            o!("  │");
            o!("  ✗ 没有成功创建任何房间，压测停止");
            for failure in failures.iter().take(8) {
                o!("  │  - {failure}");
            }
            let mut report = BenchmarkReport::new(
                BenchmarkMode::Real,
                "real TCP compatibility benchmark",
                duration_secs,
            )
            .with_target_rooms(target_rooms);
            report.rooms_created = Some(0);
            report.rooms_rebuilt = Some(rebuilt);
            report.failed_operations = Some(failures.len() as u64);
            for failure in failures.iter().take(8) {
                report.add_failure_sample("real_network", failure.clone());
            }
            self.append_benchmark_report(&mut out, report);
            return out;
        }

        o!("  │");
        o!("  ├─ [阶段2] 剩余账号真实 JoinRoom 填充房间");
        let phase2 = Instant::now();
        if tokens.len() > target_rooms {
            for (i, token) in tokens.iter().enumerate().skip(room_count) {
                let room_id = format!("bench-{}", (i - room_count) % created.max(1));
                match bench_connect_auth(addr, token).await {
                    Ok(mut stream) => match bench_join_room(&mut stream, &room_id, false).await {
                        Ok(()) => {
                            clients.push(BenchClient { stream, room_id });
                            joined += 1;
                        }
                        Err(err) => failures.push(format!("join token#{i}: {err}")),
                    },
                    Err(err) => failures.push(format!("auth guest#{i}: {err}")),
                }
            }
        } else {
            o!("  │ 无剩余 token 可填充玩家，已跳过；账号不足时重点测试创建/重建与连接稳定性");
        }
        o!(
            "  │ ✓ 加入 {joined} 人, 活跃客户端 {}, 耗时 {:.1}s",
            clients.len(),
            phase2.elapsed().as_secs_f64()
        );

        o!("  │");
        o!("  ├─ [阶段3] 保持连接并通过 Ping/Pong 测网络链路 {duration_secs}s");
        let phase3 = Instant::now();
        let mut op_count = 0u64;
        let mut failed_ops = 0u64;
        let mut latencies = Vec::new();
        while phase3.elapsed().as_secs() < duration_secs {
            for client in &mut clients {
                let t = Instant::now();
                match bench_ping(&mut client.stream).await {
                    Ok(()) => {
                        op_count += 1;
                        latencies.push(t.elapsed().as_secs_f64() * 1000.0);
                    }
                    Err(err) => {
                        failed_ops += 1;
                        if failures.len() < 16 {
                            failures.push(format!("ping {}: {err}", client.room_id));
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let avg_ms = if latencies.is_empty() {
            0.0
        } else {
            latencies.iter().sum::<f64>() / latencies.len() as f64
        };
        let p99_ms = if latencies.len() > 1 {
            let mut sorted = latencies.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            sorted[((sorted.len() - 1) as f64 * 0.99).round() as usize]
        } else {
            avg_ms
        };
        o!("  │ ✓ Ping/Pong {op_count} 次, 失败 {failed_ops} 次, avg={avg_ms:.2}ms p99={p99_ms:.2}ms");

        o!("  │");
        o!("  ├─ [阶段4] 通过协议 LeaveRoom 并清理 bench-* 房间");
        for client in &mut clients {
            let _ = bench_leave_room(&mut client.stream).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        self.rooms
            .write()
            .await
            .retain(|rid, _| !rid.to_string().starts_with("bench-"));
        o!("  │ ✓ 清理完成");

        o!("  │");
        o!("  └─ 压测完成 ({:.1}s)", started_at.elapsed().as_secs_f64());
        o!("");
        let mut report = BenchmarkReport::new(
            BenchmarkMode::Real,
            "real TCP compatibility benchmark",
            duration_secs,
        )
        .with_target_rooms(target_rooms);
        report.active_clients = Some(clients.len());
        report.rooms_created = Some(created);
        report.rooms_rebuilt = Some(rebuilt);
        report.users_joined = Some(joined);
        report.operations = Some(op_count);
        report.failed_operations = Some(failed_ops);
        report.avg_latency_ms = Some(avg_ms);
        report.p99_latency_ms = Some(p99_ms);
        if tokens.len() < target_rooms {
            report.add_note(format!(
                "benchmark tokens were fewer than target rooms; {} tokens were reused for {} target rooms",
                tokens.len(), target_rooms,
            ));
        }
        for failure in failures.iter().take(8) {
            report.add_failure_sample("real_network", failure.clone());
        }
        self.append_benchmark_report(&mut out, report);
        out
    }

    /// 管理员强制把用户迁移到指定房间，绕过房间人数、锁定、进行中等普通加入限制。
    pub async fn force_move_user_to_room(
        &self,
        room_id: &str,
        target_id: i32,
        monitor: bool,
    ) -> Result<Value, String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let target_room = {
            let rooms = self.rooms.read().await;
            rooms.get(&rid).map(Arc::clone).ok_or("room not found")?
        };
        let user = {
            let users = self.users.read().await;
            users
                .get(&target_id)
                .map(Arc::clone)
                .ok_or("user not found")?
        };

        let old_room = user.room.read().await.as_ref().map(Arc::clone);
        let old_room_id = old_room.as_ref().map(|room| room.id.to_string());
        let was_monitor = user.monitor.load(Ordering::SeqCst);
        let same_room = old_room
            .as_ref()
            .is_some_and(|room| room.id.to_string() == rid.to_string());

        if let Some(room) = old_room.as_ref().filter(|_| !same_room) {
            let old_id = room.id.clone();
            let old_id_text = old_id.to_string();
            if room.on_user_leave(&user).await {
                self.rooms.write().await.remove(&old_id);
            }
            if !was_monitor {
                self.publish_room_event(RoomEvent::LeaveRoom {
                    room: old_id,
                    user: target_id,
                })
                .await;
            }
            self.event_bus
                .publish(crate::event_bus::MpEvent::PluginEventDispatched(
                    std::sync::Arc::new(PluginEvent::RoomLeave {
                        user_id: target_id,
                        room_id: old_id_text,
                    }),
                ));
        }

        user.monitor.store(monitor, Ordering::SeqCst);
        target_room
            .force_add_user(Arc::downgrade(&user), monitor)
            .await;
        *user.room.write().await = Some(Arc::clone(&target_room));
        if monitor {
            target_room.live.store(true, Ordering::SeqCst);
        }
        self.assign_room_host_if_missing(&target_room, &user, monitor, false)
            .await;
        self.refresh_room_display_metadata_background(&target_room);

        let join = ServerCommand::OnJoinRoom(user.to_info());
        let message = ServerCommand::Message(phira_mp_common::Message::JoinRoom {
            user: user.id,
            name: user.name.clone(),
        });
        if monitor {
            target_room.broadcast_players(join).await;
            target_room.broadcast_players(message).await;
        } else {
            target_room.broadcast(join).await;
            target_room.broadcast(message).await;
            if !same_room || was_monitor {
                self.publish_room_event(RoomEvent::JoinRoom {
                    room: rid.clone(),
                    user: target_id,
                })
                .await;
            }
        }

        let mut users = target_room.users().await;
        users.extend(target_room.monitors().await);
        user.try_send(ServerCommand::JoinRoom(Ok(
            phira_mp_common::JoinRoomResponse {
                state: target_room.client_room_state().await,
                users: users.into_iter().map(|user| user.to_info()).collect(),
                live: target_room.is_live(),
            },
        )))
        .await;
        user.try_send(ServerCommand::ChangeHost(
            target_room.check_host(&user).await.is_ok(),
        ))
        .await;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.user_room_history
            .write()
            .await
            .entry(target_id)
            .or_default()
            .push((rid.to_string(), target_room.uuid.to_string(), now));
        if let Some(db) = crate::internal_hooks::DB.get() {
            db.record_user_room_history_sync(
                target_id,
                rid.to_string(),
                target_room.uuid.to_string(),
                now,
            );
        }

        self.event_bus
            .publish(crate::event_bus::MpEvent::PluginEventDispatched(
                std::sync::Arc::new(PluginEvent::RoomJoin {
                    user_id: target_id,
                    room_id: rid.to_string(),
                    is_monitor: monitor,
                }),
            ));
        self.event_bus.publish(crate::event_bus::MpEvent::PluginEventDispatched(
            std::sync::Arc::new(PluginEvent::RoomModify {
                user_id: target_id,
                room_id: rid.to_string(),
                data: serde_json::json!({"action":"force-move","from": old_room_id.clone(),"monitor": monitor}).to_string(),
            }),
        ));

        target_room
            .send(phira_mp_common::Message::Chat {
                user: 0,
                content: format!("用户 {} 已被管理员强制转移到本房间", user.name),
            })
            .await;

        Ok(serde_json::json!({
            "ok": true,
            "room_id": rid.to_string(),
            "target_id": target_id,
            "monitor": monitor,
            "from": old_room_id,
        }))
    }

    pub async fn set_room_hidden(&self, room_id: &str, hidden: bool) -> Result<Value, String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let room = {
            let rooms = self.rooms.read().await;
            rooms.get(&rid).map(Arc::clone).ok_or("room not found")?
        };
        room.set_hidden(hidden);
        self.event_bus
            .publish(crate::event_bus::MpEvent::PluginEventDispatched(
                std::sync::Arc::new(PluginEvent::RoomModify {
                    user_id: 0,
                    room_id: rid.to_string(),
                    data: format!(r#"{{"action":"hidden","value":{hidden}}}"#),
                }),
            ));
        Ok(serde_json::json!({"ok": true, "room_id": rid.to_string(), "hidden": hidden}))
    }

    pub async fn get_room_phira_api_endpoint(&self, room_id: &str) -> Result<Value, String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let room = {
            let rooms = self.rooms.read().await;
            rooms.get(&rid).map(Arc::clone).ok_or("room not found")?
        };
        let override_endpoint = room.phira_api_endpoint_override().await;
        let using_room_override = override_endpoint.is_some();
        let effective_endpoint = override_endpoint
            .clone()
            .unwrap_or_else(|| self.config.phira_api_endpoint.clone());
        Ok(serde_json::json!({
            "ok": true,
            "room_id": rid.to_string(),
            "phira_api_endpoint": effective_endpoint,
            "phira_api_endpoint_override": override_endpoint,
            "using_room_override": using_room_override,
        }))
    }

    pub async fn set_room_phira_api_endpoint(
        &self,
        room_id: &str,
        endpoint: Option<String>,
    ) -> Result<Value, String> {
        let rid: RoomId = room_id
            .to_string()
            .try_into()
            .map_err(|_| "invalid room_id".to_string())?;
        let room = {
            let rooms = self.rooms.read().await;
            rooms.get(&rid).map(Arc::clone).ok_or("room not found")?
        };
        let normalized = match endpoint {
            Some(value) => Some(normalize_phira_api_endpoint(&value)?),
            None => None,
        };
        room.set_phira_api_endpoint_override(normalized.clone())
            .await;
        self.refresh_room_display_metadata_background(&room);
        let using_room_override = normalized.is_some();
        let effective_endpoint = normalized
            .clone()
            .unwrap_or_else(|| self.config.phira_api_endpoint.clone());
        self.event_bus
            .publish(crate::event_bus::MpEvent::PluginEventDispatched(
                std::sync::Arc::new(PluginEvent::RoomModify {
                    user_id: 0,
                    room_id: rid.to_string(),
                    data: serde_json::json!({
                        "action": "phira_api_endpoint",
                        "value": normalized.clone(),
                        "effective": effective_endpoint.clone(),
                    })
                    .to_string(),
                }),
            ));
        Ok(serde_json::json!({
            "ok": true,
            "room_id": rid.to_string(),
            "phira_api_endpoint": effective_endpoint,
            "phira_api_endpoint_override": normalized,
            "using_room_override": using_room_override,
        }))
    }
}

async fn bench_send_command(
    stream: &mut tokio::net::TcpStream,
    payload: &phira_mp_common::ClientCommand,
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;
    let mut buffer = Vec::new();
    phira_mp_common::encode_packet(payload, &mut buffer);
    let mut len_buf = [0u8; 5];
    let mut x = buffer.len() as u32;
    let mut n = 0usize;
    loop {
        len_buf[n] = (x & 0x7f) as u8;
        n += 1;
        x >>= 7;
        if x == 0 {
            break;
        }
        len_buf[n - 1] |= 0x80;
    }
    stream
        .write_all(&len_buf[..n])
        .await
        .map_err(|e| format!("write length: {e}"))?;
    stream
        .write_all(&buffer)
        .await
        .map_err(|e| format!("write payload: {e}"))?;
    stream.flush().await.map_err(|e| format!("flush: {e}"))
}

async fn bench_recv_command(
    stream: &mut tokio::net::TcpStream,
) -> Result<phira_mp_common::ServerCommand, String> {
    use tokio::io::AsyncReadExt;
    let mut len = 0u32;
    let mut pos = 0;
    loop {
        let byte = stream
            .read_u8()
            .await
            .map_err(|e| format!("read length: {e}"))?;
        len |= ((byte & 0x7f) as u32) << pos;
        pos += 7;
        if byte & 0x80 == 0 {
            break;
        }
        if pos > 32 {
            return Err("invalid packet length".to_string());
        }
    }
    if len > 2 * 1024 * 1024 {
        return Err("packet too large".to_string());
    }
    let mut buffer = vec![0u8; len as usize];
    stream
        .read_exact(&mut buffer)
        .await
        .map_err(|e| format!("read payload: {e}"))?;
    phira_mp_common::decode_packet(&buffer).map_err(|e| format!("decode packet: {e}"))
}

async fn bench_connect_auth(
    addr: std::net::SocketAddr,
    token: &str,
) -> Result<tokio::net::TcpStream, String> {
    use tokio::io::AsyncWriteExt;
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .map_err(|_| "connect timeout".to_string())?
    .map_err(|e| format!("connect: {e}"))?;
    stream
        .set_nodelay(true)
        .map_err(|e| format!("set_nodelay: {e}"))?;
    stream
        .write_u8(1)
        .await
        .map_err(|e| format!("write protocol version: {e}"))?;
    bench_send_command(
        &mut stream,
        &phira_mp_common::ClientCommand::Authenticate {
            token: token
                .to_string()
                .try_into()
                .map_err(|e| format!("invalid token: {e}"))?,
        },
    )
    .await?;
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            match bench_recv_command(&mut stream).await? {
                phira_mp_common::ServerCommand::Authenticate(Ok(_)) => return Ok(()),
                phira_mp_common::ServerCommand::Authenticate(Err(err)) => {
                    return Err(format!("authenticate rejected: {err}"))
                }
                phira_mp_common::ServerCommand::Message(_) => {}
                other => trace!(?other, "benchmark ignored packet while authenticating"),
            }
        }
    })
    .await
    .map_err(|_| "authenticate timeout".to_string())??;
    Ok(stream)
}

async fn bench_create_room(
    stream: &mut tokio::net::TcpStream,
    room_id: &str,
) -> Result<(), String> {
    bench_send_command(
        stream,
        &phira_mp_common::ClientCommand::CreateRoom {
            id: room_id
                .to_string()
                .try_into()
                .map_err(|e| format!("invalid room id: {e}"))?,
        },
    )
    .await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match bench_recv_command(stream).await? {
                phira_mp_common::ServerCommand::CreateRoom(Ok(())) => return Ok(()),
                phira_mp_common::ServerCommand::CreateRoom(Err(err)) => {
                    return Err(format!("create room rejected: {err}"))
                }
                phira_mp_common::ServerCommand::Message(_)
                | phira_mp_common::ServerCommand::OnJoinRoom(_) => {}
                other => trace!(?other, "benchmark ignored packet while creating room"),
            }
        }
    })
    .await
    .map_err(|_| "create room timeout".to_string())?
}

async fn bench_join_room(
    stream: &mut tokio::net::TcpStream,
    room_id: &str,
    monitor: bool,
) -> Result<(), String> {
    bench_send_command(
        stream,
        &phira_mp_common::ClientCommand::JoinRoom {
            id: room_id
                .to_string()
                .try_into()
                .map_err(|e| format!("invalid room id: {e}"))?,
            monitor,
        },
    )
    .await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match bench_recv_command(stream).await? {
                phira_mp_common::ServerCommand::JoinRoom(Ok(_)) => return Ok(()),
                phira_mp_common::ServerCommand::JoinRoom(Err(err)) => {
                    return Err(format!("join room rejected: {err}"))
                }
                phira_mp_common::ServerCommand::Message(_)
                | phira_mp_common::ServerCommand::OnJoinRoom(_) => {}
                other => trace!(?other, "benchmark ignored packet while joining room"),
            }
        }
    })
    .await
    .map_err(|_| "join room timeout".to_string())?
}

async fn bench_ping(stream: &mut tokio::net::TcpStream) -> Result<(), String> {
    bench_send_command(stream, &phira_mp_common::ClientCommand::Ping).await?;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            match bench_recv_command(stream).await? {
                phira_mp_common::ServerCommand::Pong => return Ok(()),
                phira_mp_common::ServerCommand::Message(_)
                | phira_mp_common::ServerCommand::OnJoinRoom(_) => {}
                other => trace!(?other, "benchmark ignored packet while waiting pong"),
            }
        }
    })
    .await
    .map_err(|_| "pong timeout".to_string())?
}

async fn bench_leave_room(stream: &mut tokio::net::TcpStream) -> Result<(), String> {
    bench_send_command(stream, &phira_mp_common::ClientCommand::LeaveRoom).await?;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            match bench_recv_command(stream).await? {
                phira_mp_common::ServerCommand::LeaveRoom(_) => return Ok(()),
                phira_mp_common::ServerCommand::Message(_)
                | phira_mp_common::ServerCommand::OnJoinRoom(_) => {}
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| "leave timeout".to_string())?
}

/// 从房间踢出用户。
async fn run_room_kick(
    state: &PlusServerState,
    room_id: &str,
    target_id: i32,
) -> Result<Value, String> {
    state
        .room_commands
        .kick_user(state, room_id, target_id)
        .await
}

/// 设置房主；target_id=None 表示系统 `?` 房主。
async fn run_room_set_host(
    state: &PlusServerState,
    room_id: &str,
    target_id: Option<i32>,
) -> Result<Value, String> {
    state
        .room_commands
        .set_host(state, room_id, target_id)
        .await
}

/// 设置房间锁定状态。
async fn run_room_set_lock(
    state: &PlusServerState,
    room_id: &str,
    locked: bool,
) -> Result<Value, String> {
    state.room_commands.set_lock(state, room_id, locked).await
}

/// 关闭/解散房间。
async fn run_room_close(state: &PlusServerState, room_id: &str) -> Result<Value, String> {
    state.room_commands.close_room(state, room_id).await
}

/// 将用户踢出服务器
async fn run_admin_kick_user(
    state: &PlusServerState,
    target_id: i32,
    reason: &str,
) -> Result<Value, String> {
    let user = state
        .users
        .read()
        .await
        .get(&target_id)
        .map(Arc::clone)
        .ok_or("user not found")?;
    {
        let room_clone = user.room.read().await.as_ref().map(Arc::clone);
        if let Some(room) = room_clone {
            let room_id = room.id.to_string();
            let room_key = room.id.clone();
            let was_monitor = user.monitor.load(Ordering::SeqCst);
            if room.on_user_leave(&user).await {
                state.rooms.write().await.remove(&room_key);
            }
            if !was_monitor {
                state
                    .publish_room_event(RoomEvent::LeaveRoom {
                        room: room_key,
                        user: target_id,
                    })
                    .await;
            }
            state
                .event_bus
                .publish(crate::event_bus::MpEvent::PluginEventDispatched(
                    std::sync::Arc::new(PluginEvent::RoomLeave {
                        user_id: target_id,
                        room_id,
                    }),
                ));
        }
    }
    {
        let sessions = state.sessions.read().await;
        for session in sessions.values() {
            if session.user.id == target_id {
                let _ = session
                    .stream
                    .send(phira_mp_common::ServerCommand::Message(
                        phira_mp_common::Message::Chat {
                            user: 0,
                            content: format!("你已被管理员踢出服务器: {reason}"),
                        },
                    ))
                    .await;
                break;
            }
        }
    }
    state.users.write().await.remove(&target_id);
    info!(user = target_id, reason = %reason, "kicked from server by admin");
    state
        .event_bus
        .publish(crate::event_bus::MpEvent::PluginEventDispatched(
            std::sync::Arc::new(PluginEvent::UserDisconnect {
                user_id: target_id,
                user_name: user.name.clone(),
            }),
        ));
    state.publish_runtime_event(crate::event_bus::MpEvent::UserDisconnected { user_id: target_id });
    Ok(serde_json::json!({"ok": true, "reason": reason}))
}

/// 统一状态查询入口（插件 / WASM host API 均通过此函数查询）
///
/// 支持的 method:
/// - `player.touches`  → 查询指定用户的最近触控数据
/// - `player.judges`   → 查询指定用户的最近判定数据
/// - 其它方法委托给 webapi feature 下的 server_state_query
fn runtime_state_query_timeout() -> std::time::Duration {
    crate::runtime_diagnostics::RUNTIME_STATE_QUERY_TIMEOUT
}

fn parse_benchmark_mode_arg(value: &str) -> Option<BenchmarkMode> {
    match value {
        "simulation" | "sim" => Some(BenchmarkMode::Simulation),
        "hybrid" => Some(BenchmarkMode::Hybrid),
        "real" => Some(BenchmarkMode::Real),
        _ => None,
    }
}

fn server_state_query_inner(
    state: &Arc<PlusServerState>,
    method: &str,
    args: &[Value],
) -> Result<Value, String> {
    match method {
        "runtime.status" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let simulation = s.simulation.status().await;
                let persistence = s.persistence_worker.stats().await;
                let events = s
                    .event_bus
                    .stats(crate::runtime_diagnostics::BENCHMARK_REPORT_RECENT_DEFAULT);
                let commands = s.command_registry.iter().count();
                let room_commands = s.room_commands.stats();
                let phira_http = s.phira_client.stats();
                let benchmark_reports = s
                    .benchmark_reports
                    .snapshot(crate::runtime_diagnostics::BENCHMARK_REPORT_RECENT_DEFAULT);
                let plan = s.runtime_plan.snapshot();
                let _ = tx.send(Ok(serde_json::json!({
                    "runtime_v2": true,
                    "note": "Runtime v2 is partially installed; real Room/Session runtime is still the current production path.",
                    "commands": {"registered": commands},
                    "event_bus": events,
                    "simulation": simulation,
                    "persistence_worker": persistence,
                    "room_command_gateway": room_commands,
                    "phira_http": phira_http,
                    "benchmark_reports": benchmark_reports,
                    "plan": plan,
                })));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("runtime.status timeout".to_string()))
        }
        "simulation.status" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let status = s.simulation.status().await;
                let _ = tx.send(Ok(serde_json::to_value(status).unwrap_or_default()));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("simulation.status timeout".to_string()))
        }
        "simulation.world" => {
            let limit = args.get(0).and_then(|v| v.as_u64()).unwrap_or(20) as usize;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s
                    .simulation
                    .world_snapshot(limit)
                    .await
                    .map(|world| serde_json::to_value(world).unwrap_or_default())
                    .ok_or_else(|| "simulation shadow world not available".to_string());
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("simulation.world timeout".to_string()))
        }
        "benchmark.reports" => {
            let limit = args
                .get(0)
                .and_then(|v| v.as_u64())
                .map(|value| value as usize)
                .unwrap_or(crate::runtime_diagnostics::BENCHMARK_REPORT_RECENT_DEFAULT);
            Ok(serde_json::to_value(state.benchmark_reports.snapshot(limit)).unwrap_or_default())
        }
        "benchmark.latest" => {
            let mode = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
            let mode = match mode {
                "simulation" | "sim" => Some(BenchmarkMode::Simulation),
                "hybrid" => Some(BenchmarkMode::Hybrid),
                "real" => Some(BenchmarkMode::Real),
                _ => None,
            };
            if let Some(mode) = mode {
                state
                    .benchmark_reports
                    .latest(mode)
                    .map(|entry| serde_json::to_value(entry).unwrap_or_default())
                    .ok_or_else(|| format!("no benchmark report for mode {}", mode.as_str()))
            } else {
                Err("benchmark.latest requires mode: simulation|hybrid|real".to_string())
            }
        }
        "benchmark.history" => {
            let mode = args
                .get(0)
                .and_then(|v| v.as_str())
                .and_then(parse_benchmark_mode_arg);
            let limit = args
                .get(1)
                .and_then(|v| v.as_u64())
                .map(|value| value as usize)
                .unwrap_or(crate::runtime_diagnostics::BENCHMARK_REPORT_RECENT_DEFAULT);
            let (tx, rx) = std::sync::mpsc::channel::<Result<Value, String>>();
            tokio::spawn(async move {
                let rows = if let Some(db) = crate::internal_hooks::DB.get() {
                    db.runtime_benchmark_report_history(
                        crate::persistence::BenchmarkReportHistoryQuery::new(mode, limit),
                    )
                    .await
                } else {
                    Vec::new()
                };
                let _ = tx.send(Ok(serde_json::json!({
                    "rows": rows,
                    "source": "mp_runtime_benchmark_reports",
                })));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("benchmark.history timeout".to_string()))
        }
        "player.touches" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = async {
                    let rooms = s.rooms.read().await;
                    for room in rooms.values() {
                        let data = room.get_player_touches(uid).await;
                        if !data.is_empty() {
                            return Ok(serde_json::to_value(data).unwrap_or_default());
                        }
                    }
                    Err("no data".to_string())
                }
                .await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("query timeout".to_string()))
        }
        "player.judges" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = async {
                    let rooms = s.rooms.read().await;
                    for room in rooms.values() {
                        let data = room.get_player_judges(uid).await;
                        if !data.is_empty() {
                            return Ok(serde_json::to_value(data).unwrap_or_default());
                        }
                    }
                    Err("no data".to_string())
                }
                .await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("query timeout".to_string()))
        }
        "round.data" => {
            let round_uuid = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
            let player_id = args.get(1).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if round_uuid.is_empty() {
                return Err("missing round_uuid".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            let uuid = round_uuid.to_string();
            tokio::spawn(async move {
                let result = s
                    .round_store
                    .read_player_data(&uuid, player_id)
                    .await
                    .map(|data| serde_json::to_value(data).unwrap_or_default())
                    .ok_or_else(|| "round data not found".to_string());
                let _ = tx.send(result);
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("query timeout".to_string()))
        }
        "round.list" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let rounds = s.round_store.list_rounds().await;
                let _ = tx.send(Ok(serde_json::to_value(rounds).unwrap_or_default()));
            });
            rx.recv_timeout(runtime_state_query_timeout())
                .unwrap_or(Err("query timeout".to_string()))
        }
        "test.run_benchmark" => {
            let duration = args
                .get(0)
                .and_then(|v| v.as_u64())
                .unwrap_or(10)
                .max(5)
                .min(300);
            let rooms = args
                .get(1)
                .and_then(|v| v.as_u64())
                .unwrap_or(100)
                .max(1)
                .min(5000) as usize;

            // 通过 mpsc 通道发送请求给背景 tokio 任务，阻塞等待结果
            let (tx, rx) = std::sync::mpsc::channel();
            if state
                .bench_tx
                .send(BenchRequest::real(duration, rooms, tx))
                .is_err()
            {
                return Err("benchmark channel closed".to_string());
            }
            match rx.recv_timeout(std::time::Duration::from_secs(duration + 120)) {
                Ok(result) => Ok(serde_json::json!({"output": result})),
                Err(_) => Err("benchmark timeout or cancelled".to_string()),
            }
        }
        "test.bind_phira_tokens" => {
            let mut raw = Vec::new();
            for arg in args {
                if let Some(s) = arg.as_str() {
                    raw.push(s.to_string());
                } else if let Some(list) = arg.as_array() {
                    raw.extend(
                        list.iter()
                            .filter_map(|v| v.as_str().map(ToString::to_string)),
                    );
                }
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s.bind_benchmark_tokens(raw).await
                    .map(|count| serde_json::json!({"ok": true, "count": count, "path": BENCH_AUTH_FILE}));
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("test.bind_phira_tokens timeout".to_string()))
        }
        "test.cleanup" => {
            let s = Arc::clone(state);
            tokio::spawn(async move {
                s.rooms
                    .write()
                    .await
                    .retain(|rid, _| !rid.to_string().starts_with("bench-"));
                s.users.write().await.retain(|id, _| *id >= 1 || *id < 0);
            });
            Ok(serde_json::json!({"ok": true, "note": "benchmark rooms removed via async path"}))
        }
        "room.uuid" => {
            let room_id = args
                .get(0)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if room_id.is_empty() {
                return Err("missing room_id".to_string());
            }
            use phira_mp_common::RoomId;
            let rid: RoomId = match room_id.try_into() {
                Ok(r) => r,
                Err(_) => return Err("invalid room_id".to_string()),
            };
            let rooms = state
                .rooms
                .try_read()
                .map_err(|_| "lock error".to_string())?;
            match rooms.get(&rid) {
                Some(room) => Ok(
                    serde_json::json!({"uuid": room.uuid.to_string(), "created_at": room.created_at}),
                ),
                None => Err("room not found".to_string()),
            }
        }
        "room.history" => {
            let room_id = args
                .get(0)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if room_id.is_empty() {
                return Err("missing room_id".to_string());
            }
            use phira_mp_common::RoomId;
            let rid: RoomId = match room_id.try_into() {
                Ok(r) => r,
                Err(_) => return Err("invalid room_id".to_string()),
            };
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let rooms = s.rooms.read().await;
                let result = match rooms.get(&rid) {
                    Some(room) => {
                        let history = room.play_history.read().await;
                        let rounds: Vec<Value> = history
                            .iter()
                            .map(|r| {
                                serde_json::json!({
                                    "round_id": r.round_id.to_string(),
                                    "chart_id": r.chart_id,
                                    "chart_name": r.chart_name,
                                    "players": r.results.len(),
                                })
                            })
                            .collect();
                        let total = rounds.len();
                        Ok(serde_json::json!({"rounds": rounds, "total": total}))
                    }
                    None => Err("room not found".to_string()),
                };
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(3))
                .unwrap_or(Err("room.history timeout".to_string()))
        }
        "room.round_info" => {
            let round_uuid = args
                .get(0)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if round_uuid.is_empty() {
                return Err("missing round_uuid".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let rooms = s.rooms.read().await;
                let mut found = None;
                for room in rooms.values() {
                    let history = room.play_history.read().await;
                    if let Some(round) = history
                        .iter()
                        .find(|r| r.round_id.to_string() == round_uuid)
                    {
                        found = Some((room.uuid.to_string(), room.id.to_string(), round.clone()));
                        break;
                    }
                }
                let result = match found {
                    Some((room_uuid, room_id, round)) => Ok(serde_json::json!({
                        "room_uuid": room_uuid, "room_id": room_id,
                        "round_id": round.round_id.to_string(),
                        "chart_id": round.chart_id, "chart_name": round.chart_name,
                        "results": round.results.iter().map(|r| serde_json::json!({
                            "user_id": r.user_id, "user_name": r.user_name,
                            "score": r.score, "accuracy": r.accuracy,
                            "full_combo": r.full_combo, "aborted": r.aborted,
                        })).collect::<Vec<_>>(),
                    })),
                    None => Err("round not found".to_string()),
                };
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(3))
                .unwrap_or(Err("room.round_info timeout".to_string()))
        }
        "room.list_since" => {
            let since_ms = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let rooms = s.rooms.read().await;
                let list: Vec<Value> = rooms.values().filter(|r| r.created_at >= since_ms).map(|r| {
                    serde_json::json!({
                        "room_id": r.id.to_string(),
                        "uuid": r.uuid.to_string(),
                        "created_at": r.created_at,
                        "phira_api_endpoint": r.effective_phira_api_endpoint_sync(&s.config.phira_api_endpoint),
                    })
                }).collect();
                let total = list.len();
                let _ = tx.send(Ok(serde_json::json!({"rooms": list, "total": total})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(3))
                .unwrap_or(Err("room.list_since timeout".to_string()))
        }

        "room.create_empty" => {
            let room_id = args
                .get(0)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if room_id.is_empty() {
                return Err("missing room_id".to_string());
            }
            let endpoint = match args.get(1) {
                Some(Value::Null) | None => None,
                Some(v) => Some(parse_room_endpoint_value(v.as_str().unwrap_or(""))?),
            }
            .flatten();
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s.create_empty_room(&room_id, endpoint, true).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.create_empty timeout".to_string()))
        }
        "room.set_persistent_empty" => {
            let room_id = args
                .get(0)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let persistent = args.get(1).and_then(|v| v.as_bool()).unwrap_or(true);
            if room_id.is_empty() {
                return Err("missing room_id".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s.set_room_persistent_empty(&room_id, persistent).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.set_persistent_empty timeout".to_string()))
        }
        "room.kick" => {
            let room_id = args
                .get(0)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let target_id = args.get(1).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if room_id.is_empty() {
                return Err("missing room_id".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = run_room_kick(&s, &room_id, target_id).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.kick timeout".to_string()))
        }
        "room.set_host" => {
            let room_id = args
                .get(0)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let target = args.get(1);
            let target_id = match target {
                None | Some(Value::Null) => None,
                Some(Value::String(s))
                    if s.trim() == "?"
                        || s.eq_ignore_ascii_case("system")
                        || s.eq_ignore_ascii_case("none") =>
                {
                    None
                }
                Some(Value::String(s)) => Some(
                    s.parse::<i32>()
                        .map_err(|_| "invalid target_id".to_string())?,
                ),
                Some(v) => v.as_i64().map(|n| n as i32),
            };
            if room_id.is_empty() {
                return Err("missing room_id".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = run_room_set_host(&s, &room_id, target_id).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.set_host timeout".to_string()))
        }
        "room.force_move" => {
            let room_id = args
                .get(0)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let target_id = args.get(1).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let monitor = args.get(2).and_then(|v| v.as_bool()).unwrap_or(false);
            if room_id.is_empty() {
                return Err("missing room_id".to_string());
            }
            if target_id == 0 {
                return Err("invalid target_id".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s
                    .force_move_user_to_room(&room_id, target_id, monitor)
                    .await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.force_move timeout".to_string()))
        }
        "room.set_lock" => {
            let room_id = args
                .get(0)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let locked = args.get(1).and_then(|v| v.as_bool()).unwrap_or(true);
            if room_id.is_empty() {
                return Err("missing room_id".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = run_room_set_lock(&s, &room_id, locked).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.set_lock timeout".to_string()))
        }
        "room.set_hidden" => {
            let room_id = args
                .get(0)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let hidden = args.get(1).and_then(|v| v.as_bool()).unwrap_or(true);
            if room_id.is_empty() {
                return Err("missing room_id".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s.set_room_hidden(&room_id, hidden).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.set_hidden timeout".to_string()))
        }
        "room.is_hidden" => {
            let room_id = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
            if room_id.is_empty() {
                return Err("missing room_id".to_string());
            }
            let rid: RoomId = room_id
                .to_string()
                .try_into()
                .map_err(|_| "invalid room_id".to_string())?;
            let rooms = state
                .rooms
                .try_read()
                .map_err(|_| "lock error".to_string())?;
            let room = rooms.get(&rid).ok_or("room not found")?;
            Ok(serde_json::json!({"room_id": room_id, "hidden": room.is_hidden()}))
        }
        "room.get_phira_api_endpoint" => {
            let room_id = args
                .get(0)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if room_id.is_empty() {
                return Err("missing room_id".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s.get_room_phira_api_endpoint(&room_id).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.get_phira_api_endpoint timeout".to_string()))
        }
        "room.set_phira_api_endpoint" => {
            let room_id = args
                .get(0)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if room_id.is_empty() {
                return Err("missing room_id".to_string());
            }
            let endpoint = match args.get(1) {
                Some(Value::Null) | None => None,
                Some(v) => Some(parse_room_endpoint_value(v.as_str().unwrap_or(""))?),
            }
            .flatten();
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s.set_room_phira_api_endpoint(&room_id, endpoint).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.set_phira_api_endpoint timeout".to_string()))
        }
        "room.clear_phira_api_endpoint" => {
            let room_id = args
                .get(0)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if room_id.is_empty() {
                return Err("missing room_id".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s.set_room_phira_api_endpoint(&room_id, None).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.clear_phira_api_endpoint timeout".to_string()))
        }
        "room.close" => {
            let room_id = args
                .get(0)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if room_id.is_empty() {
                return Err("missing room_id".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = run_room_close(&s, &room_id).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("room.close timeout".to_string()))
        }
        "admin.kick_user" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if uid <= 0 {
                return Err("invalid user_id".to_string());
            }
            let reason = args
                .get(1)
                .and_then(|v| v.as_str())
                .unwrap_or("kicked by admin")
                .to_string();
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = run_admin_kick_user(&s, uid, &reason).await;
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.kick_user timeout".to_string()))
        }
        "admin.ban_user" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let reason = args
                .get(1)
                .and_then(|v| v.as_str())
                .unwrap_or("banned")
                .to_string();
            if uid <= 0 {
                return Err("invalid user_id".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s
                    .ban_manager
                    .ban_user(uid, &reason)
                    .await
                    .map(|reason| serde_json::json!({"ok": true, "reason": reason}));
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.ban_user timeout".to_string()))
        }
        "admin.unban_user" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if uid <= 0 {
                return Err("invalid user_id".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let result = s
                    .ban_manager
                    .unban_user(uid)
                    .await
                    .map(|_| serde_json::json!({"ok": true}));
                let _ = tx.send(result);
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.unban_user timeout".to_string()))
        }
        "admin.is_banned" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if uid <= 0 {
                return Err("invalid user_id".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let reason = s.ban_manager.ban_reason(uid).await;
                let _ = tx.send(Ok(serde_json::json!({
                    "banned": reason.is_some(),
                    "reason": reason,
                })));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.is_banned timeout".to_string()))
        }
        "admin.ban_list" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let list = s.ban_manager.list_banned().await;
                let _ = tx.send(Ok(serde_json::to_value(list).unwrap_or_default()));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.ban_list timeout".to_string()))
        }
        "admin.list_users" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let users = s.users.read().await;
                let list: Vec<Value> = users
                    .values()
                    .filter(|user| user.id > 0)
                    .map(|u| {
                        serde_json::json!({
                            "id": u.id, "name": u.name, "monitor": u.monitor.load(Ordering::SeqCst)
                        })
                    })
                    .collect();
                let _ = tx.send(Ok(serde_json::to_value(list).unwrap_or_default()));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.list_users timeout".to_string()))
        }
        "user.room_history" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if uid <= 0 {
                return Err("invalid user_id".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let history = s.user_room_history.read().await;
                let entries = history.get(&uid).cloned().unwrap_or_default();
                let list: Vec<Value> = entries.iter().map(|(room_id, room_uuid, ts)| {
                    serde_json::json!({"room_id": room_id, "room_uuid": room_uuid, "joined_at": ts})
                }).collect();
                let total = list.len();
                let _ = tx.send(Ok(serde_json::json!({"rooms": list, "total": total})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("user.room_history timeout".to_string()))
        }

        "persist.events" => {
            let since = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
            let limit = args
                .get(1)
                .and_then(|v| v.as_i64())
                .unwrap_or(100)
                .clamp(1, 1000);
            let kind = args.get(2).and_then(|v| v.as_str()).map(str::to_string);
            let room_id = args.get(3).and_then(|v| v.as_str()).map(str::to_string);
            let user_id = args
                .get(4)
                .and_then(|v| v.as_i64())
                .and_then(|v| i32::try_from(v).ok());
            let (tx, rx) = std::sync::mpsc::channel::<Result<Value, String>>();
            tokio::spawn(async move {
                let rows = if let Some(db) = crate::internal_hooks::DB.get() {
                    db.query_events(since, limit, kind.as_deref(), room_id.as_deref(), user_id)
                        .await
                } else {
                    Vec::new()
                };
                let total = rows.len();
                let _ = tx.send(Ok(serde_json::json!({"events": rows, "total": total})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("persist.events timeout".to_string()))
        }
        "persist.rooms" => {
            let since = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
            let limit = args
                .get(1)
                .and_then(|v| v.as_i64())
                .unwrap_or(100)
                .clamp(1, 1000);
            let (tx, rx) = std::sync::mpsc::channel::<Result<Value, String>>();
            tokio::spawn(async move {
                let rows = if let Some(db) = crate::internal_hooks::DB.get() {
                    db.query_room_snapshots(since, limit).await
                } else {
                    Vec::new()
                };
                let total = rows.len();
                let _ = tx.send(Ok(serde_json::json!({"rooms": rows, "total": total})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("persist.rooms timeout".to_string()))
        }

        "persist.touches" => {
            let since = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
            let limit = args
                .get(1)
                .and_then(|v| v.as_i64())
                .unwrap_or(100)
                .clamp(1, 1000);
            let round_uuid = args
                .get(2)
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .map(str::to_string);
            let player_id = args
                .get(3)
                .and_then(|v| v.as_i64())
                .and_then(|v| i32::try_from(v).ok());
            let (tx, rx) = std::sync::mpsc::channel::<Result<Value, String>>();
            tokio::spawn(async move {
                let rows = if let Some(db) = crate::internal_hooks::DB.get() {
                    db.query_touch_batches(since, limit, round_uuid.as_deref(), player_id)
                        .await
                } else {
                    Vec::new()
                };
                let total = rows.len();
                let _ = tx.send(Ok(serde_json::json!({"touches": rows, "total": total})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("persist.touches timeout".to_string()))
        }
        "persist.judges" => {
            let since = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
            let limit = args
                .get(1)
                .and_then(|v| v.as_i64())
                .unwrap_or(100)
                .clamp(1, 1000);
            let round_uuid = args
                .get(2)
                .and_then(|v| v.as_str())
                .filter(|v| !v.is_empty())
                .map(str::to_string);
            let player_id = args
                .get(3)
                .and_then(|v| v.as_i64())
                .and_then(|v| i32::try_from(v).ok());
            let (tx, rx) = std::sync::mpsc::channel::<Result<Value, String>>();
            tokio::spawn(async move {
                let rows = if let Some(db) = crate::internal_hooks::DB.get() {
                    db.query_judge_batches(since, limit, round_uuid.as_deref(), player_id)
                        .await
                } else {
                    Vec::new()
                };
                let total = rows.len();
                let _ = tx.send(Ok(serde_json::json!({"judges": rows, "total": total})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("persist.judges timeout".to_string()))
        }
        "persist.playtime" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if uid <= 0 {
                return Err("invalid user_id".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            tokio::spawn(async move {
                let row = if let Some(db) = crate::internal_hooks::DB.get() {
                    db.get_playtime(uid).await
                } else {
                    None
                };
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let total = row
                    .as_ref()
                    .map(|r| {
                        r.total_secs + r.session_start.map(|s| now.saturating_sub(s)).unwrap_or(0)
                    })
                    .unwrap_or(0);
                let _ = tx.send(Ok(serde_json::json!({"user_id": uid, "total_secs": total, "session_start": row.and_then(|r| r.session_start)})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("persist.playtime timeout".to_string()))
        }
        "persist.top_playtime" => {
            let limit = args
                .get(0)
                .and_then(|v| v.as_i64())
                .unwrap_or(10)
                .clamp(1, 100);
            let (tx, rx) = std::sync::mpsc::channel::<Result<Value, String>>();
            tokio::spawn(async move {
                let rows = if let Some(db) = crate::internal_hooks::DB.get() {
                    db.top_playtime(limit).await
                } else {
                    Vec::new()
                };
                let total = rows.len();
                let _ = tx.send(Ok(serde_json::json!({"players": rows, "total": total})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("persist.top_playtime timeout".to_string()))
        }
        "admin.ids" => {
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let ids = s.admin_id_list().await;
                let _ = tx.send(Ok(serde_json::json!({"admin_phira_ids": ids})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.ids timeout".to_string()))
        }
        "admin.is_admin" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let value = s.is_admin_id(uid).await;
                let _ = tx.send(Ok(serde_json::json!({"user_id": uid, "admin": value})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.is_admin timeout".to_string()))
        }
        "admin.add_id" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if uid <= 0 {
                return Err("invalid user_id".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let ids = s.add_admin_id(uid).await;
                let _ = tx.send(Ok(serde_json::json!({"admin_phira_ids": ids})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.add_id timeout".to_string()))
        }
        "admin.remove_id" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            if uid <= 0 {
                return Err("invalid user_id".to_string());
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let ids = s.remove_admin_id(uid).await;
                let _ = tx.send(Ok(serde_json::json!({"admin_phira_ids": ids})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.remove_id timeout".to_string()))
        }
        "admin.set_ids" => {
            let mut ids = Vec::new();
            if let Some(array) = args.get(0).and_then(|v| v.as_array()) {
                ids.extend(
                    array
                        .iter()
                        .filter_map(|v| v.as_i64())
                        .filter_map(|v| i32::try_from(v).ok())
                        .filter(|id| *id > 0),
                );
            } else {
                ids.extend(
                    args.iter()
                        .filter_map(|v| v.as_i64())
                        .filter_map(|v| i32::try_from(v).ok())
                        .filter(|id| *id > 0),
                );
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let s = Arc::clone(state);
            tokio::spawn(async move {
                let ids = s.set_admin_ids(ids).await;
                let _ = tx.send(Ok(serde_json::json!({"admin_phira_ids": ids})));
            });
            rx.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or(Err("admin.set_ids timeout".to_string()))
        }
        _ => {
            // webapi feature 下的扩展查询
            server_state_query(state, method, args)
        }
    }
}

/// Public wrapper for [`server_state_query`], usable from WIT host traits.
pub fn server_state_query_for_host(
    state: &Arc<PlusServerState>,
    method: &str,
    args: &[Value],
) -> Result<Value, String> {
    server_state_query(state, method, args)
}

/// Web API 状态查询（内置，无 feature gate）
fn server_state_query(
    state: &Arc<PlusServerState>,
    method: &str,
    args: &[Value],
) -> Result<Value, String> {
    #[derive(Serialize)]
    struct RoomSnapshot {
        name: String,
        data: RoomData,
    }
    #[derive(Serialize)]
    struct UserSnapshot {
        id: i32,
        name: String,
        monitor: bool,
        is_host: bool,
        in_room: bool,
        has_session: bool,
    }
    #[derive(Serialize)]
    struct RoomData {
        // Backward-compatible fields used by the old Web API consumers.
        host: i32,
        users: Vec<i32>,
        lock: bool,
        cycle: bool,
        chart: Option<i32>,
        chart_name: Option<String>,
        state: String,
        playing_users: Vec<i32>,
        rounds: Vec<RoundInfo>,
        hidden: bool,
        phira_api_endpoint: String,
        phira_api_endpoint_override: Option<String>,

        // Full room information for dashboards and admin panels.
        id: String,
        uuid: String,
        created_at: i64,
        live: bool,
        locked: bool,
        cycling: bool,
        persistent_empty: bool,
        max_users: usize,
        player_count: usize,
        monitor_count: usize,
        user_ids: Vec<i32>,
        monitor_ids: Vec<i32>,
        host_user: Option<UserSnapshot>,
        host_is_system: bool,
        users_info: Vec<UserSnapshot>,
        monitors_info: Vec<UserSnapshot>,
        chart_info: Option<Value>,
        phira_api_endpoint_effective: String,
        phira_api_endpoint_using_override: bool,
        ready_users: Vec<i32>,
        finished_users: Vec<i32>,
        aborted_users: Vec<i32>,
        result_count: usize,
        current_round_id: Option<String>,
        state_detail: Value,
        round_history: Vec<RoundInfo>,
    }
    #[derive(Serialize, Clone)]
    struct RoundInfo {
        round_id: String,
        chart: i32,
        chart_id: i32,
        chart_name: String,
        records: Vec<Value>,
        results: Vec<Value>,
    }

    fn user_snapshot(
        room: &crate::room::Room,
        user: &super::session::User,
        is_host: bool,
        in_room: bool,
    ) -> UserSnapshot {
        let has_session = user
            .session
            .try_read()
            .ok()
            .and_then(|session| session.as_ref().and_then(|weak| weak.upgrade()))
            .is_some();
        UserSnapshot {
            id: user.id,
            name: room.display_name_sync(user),
            monitor: user.monitor.load(Ordering::SeqCst),
            is_host,
            in_room,
            has_session,
        }
    }

    fn build_snapshot(
        state: &PlusServerState,
        name: &str,
        room: &crate::room::Room,
    ) -> RoomSnapshot {
        let chart_op = read_lock!(room.chart).clone();
        let users_arcs: Vec<_> = {
            let ul = read_lock!(room.users);
            ul.iter().filter_map(|w| w.upgrade()).collect()
        };
        let monitor_arcs: Vec<_> = {
            let ml = read_lock!(room.monitors);
            ml.iter().filter_map(|w| w.upgrade()).collect()
        };
        let host_arc = read_lock!(room.host).upgrade();
        let host_is_system = room.is_system_host();
        let host = host_arc.as_ref().map(|u| u.id).unwrap_or(-1);

        let guard = read_lock!(room.state);
        let (
            st,
            playing_users,
            ready_users,
            finished_users,
            aborted_users,
            result_count,
            state_detail,
        ) = match &*guard {
            crate::room::InternalRoomState::SelectChart => (
                "SELECTING_CHART".to_string(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                0usize,
                serde_json::json!({"kind":"select_chart"}),
            ),
            crate::room::InternalRoomState::WaitForReady { started, .. } => {
                let mut ready: Vec<i32> = started.iter().copied().collect();
                ready.sort_unstable();
                (
                    "WAITING_FOR_READY".to_string(),
                    Vec::new(),
                    ready.clone(),
                    Vec::new(),
                    Vec::new(),
                    0usize,
                    serde_json::json!({"kind":"wait_for_ready", "ready_users": ready}),
                )
            }
            crate::room::InternalRoomState::Playing { results, aborted } => {
                let mut finished: Vec<i32> = results.keys().copied().collect();
                finished.sort_unstable();
                let mut aborted_vec: Vec<i32> = aborted.iter().copied().collect();
                aborted_vec.sort_unstable();
                let playing: Vec<i32> = users_arcs
                    .iter()
                    .filter(|u| !results.contains_key(&u.id) && !aborted.contains(&u.id))
                    .map(|u| u.id)
                    .collect();
                (
                    "PLAYING".to_string(),
                    playing,
                    Vec::new(),
                    finished.clone(),
                    aborted_vec.clone(),
                    results.len(),
                    serde_json::json!({
                        "kind":"playing",
                        "finished_users": finished,
                        "aborted_users": aborted_vec,
                        "result_count": results.len(),
                    }),
                )
            }
        };
        drop(guard);

        let mut users: Vec<i32> = users_arcs.iter().map(|u| u.id).collect();
        let monitor_ids: Vec<i32> = monitor_arcs.iter().map(|u| u.id).collect();
        users.extend(monitor_ids.iter().copied());
        let user_ids: Vec<i32> = users_arcs.iter().map(|u| u.id).collect();

        let users_info: Vec<UserSnapshot> = users_arcs
            .iter()
            .map(|u| user_snapshot(room, u, u.id == host, true))
            .collect();
        let monitors_info: Vec<UserSnapshot> = monitor_arcs
            .iter()
            .map(|u| user_snapshot(room, u, u.id == host, true))
            .collect();
        let host_user = host_arc.as_ref().map(|u| {
            user_snapshot(
                room,
                u,
                true,
                user_ids.contains(&u.id) || monitor_ids.contains(&u.id),
            )
        });

        let phira_api_endpoint =
            room.effective_phira_api_endpoint_sync(&state.config.phira_api_endpoint);
        let phira_api_endpoint_override = room.phira_api_endpoint_override_sync();
        let using_override = phira_api_endpoint_override.is_some();
        let current_round_id = read_lock!(room.current_round_id)
            .as_ref()
            .map(|id| id.to_string());
        let hist = read_lock!(room.play_history);
        let rounds: Vec<RoundInfo> = hist
            .iter()
            .map(|r| {
                let results: Vec<Value> = r
                    .results
                    .iter()
                    .map(|res| {
                        serde_json::json!({
                            "player": res.user_id,
                            "user_id": res.user_id,
                            "user_name": res.user_name.clone(),
                            "score": res.score,
                            "accuracy": res.accuracy,
                            "perfect": res.perfect,
                            "good": res.good,
                            "bad": res.bad,
                            "miss": res.miss,
                            "max_combo": res.max_combo,
                            "full_combo": res.full_combo,
                            "aborted": res.aborted,
                            "std_score": res.std_score,
                        })
                    })
                    .collect();
                RoundInfo {
                    round_id: r.round_id.to_string(),
                    chart: r.chart_id,
                    chart_id: r.chart_id,
                    chart_name: r.chart_name.clone(),
                    records: results.clone(),
                    results,
                }
            })
            .collect();
        drop(hist);

        let chart_info = chart_op.as_ref().map(|c| {
            serde_json::json!({
                "id": c.id,
                "name": c.name.clone(),
            })
        });

        RoomSnapshot {
            name: name.into(),
            data: RoomData {
                host,
                users,
                lock: room.locked.load(Ordering::SeqCst),
                cycle: room.cycle.load(Ordering::SeqCst),
                chart: chart_op.as_ref().map(|c| c.id),
                chart_name: chart_op.as_ref().map(|c| c.name.clone()),
                state: st,
                playing_users,
                rounds: rounds.clone(),
                hidden: room.is_hidden(),
                phira_api_endpoint: phira_api_endpoint.clone(),
                phira_api_endpoint_override: phira_api_endpoint_override.clone(),
                id: name.into(),
                uuid: room.uuid.to_string(),
                created_at: room.created_at,
                live: room.is_live(),
                locked: room.is_locked(),
                cycling: room.is_cycle(),
                persistent_empty: room.is_persistent_empty(),
                max_users: room.max_users_count(),
                player_count: user_ids.len(),
                monitor_count: monitor_ids.len(),
                user_ids,
                monitor_ids,
                host_user,
                host_is_system,
                users_info,
                monitors_info,
                chart_info,
                phira_api_endpoint_effective: phira_api_endpoint,
                phira_api_endpoint_using_override: using_override,
                ready_users,
                finished_users,
                aborted_users,
                result_count,
                current_round_id,
                state_detail,
                round_history: rounds,
            },
        }
    }

    match method {
        "rooms.list" => {
            let rooms = read_lock!(state.rooms);
            let list: Vec<Value> = rooms
                .iter()
                .filter(|(_, room)| !room.is_hidden())
                .map(|(rid, room)| {
                    let ss = build_snapshot(state, &rid.to_string(), room);
                    serde_json::to_value(ss).unwrap_or_default()
                })
                .collect();
            Ok(Value::Array(list))
        }
        "rooms.by_name" => {
            let name = args.get(0).and_then(|v| v.as_str()).unwrap_or("");
            let rid: phira_mp_common::RoomId = name
                .to_string()
                .try_into()
                .map_err(|_| "invalid room name".to_string())?;
            let rooms = read_lock!(state.rooms);
            let room = rooms.get(&rid).ok_or("room not found")?;
            if room.is_hidden() {
                return Err("room not found".to_string());
            }
            let ss = build_snapshot(state, name, room);
            serde_json::to_value(ss).map_err(|e| e.to_string())
        }
        "user_name" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let users = read_lock!(state.users);
            let name = users.get(&uid).map(|u| u.name.clone());
            drop(users);
            Ok(serde_json::json!({"user_id": uid, "name": name}))
        }
        "send_chat" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let msg = args
                .get(1)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let server = Arc::clone(state);
            tokio::spawn(async move {
                let users = server.users.read().await;
                if let Some(user) = users.get(&uid) {
                    user.try_send(phira_mp_common::ServerCommand::Message(
                        phira_mp_common::Message::Chat {
                            user: 0,
                            content: msg,
                        },
                    ))
                    .await;
                }
            });
            Ok(serde_json::json!({"sent": true}))
        }
        "send_room_chat" => {
            let room_name = args
                .get(0)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let msg = args
                .get(1)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if room_name.starts_with('.') {
                return Ok(serde_json::json!({"sent": false}));
            }
            let rid: phira_mp_common::RoomId = match room_name.try_into() {
                Ok(r) => r,
                Err(_) => return Ok(serde_json::json!({"sent": false, "error": "invalid room"})),
            };
            let rooms = read_lock!(state.rooms);
            if let Some(room) = rooms.get(&rid) {
                let content = format!("[结算] {}", msg);
                let cmd = phira_mp_common::ServerCommand::Message(phira_mp_common::Message::Chat {
                    user: 0,
                    content,
                });
                let users_list = read_lock!(room.users);
                let monitors_list = read_lock!(room.monitors);
                let mut sent = 0usize;
                for wu in users_list.iter().chain(monitors_list.iter()) {
                    if let Some(u) = wu.upgrade() {
                        let session = read_lock!(u.session);
                        if let Some(session) = session.as_ref().and_then(|w| w.upgrade()) {
                            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                                let cmd = cmd.clone();
                                handle.spawn(async move {
                                    let _ = session.stream.send(cmd).await;
                                });
                                sent += 1;
                            } else {
                                let cmd = cmd.clone();
                                std::thread::spawn(move || {
                                    // 没有 tokio 上下文，创建临时运行时
                                    let rt = tokio::runtime::Builder::new_current_thread()
                                        .enable_all()
                                        .build()
                                        .expect("build rt");
                                    rt.block_on(async move {
                                        let _ = session.stream.send(cmd).await;
                                    });
                                });
                                sent += 1;
                            }
                        }
                    }
                }
                Ok(serde_json::json!({"sent": sent}))
            } else {
                Ok(serde_json::json!({"sent": false, "error": "room not found"}))
            }
        }
        "rooms.by_user" => {
            let uid = args.get(0).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let user = {
                let users = read_lock!(state.users);
                users.get(&uid).map(Arc::clone).ok_or("user not found")?
            };
            let rg = read_lock!(user.room);
            let room = rg.as_ref().ok_or("user not in room")?;
            let name = room.id.to_string();
            let ss = build_snapshot(state, &name, room);
            serde_json::to_value(ss).map_err(|e| e.to_string())
        }
        _ => Err(format!("unknown query method: {method}")),
    }
}

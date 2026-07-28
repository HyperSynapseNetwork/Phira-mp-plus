//! Local Mock Phira HTTP server
//!
//! 本地 Mock Phira HTTP 服务器，在 Real 模式下替代真实的 Phira API。
//! 使用 Axum 在随机空闲端口启动。
//! 提供标准的 /me、/user/{id}、/chart/{id} 端点用于认证和数据查询。
//!
//! ## 故障注入
//!
//! `MockPhiraConfig` 中的故障参数：
//! - `delay_ms`: 人工响应延迟
//! - `jitter_ms`: 延迟上的随机抖动
//! - `error_rate`: 返回错误的请求比例 (0.0-1.0)
//! - `timeout_ms`: 触发客户端超时的慢响应（使延迟显著增加）
//! - `seed`: 确定性随机种子

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::info;

/// Mock Phira 服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockPhiraConfig {
    /// 模拟的 API 响应延迟（毫秒）
    pub delay_ms: u64,
    /// 随机的延迟抖动范围（毫秒）
    pub jitter_ms: u64,
    /// 模拟的错误率（0.0 ~ 1.0），0.01 表示 1% 的请求会返回错误
    pub error_rate: f64,
    /// 模拟的请求超时时间——触发客户端超时的慢响应
    pub timeout_ms: u64,
    /// 超时注入概率（0.0 ~ 1.0），默认 0.0，即不注入超时
    pub timeout_rate: f64,
    /// 随机种子，用于确定性回放
    pub seed: u64,
    /// 模拟的响应体大小（字节）
    pub response_size: usize,
    /// 监听的地址
    pub listen_addr: String,
    /// 是否记录所有请求日志
    pub verbose: bool,
}

impl Default for MockPhiraConfig {
    fn default() -> Self {
        Self {
            delay_ms: 5,
            jitter_ms: 2,
            error_rate: 0.0,
            timeout_ms: 30_000,
            timeout_rate: 0.0,
            seed: 114_514,
            response_size: 1024,
            listen_addr: "127.0.0.1:9877".to_string(),
            verbose: false,
        }
    }
}

/// Mock Phira 服务器
///
/// 在后台启动一个 Axum HTTP 服务器，模拟 Phira API 端点。
/// 提供 `start` / `stop` / `port` 方法控制生命周期和获取端口。
pub struct MockPhiraServer {
    config: MockPhiraConfig,
    shutdown_tx: Arc<std::sync::RwLock<Option<oneshot::Sender<()>>>>,
    handle: Arc<std::sync::RwLock<Option<JoinHandle<()>>>>,
    port: Arc<std::sync::RwLock<Option<u16>>>,
}

impl MockPhiraServer {
    /// 使用给定配置创建 Mock Phira 服务器（尚未启动）
    pub fn new(config: MockPhiraConfig) -> Self {
        Self {
            config,
            shutdown_tx: Arc::new(std::sync::RwLock::new(None)),
            handle: Arc::new(std::sync::RwLock::new(None)),
            port: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// 启动 Mock Phira HTTP 服务器
    ///
    /// 绑定到随机空闲端口，构建 Axum 路由并将配置注入为共享状态。
    /// 路由：
    /// - `GET /me` — 返回基准用户信息（从 Authorization header 提取 token）
    /// - `GET /user/{id}` — 返回测试用户数据
    /// - `GET /chart/{id}` — 返回测试谱面数据
    /// - `GET /record/{id}` — 返回测试游玩记录
    pub async fn start(&self) -> Result<(), String> {
        let state = Arc::new(MockPhiraState {
            config: self.config.clone(),
            counter: std::sync::atomic::AtomicU64::new(0),
        });

        let app = Router::new()
            .route("/me", get(me_handler))
            .route("/user/{id}", get(user_handler))
            .route("/chart/{id}", get(chart_handler))
            .route("/record/{id}", get(record_handler))
            .with_state(Arc::clone(&state));

        let bind_addr = if self.config.listen_addr != "127.0.0.1:9877" {
            self.config.listen_addr.as_str()
        } else {
            "127.0.0.1:0"
        };
        let listener = TcpListener::bind(bind_addr)
            .await
            .map_err(|e| format!("failed to bind Mock Phira server on {bind_addr}: {e}"))?;

        let local_addr = listener
            .local_addr()
            .map_err(|e| format!("failed to get local address: {e}"))?;
        let port = local_addr.port();

        let (tx, rx) = oneshot::channel::<()>();
        *self.shutdown_tx.write().map_err(|_| "lock error")? = Some(tx);
        *self.port.write().map_err(|_| "lock error")? = Some(port);

        let handle = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
                .ok();
        });

        *self.handle.write().map_err(|_| "lock error")? = Some(handle);

        info!("Mock Phira server listening on 127.0.0.1:{}", port);
        Ok(())
    }

    /// 停止 Mock Phira HTTP 服务器
    pub async fn stop(&self) -> Result<(), String> {
        let tx = self
            .shutdown_tx
            .write()
            .map_err(|_| "lock error")?
            .take();
        if let Some(tx) = tx {
            let _ = tx.send(());
        }

        let handle = { self.handle.write().map_err(|_| "lock error")?.take() };
        self.port.write().map_err(|_| "lock error")?.take();

        if let Some(handle) = handle {
            let _ = handle.await;
        }
        Ok(())
    }

    /// 返回服务器当前监听的端口号
    pub fn port(&self) -> Option<u16> {
        self.port.read().ok().and_then(|g| *g)
    }

    /// 返回服务器当前监听地址（含端口）
    pub fn listen_addr(&self) -> String {
        self.port()
            .map(|p| format!("127.0.0.1:{}", p))
            .unwrap_or_else(|| self.config.listen_addr.clone())
    }

    /// 返回配置引用
    pub fn config(&self) -> &MockPhiraConfig {
        &self.config
    }
}

// ── MockPhiraState ────────────────────────────────────────────────────

/// Axum 共享状态，包含配置和 per-instance 计数器。
struct MockPhiraState {
    config: MockPhiraConfig,
    counter: std::sync::atomic::AtomicU64,
}

// ── 故障注入辅助函数 ──────────────────────────────────────────────

/// 基于 seed 和 counter 的确定性伪随机数生成器 (0.0 .. 1.0)
fn seeded_f64(seed: u64, counter: u64) -> f64 {
    let mut hasher = Sha256::new();
    hasher.update(seed.to_le_bytes());
    hasher.update(counter.to_le_bytes());
    let hash = hasher.finalize();
    let val = u64::from_le_bytes(hash[..8].try_into().unwrap());
    (val >> 11) as f64 / (1u64 << 53) as f64
}

/// 获取 per-server-instance 的单调递增计数器。
fn next_counter(state: &MockPhiraState) -> u64 {
    state.counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// 根据配置应用延迟和抖动。
async fn apply_delay(state: &MockPhiraState) {
    let base = state.config.delay_ms as f64;
    if base <= 0.0 && state.config.jitter_ms == 0 {
        return;
    }
    let jitter = if state.config.jitter_ms > 0 {
        let r = seeded_f64(state.config.seed, next_counter(state));
        (r * 2.0 - 1.0) * state.config.jitter_ms as f64
    } else {
        0.0
    };
    let delay_ms = (base + jitter).max(0.0);
    if delay_ms > 0.0 {
        tokio::time::sleep(Duration::from_secs_f64(delay_ms / 1000.0)).await;
    }
}

/// 检查当前请求是否应返回错误（基于 error_rate）
fn should_error(state: &MockPhiraState) -> bool {
    if state.config.error_rate <= 0.0 {
        return false;
    }
    let r = seeded_f64(state.config.seed, next_counter(state));
    r < state.config.error_rate
}

/// 检查当前请求是否应触发超时（基于 timeout_rate）
fn should_timeout(state: &MockPhiraState) -> bool {
    if state.config.timeout_rate <= 0.0 || state.config.timeout_ms <= state.config.delay_ms {
        return false;
    }
    let r = seeded_f64(state.config.seed, next_counter(state));
    r < state.config.timeout_rate
}

/// 从 Authorization header 提取 token
fn extract_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")?
        .to_str()
        .ok()
        .map(|v| {
            v.trim_start_matches("Bearer ")
                .trim_start_matches("bearer ")
        })
}

/// 从 token 字符串解析出确定的用户 ID
///
/// Token 格式: `bench-{hex_hash}` (22 字符)
/// 用户 ID 范围: 1_000_000 + (hash % 1_000_000)
/// 对非 bench- 前缀的 token 返回 999
fn token_to_user_id(token: &str) -> i32 {
    if let Some(hash_str) = token.strip_prefix("bench-") {
        if let Ok(hash) = u64::from_str_radix(hash_str, 16) {
            return 1_000_000 + (hash % 1_000_000) as i32;
        }
    }
    999 // fallback
}

/// 组合故障注入检查（延迟 + 错误率）
async fn apply_faults(state: &MockPhiraState) -> Result<(), ()> {
    if should_timeout(state) {
        tokio::time::sleep(Duration::from_millis(state.config.timeout_ms)).await;
    } else {
        apply_delay(state).await;
    }

    if should_error(state) {
        if state.config.verbose {
            tracing::info!("mock_phira: injecting error");
        }
        return Err(());
    }
    Ok(())
}

// ── Axum 请求处理器 ──────────────────────────────────────────────

/// 构建响应体，如果 response_size 配置大于默认 JSON 大小则用空格填充。
fn build_payload(mut base: Value, state: &MockPhiraState) -> Value {
    if state.config.response_size > 0 {
        let default_size = serde_json::to_string(&base).unwrap_or_default().len();
        if let Some(pad_needed) = state.config.response_size.checked_sub(default_size) {
            if pad_needed > 0 {
                let padding = " ".repeat(pad_needed);
                base["_padding"] = Value::String(padding);
            }
        }
    }
    base
}

/// `GET /me` — 返回基准用户信息
async fn me_handler(
    State(state): State<Arc<MockPhiraState>>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    if state.config.verbose {
        tracing::info!("mock_phira: GET /me");
    }
    if apply_faults(&state).await.is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "mock internal error"})),
        );
    }

    let user_id = extract_token(&headers)
        .map(token_to_user_id)
        .unwrap_or(999);

    let payload = build_payload(json!({
        "id": user_id,
        "name": format!("bench-user-{}", user_id),
        "language": "zh-CN"
    }), &state);
    (StatusCode::OK, Json(payload))
}

/// `GET /user/{id}` — 返回指定用户信息
async fn user_handler(
    State(state): State<Arc<MockPhiraState>>,
    Path(id): Path<i32>,
) -> (StatusCode, Json<Value>) {
    if state.config.verbose {
        tracing::info!("mock_phira: GET /user/{id}");
    }
    if apply_faults(&state).await.is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "mock internal error"})),
        );
    }

    let payload = build_payload(json!({
        "id": id,
        "name": format!("user-{}", id),
        "language": "zh-CN"
    }), &state);
    (StatusCode::OK, Json(payload))
}

/// `GET /chart/{id}` — 返回指定谱面信息
async fn chart_handler(
    State(state): State<Arc<MockPhiraState>>,
    Path(id): Path<i32>,
) -> (StatusCode, Json<Value>) {
    if state.config.verbose {
        tracing::info!("mock_phira: GET /chart/{id}");
    }
    if apply_faults(&state).await.is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "mock internal error"})),
        );
    }

    let payload = build_payload(json!({
        "id": id,
        "name": format!("chart-{}", id)
    }), &state);
    (StatusCode::OK, Json(payload))
}

/// `GET /record/{id}` — 返回测试游玩记录
async fn record_handler(
    State(state): State<Arc<MockPhiraState>>,
    Path(id): Path<i32>,
) -> (StatusCode, Json<Value>) {
    if state.config.verbose {
        tracing::info!("mock_phira: GET /record/{id}");
    }
    if apply_faults(&state).await.is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "mock internal error"})),
        );
    }

    let payload = build_payload(json!({
        "id": id,
        "player": 999,
        "score": 998877,
        "accuracy": 0.9876,
        "perfect": 800,
        "good": 10,
        "bad": 2,
        "miss": 1,
        "max_combo": 500,
        "full_combo": true,
        "std": 0,
        "std_score": 998877
    }), &state);
    (StatusCode::OK, Json(payload))
}

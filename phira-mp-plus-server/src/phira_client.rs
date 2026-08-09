//! Unified Phira HTTP client for Runtime.
//!
//! The old code had retry/timeout handling embedded in session paths.  This
//! module is the first central seam for all Phira HTTP traffic: authentication,
//! chart lookup and record lookup converge here.

use anyhow::{bail, Result};
use phira_mp_common::{Message, ServerCommand, StreamSender};
use reqwest::header::RANGE;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::HashMap;
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        RwLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::time;
use tracing::warn;

pub const PHIRA_RETRY_NOTICE: &str = "Phira服务器太烂了，我们正在重试以保证你的流畅体验";
pub const PHIRA_LEGACY_502_MARKER: &str = "认证失败 502错误";
pub const PHIRA_LEGACY_502_TEXT: &str =
    "认证失败 502错误 Phira服务器太烂了，我们正在重试以保证你的流畅体验 /拜谢";

/// zip 中央目录条目（只取谱面时长解析所需的字段）。
struct ZipEntry {
    compression: u16,
    compressed_size: u32,
    uncompressed_size: u32,
    fn_len: u16,
    extra_len: u16,
    local_offset: u32,
}

/// MPEG Layer III 比特率表（kbps）。index 0/15 = free/bad。
const MP3_BITRATES_V1: [u32; 16] = [0,32,40,48,56,64,80,96,112,128,160,192,224,256,320,0];
const MP3_BITRATES_V2: [u32; 16] = [0,8,16,24,32,40,48,56,64,80,96,112,128,144,160,0];
const MP3_SAMPLES_V1: [u32; 4] = [44100,48000,32000,0];
const MP3_SAMPLES_V2: [u32; 4] = [22050,24000,16000,0];

/// 手动扫描 MPEG1/2 Layer III 帧计算时长（秒）。VBR 也准确——
/// 逐帧用实际比特率累加帧时长，不依赖 Xing/首帧估算。
/// 返回 None 表示不是（或无法解析为）MP3 帧流。
fn mp3_duration_scan(data: &[u8]) -> Option<f64> {
    let mut total = 0.0f64;
    let mut frames = 0u32;
    let mut i = 0usize;
    while i + 4 <= data.len() {
        // 帧同步：0xFF + 前 3 位 1。
        if data[i] != 0xFF || data[i + 1] & 0xE0 != 0xE0 {
            i += 1;
            continue;
        }
        let h = &data[i..i + 4];
        let version = (h[1] >> 3) & 3; // 3=MPEG1, 2=MPEG2
        let layer = (h[1] >> 1) & 3; // 3 = Layer III
        if layer != 3 {
            i += 1;
            continue;
        }
        let bri = ((h[2] >> 4) & 0xF) as usize;
        let sri = ((h[2] >> 2) & 3) as usize;
        let padding = (h[2] >> 1) & 1;
        let (bitrate, sample) = if version == 3 {
            (MP3_BITRATES_V1[bri], MP3_SAMPLES_V1[sri])
        } else {
            (MP3_BITRATES_V2[bri], MP3_SAMPLES_V2[sri])
        };
        if bitrate == 0 || sample == 0 {
            i += 1;
            continue;
        }
        // MPEG1 Layer III：帧长 = 144 * bitrate / sample + padding；
        // MPEG2 Layer III 每帧半采样率，帧长减半（72 * bitrate / sample）。
        let frame_len = if version == 3 {
            144 * bitrate * 1000 / sample + padding as u32
        } else {
            72 * bitrate * 1000 / sample + padding as u32
        };
        if frame_len == 0 {
            i += 1;
            continue;
        }
        total += frame_len as f64 * 8.0 / (bitrate as f64 * 1000.0);
        frames += 1;
        i += frame_len as usize;
    }
    if frames > 0 { Some(total) } else { None }
}

/// 用 lofty 探针解析音频时长（MP3/FLAC/WAV/OGG），返回秒。
/// lofty 对 VBR MP3 的时长估算不可靠（按首帧比特率），故 MP3 优先用手动
/// 帧扫描（`mp3_duration_scan`）；其它格式回落 lofty。
/// lofty 的 Probe API 只接受文件路径，音频先落临时文件再解析。
fn probe_audio_duration(audio: &[u8]) -> Option<f64> {
    // MP3：跳过 ID3v2 标签头后手动扫描帧（VBR 准确）。
    let mut offset = 0usize;
    if audio.len() >= 10 && &audio[0..3] == b"ID3" {
        let tag_size = u32::from_be_bytes([audio[6], audio[7], audio[8], audio[9]]) as usize;
        offset = 10 + (tag_size & 0x7f) + ((tag_size >> 8) & 0x7f) + ((tag_size >> 16) & 0x7f) + ((tag_size >> 24) & 0x7f);
    }
    if let Some(dur) = mp3_duration_scan(&audio[offset.min(audio.len())..]) {
        return Some(dur);
    }

    use lofty::prelude::*;
    use lofty::probe::Probe;

    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    // pid + 自增序号保证并发房间同时解析时不冲突
    let path = std::env::temp_dir().join(format!(
        "pmp-audio-{}-{}.bin",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&path, audio).ok()?;
    let result = Probe::open(&path)
        .ok()
        .and_then(|p| p.read().ok())
        .map(|t| t.properties().duration().as_secs_f64());
    let _ = std::fs::remove_file(&path);
    result
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct PhiraHttpPolicyConfig {
    /// Per-request timeout in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Number of retry attempts after the first failed attempt.
    #[serde(default)]
    pub max_retries: Option<usize>,
    /// Initial retry backoff in milliseconds.
    #[serde(default)]
    pub base_backoff_ms: Option<u64>,
    /// Maximum retry backoff in milliseconds.
    #[serde(default)]
    pub max_backoff_ms: Option<u64>,
    /// Circuit breaker settings for fragile Phira upstreams.
    #[serde(default)]
    pub circuit_breaker: PhiraCircuitBreakerConfig,
}

impl PhiraHttpPolicyConfig {
    pub fn into_policy(self) -> PhiraHttpPolicy {
        let defaults = PhiraHttpPolicy::default();
        let timeout_ms = self
            .timeout_ms
            .unwrap_or(defaults.timeout.as_millis() as u64)
            .clamp(500, 60_000);
        let max_retries = self.max_retries.unwrap_or(defaults.max_retries).min(10);
        let base_ms = self
            .base_backoff_ms
            .unwrap_or(defaults.base_backoff.as_millis() as u64)
            .clamp(50, 30_000);
        let max_ms = self
            .max_backoff_ms
            .unwrap_or(defaults.max_backoff.as_millis() as u64)
            .clamp(base_ms, 120_000);
        PhiraHttpPolicy {
            timeout: Duration::from_millis(timeout_ms),
            max_retries,
            base_backoff: Duration::from_millis(base_ms),
            max_backoff: Duration::from_millis(max_ms),
            circuit_breaker: self.circuit_breaker.into_policy(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct PhiraCircuitBreakerConfig {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub failure_threshold: Option<u64>,
    #[serde(default)]
    pub open_duration_ms: Option<u64>,
}

impl PhiraCircuitBreakerConfig {
    fn into_policy(self) -> PhiraCircuitBreakerPolicy {
        let defaults = PhiraCircuitBreakerPolicy::default();
        PhiraCircuitBreakerPolicy {
            enabled: self.enabled.unwrap_or(defaults.enabled),
            failure_threshold: self
                .failure_threshold
                .unwrap_or(defaults.failure_threshold)
                .clamp(2, 100),
            open_duration: Duration::from_millis(
                self.open_duration_ms
                    .unwrap_or(defaults.open_duration.as_millis() as u64)
                    .clamp(1_000, 300_000),
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PhiraHttpPolicySnapshot {
    pub timeout_ms: u64,
    pub max_retries: usize,
    pub base_backoff_ms: u64,
    pub max_backoff_ms: u64,
    pub circuit_breaker_enabled: bool,
    pub circuit_breaker_failure_threshold: u64,
    pub circuit_breaker_open_ms: u64,
}

#[derive(Debug, Clone)]
pub struct PhiraHttpPolicy {
    pub timeout: Duration,
    pub max_retries: usize,
    pub base_backoff: Duration,
    pub max_backoff: Duration,
    pub circuit_breaker: PhiraCircuitBreakerPolicy,
}

impl Default for PhiraHttpPolicy {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            max_retries: 3,
            base_backoff: Duration::from_millis(200),
            max_backoff: Duration::from_secs(3),
            circuit_breaker: PhiraCircuitBreakerPolicy::default(),
        }
    }
}

impl PhiraHttpPolicy {
    pub fn snapshot(&self) -> PhiraHttpPolicySnapshot {
        PhiraHttpPolicySnapshot {
            timeout_ms: self.timeout.as_millis() as u64,
            max_retries: self.max_retries,
            base_backoff_ms: self.base_backoff.as_millis() as u64,
            max_backoff_ms: self.max_backoff.as_millis() as u64,
            circuit_breaker_enabled: self.circuit_breaker.enabled,
            circuit_breaker_failure_threshold: self.circuit_breaker.failure_threshold,
            circuit_breaker_open_ms: self.circuit_breaker.open_duration.as_millis() as u64,
        }
    }

    fn backoff_delay(&self, attempt: usize) -> Duration {
        let base_ms = self.base_backoff.as_millis().max(1) as u64;
        let max_ms = self.max_backoff.as_millis().max(1) as u64;
        // Deterministic jitter: enough to avoid perfectly synchronized retries
        // without adding another randomness dependency to hot paths.
        let jitter_ms = ((attempt as u64 * 37) + 11) % 50;
        let delay_ms = base_ms
            .saturating_mul(attempt as u64 + 1)
            .saturating_add(jitter_ms)
            .min(max_ms);
        Duration::from_millis(delay_ms)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PhiraCircuitBreakerStats {
    pub enabled: bool,
    pub state: String,
    pub failure_threshold: u64,
    pub open_duration_ms: u64,
    pub consecutive_failures: u64,
    pub opened: u64,
    pub rejected: u64,
    pub open_until_ms: u64,
    pub remaining_open_ms: u64,
}

#[derive(Debug, Clone)]
pub struct PhiraCircuitBreakerPolicy {
    pub enabled: bool,
    pub failure_threshold: u64,
    pub open_duration: Duration,
}

impl Default for PhiraCircuitBreakerPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            failure_threshold: 8,
            open_duration: Duration::from_secs(20),
        }
    }
}

#[derive(Debug)]
struct PhiraCircuitBreaker {
    policy: PhiraCircuitBreakerPolicy,
    consecutive_failures: AtomicU64,
    opened: AtomicU64,
    rejected: AtomicU64,
    open_until_ms: AtomicU64,
}

impl PhiraCircuitBreaker {
    fn new(policy: PhiraCircuitBreakerPolicy) -> Self {
        Self {
            policy,
            consecutive_failures: AtomicU64::new(0),
            opened: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            open_until_ms: AtomicU64::new(0),
        }
    }

    fn allow_request(&self) -> bool {
        if !self.policy.enabled {
            return true;
        }
        let now = now_ms();
        let open_until = self.open_until_ms.load(Ordering::Relaxed);
        if open_until > now {
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
        self.open_until_ms.store(0, Ordering::Relaxed);
    }

    fn record_failure(&self) {
        if !self.policy.enabled {
            return;
        }
        let now = now_ms();
        let open_until = self.open_until_ms.load(Ordering::Relaxed);
        let half_open_probe_failed = open_until > 0 && open_until <= now;
        let failures = if half_open_probe_failed {
            self.policy.failure_threshold
        } else {
            self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1
        };
        if failures >= self.policy.failure_threshold {
            let until = now.saturating_add(self.policy.open_duration.as_millis() as u64);
            self.open_until_ms.store(until, Ordering::Relaxed);
            self.consecutive_failures.store(0, Ordering::Relaxed);
            self.opened.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn stats(&self) -> PhiraCircuitBreakerStats {
        let now = now_ms();
        let open_until = self.open_until_ms.load(Ordering::Relaxed);
        let remaining_open_ms = open_until.saturating_sub(now);
        let state = if !self.policy.enabled {
            "disabled"
        } else if open_until > now {
            "open"
        } else if open_until > 0 {
            "half_open"
        } else if self.consecutive_failures.load(Ordering::Relaxed) > 0 {
            "closed_with_failures"
        } else {
            "closed"
        };
        PhiraCircuitBreakerStats {
            enabled: self.policy.enabled,
            state: state.to_string(),
            failure_threshold: self.policy.failure_threshold,
            open_duration_ms: self.policy.open_duration.as_millis() as u64,
            consecutive_failures: self.consecutive_failures.load(Ordering::Relaxed),
            opened: self.opened.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            open_until_ms: open_until,
            remaining_open_ms,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Debug, Clone, Serialize)]
pub struct PhiraHttpStats {
    pub requests: u64,
    pub successes: u64,
    pub retry_attempts: u64,
    pub failures: u64,
    pub retry_notices: u64,
    pub circuit_open_rejections: u64,
    pub transport_errors: u64,
    pub status_errors: u64,
    pub retryable_status_failures: u64,
    pub non_retryable_status_failures: u64,
    pub decode_errors: u64,
    pub last_error: Option<String>,
    pub policy: PhiraHttpPolicySnapshot,
    pub circuit_breaker: PhiraCircuitBreakerStats,
    /// Per-endpoint breakdown for observability.
    pub endpoints: Vec<PhiraEndpointStats>,
}

/// Per-endpoint health statistics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PhiraEndpointStats {
    pub endpoint: String,
    pub requests: u64,
    pub successes: u64,
    pub failures: u64,
    pub last_status: Option<u16>,
}

impl PhiraHttpCounters {
    fn record_endpoint_success(&self, path: &str, status: u16) {
        if let Ok(mut ec) = self.endpoint_counters.write() {
            let e = ec.entry(path.to_string()).or_default();
            e.requests += 1;
            e.successes += 1;
            e.last_status = Some(status);
        }
    }
    fn endpoint_stats(&self) -> Vec<PhiraEndpointStats> {
        self.endpoint_counters
            .read()
            .ok()
            .map(|ec| {
                let mut v: Vec<PhiraEndpointStats> = ec
                    .iter()
                    .map(|(path, c)| PhiraEndpointStats {
                        endpoint: path.clone(),
                        requests: c.requests,
                        successes: c.successes,
                        failures: c.failures,
                        last_status: c.last_status,
                    })
                    .collect();
                v.sort_by(|a, b| b.requests.cmp(&a.requests));
                v
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Default)]
struct PhiraHttpCounters {
    requests: AtomicU64,
    successes: AtomicU64,
    retry_attempts: AtomicU64,
    failures: AtomicU64,
    retry_notices: AtomicU64,
    circuit_open_rejections: AtomicU64,
    transport_errors: AtomicU64,
    status_errors: AtomicU64,
    retryable_status_failures: AtomicU64,
    non_retryable_status_failures: AtomicU64,
    decode_errors: AtomicU64,
    last_error: RwLock<Option<String>>,
    /// Per-endpoint counters keyed by URL path.
    endpoint_counters: RwLock<HashMap<String, EndpointCounters>>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
struct EndpointCounters {
    requests: u64,
    successes: u64,
    failures: u64,
    last_status: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PhiraHttpFailureKind {
    CircuitOpen,
    Transport,
    RetryableStatus,
    NonRetryableStatus,
    Decode,
    /// 认证/命令绝对预算耗尽（PMP44 P0-D）——重试与退避不得把总耗时
    /// 推到官方客户端约 7 秒 deadline 之后。
    Timeout,
}

pub enum PhiraRetryNoticeTarget<'a> {
    /// No user-facing retry notice target. Used by diagnostic benchmark
    /// probes where retry behavior should be measured without
    /// sending chat messages to real players.
    Silent,
    Stream(&'a StreamSender<ServerCommand>),
    /// PMP44 P0-F: 官方客户端认证序列隔离。认证握手窗口内（Authenticate(Ok)
    /// 送达前）绝不向客户端发送 PMP 扩展 Chat 包，只记录重试计数与日志。
    /// 无字段的单元变体：`StreamSender` 在 phira_mp_common 内部构造，
    /// 测试无法获得实例；且该变体本就不发送任何包，携带 sender 无意义。
    StreamLogOnly,
    User(&'a crate::session::User),
}

#[derive(Debug)]
pub struct PhiraRetryClient {
    client: reqwest::Client,
    policy: PhiraHttpPolicy,
    counters: PhiraHttpCounters,
    circuit_breaker: PhiraCircuitBreaker,
}

impl PhiraRetryClient {
    pub fn new(policy: PhiraHttpPolicy) -> Result<Self> {
        let client = reqwest::Client::builder().timeout(policy.timeout).build()?;
        let circuit_breaker = PhiraCircuitBreaker::new(policy.circuit_breaker.clone());
        Ok(Self {
            client,
            policy,
            counters: PhiraHttpCounters::default(),
            circuit_breaker,
        })
    }

    pub fn stats(&self) -> PhiraHttpStats {
        PhiraHttpStats {
            requests: self.counters.requests.load(Ordering::Relaxed),
            successes: self.counters.successes.load(Ordering::Relaxed),
            retry_attempts: self.counters.retry_attempts.load(Ordering::Relaxed),
            failures: self.counters.failures.load(Ordering::Relaxed),
            retry_notices: self.counters.retry_notices.load(Ordering::Relaxed),
            circuit_open_rejections: self
                .counters
                .circuit_open_rejections
                .load(Ordering::Relaxed),
            transport_errors: self.counters.transport_errors.load(Ordering::Relaxed),
            status_errors: self.counters.status_errors.load(Ordering::Relaxed),
            retryable_status_failures: self
                .counters
                .retryable_status_failures
                .load(Ordering::Relaxed),
            non_retryable_status_failures: self
                .counters
                .non_retryable_status_failures
                .load(Ordering::Relaxed),
            decode_errors: self.counters.decode_errors.load(Ordering::Relaxed),
            last_error: self
                .counters
                .last_error
                .read()
                .ok()
                .and_then(|value| value.clone()),
            policy: self.policy.snapshot(),
            circuit_breaker: self.circuit_breaker.stats(),
            endpoints: self.counters.endpoint_stats(),
        }
    }

    pub async fn get_json<T>(
        &self,
        default_endpoint: &str,
        endpoint_override: Option<&str>,
        path: &str,
        bearer: Option<&str>,
        target: PhiraRetryNoticeTarget<'_>,
        deadline: Option<std::time::Instant>,
    ) -> Result<T>
    where
        T: DeserializeOwned,
    {
        self.counters.requests.fetch_add(1, Ordering::Relaxed);
        if !self.circuit_breaker.allow_request() {
            let msg = "Phira API circuit breaker is open".to_string();
            self.record_failure_kind(PhiraHttpFailureKind::CircuitOpen, msg.clone());
            bail!(msg);
        }

        let endpoint = endpoint_override
            .unwrap_or(default_endpoint)
            .trim_end_matches('/');
        let path = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        };
        let url = format!("{endpoint}{path}");
        let endpoint_key = path.clone();

        for attempt in 0..=self.policy.max_retries {
            // PMP44 P0-D: 认证绝对预算内的每一次请求发送前都检查剩余预算；
            // 预算耗尽立即失败，而不是让重试把总耗时推到官方客户端 deadline 之后。
            if let Some(d) = deadline {
                let remaining = d.saturating_duration_since(std::time::Instant::now());
                if remaining.is_zero() {
                    self.record_failure_kind(
                        PhiraHttpFailureKind::Timeout,
                        "deadline elapsed".to_string(),
                    );
                    bail!("Phira API deadline elapsed");
                }
            }
            let mut request = self.client.get(&url);
            if let Some(token) = bearer {
                request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"));
            }

            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        self.counters
                            .record_endpoint_success(&endpoint_key, status.as_u16());
                        match response.json::<T>().await {
                            Ok(value) => {
                                self.counters.successes.fetch_add(1, Ordering::Relaxed);
                                self.circuit_breaker.record_success();
                                return Ok(value);
                            }
                            Err(err) => {
                                self.circuit_breaker.record_failure();
                                self.record_failure_kind(
                                    PhiraHttpFailureKind::Decode,
                                    err.to_string(),
                                );
                                return Err(err.into());
                            }
                        }
                    }
                    let body = response.text().await.unwrap_or_default();
                    let retryable = phira_status_retryable(status, &body);
                    if retryable && attempt < self.policy.max_retries {
                        self.counters.retry_attempts.fetch_add(1, Ordering::Relaxed);
                        self.send_retry_notice(&target).await;
                        self.bounded_backoff_sleep(deadline, attempt).await;
                        continue;
                    }
                    if retryable {
                        self.circuit_breaker.record_failure();
                        self.record_failure_kind(
                            PhiraHttpFailureKind::RetryableStatus,
                            format!("Phira API request failed: {status} {body}"),
                        );
                    } else {
                        // Client-side/non-retryable statuses are real failures for the caller,
                        // but they should not open the upstream circuit breaker.
                        self.record_failure_kind(
                            PhiraHttpFailureKind::NonRetryableStatus,
                            format!("Phira API request failed: {status} {body}"),
                        );
                    }
                    if status == reqwest::StatusCode::BAD_GATEWAY
                        || body.contains(PHIRA_LEGACY_502_MARKER)
                    {
                        bail!(PHIRA_LEGACY_502_TEXT);
                    }
                    bail!("Phira API request failed: {status} {body}");
                }
                Err(err) if phira_error_retryable(&err) && attempt < self.policy.max_retries => {
                    self.counters.retry_attempts.fetch_add(1, Ordering::Relaxed);
                    self.send_retry_notice(&target).await;
                    self.bounded_backoff_sleep(deadline, attempt).await;
                }
                Err(err) => {
                    self.circuit_breaker.record_failure();
                    self.record_failure_kind(PhiraHttpFailureKind::Transport, err.to_string());
                    return Err(err.into());
                }
            }
        }

        self.circuit_breaker.record_failure();
        self.record_failure_kind(
            PhiraHttpFailureKind::RetryableStatus,
            "Phira API request failed after retries".to_string(),
        );
        bail!("Phira API request failed after retries")
    }

    /// Fetch Phira user name/id by bearer token.
    /// Returns `(user_id, user_name)` on success.
    pub async fn fetch_user_by_token(
        &self,
        default_endpoint: &str,
        endpoint_override: Option<&str>,
        bearer: &str,
    ) -> Option<(i32, String)> {
        #[derive(Deserialize)]
        struct PhiraUserInfo {
            id: i32,
            name: String,
        }
        self.get_json::<PhiraUserInfo>(
            default_endpoint,
            endpoint_override,
            "/me",
            Some(bearer),
            PhiraRetryNoticeTarget::Silent,
            None,
        )
        .await
        .ok()
        .map(|info| (info.id, info.name))
    }

    /// Fetch chart name by chart ID.
    pub async fn fetch_chart_by_id(
        &self,
        endpoint: &str,
        chart_id: i32,
    ) -> Option<crate::server::Chart> {
        self.get_json::<crate::server::Chart>(
            endpoint,
            None,
            &format!("/chart/{chart_id}"),
            None,
            PhiraRetryNoticeTarget::Silent,
            None,
        )
        .await
        .ok()
    }

    /// Fetch chart duration by downloading just the audio file from the
    /// .phira zip via HTTP Range requests and probing it with lofty
    /// (MP3/FLAC/WAV/OGG). Returns seconds, or None if unavailable.
    pub async fn fetch_chart_duration(&self, file_url: &str) -> Option<f64> {
        // 1. Download last 64KB to find EOCD + central directory offset
        let tail = self
            .client
            .get(file_url)
            .header(RANGE, "bytes=-65536")
            .send()
            .await
            .ok()?
            .bytes()
            .await
            .ok()?;

        // 2. Find EOCD signature (PK\x05\x06) from the end
        let eocd_sig = b"PK\x05\x06";
        let eocd_pos = tail.windows(4).rposition(|w| w == eocd_sig)?;
        let eocd = &tail[eocd_pos..];

        let central_offset = u64::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19], 0, 0, 0, 0]);
        let central_size = u64::from_le_bytes([eocd[12], eocd[13], eocd[14], eocd[15], 0, 0, 0, 0]);
        if central_size == 0 {
            return None;
        }

        // 3. Download central directory
        let central = self
            .client
            .get(file_url)
            .header(RANGE, format!("bytes={}-{}", central_offset, central_offset + central_size - 1))
            .send()
            .await
            .ok()?
            .bytes()
            .await
            .ok()?;

        // 4. Walk central directory, pick the largest audio entry (正曲，排除 preview)
        const AUDIO_EXTS: [&str; 8] = [".mp3", ".flac", ".wav", ".ogg", ".oga", ".opus", ".m4a", ".aac"];
        let mut best: Option<ZipEntry> = None;
        let mut pos = 0usize;
        while pos + 46 <= central.len() {
            if &central[pos..pos + 4] != b"PK\x01\x02" {
                break;
            }
            let entry = ZipEntry {
                compression: u16::from_le_bytes([central[pos + 10], central[pos + 11]]),
                compressed_size: u32::from_le_bytes([
                    central[pos + 20], central[pos + 21], central[pos + 22], central[pos + 23],
                ]),
                uncompressed_size: u32::from_le_bytes([
                    central[pos + 24], central[pos + 25], central[pos + 26], central[pos + 27],
                ]),
                fn_len: u16::from_le_bytes([central[pos + 28], central[pos + 29]]),
                extra_len: u16::from_le_bytes([central[pos + 30], central[pos + 31]]),
                local_offset: u32::from_le_bytes([
                    central[pos + 42], central[pos + 43], central[pos + 44], central[pos + 45],
                ]),
            };
            let filename = String::from_utf8_lossy(&central[pos + 46..pos + 46 + entry.fn_len as usize]);
            pos += 46 + entry.fn_len as usize + entry.extra_len as usize;
            if !AUDIO_EXTS.iter().any(|e| filename.to_ascii_lowercase().ends_with(e)) {
                continue;
            }
            if best
                .as_ref()
                .map(|b| entry.uncompressed_size > b.uncompressed_size)
                .unwrap_or(true)
            {
                best = Some(entry);
            }
        }
        let entry = best?;

        // 5. Download local file header + compressed audio（4KB 缓冲覆盖文件名/extra）
        let range_end = entry.local_offset as u64
            + 30
            + std::cmp::max(4096u64, entry.fn_len as u64 + entry.extra_len as u64 + 64)
            + entry.compressed_size as u64;
        let raw = self
            .client
            .get(file_url)
            .header(RANGE, format!("bytes={}-{}", entry.local_offset, range_end))
            .send()
            .await
            .ok()?
            .bytes()
            .await
            .ok()?;

        let lh_fn_len = u16::from_le_bytes([raw[26], raw[27]]);
        let lh_extra_len = u16::from_le_bytes([raw[28], raw[29]]);
        let data_start = 30 + lh_fn_len as usize + lh_extra_len as usize;
        let compressed = &raw[data_start..data_start + entry.compressed_size as usize];

        // 6. Decompress（stored 直读 / deflate 解压）
        let audio: Vec<u8> = if entry.compression == 0 {
            compressed[..entry.uncompressed_size as usize].to_vec()
        } else if entry.compression == 8 {
            use std::io::Read;
            let mut decoder = flate2::read::DeflateDecoder::new(compressed);
            let mut buf = Vec::with_capacity(entry.uncompressed_size as usize);
            decoder.read_to_end(&mut buf).ok()?;
            buf
        } else {
            return None;
        };

        // 7. Probe duration（lofty 支持 MP3/FLAC/WAV/OGG）
        probe_audio_duration(&audio)
    }

    /// Fetch user name by Phira user ID (unauthenticated).
    pub async fn fetch_user_by_id(&self, endpoint: &str, user_id: i32) -> Option<String> {
        #[derive(serde::Deserialize)]
        struct UserInfo {
            name: String,
        }
        self.get_json::<UserInfo>(
            endpoint,
            None,
            &format!("/user/{user_id}"),
            None,
            PhiraRetryNoticeTarget::Silent,
            None,
        )
        .await
        .ok()
        .map(|info| info.name)
    }

    /// PMP44 P0-D: 重试退避 sleep 受绝对预算约束——绝不睡过 deadline。
    /// 预算已耗尽时本轮直接跳过退避（下一次循环的请求前检查会拒绝请求）。
    async fn bounded_backoff_sleep(
        &self,
        deadline: Option<std::time::Instant>,
        attempt: usize,
    ) {
        if let Some(d) = deadline {
            let remaining = d.saturating_duration_since(std::time::Instant::now());
            if !remaining.is_zero() {
                tokio::time::sleep(remaining.min(self.policy.backoff_delay(attempt))).await;
            }
        } else {
            time::sleep(self.policy.backoff_delay(attempt)).await;
        }
    }

    async fn send_retry_notice(&self, target: &PhiraRetryNoticeTarget<'_>) {
        match target {
            PhiraRetryNoticeTarget::Silent => {}
            // PMP44 P0-F: 认证预授权窗口内只记录重试计数与日志，绝不向官方
            // 客户端发送非 Authenticate 包（否则会破坏官方认证序列）。
            PhiraRetryNoticeTarget::StreamLogOnly => {
                self.counters.retry_notices.fetch_add(1, Ordering::Relaxed);
                warn!("Phira API retry (pre-auth): no client notice sent");
            }
            PhiraRetryNoticeTarget::Stream(sender) => {
                self.counters.retry_notices.fetch_add(1, Ordering::Relaxed);
                let cmd = ServerCommand::Message(Message::Chat {
                    user: 0,
                    content: PHIRA_RETRY_NOTICE.to_string(),
                });
                if let Err(err) = sender.send(cmd).await {
                    warn!("failed to send Phira retry notice: {err:?}");
                }
            }
            PhiraRetryNoticeTarget::User(user) => {
                self.counters.retry_notices.fetch_add(1, Ordering::Relaxed);
                let lang = user.lang.clone();
                let content = crate::l10n::translate_system(
                    &lang,
                    "phira-retry-notice",
                    &fluent::FluentArgs::new(),
                );
                let cmd = ServerCommand::Message(Message::Chat { user: 0, content });
                // 重试通知非房间状态事件，cutover 不适用。
                user.try_send(cmd, None).await;
            }
        }
    }

    fn record_failure_kind(&self, kind: PhiraHttpFailureKind, error: String) {
        self.counters.failures.fetch_add(1, Ordering::Relaxed);
        match kind {
            PhiraHttpFailureKind::CircuitOpen => {
                self.counters
                    .circuit_open_rejections
                    .fetch_add(1, Ordering::Relaxed);
            }
            PhiraHttpFailureKind::Transport => {
                self.counters
                    .transport_errors
                    .fetch_add(1, Ordering::Relaxed);
            }
            PhiraHttpFailureKind::RetryableStatus => {
                self.counters.status_errors.fetch_add(1, Ordering::Relaxed);
                self.counters
                    .retryable_status_failures
                    .fetch_add(1, Ordering::Relaxed);
            }
            PhiraHttpFailureKind::NonRetryableStatus => {
                self.counters.status_errors.fetch_add(1, Ordering::Relaxed);
                self.counters
                    .non_retryable_status_failures
                    .fetch_add(1, Ordering::Relaxed);
            }
            PhiraHttpFailureKind::Decode => {
                self.counters.decode_errors.fetch_add(1, Ordering::Relaxed);
            }
            PhiraHttpFailureKind::Timeout => {
                self.counters.status_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
        if let Ok(mut last) = self.counters.last_error.write() {
            *last = Some(error);
        }
    }
}

fn phira_status_retryable(status: reqwest::StatusCode, body: &str) -> bool {
    status == reqwest::StatusCode::BAD_GATEWAY
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
        || body.contains(PHIRA_LEGACY_502_MARKER)
        || body.contains(PHIRA_LEGACY_502_TEXT)
}

fn phira_error_retryable(err: &reqwest::Error) -> bool {
    err.is_timeout()
        || err.is_connect()
        || err
            .status()
            .is_some_and(|status| phira_status_retryable(status, ""))
        || err.to_string().contains(PHIRA_LEGACY_502_MARKER)
        || err.to_string().contains(PHIRA_LEGACY_502_TEXT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造 N 个 MPEG1 Layer III 128kbps/44100Hz CBR 帧。
    /// 帧长 = 144 * 128000 / 44100 ≈ 417（无 padding），单帧时长 = 1152/44100 ≈ 26.12ms。
    fn synth_mp3_frames(n: usize) -> Vec<u8> {
        let frame_len = 417usize;
        let mut out = Vec::with_capacity(n * frame_len);
        for _ in 0..n {
            // 0xFF 0xFB: MPEG1 / Layer III / 无 CRC；0x90: 128kbps + 44100Hz
            out.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00]);
            out.extend(std::iter::repeat(0u8).take(frame_len - 4));
        }
        out
    }

    #[test]
    fn mp3_scan_duration_is_accurate() {
        let n = 100;
        let dur = mp3_duration_scan(&synth_mp3_frames(n)).expect("should parse MP3 frames");
        let expected = n as f64 * 1152.0 / 44100.0; // ≈ 2.61s
        assert!(
            (dur - expected).abs() < 0.01,
            "mp3 scan duration {dur} vs expected {expected}"
        );
    }

    #[test]
    fn mp3_scan_handles_id3_tag_prefix() {
        // ID3v2.3 头（10 字节）+ 声明 10 字节 payload → 音频从 offset 20 开始。
        let mut audio = vec![b'I', b'D', b'3', 3, 0, 0, 0, 0, 0, 10];
        audio.extend(std::iter::repeat(0u8).take(10)); // tag payload
        audio.extend(synth_mp3_frames(10));
        let dur = probe_audio_duration(&audio).expect("should parse after ID3");
        let expected = 10.0 * 1152.0 / 44100.0;
        assert!((dur - expected).abs() < 0.01, "id3 duration {dur} vs {expected}");
    }

    #[test]
    fn phira_502_marker_is_retryable_without_full_notice_text() {
        assert!(phira_status_retryable(
            reqwest::StatusCode::BAD_GATEWAY,
            PHIRA_LEGACY_502_MARKER
        ));
        assert!(phira_status_retryable(
            reqwest::StatusCode::OK,
            "认证失败 502错误"
        ));
    }

    #[test]
    fn client_side_status_is_not_retryable() {
        assert!(!phira_status_retryable(
            reqwest::StatusCode::BAD_REQUEST,
            "bad request"
        ));
        assert!(!phira_status_retryable(
            reqwest::StatusCode::UNAUTHORIZED,
            "unauthorized"
        ));
    }

    #[test]
    fn circuit_breaker_reopens_after_half_open_probe_failure() {
        let breaker = PhiraCircuitBreaker::new(PhiraCircuitBreakerPolicy {
            enabled: true,
            failure_threshold: 2,
            open_duration: Duration::from_millis(1),
        });
        breaker.record_failure();
        assert_eq!(breaker.stats().state, "closed_with_failures");
        breaker.record_failure();
        assert_eq!(breaker.stats().state, "open");

        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(breaker.stats().state, "half_open");
        breaker.record_failure();
        assert_eq!(breaker.stats().state, "open");
    }

    #[test]
    fn policy_config_clamps_extreme_values() {
        let policy = PhiraHttpPolicyConfig {
            timeout_ms: Some(1),
            max_retries: Some(usize::MAX),
            base_backoff_ms: Some(1),
            max_backoff_ms: Some(1),
            circuit_breaker: PhiraCircuitBreakerConfig {
                enabled: Some(true),
                failure_threshold: Some(1),
                open_duration_ms: Some(1),
            },
        }
        .into_policy();

        assert_eq!(policy.timeout, Duration::from_millis(500));
        assert_eq!(policy.max_retries, 10);
        assert_eq!(policy.base_backoff, Duration::from_millis(50));
        assert_eq!(policy.max_backoff, Duration::from_millis(50));
        assert_eq!(policy.circuit_breaker.failure_threshold, 2);
        assert_eq!(
            policy.circuit_breaker.open_duration,
            Duration::from_millis(1_000)
        );
    }

    #[test]
    fn circuit_breaker_recovers_after_half_open_probe_success() {
        let breaker = PhiraCircuitBreaker::new(PhiraCircuitBreakerPolicy {
            enabled: true,
            failure_threshold: 2,
            open_duration: Duration::from_millis(1),
        });
        // Trip breaker
        breaker.record_failure();
        breaker.record_failure();
        assert_eq!(breaker.stats().state, "open");

        // Wait for half-open
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(breaker.stats().state, "half_open");

        // Successful probe resets breaker
        breaker.record_success();
        assert_eq!(breaker.stats().state, "closed");
        assert_eq!(breaker.stats().consecutive_failures, 0);
    }

    #[test]
    fn circuit_breaker_probation_accumulates_failures() {
        let breaker = PhiraCircuitBreaker::new(PhiraCircuitBreakerPolicy {
            enabled: true,
            failure_threshold: 3,
            open_duration: Duration::from_millis(50),
        });
        // One failure should not trip
        breaker.record_failure();
        assert_eq!(breaker.stats().state, "closed_with_failures");
        assert!(breaker.allow_request());

        // Reset with success
        breaker.record_success();
        assert_eq!(breaker.stats().state, "closed");
        assert_eq!(breaker.stats().consecutive_failures, 0);
    }

    #[test]
    fn fetch_user_by_token_method_signature_compiles() {
        let _ = PhiraRetryClient::fetch_user_by_token;
    }

    #[test]
    fn fetch_chart_by_id_method_signature_compiles() {
        let _ = PhiraRetryClient::fetch_chart_by_id;
    }

    #[test]
    fn default_policy_has_safe_timeout_and_retries() {
        let cfg = PhiraHttpPolicyConfig::default();
        let policy = cfg.into_policy();
        assert!(policy.timeout.as_millis() >= 5000, "default timeout >= 5s");
        assert!(policy.max_retries >= 1, "default retries >= 1");
    }

    /// PMP44 P0-F: `StreamLogOnly` 只递增重试计数并记录日志，绝不向任何
    /// 传输发送包（官方客户端在认证握手窗口内不得收到 PMP 扩展 Chat）。
    /// 该变体不携带 sender，因此无需构造 `StreamSender` 即可验证。
    #[tokio::test]
    async fn stream_log_only_notice_counts_without_sending() {
        let client = PhiraRetryClient::new(PhiraHttpPolicy::default()).unwrap();
        let before = client.counters.retry_notices.load(Ordering::Relaxed);
        client
            .send_retry_notice(&PhiraRetryNoticeTarget::StreamLogOnly)
            .await;
        assert_eq!(
            client.counters.retry_notices.load(Ordering::Relaxed),
            before + 1,
            "StreamLogOnly must still count the retry for operator observability"
        );
    }
}

//! Unified Phira HTTP client for Runtime v2.
//!
//! The old code had retry/timeout handling embedded in session paths.  This
//! module is the first central seam for all Phira HTTP traffic: authentication,
//! chart lookup, record lookup and future hybrid/real benchmark modes should
//! converge here.  Simulation remains the default benchmark path and does not
//! call this client.

use anyhow::{bail, Result};
use phira_mp_common::{Message, ServerCommand, StreamSender};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
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
pub const PHIRA_LEGACY_502_TEXT: &str = "认证失败 502错误 Phira服务器太烂了，我们正在重试以保证你的流畅体验 /拜谢";

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
        let timeout_ms = self.timeout_ms.unwrap_or(defaults.timeout.as_millis() as u64).clamp(500, 60_000);
        let max_retries = self.max_retries.unwrap_or(defaults.max_retries).min(10);
        let base_ms = self.base_backoff_ms.unwrap_or(defaults.base_backoff.as_millis() as u64).clamp(50, 30_000);
        let max_ms = self.max_backoff_ms.unwrap_or(defaults.max_backoff.as_millis() as u64).clamp(base_ms, 120_000);
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
            failure_threshold: self.failure_threshold.unwrap_or(defaults.failure_threshold).clamp(2, 100),
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
        let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if failures >= self.policy.failure_threshold {
            let until = now_ms().saturating_add(self.policy.open_duration.as_millis() as u64);
            self.open_until_ms.store(until, Ordering::Relaxed);
            self.consecutive_failures.store(0, Ordering::Relaxed);
            self.opened.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn stats(&self) -> PhiraCircuitBreakerStats {
        let now = now_ms();
        let open_until = self.open_until_ms.load(Ordering::Relaxed);
        let state = if !self.policy.enabled {
            "disabled"
        } else if open_until > now {
            "open"
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
    pub last_error: Option<String>,
    pub policy: PhiraHttpPolicySnapshot,
    pub circuit_breaker: PhiraCircuitBreakerStats,
}

#[derive(Debug, Default)]
struct PhiraHttpCounters {
    requests: AtomicU64,
    successes: AtomicU64,
    retry_attempts: AtomicU64,
    failures: AtomicU64,
    retry_notices: AtomicU64,
    last_error: RwLock<Option<String>>,
}

pub enum PhiraRetryNoticeTarget<'a> {
    Stream(&'a StreamSender<ServerCommand>),
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
        let client = reqwest::Client::builder()
            .timeout(policy.timeout)
            .build()?;
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
            last_error: self.counters.last_error.read().ok().and_then(|value| value.clone()),
            policy: self.policy.snapshot(),
            circuit_breaker: self.circuit_breaker.stats(),
        }
    }

    pub async fn get_json<T>(
        &self,
        default_endpoint: &str,
        endpoint_override: Option<&str>,
        path: &str,
        bearer: Option<&str>,
        target: PhiraRetryNoticeTarget<'_>,
    ) -> Result<T>
    where
        T: DeserializeOwned,
    {
        self.counters.requests.fetch_add(1, Ordering::Relaxed);
        if !self.circuit_breaker.allow_request() {
            let msg = "Phira API circuit breaker is open".to_string();
            self.record_failure(msg.clone());
            bail!(msg);
        }

        let endpoint = endpoint_override
            .unwrap_or(default_endpoint)
            .trim_end_matches('/');
        let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
        let url = format!("{endpoint}{path}");

        for attempt in 0..=self.policy.max_retries {
            let mut request = self.client.get(&url);
            if let Some(token) = bearer {
                request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"));
            }

            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        self.counters.successes.fetch_add(1, Ordering::Relaxed);
                        self.circuit_breaker.record_success();
                        return response.json::<T>().await.map_err(Into::into);
                    }
                    let body = response.text().await.unwrap_or_default();
                    let retryable = phira_status_retryable(status, &body);
                    if retryable && attempt < self.policy.max_retries {
                        self.counters.retry_attempts.fetch_add(1, Ordering::Relaxed);
                        self.send_retry_notice(&target).await;
                        time::sleep(self.policy.backoff_delay(attempt)).await;
                        continue;
                    }
                    self.circuit_breaker.record_failure();
                    self.record_failure(format!("Phira API request failed: {status} {body}"));
                    if status == reqwest::StatusCode::BAD_GATEWAY || body.contains("认证失败 502错误") {
                        bail!(PHIRA_LEGACY_502_TEXT);
                    }
                    bail!("Phira API request failed: {status} {body}");
                }
                Err(err) if phira_error_retryable(&err) && attempt < self.policy.max_retries => {
                    self.counters.retry_attempts.fetch_add(1, Ordering::Relaxed);
                    self.send_retry_notice(&target).await;
                    time::sleep(self.policy.backoff_delay(attempt)).await;
                }
                Err(err) => {
                    self.circuit_breaker.record_failure();
                    self.record_failure(err.to_string());
                    return Err(err.into());
                }
            }
        }

        self.circuit_breaker.record_failure();
        self.record_failure("Phira API request failed after retries".to_string());
        bail!("Phira API request failed after retries")
    }

    async fn send_retry_notice(&self, target: &PhiraRetryNoticeTarget<'_>) {
        let cmd = ServerCommand::Message(Message::Chat {
            user: 0,
            content: PHIRA_RETRY_NOTICE.to_string(),
        });
        match target {
            PhiraRetryNoticeTarget::Stream(sender) => {
                self.counters.retry_notices.fetch_add(1, Ordering::Relaxed);
                if let Err(err) = sender.send(cmd).await {
                    warn!("failed to send Phira retry notice: {err:?}");
                }
            }
            PhiraRetryNoticeTarget::User(user) => {
                self.counters.retry_notices.fetch_add(1, Ordering::Relaxed);
                user.try_send(cmd).await;
            }
        }
    }

    fn record_failure(&self, error: String) {
        self.counters.failures.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut last) = self.counters.last_error.write() {
            *last = Some(error);
        }
    }
}

fn phira_status_retryable(status: reqwest::StatusCode, body: &str) -> bool {
    status == reqwest::StatusCode::BAD_GATEWAY
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
        || body.contains(PHIRA_LEGACY_502_TEXT)
}

fn phira_error_retryable(err: &reqwest::Error) -> bool {
    err.is_timeout()
        || err.is_connect()
        || err.status().is_some_and(|status| phira_status_retryable(status, ""))
        || err.to_string().contains(PHIRA_LEGACY_502_TEXT)
}

//! Unified Phira HTTP client for Runtime v2.
//!
//! The old code had retry/timeout handling embedded in session paths.  This
//! module is the first central seam for all Phira HTTP traffic: authentication,
//! chart lookup, record lookup and future hybrid/real benchmark modes should
//! converge here.  Simulation remains the default benchmark path and does not
//! call this client.

use anyhow::{bail, Result};
use phira_mp_common::{Message, ServerCommand, StreamSender};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        RwLock,
    },
    time::Duration,
};
use tokio::time;
use tracing::warn;

pub const PHIRA_RETRY_NOTICE: &str = "Phira服务器太烂了，我们正在重试以保证你的流畅体验";
pub const PHIRA_LEGACY_502_TEXT: &str = "认证失败 502错误 Phira服务器太烂了，我们正在重试以保证你的流畅体验 /拜谢";

#[derive(Debug, Clone, Serialize)]
pub struct PhiraHttpPolicySnapshot {
    pub timeout_ms: u64,
    pub max_retries: usize,
    pub base_backoff_ms: u64,
    pub max_backoff_ms: u64,
    pub circuit_breaker: &'static str,
}

#[derive(Debug, Clone)]
pub struct PhiraHttpPolicy {
    pub timeout: Duration,
    pub max_retries: usize,
    pub base_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for PhiraHttpPolicy {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            max_retries: 3,
            base_backoff: Duration::from_millis(200),
            max_backoff: Duration::from_secs(3),
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
            circuit_breaker: "planned",
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
pub struct PhiraHttpStats {
    pub requests: u64,
    pub successes: u64,
    pub retry_attempts: u64,
    pub failures: u64,
    pub retry_notices: u64,
    pub last_error: Option<String>,
    pub policy: PhiraHttpPolicySnapshot,
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
    None,
    Stream(&'a StreamSender<ServerCommand>),
    User(&'a crate::session::User),
}

#[derive(Debug)]
pub struct PhiraRetryClient {
    client: reqwest::Client,
    policy: PhiraHttpPolicy,
    counters: PhiraHttpCounters,
}

impl PhiraRetryClient {
    pub fn new(policy: PhiraHttpPolicy) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(policy.timeout)
            .build()?;
        Ok(Self {
            client,
            policy,
            counters: PhiraHttpCounters::default(),
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
                    self.record_failure(err.to_string());
                    return Err(err.into());
                }
            }
        }

        self.record_failure("Phira API request failed after retries".to_string());
        bail!("Phira API request failed after retries")
    }

    async fn send_retry_notice(&self, target: &PhiraRetryNoticeTarget<'_>) {
        let cmd = ServerCommand::Message(Message::Chat {
            user: 0,
            content: PHIRA_RETRY_NOTICE.to_string(),
        });
        match target {
            PhiraRetryNoticeTarget::None => {}
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

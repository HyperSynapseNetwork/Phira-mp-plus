//! Phira-mp+ 速率限制器
//!
//! 提供两种速率限制策略：
//! - SlidingWindowRateLimiter: 滑动窗口计数器（用于连接速率限制）
//! - TokenBucketRateLimiter: 令牌桶（用于命令速率限制）

use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

// ── 滑动窗口速率限制器 ──

/// 滑动窗口速率限制器
///
/// 在给定的时间窗口内限制操作次数。例如 10 秒内最多 30 次连接。
pub struct SlidingWindowRateLimiter {
    buckets: Mutex<HashMap<String, Vec<Instant>>>,
    max_count: u32,
    window_secs: u32,
}

impl SlidingWindowRateLimiter {
    pub fn new(max_count: u32, window_secs: u32) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            max_count,
            window_secs,
        }
    }

    /// 检查 key 是否被允许操作。
    /// 返回 true 表示允许，false 表示被限流。
    pub async fn check(&self, key: &str) -> bool {
        let mut buckets = self.buckets.lock().await;
        let now = Instant::now();
        let window = Duration::from_secs(self.window_secs as u64);

        let timestamps = buckets.entry(key.to_string()).or_insert_with(Vec::new);

        // 移除窗口外的记录
        timestamps.retain(|t| now.duration_since(*t) < window);

        if timestamps.len() >= self.max_count as usize {
            false
        } else {
            timestamps.push(now);
            true
        }
    }

    /// 清理过期条目，防止内存泄漏
    pub async fn cleanup(&self) {
        let mut buckets = self.buckets.lock().await;
        let now = Instant::now();
        let window = Duration::from_secs(self.window_secs as u64);
        buckets.retain(|_, timestamps| {
            timestamps.retain(|t| now.duration_since(*t) < window);
            !timestamps.is_empty()
        });
    }
}

// ── 令牌桶速率限制器 ──

/// 令牌桶速率限制器
///
/// 以给定的速率填充令牌，每次操作消耗一个令牌。
/// burst: 最大突发大小（桶容量）
/// rate: 每秒填充的令牌数
pub struct TokenBucketRateLimiter {
    tokens: Mutex<Inner>,
    burst: u32,
    rate: f64,
}

struct Inner {
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucketRateLimiter {
    pub fn new(burst: u32, rate: f64) -> Self {
        Self {
            tokens: Mutex::new(Inner {
                tokens: burst as f64,
                last_refill: Instant::now(),
            }),
            burst,
            rate,
        }
    }

    /// 尝试消耗一个令牌。
    /// 返回 true 表示允许操作，false 表示被限流。
    pub async fn try_consume(&self) -> bool {
        let mut inner = self.tokens.lock().await;
        let now = Instant::now();
        let elapsed = now.duration_since(inner.last_refill).as_secs_f64();
        inner.tokens = (inner.tokens + elapsed * self.rate).min(self.burst as f64);
        inner.last_refill = now;

        if inner.tokens >= 1.0 {
            inner.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

// ── 复合命令速率限制器（按类别） ──

/// 命令类别
pub enum CommandCategory {
    Chat,
    RoomOp,
    Api,
}

/// 多类别令牌桶限制器
pub struct CommandRateLimiter {
    chat: TokenBucketRateLimiter,
    room_op: TokenBucketRateLimiter,
    api: TokenBucketRateLimiter,
}

impl CommandRateLimiter {
    pub fn new() -> Self {
        Self {
            // 聊天消息: 10 突发, 3/s
            chat: TokenBucketRateLimiter::new(10, 3.0),
            // 房间操作: 20 突发, 6/s
            room_op: TokenBucketRateLimiter::new(20, 6.0),
            // API 调用: 12 突发, 3/s
            api: TokenBucketRateLimiter::new(12, 3.0),
        }
    }

    pub async fn check(&self, category: CommandCategory) -> bool {
        match category {
            CommandCategory::Chat => self.chat.try_consume().await,
            CommandCategory::RoomOp => self.room_op.try_consume().await,
            CommandCategory::Api => self.api.try_consume().await,
        }
    }
}

// ── 连接速率限制器 ──

/// 每个 IP 的连接速率限制器
pub type ConnectionRateLimiter = SlidingWindowRateLimiter;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_sliding_window() {
        let limiter = SlidingWindowRateLimiter::new(3, 10);
        assert!(limiter.check("test").await);
        assert!(limiter.check("test").await);
        assert!(limiter.check("test").await);
        assert!(!limiter.check("test").await); // 第4次应被限流
    }

    #[tokio::test]
    async fn test_token_bucket() {
        let limiter = TokenBucketRateLimiter::new(5, 10.0);
        for _ in 0..5 {
            assert!(limiter.try_consume().await);
        }
        assert!(!limiter.try_consume().await); // 桶空
    }

    #[tokio::test]
    async fn test_token_bucket_refill() {
        let limiter = TokenBucketRateLimiter::new(1, 100.0); // 100/s
        assert!(limiter.try_consume().await);
        assert!(!limiter.try_consume().await);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(limiter.try_consume().await); // 应已补充
    }
}

//! Protocol observability counters (PMP42 P1).
//!
//! Tracks the official-client response pipeline end to end:
//!
//! ```text
//! request_received → … → response_queued → (critical) → response_flushed
//! ```
//!
//! Two counters must stay at zero in production:
//! - `silent_response_paths`: a request-type command produced no response and
//!   was not a `NoResponseExpected` (Touches/Judges) path.
//! - `late_commit`: a command reached the actor after its absolute deadline and
//!   was refused execution.
//!
//! The latency histogram records the server-side response latency from command
//! receipt to the point the response is queued, bucketed in milliseconds.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Millisecond bucket boundaries (upper bound exclusive). Bucket `i` covers
/// `[boundaries[i-1], boundaries[i])`; the final bucket covers
/// `[last_boundary, +∞)`.
const LATENCY_BOUNDARIES_MS: [u64; 8] = [1, 5, 10, 50, 100, 500, 1_000, 5_000];

/// Simple fixed-bucket latency histogram (all fields const-constructible).
#[derive(Debug)]
pub(crate) struct LatencyHistogram {
    counts: [AtomicU64; LATENCY_BOUNDARIES_MS.len() + 1],
}

impl LatencyHistogram {
    pub(crate) const fn new() -> Self {
        Self {
            counts: [
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
                AtomicU64::new(0),
            ],
        }
    }

    pub(crate) fn record(&self, elapsed: Duration) {
        let ms = elapsed.as_millis() as u64;
        let idx = LATENCY_BOUNDARIES_MS
            .iter()
            .position(|boundary| ms < *boundary)
            .unwrap_or(self.counts.len() - 1);
        self.counts[idx].fetch_add(1, Ordering::Relaxed);
    }

    /// Total number of recorded samples (test aid).
    #[cfg(test)]
    pub(crate) fn total(&self) -> u64 {
        self.counts.iter().map(|c| c.load(Ordering::Relaxed)).sum()
    }
}

/// Global protocol trace. Process-wide counters are sufficient because the
/// server runs one process per deployment; values are `Relaxed` and only used
/// for observability and CI assertions.
#[derive(Debug)]
pub(crate) struct ProtocolTrace {
    /// Every request-type command received by the dispatch path.
    pub request_received: AtomicU64,
    /// Responses accepted into the outbound send queue.
    pub response_queued: AtomicU64,
    /// Responses that were flushed to the socket via `send_and_flush`.
    pub response_flushed: AtomicU64,
    /// Dispatch `None` results that were NOT a NoResponseExpected path.
    /// MUST be 0 under normal operation.
    pub silent_response_paths: AtomicU64,
    /// Commands that arrived at the actor after their absolute deadline and
    /// were refused execution. MUST be 0 under normal operation.
    pub late_commit: AtomicU64,
    /// Post-response compat-queue items dropped because their origin session
    /// became stale (reconnect bumped the generation) or was torn down. MUST be
    /// 0 under normal operation (P1 observability).
    pub compat_queue_drop: AtomicU64,
    /// Room-actor commits refused because the originating Session was superseded
    /// (P0-C). MUST be 0 under normal operation.
    pub stale_commit_prevented: AtomicU64,
    /// PMP44 P0-G/P0-H: SessionOutboundGate 丢弃的缓冲事件（高频遥测 coalesce、
    /// 控制事件 drop-oldest、以及快照切换屏障 cutover 剔除的快照内事件）。
    /// 握手窗口内因速率限制丢弃是预期的；稳态活跃后应趋近 0。
    pub gate_dropped: AtomicU64,
    /// PMP44 P1 §33: 跨会话命令——命令到达时其 origin session 已非该用户
    /// 当前绑定（重连抬升代际），被 `worker_should_run` 拒绝执行。正常运行时
    /// 应趋近 0（仅在重连竞态窗口出现）。
    pub cross_session_command: AtomicU64,
    /// PMP44 P1 §33: 出站认证屏障激活（drain）失败次数。`SessionOutboundGate`
    /// 排空缓冲期间发送失败、或激活超时，会话 fail-closed 关闭传输。正常应趋近 0。
    pub activation_drain_failure: AtomicU64,
    /// PMP44 P1 §33: gauge——认证屏障当前缓冲的事件数。`push_bounded` 每次
    /// 入队/丢弃后、`activate` 排空后 `.store()` 当前值。
    pub auth_barrier_pending_events: AtomicU64,
    /// PMP44 P1 §33: gauge——认证屏障当前缓冲的字节粗估（`GatePending.bytes`）。
    /// 与 `auth_barrier_pending_events` 一起提供预认证缓冲的实时视图。
    pub auth_barrier_pending_bytes: AtomicU64,
    /// PMP44 P1 §33: 快照切换屏障在激活时剔除的缓冲事件数（`seq <= cutover`，
    /// 已包含在即将构建的快照中）。与 `gate_dropped` 度量不同：后者是
    /// 有界丢弃策略（coalesce / drop-oldest），此处专指 cutover 剔除。
    pub snapshot_duplicate_event: AtomicU64,
    /// PMP45 P0-H: 控制事件溢出次数——某条非遥测事件即使清空整个认证缓冲
    /// 仍超字节预算，`push_bounded` 置 `overflowed`、`activate` fail-closed。
    /// 正常运行时必须为 0（状态不完整的会话绝不允许激活）。
    pub gate_control_overflow: AtomicU64,
    /// Server-side response latency histogram (ms buckets).
    pub latency_histogram: LatencyHistogram,
}

pub(crate) static PROTOCOL_TRACE: ProtocolTrace = ProtocolTrace {
    request_received: AtomicU64::new(0),
    response_queued: AtomicU64::new(0),
    response_flushed: AtomicU64::new(0),
    silent_response_paths: AtomicU64::new(0),
    late_commit: AtomicU64::new(0),
    compat_queue_drop: AtomicU64::new(0),
    stale_commit_prevented: AtomicU64::new(0),
    gate_dropped: AtomicU64::new(0),
    cross_session_command: AtomicU64::new(0),
    activation_drain_failure: AtomicU64::new(0),
    auth_barrier_pending_events: AtomicU64::new(0),
    auth_barrier_pending_bytes: AtomicU64::new(0),
    snapshot_duplicate_event: AtomicU64::new(0),
    gate_control_overflow: AtomicU64::new(0),
    latency_histogram: LatencyHistogram::new(),
};

impl ProtocolTrace {
    pub(crate) fn get() -> &'static Self {
        &PROTOCOL_TRACE
    }

    pub(crate) fn record_response_latency(&self, received_at: Instant) {
        self.latency_histogram.record(received_at.elapsed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_buckets_by_ms() {
        let hist = LatencyHistogram::new();
        hist.record(Duration::from_micros(500)); // < 1ms
        hist.record(Duration::from_millis(3)); // 1..5
        hist.record(Duration::from_millis(60)); // 50..100
        hist.record(Duration::from_secs(10)); // overflow
        assert_eq!(hist.total(), 4);
        assert_eq!(hist.counts[0].load(Ordering::Relaxed), 1);
        assert_eq!(hist.counts[1].load(Ordering::Relaxed), 1);
        assert_eq!(hist.counts[4].load(Ordering::Relaxed), 1);
        assert_eq!(
            hist.counts[LATENCY_BOUNDARIES_MS.len()].load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn trace_counters_are_accessible() {
        let trace = ProtocolTrace::get();
        trace
            .request_received
            .fetch_add(1, Ordering::Relaxed);
        assert!(trace.request_received.load(Ordering::Relaxed) >= 1);
        // PMP44 P0-G: gate 丢弃计数器可读可自增。
        trace.gate_dropped.fetch_add(1, Ordering::Relaxed);
        assert!(trace.gate_dropped.load(Ordering::Relaxed) >= 1);
        // PMP44 P1 §33: 新增观测计数器全部可读可自增。
        trace.cross_session_command.fetch_add(1, Ordering::Relaxed);
        assert!(trace.cross_session_command.load(Ordering::Relaxed) >= 1);
        trace.activation_drain_failure.fetch_add(1, Ordering::Relaxed);
        assert!(trace.activation_drain_failure.load(Ordering::Relaxed) >= 1);
        trace
            .auth_barrier_pending_events
            .store(1, Ordering::Relaxed);
        assert!(
            trace
                .auth_barrier_pending_events
                .load(Ordering::Relaxed)
                >= 1
        );
        trace
            .auth_barrier_pending_bytes
            .store(1, Ordering::Relaxed);
        assert!(
            trace
                .auth_barrier_pending_bytes
                .load(Ordering::Relaxed)
                >= 1
        );
        trace.snapshot_duplicate_event.fetch_add(1, Ordering::Relaxed);
        assert!(trace.snapshot_duplicate_event.load(Ordering::Relaxed) >= 1);
    }
}

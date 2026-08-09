use anyhow::Result;
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use tokio::sync::mpsc;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Log-stream channel for OpenUDS external tools.
///
/// The sender is stored during `init()`; the OpenUDS server takes the
/// receiver via [`take_openuds_log_rx`] and forwards formatted log lines to
/// sessions subscribed to the `"logs"` stream.  Bounded (drop-oldest when the
/// broker is slow, matching the TUI channel) so the tracing subscriber never
/// blocks.
static OPENUDS_LOG_TX: OnceLock<mpsc::Sender<String>> = OnceLock::new();
static OPENUDS_LOG_RX: OnceLock<Mutex<Option<mpsc::Receiver<String>>>> = OnceLock::new();

enum ChannelWriter {
    Sink,
    Channel(mpsc::Sender<String>, Vec<u8>),
}

impl Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Sink => Ok(buf.len()),
            Self::Channel(_, pending) => {
                pending.extend_from_slice(buf);
                Ok(buf.len())
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Self::Channel(tx, pending) = self {
            if !pending.is_empty() {
                let message = String::from_utf8_lossy(pending).into_owned();
                pending.clear();
                // 使用 try_send 避免 tracing subscriber 阻塞；channel 满时丢弃旧日志
                let _ = tx.try_send(message);
            }
        }
        Ok(())
    }
}

impl Drop for ChannelWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

struct ChannelWriterFactory(Option<mpsc::Sender<String>>);

impl<'a> fmt::MakeWriter<'a> for ChannelWriterFactory {
    type Writer = ChannelWriter;

    fn make_writer(&'a self) -> Self::Writer {
        match &self.0 {
            Some(tx) => ChannelWriter::Channel(tx.clone(), Vec::new()),
            None => ChannelWriter::Sink,
        }
    }
}

struct StdoutWriter(bool);

impl<'a> fmt::MakeWriter<'a> for StdoutWriter {
    type Writer = Box<dyn Write + Send + Sync>;

    fn make_writer(&'a self) -> Self::Writer {
        if self.0 {
            Box::new(io::sink())
        } else {
            Box::new(io::stdout())
        }
    }
}

// ── Rate Limiting Layer ──

/// Limits log events per-module per-second to prevent log storms.
///
/// When a module exceeds `burst` events in a rolling 1-second window,
/// subsequent events are suppressed until the next window.  A summary
/// line is emitted once per suppressed batch.
#[derive(Clone)]
pub struct RateLimitLayer {
    burst: u32,
}

impl RateLimitLayer {
    pub fn new(burst: u32) -> Self {
        Self { burst }
    }
}

const RATE_LIMIT_BURST: u32 = 100;

impl<S> tracing_subscriber::Layer<S> for RateLimitLayer
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn enabled(
        &self,
        metadata: &tracing::Metadata<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        // Always allow errors and warnings through.
        if *metadata.level() <= tracing::Level::WARN {
            return true;
        }
        RATE_LIMITER.check(metadata.target(), self.burst)
    }
}

static RATE_LIMITER: std::sync::LazyLock<LogRateLimiter> =
    std::sync::LazyLock::new(LogRateLimiter::new);

struct LogRateLimiter {
    buckets: std::sync::Mutex<HashMap<String, RateBucket>>,
}

struct RateBucket {
    count: u32,
    window_start: Instant,
    suppressed: bool,
}

impl LogRateLimiter {
    fn new() -> Self {
        Self {
            buckets: std::sync::Mutex::new(HashMap::new()),
        }
    }

    fn check(&self, target: &str, burst: u32) -> bool {
        let mut buckets = self.buckets.lock().unwrap();
        let now = Instant::now();
        let bucket = buckets.entry(target.to_string()).or_insert(RateBucket {
            count: 0,
            window_start: now,
            suppressed: false,
        });

        if now.duration_since(bucket.window_start).as_secs() >= 1 {
            // New window.
            bucket.count = 0;
            bucket.window_start = now;
            bucket.suppressed = false;
        }

        bucket.count += 1;
        if bucket.count <= burst {
            true
        } else {
            if !bucket.suppressed {
                bucket.suppressed = true;
                tracing::warn!(target, burst, "rate limit: suppressing excess logs");
            }
            false
        }
    }
}

// ── Sensitive Data Filter Layer ──

/// Patterns to redact: (lookup, replacement).
/// Uses simple substring search and replacement (no regex dependency).
const SENSITIVE_REDACTIONS: &[(&str, &str)] = &[
    // Query-param style
    ("token=", "token=[REDACTED]"),
    ("password=", "password=[REDACTED]"),
    ("admin_token=", "admin_token=[REDACTED]"),
    ("access_token=", "access_token=[REDACTED]"),
    ("refresh_token=", "refresh_token=[REDACTED]"),
    // Header style
    ("authorization:", "authorization: [REDACTED]"),
    ("Authorization:", "Authorization: [REDACTED]"),
    ("x-phira-token:", "x-phira-token: [REDACTED]"),
    // Bearer token in any position
    ("bearer ", "bearer [REDACTED]"),
    ("Bearer ", "Bearer [REDACTED]"),
];

/// Redact sensitive patterns from a log message string.
pub fn redact_sensitive(message: &str) -> String {
    let mut result = message.to_string();
    for &(lookup, replacement) in SENSITIVE_REDACTIONS {
        // Repeatedly replace until no more matches (handles multiple occurrences).
        while let Some(pos) = result.to_lowercase().find(&lookup.to_lowercase()) {
            let end = pos + lookup.len();
            // Find the end of the value (next space, quote, &, or end of string).
            let value_end = result[end..]
                .find(|c: char| {
                    c == ' ' || c == '"' || c == '&' || c == '\'' || c == ']' || c == ')'
                })
                .map(|i| end + i)
                .unwrap_or(result.len());
            result.replace_range(pos..value_end, replacement);
        }
    }
    result
}

/// Take the OpenUDS log-stream receiver.  Returns `None` on the first call
/// after the receiver was already claimed, or if `init()` has not run.
pub fn take_openuds_log_rx() -> Option<mpsc::Receiver<String>> {
    let lock = OPENUDS_LOG_RX.get_or_init(|| Mutex::new(None));
    lock.lock().unwrap().take()
}

/// 删除 `log_dir` 下超过 `retention_days` 天的日志文件（按文件名前缀过滤，
/// 只清理本服务产出的轮转文件；0 = 不清理）。基于 mtime 判定，返回删除数。
pub fn cleanup_old_logs(log_dir: &Path, file_name: &str, retention_days: u32) -> usize {
    if retention_days == 0 {
        return 0;
    }
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(retention_days as u64 * 86_400))
        .unwrap_or(std::time::UNIX_EPOCH);
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with(file_name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if meta.modified().map(|m| m < cutoff).unwrap_or(false)
            && std::fs::remove_file(entry.path()).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

pub fn init(file_name: &str, tui_tx: Option<mpsc::Sender<String>>) -> Result<WorkerGuard> {
    let log_dir = Path::new("log");
    if log_dir.exists() && !log_dir.is_dir() {
        anyhow::bail!("'log' exists and is not a directory");
    }
    std::fs::create_dir_all(log_dir)?;

    let (file_writer, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::hourly(log_dir, file_name));
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"))
        .add_directive("hyper=info".parse()?)
        .add_directive("rustls=info".parse()?)
        .add_directive("isahc=info".parse()?)
        .add_directive("h2=info".parse()?)
        .add_directive("reqwest=info".parse()?)
        .add_directive("wasmtime=info".parse()?);

    let has_tui = tui_tx.is_some();
    let log_format = std::env::var("RUST_LOG_FORMAT").unwrap_or_default();

    // Create the OpenUDS log-stream channel.  The broker task is started by
    // the OpenUDS server via take_openuds_log_rx(); if no OpenUDS server is
    // enabled, the sender's channel is never read and the broker never runs.
    let (openuds_log_tx, openuds_log_rx) = mpsc::channel::<String>(1024);
    let _ = OPENUDS_LOG_TX.set(openuds_log_tx);
    *OPENUDS_LOG_RX.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(openuds_log_rx);

    if log_format == "json" {
        // JSON structured output (machine-parseable, for production).
        // 文件层跟随 RUST_LOG（默认 info）；需要全量 trace 时设 RUST_LOG=trace。
        let json_file_layer = fmt::layer()
            .json()
            .with_writer(file_writer)
            .with_ansi(false)
            .with_current_span(true)
            .with_span_list(true)
            .with_filter(filter.clone());
        let stdout_layer = fmt::layer()
            .json()
            .with_writer(StdoutWriter(has_tui))
            .with_ansi(false)
            .with_current_span(true)
            .with_span_list(true)
            .with_filter(filter.clone());
        let tui_layer = fmt::layer()
            .with_writer(ChannelWriterFactory(tui_tx))
            .with_ansi(false)
            .with_filter(filter.clone());
        // OpenUDS log stream: always human-readable lines for external tools.
        let openuds_layer = fmt::layer()
            .with_writer(ChannelWriterFactory(Some(OPENUDS_LOG_TX.get().unwrap().clone())))
            .with_ansi(false)
            .with_filter(filter);
        tracing_subscriber::registry()
            .with(RateLimitLayer::new(RATE_LIMIT_BURST))
            .with(json_file_layer)
            .with(stdout_layer)
            .with(tui_layer)
            .with(openuds_layer)
            .init();
    } else {
        // Human-readable text output (default).
        // 文件层跟随 RUST_LOG（默认 info）；需要全量 trace 时设 RUST_LOG=trace。
        let file_layer = fmt::layer()
            .with_writer(file_writer)
            .with_ansi(false)
            .with_filter(filter.clone());
        let stdout_layer = fmt::layer()
            .with_writer(StdoutWriter(has_tui))
            .with_ansi(false)
            .with_filter(filter.clone());
        let tui_layer = fmt::layer()
            .with_writer(ChannelWriterFactory(tui_tx))
            .with_ansi(false)
            .with_filter(filter.clone());
        let openuds_layer = fmt::layer()
            .with_writer(ChannelWriterFactory(Some(OPENUDS_LOG_TX.get().unwrap().clone())))
            .with_ansi(false)
            .with_filter(filter);
        tracing_subscriber::registry()
            .with(RateLimitLayer::new(RATE_LIMIT_BURST))
            .with(file_layer)
            .with(stdout_layer)
            .with(tui_layer)
            .with(openuds_layer)
            .init();
    }

    Ok(guard)
}

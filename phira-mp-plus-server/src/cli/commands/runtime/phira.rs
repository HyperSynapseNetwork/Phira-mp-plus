//! Phira HTTP retry/circuit-breaker diagnostics.

use super::super::super::*;

impl CliHandler {
    pub(in crate::cli) fn print_runtime_phira(&self) {
        let stats = self.state.phira_client.stats();
        self.out(format!("  {} Phira HTTP RetryClient", c::green("◆")));
        self.out(format!("  {} requests:       {}", c::dim("│"), stats.requests));
        self.out(format!("  {} successes:      {}", c::dim("│"), stats.successes));
        self.out(format!("  {} retry_attempts: {}", c::dim("│"), stats.retry_attempts));
        self.out(format!("  {} retry_notices:  {}", c::dim("│"), stats.retry_notices));
        self.out(format!("  {} failures:       {}", c::dim("│"), stats.failures));
        self.out(format!("  {} last_error:     {}", c::dim("│"), stats.last_error.unwrap_or_else(|| "-".to_string())));
        self.out(format!(
            "  {} policy: timeout={}ms retries={} backoff={}..{}ms",
            c::dim("│"), stats.policy.timeout_ms, stats.policy.max_retries,
            stats.policy.base_backoff_ms, stats.policy.max_backoff_ms
        ));
        self.out(format!(
            "  {} breaker: {} enabled={} opened={} rejected={} threshold={} open={}ms",
            c::dim("│"), stats.circuit_breaker.state, stats.circuit_breaker.enabled,
            stats.circuit_breaker.opened, stats.circuit_breaker.rejected,
            stats.circuit_breaker.failure_threshold, stats.circuit_breaker.open_duration_ms
        ));
        self.out(format!("  {} Phira HTTP 策略来自 server_config.yml 的 runtime_v2.phira_http；Simulation 默认不访问 Phira", c::dim("▸")));
    }
}

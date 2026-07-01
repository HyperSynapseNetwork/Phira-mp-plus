//! Runtime v2 low-resource infrastructure budget.
//!
//! This module centralizes bounded in-memory capacities and diagnostic timeout
//! choices. Runtime v2 should grow through explicit infrastructure budgets, not
//! accidental unbounded Vecs, traces, broadcast buffers or forever-growing CLI
//! snapshots.

use serde::{Deserialize, Serialize};
use std::time::Duration;

const MIN_EVENT_BUS_CAPACITY: usize = 16;
const MAX_EVENT_BUS_CAPACITY: usize = 8192;
const MIN_EVENT_TRACE_CAPACITY: usize = 8;
const MAX_EVENT_TRACE_CAPACITY: usize = 2048;
const MIN_BENCHMARK_REPORT_CAPACITY: usize = 1;
const MAX_BENCHMARK_REPORT_CAPACITY: usize = 512;
const MIN_RUNTIME_QUERY_TIMEOUT_MS: u64 = 100;
const MAX_RUNTIME_QUERY_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeResourceBudgetConfig {
    /// Prefer bounded queues/traces and lower default capacities. This project is
    /// designed to run on low-resource servers, so the default is intentionally
    /// conservative.
    #[serde(default = "default_true")]
    pub low_resource_mode: bool,
    /// Tokio broadcast capacity for Runtime v2 EventBus. Dropping old observer
    /// messages is acceptable; losing gameplay side effects is not, and those do
    /// not rely on this observation bus.
    #[serde(default = "default_event_bus_capacity")]
    pub event_bus_capacity: usize,
    /// Recent EventBus trace entries kept in memory.
    #[serde(default = "default_event_trace_capacity")]
    pub event_trace_capacity: usize,
    /// Recent benchmark reports kept in memory for CLI/TUI/Web readonly views.
    #[serde(default = "default_benchmark_report_capacity")]
    pub benchmark_report_capacity: usize,
    /// Default limit for lightweight report/event lists.
    #[serde(default = "default_recent_list_limit")]
    pub recent_list_limit: usize,
    /// Timeout for synchronous state.query bridge calls used by Web readonly and
    /// plugin diagnostics. Keep this short so diagnostics cannot pin worker
    /// threads forever under load.
    #[serde(default = "default_runtime_query_timeout_ms")]
    pub runtime_query_timeout_ms: u64,
}

impl Default for RuntimeResourceBudgetConfig {
    fn default() -> Self {
        Self {
            low_resource_mode: default_true(),
            event_bus_capacity: default_event_bus_capacity(),
            event_trace_capacity: default_event_trace_capacity(),
            benchmark_report_capacity: default_benchmark_report_capacity(),
            recent_list_limit: default_recent_list_limit(),
            runtime_query_timeout_ms: default_runtime_query_timeout_ms(),
        }
    }
}

impl RuntimeResourceBudgetConfig {
    pub fn sanitized(&self) -> RuntimeResourceBudget {
        RuntimeResourceBudget {
            low_resource_mode: self.low_resource_mode,
            event_bus_capacity: self.event_bus_capacity.clamp(MIN_EVENT_BUS_CAPACITY, MAX_EVENT_BUS_CAPACITY),
            event_trace_capacity: self.event_trace_capacity.clamp(MIN_EVENT_TRACE_CAPACITY, MAX_EVENT_TRACE_CAPACITY),
            benchmark_report_capacity: self
                .benchmark_report_capacity
                .clamp(MIN_BENCHMARK_REPORT_CAPACITY, MAX_BENCHMARK_REPORT_CAPACITY),
            recent_list_limit: self.recent_list_limit.clamp(1, 128),
            runtime_query_timeout_ms: self
                .runtime_query_timeout_ms
                .clamp(MIN_RUNTIME_QUERY_TIMEOUT_MS, MAX_RUNTIME_QUERY_TIMEOUT_MS),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeResourceBudget {
    pub low_resource_mode: bool,
    pub event_bus_capacity: usize,
    pub event_trace_capacity: usize,
    pub benchmark_report_capacity: usize,
    pub recent_list_limit: usize,
    pub runtime_query_timeout_ms: u64,
}

impl RuntimeResourceBudget {
    pub fn state_query_timeout(&self) -> Duration {
        Duration::from_millis(self.runtime_query_timeout_ms)
    }

    pub fn summary(&self) -> RuntimeResourceBudgetSummary {
        RuntimeResourceBudgetSummary {
            low_resource_mode: self.low_resource_mode,
            event_bus_capacity: self.event_bus_capacity,
            event_trace_capacity: self.event_trace_capacity,
            benchmark_report_capacity: self.benchmark_report_capacity,
            recent_list_limit: self.recent_list_limit,
            runtime_query_timeout_ms: self.runtime_query_timeout_ms,
            policy: if self.low_resource_mode { "low_resource" } else { "custom" },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeResourceBudgetSummary {
    pub low_resource_mode: bool,
    pub event_bus_capacity: usize,
    pub event_trace_capacity: usize,
    pub benchmark_report_capacity: usize,
    pub recent_list_limit: usize,
    pub runtime_query_timeout_ms: u64,
    pub policy: &'static str,
}

fn default_true() -> bool { true }
fn default_event_bus_capacity() -> usize { 512 }
fn default_event_trace_capacity() -> usize { 128 }
fn default_benchmark_report_capacity() -> usize { 32 }
fn default_recent_list_limit() -> usize { 12 }
fn default_runtime_query_timeout_ms() -> u64 { 1500 }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_defaults_are_low_resource() {
        let budget = RuntimeResourceBudgetConfig::default().sanitized();
        assert!(budget.low_resource_mode);
        assert_eq!(budget.event_bus_capacity, 512);
        assert_eq!(budget.event_trace_capacity, 128);
        assert_eq!(budget.benchmark_report_capacity, 32);
        assert_eq!(budget.runtime_query_timeout_ms, 1500);
    }

    #[test]
    fn budget_clamps_extremes() {
        let budget = RuntimeResourceBudgetConfig {
            low_resource_mode: false,
            event_bus_capacity: 0,
            event_trace_capacity: usize::MAX,
            benchmark_report_capacity: usize::MAX,
            recent_list_limit: 0,
            runtime_query_timeout_ms: 1,
        }
        .sanitized();
        assert_eq!(budget.event_bus_capacity, MIN_EVENT_BUS_CAPACITY);
        assert_eq!(budget.event_trace_capacity, MAX_EVENT_TRACE_CAPACITY);
        assert_eq!(budget.benchmark_report_capacity, MAX_BENCHMARK_REPORT_CAPACITY);
        assert_eq!(budget.recent_list_limit, 1);
        assert_eq!(budget.runtime_query_timeout_ms, MIN_RUNTIME_QUERY_TIMEOUT_MS);
    }
}

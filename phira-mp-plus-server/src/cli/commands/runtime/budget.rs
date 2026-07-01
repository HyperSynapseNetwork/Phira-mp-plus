//! Runtime v2 low-resource budget diagnostics.

use super::super::super::*;

impl CliHandler {
    pub(in crate::cli) fn print_runtime_budget(&self) {
        let budget = self.state.runtime_budget.summary();
        let event_stats = self.state.event_bus.stats(self.state.runtime_budget.recent_list_limit);
        let benchmark_reports = self.state.benchmark_reports.snapshot(self.state.runtime_budget.recent_list_limit);
        self.out(format!("  {} Runtime resource budget", c::green("◆")));
        self.out(format!("  {} policy:             {}", c::dim("│"), budget.policy));
        self.out(format!("  {} low_resource_mode:  {}", c::dim("│"), budget.low_resource_mode));
        self.out(format!("  {} event_bus_capacity: {}", c::dim("│"), budget.event_bus_capacity));
        self.out(format!("  {} event_trace_cap:    {}", c::dim("│"), budget.event_trace_capacity));
        self.out(format!("  {} benchmark_cap:      {}", c::dim("│"), budget.benchmark_report_capacity));
        self.out(format!("  {} recent_list_limit:  {}", c::dim("│"), budget.recent_list_limit));
        self.out(format!("  {} query_timeout_ms:   {}", c::dim("│"), budget.runtime_query_timeout_ms));
        self.out(format!("  {} live bounded stores", c::cyan("▸")));
        self.out(format!(
            "    event_bus: channel_capacity={} trace_capacity={} recent={} published={}",
            event_stats.channel_capacity,
            event_stats.trace_capacity,
            event_stats.recent.len(),
            event_stats.published,
        ));
        self.out(format!(
            "    benchmark_reports: capacity={} recent={} total={}",
            benchmark_reports.capacity,
            benchmark_reports.recent.len(),
            benchmark_reports.total,
        ));
        self.out(format!("  {} 配置入口: runtime_v2.budget；默认低资源模式，不让诊断快照无限增长", c::dim("▸")));
    }
}

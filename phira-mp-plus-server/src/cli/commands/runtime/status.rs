//! Runtime v2 status summary.

use super::super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn print_runtime_status(&self) {
        let sim = self.state.simulation.status().await;
        let persistence = self.state.persistence_worker.stats().await;
        self.out(format!("  {} Runtime v2 skeleton", c::green("◆")));
        let event_stats = self.state.event_bus.stats(self.state.runtime_budget.recent_list_limit.min(5));
        let budget = self.state.runtime_budget.summary();
        self.out(format!("  {} command specs:      {}", c::dim("│"), self.state.command_registry.iter().count()));
        self.out(format!("  {} event subscribers:  {}", c::dim("│"), event_stats.receiver_count));
        self.out(format!("  {} events published:   {}", c::dim("│"), event_stats.published));
        let actors = self.state.actor_runtime.stats().await;
        let room_commands = self.state.room_commands.stats();
        let benchmark_reports = self.state.benchmark_reports.snapshot(self.state.runtime_budget.recent_list_limit.min(3));
        self.out(format!("  {} simulation running: {}", c::dim("│"), sim.running));
        self.out(format!("  {} persistence queue:  queued={} processed={} dropped={}", c::dim("│"), persistence.queued, persistence.processed, persistence.dropped));
        self.out(format!("  {} telemetry cutover:  {}", c::dim("│"), persistence.telemetry_cutover_mode));
        let phira = self.state.phira_client.stats();
        let plan = self.state.runtime_plan.snapshot();
        self.out(format!("  {} room command gw:    routed={} ok={} failed={} mailbox={}", c::dim("│"), room_commands.routed, room_commands.succeeded, room_commands.failed, room_commands.mailbox_enabled));
        self.out(format!("  {} phira http:         requests={} retry={} failures={}", c::dim("│"), phira.requests, phira.retry_attempts, phira.failures));
        self.out(format!("  {} benchmark reports:  total={} latest_modes={} recent={}", c::dim("│"), benchmark_reports.total, benchmark_reports.latest_by_mode.len(), benchmark_reports.recent.len()));
        self.out(format!("  {} resource budget:    low={} event_cap={} trace_cap={} report_cap={} query={}ms", c::dim("│"), budget.low_resource_mode, budget.event_bus_capacity, budget.event_trace_capacity, budget.benchmark_report_capacity, budget.runtime_query_timeout_ms));
        self.out(format!("  {} runtime plan:       total={} active={} planned={} blocked={}", c::dim("│"), plan.total, plan.active, plan.planned, plan.blocked));
        self.out(format!("  {} actor blueprint:    {} boundaries", c::dim("│"), actors.boundaries.len()));
        self.out(format!("  {} web management API: {}", c::dim("│"), actors.web_management_api));
        self.out(format!("  {} 现有 Room/Session/DB 主逻辑仍未完全迁移；Actor 模型是最终架构，Web 管理 API 不做", c::dim("▸")));
    }
}

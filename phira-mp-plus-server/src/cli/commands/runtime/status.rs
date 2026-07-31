//! Runtime status summary.

use super::super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn print_runtime_status(&self) {
        let persistence = self.state.persistence_worker.stats().await;
        self.out(format!("  {} Runtime skeleton", c::green("◆")));
        let event_stats = self.state.event_bus.stats(5);
        self.out(format!(
            "  {} command specs:      {}",
            c::dim("│"),
            self.state.command_registry.iter().count()
        ));
        self.out(format!(
            "  {} event subscribers:  {}",
            c::dim("│"),
            event_stats.receiver_count
        ));
        self.out(format!(
            "  {} events published:   {}",
            c::dim("│"),
            event_stats.published
        ));
        let room_commands = self.state.room_commands.stats();
        self.out(format!(
            "  {} persistence queue:  queued={} processed={} dropped={} health={} pending={}%",
            c::dim("│"),
            persistence.queued,
            persistence.processed,
            persistence.dropped,
            persistence.queue_health,
            persistence.pending_ratio_percent
        ));
        self.out(format!(
            "  {} telemetry path: HighFrequencyWriter (bypasses WAL)",
            c::dim("│"),
        ));
        let phira = self.state.phira_client.stats();
        self.out(format!(
            "  {} room command gw:    routed={} ok={} failed={} mailbox={}",
            c::dim("│"),
            room_commands.routed,
            room_commands.succeeded,
            room_commands.failed,
            room_commands.mailbox_enabled
        ));
        self.out(format!(
            "  {} phira http:         requests={} retry={} failures={}",
            c::dim("│"),
            phira.requests,
            phira.retry_attempts,
            phira.failures
        ));
        self.out(format!(
            "  {} diagnostics cache:  event_trace={}",
            c::dim("│"),
            event_stats.trace_capacity
        ));
        // Plugin TCP per-plugin metrics (pending/dropped events, buffered bytes).
        if let Some(tx) = &self.state.plugin_tcp_tx {
            let (reply, rx) = std::sync::mpsc::channel();
            if tx
                .try_send(crate::plugin_tcp::PluginTcpCommand::Stats { reply })
                .is_ok()
            {
                if let Ok(Ok(stats)) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
                    let mut total_pending: u64 = 0;
                    let mut total_dropped: u64 = 0;
                    let mut total_bytes: u64 = 0;
                    let plugin_count = match &stats {
                        serde_json::Value::Object(plugins) => {
                            for (_pid, v) in plugins {
                                total_pending += v.get("pending_events").and_then(|x| x.as_u64()).unwrap_or(0);
                                total_dropped += v.get("dropped_events").and_then(|x| x.as_u64()).unwrap_or(0);
                                total_bytes += v.get("pending_read_bytes").and_then(|x| x.as_u64()).unwrap_or(0);
                            }
                            plugins.len()
                        }
                        _ => 0,
                    };
                    self.out(format!(
                        "  {} plugin tcp:        plugins={} pending_events={} dropped_events={} pending_read_bytes={}",
                        c::dim("│"),
                        plugin_count,
                        total_pending,
                        total_dropped,
                        total_bytes,
                    ));
                }
            }
        }
        self.out(format!(
            "  {} 现有 Room/Session/DB 主逻辑仍未完全迁移；Actor 模型是最终架构，Web 管理 API 不做",
            c::dim("▸")
        ));
    }
}

//! Runtime v2 persistence schema diagnostics.

use super::super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn print_runtime_schema(&self) {
        self.out(format!("  {} Runtime v2 persistence schema", c::green("◆")));
        self.out(format!("  {} telemetry schema version: 2", c::dim("│")));
        self.out(format!(
            "  {} batch table: mp_runtime_telemetry_batches",
            c::dim("│")
        ));
        self.out(format!(
            "  {} item table:  mp_runtime_telemetry_items",
            c::dim("│")
        ));
        self.out(format!(
            "  {} meta table:  mp_runtime_persistence_meta",
            c::dim("│")
        ));
        self.out(format!(
            "  {} policy table: mp_runtime_retention_policies",
            c::dim("│")
        ));
        self.out(format!("  {} important columns", c::cyan("▸")));
        self.out("    batch_uuid, run_id, scope, pipeline, source, dual_write, schema_version, flush_reason".to_string());
        self.out("    round_uuid, room_id, player_id, item_count, payload, created_at".to_string());
        self.out(format!("  {} mode", c::cyan("▸")));
        let stats = self.state.persistence_worker.stats().await;
        self.out(format!(
            "    production Touch/Judge cutover: {}",
            stats.telemetry_cutover_mode
        ));
        self.out("    modes: direct_only | worker_preferred | worker_authoritative".to_string());
        self.out(
            "    read path: direct mp_round_player_data first, Runtime v2 item table fallback"
                .to_string(),
        );
        self.out("    persist.touches / persist.judges also fall back to Runtime v2 batch table when direct batches are absent".to_string());
        self.out(
            "    simulation: mp_sim_events + Runtime v2 simulation telemetry path".to_string(),
        );
        self.out(format!("  {} 项目仍处测试阶段，schema 和持久化路径可继续自由演进；runtime cutover <mode> 可切换", c::dim("▸")));
    }
}

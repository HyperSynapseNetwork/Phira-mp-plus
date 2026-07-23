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
        self.out("    batch_uuid, run_id, scope, pipeline, source, schema_version, flush_reason".to_string());
        self.out("    round_uuid, room_id, player_id, item_count, payload, created_at".to_string());
        self.out(format!("  {} persistence path", c::cyan("▸")));
        self.out("    production Touch/Judge: PersistenceWorker/TelemetryBatcher (unified path)".to_string());
        self.out("    persist.touches / persist.judges: TelemetryBatcher batch table".to_string());
        self.out(
            "    simulation: mp_sim_events + Runtime v2 simulation telemetry path".to_string(),
        );
        self.out(format!("  {} 项目仍处测试阶段，schema 和持久化路径可继续自由演进", c::dim("▸")));
    }
}

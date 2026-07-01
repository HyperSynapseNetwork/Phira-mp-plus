//! Runtime telemetry cutover controls.

use super::super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn runtime_cutover(&self, args: &[&str]) {
        if let Some(raw_mode) = args.get(1) {
            match crate::telemetry_batcher::TelemetryCutoverMode::parse(raw_mode) {
                Some(mode) => {
                    let mode = self.state.persistence_worker.set_telemetry_cutover_mode(mode).await;
                    self.out(format!("  {} telemetry cutover mode set to {}", c::green("✓"), c::bold(mode.as_str())));
                    self.out(format!("  {} {}", c::dim("▸"), mode.description()));
                    let decision = mode.cutover_decision();
                    self.out(format!(
                        "  {} decision: enqueue_worker={} direct_before_result={}",
                        c::dim("▸"),
                        decision.enqueue_worker,
                        decision.write_direct_before_worker_result,
                    ));
                }
                None => {
                    self.out(format!("  {} unknown telemetry cutover mode: {}", c::red("✗"), c::yellow(raw_mode)));
                    self.out("  available: direct_only | worker_only".to_string());
                }
            }
        } else {
            let stats = self.state.persistence_worker.stats().await;
            self.out(format!("  {} Telemetry cutover", c::green("◆")));
            self.out(format!("  {} current: {}", c::dim("│"), c::bold(&stats.telemetry_cutover_mode)));
            self.out(format!("  {} changes: {}", c::dim("│"), stats.telemetry_cutover_changes));
            self.out(format!("  {} modes", c::cyan("▸")));
            for mode in crate::telemetry_batcher::TelemetryCutoverMode::variants() {
                let marker = if mode.as_str() == stats.telemetry_cutover_mode { "*" } else { " " };
                let decision = mode.cutover_decision();
                self.out(format!(
                    "    {} {:<14} {} [worker={} direct={}]",
                    marker,
                    mode.as_str(),
                    mode.description(),
                    decision.enqueue_worker,
                    decision.write_direct_before_worker_result,
                ));
            }
            self.out(format!("  {} examples", c::cyan("▸")));
            self.out("    runtime cutover worker_only".to_string());
            self.out("    runtime cutover direct_only".to_string());
        }
    }
}

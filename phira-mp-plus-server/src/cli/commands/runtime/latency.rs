//! Runtime latency histograms: server-side command response + handshake.

use super::super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn print_runtime_latency(&self) {
        let trace = crate::official_client_compat::protocol_trace::ProtocolTrace::get();
        self.print_latency_histogram(
            "响应延迟直方图（命令收到→响应）",
            trace.latency_histogram.snapshot(),
        );
        self.print_latency_histogram(
            "握手延迟直方图（收到认证→AuthOK 发出）",
            trace.handshake_latency_histogram.snapshot(),
        );
    }

    fn print_latency_histogram(&self, title: &str, counts: Vec<u64>) {
        let boundaries = crate::official_client_compat::protocol_trace::LATENCY_BOUNDARIES_MS;
        let total: u64 = counts.iter().sum();
        let max_count: u64 = counts.iter().max().copied().unwrap_or(1).max(1);
        self.out(format!("  {} {}（总 {} 次）", c::green("◆"), title, total));
        for (i, count) in counts.iter().enumerate() {
            let range = if i == 0 {
                format!("< {}ms", boundaries[0])
            } else if i < boundaries.len() {
                format!("{}–{}ms", boundaries[i - 1], boundaries[i])
            } else {
                format!("≥ {}ms", boundaries[i - 1])
            };
            let pct = if total > 0 {
                100.0 * *count as f64 / total as f64
            } else {
                0.0
            };
            let bar_len = (30.0 * *count as f64 / max_count as f64).round() as usize;
            let bar = "█".repeat(bar_len);
            self.out(format!(
                "  {} {:<12} {:<30} {:>6} ({:>5.1}%)",
                c::dim("│"),
                range,
                bar,
                count,
                pct
            ));
        }
    }
}

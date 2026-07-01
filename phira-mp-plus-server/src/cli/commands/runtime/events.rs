//! EventBus diagnostics.

use super::super::super::*;

impl CliHandler {
    pub(in crate::cli) fn print_runtime_events(&self) {
        let stats = self.state.event_bus.stats(12);
        self.out(format!("  {} EventBus", c::green("◆")));
        self.out(format!("  {} subscribers:      {}", c::dim("│"), stats.receiver_count));
        self.out(format!("  {} published:        {}", c::dim("│"), stats.published));
        self.out(format!("  {} delivered_total:  {}", c::dim("│"), stats.delivered_total));
        self.out(format!("  {} no_subscriber:    {}", c::dim("│"), stats.no_subscriber));
        self.out(format!("  {} lagged_or_closed: {}", c::dim("│"), stats.lagged_or_closed));
        if !stats.by_kind.is_empty() {
            self.out(format!("  {} by kind", c::cyan("▸")));
            for item in stats.by_kind.iter().rev().take(16) {
                self.out(format!("    {:<28} {}", item.kind, item.count));
            }
        }
        if !stats.recent.is_empty() {
            self.out(format!("  {} recent", c::cyan("▸")));
            for event in stats.recent {
                self.out(format!("    #{:<4} {:<24} subscribers={} {}", event.seq, event.kind, event.subscribers, event.summary));
            }
        }
        self.out(format!("  {} 当前只作为 Runtime v2 新功能事件脊柱，未替换旧插件/房间调用", c::dim("▸")));
    }
}

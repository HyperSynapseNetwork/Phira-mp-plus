//! RoomCommandGateway diagnostics.

use super::super::super::*;

impl CliHandler {
    pub(in crate::cli) fn print_runtime_rooms(&self) {
        let stats = self.state.room_commands.stats();
        self.out(format!("  {} RoomCommandGateway", c::green("◆")));
        self.out(format!("  {} phase:     {}", c::dim("│"), stats.phase));
        self.out(format!("  {} routed:    {}", c::dim("│"), stats.routed));
        self.out(format!("  {} succeeded: {}", c::dim("│"), stats.succeeded));
        self.out(format!("  {} failed:    {}", c::dim("│"), stats.failed));
        self.out(format!(
            "  {} mailbox:   enabled={} active_rooms={} created={} enqueued={} completed={} failed={} fallback={} closed={}",
            c::dim("│"), stats.mailbox_enabled, stats.room_mailboxes, stats.mailbox_created,
            stats.mailbox_enqueued, stats.mailbox_completed, stats.mailbox_failed,
            stats.mailbox_fallback, stats.mailbox_closed
        ));
        self.out(format!("  {} registry:  hit={} miss={}", c::dim("│"), stats.mailbox_registry_hit, stats.mailbox_registry_miss));
        let avg_us = if stats.audited > 0 { stats.latency_total_us / stats.audited } else { 0 };
        self.out(format!("  {} audit:     commands={} avg_us={} max_us={}", c::dim("│"), stats.audited, avg_us, stats.latency_max_us));
        if !stats.recent_commands.is_empty() {
            self.out(format!("  {} recent commands", c::cyan("▸")));
            for item in stats.recent_commands.iter().take(8) {
                let status = if item.ok { c::green("ok") } else { c::red("err") };
                let err = item.error.as_deref().unwrap_or("");
                self.out(format!(
                    "    #{:<4} {:<9} room={} {:>6}us {:<16} {} {}",
                    item.command_id, item.action, item.room_id, item.latency_us, item.delivery, status, err
                ));
            }
        }
        self.out(format!("  {} note:      {}", c::dim("│"), stats.note));
        self.out(format!("  {} set_lock/set_cycle/set_host/close/kick/start/cancel 已穿过 per-room mailbox registry", c::dim("▸")));
    }
}

//! Runtime v2 Actor model blueprint diagnostics.

use super::super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn print_runtime_actors(&self) {
        let stats = self.state.actor_runtime.stats().await;
        self.out(format!(
            "  {} Runtime v2 Actor Model Blueprint",
            c::green("◆")
        ));
        self.out(format!(
            "  {} phase:              {}",
            c::dim("│"),
            stats.phase
        ));
        self.out(format!(
            "  {} web management API: {}",
            c::dim("│"),
            stats.web_management_api
        ));
        self.out(format!(
            "  {} rule:               {}",
            c::dim("│"),
            stats.rule
        ));
        let room_commands = self.state.room_commands.stats();
        self.out(format!(
            "  {} room gateway:       phase={} routed={} ok={} failed={} mailbox={} audited={} max_us={}",
            c::dim("│"), room_commands.phase, room_commands.routed, room_commands.succeeded,
            room_commands.failed, room_commands.mailbox_enabled, room_commands.audited,
            room_commands.latency_max_us
        ));
        self.out(format!("  {} boundaries", c::cyan("▸")));
        for boundary in stats.boundaries {
            self.out(format!(
                "    {:<20} {:<12} {}",
                c::bold(&boundary.name),
                boundary.status.as_str(),
                boundary.responsibility
            ));
            self.out(format!(
                "      {} next: {}",
                c::dim("▸"),
                boundary.next_step
            ));
            self.out(format!(
                "      {} files: {}",
                c::dim("▸"),
                boundary.source_files.join(", ")
            ));
        }
        self.out(format!(
            "  {} 迁移节奏：先镜像事件，再迁移读路径，再迁移写路径，最后删旧直连调用",
            c::dim("▸")
        ));
    }
}

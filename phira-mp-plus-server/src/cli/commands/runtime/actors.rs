//! Runtime actor diagnostics — room command gateway status.

use super::super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn print_runtime_actors(&self) {
        let room_commands = self.state.room_commands.stats();
        self.out(format!(
            "  {} Runtime Room Command Gateway",
            c::green("◆")
        ));
        self.out(format!(
            "  {} phase:       {}",
            c::dim("│"),
            room_commands.phase
        ));
        self.out(format!(
            "  {} routed:      {}",
            c::dim("│"),
            room_commands.routed
        ));
        self.out(format!(
            "  {} ok:          {}",
            c::dim("│"),
            room_commands.succeeded
        ));
        self.out(format!(
            "  {} failed:      {}",
            c::dim("│"),
            room_commands.failed
        ));
        self.out(format!(
            "  {} mailbox:     {}",
            c::dim("│"),
            room_commands.mailbox_enabled
        ));
        self.out(format!(
            "  {} audited:     {}",
            c::dim("│"),
            room_commands.audited
        ));
        self.out(format!(
            "  {} max_latency: {} us",
            c::dim("│"),
            room_commands.latency_max_us
        ));
        self.out(format!(
            "  {} 迁移节奏：先镜像事件，再迁移读路径，再迁移写路径，最后删旧直连调用",
            c::dim("▸")
        ));
    }
}

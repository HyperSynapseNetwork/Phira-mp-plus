//! Runtime v2 roadmap/workboard output.

use super::super::super::*;

impl CliHandler {
    pub(in crate::cli) fn print_runtime_roadmap(&self) {
        let plan = self.state.runtime_plan.snapshot();
        self.out(format!("  {} Runtime v2 master workboard", c::green("◆")));
        self.out(format!("  {} final architecture: {}", c::dim("│"), plan.final_architecture));
        self.out(format!("  {} web management API: disabled by policy", c::dim("│")));
        self.out(format!("  {} total={} active={} planned={} blocked={} done={}", c::dim("│"), plan.total, plan.active, plan.planned, plan.blocked, plan.done));
        self.out(format!("  {} objectives", c::cyan("▸")));
        for item in plan.objectives {
            let status = match item.status {
                "active" => c::green(item.status),
                "planned" => c::yellow(item.status),
                "blocked" => c::red(item.status),
                _ => c::dim(item.status),
            };
            self.out(format!("    [{:<7}] {:<2} {:<24} {}", status, item.priority, item.key, item.title));
            self.out(format!("      {} next: {}", c::dim("▸"), item.next_step));
        }
    }
}

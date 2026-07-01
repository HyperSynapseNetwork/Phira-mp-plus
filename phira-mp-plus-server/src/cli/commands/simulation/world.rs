//! Simulation status, world inspection, sample data, and snapshot commands.

use super::runner;
use super::super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn simulation_persist(&self) {
        let status = self.state.simulation.status().await;
        let Some(run_id) = status.run_id else {
            self.out(format!("  {} simulation 未运行，无法生成 snapshot", c::red("✗")));
            return;
        };
        runner::publish_simulation_snapshot(&self.state, run_id, &status, "cli.simulation.persist").await;
        self.out(format!("  {} simulation snapshot 已发送到 EventBus / PersistenceWorker", c::green("✓")));
        self.out(format!("  {} run_id={}", c::dim("│"), run_id));
        self.out(format!("  {} 这是 simulation 专用诊断数据，不会写入真实 mp_* 玩家/房间表", c::dim("▸")));
    }

    pub(in crate::cli) async fn print_simulation_status(&self) {
        let status = self.state.simulation.status().await;
        self.out(format!("  {} Runtime v2 Simulation", c::green("◆")));
        self.out(format!("  {} running:        {}", c::dim("│"), status.running));
        self.out(format!("  {} run_id:         {}", c::dim("│"), status.run_id.as_ref().map(|id| id.to_string()).unwrap_or_else(|| "-".to_string())));
        self.out(format!("  {} seed:           {}", c::dim("│"), status.seed));
        self.out(format!("  {} preset:         {:?}", c::dim("│"), status.config.preset));
        self.out(format!("  {} scenario:       {} - {}", c::dim("│"), status.config.scenario.as_str(), status.config.scenario.description()));
        self.out(format!("  {} target:         {} users / {} rooms / {}s", c::dim("│"), status.config.users, status.config.rooms, status.config.duration_secs));
        self.out(format!("  {} elapsed/remain: {}/{}s", c::dim("│"), status.elapsed_secs, status.remaining_secs.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string())));
        self.out(format!("  {} runner:         auto={} tick_ms={} persist_every={}", c::dim("│"), status.runner_enabled, status.config.tick_interval_ms, status.config.persist_every_ticks));
        self.out(format!("  {} touch/judge:    {} / {}", c::dim("│"), status.config.touch, status.config.judge));
        self.out(format!("  {} virtual state:  {} users / {} rooms", c::dim("│"), status.virtual_users, status.virtual_rooms));
        self.out(format!("  {} materialized:   {} users / {} rooms / {} rounds", c::dim("│"), status.materialized_users, status.materialized_rooms, status.materialized_rounds));
        self.out(format!("  {} counters:       ticks={} chats={} ready={} touch={} judge={} result={}",
            c::dim("│"), status.counters.ticks, status.counters.chat_messages,
            status.counters.ready_events, status.counters.touch_batches,
            status.counters.judge_batches, status.counters.round_results));
        self.out(format!("  {} note:           {}", c::dim("│"), status.note));
    }

    pub(in crate::cli) async fn print_simulation_world(&self, limit: usize) {
        let Some(world) = self.state.simulation.world_snapshot(limit).await else {
            self.out(format!("  {} simulation shadow world 不存在；请先执行 simulation run baseline", c::yellow("!")));
            return;
        };
        self.out(format!("  {} Simulation Shadow World", c::green("◆")));
        self.out(format!("  {} run_id:       {}", c::dim("│"), world.run_id.as_ref().map(|id| id.to_string()).unwrap_or_else(|| "-".to_string())));
        self.out(format!("  {} totals:       {} users / {} rooms", c::dim("│"), world.users_total, world.rooms_total));
        self.out(format!("  {} materialized: {} users / {} rooms / {} rounds", c::dim("│"), world.users_materialized, world.rooms_materialized, world.rounds_materialized));
        self.out(format!("  {} {}", c::dim("▸"), world.materialization_note));
        self.out(format!("  {} sample rooms", c::cyan("▸")));
        for room in world.sample_rooms.iter().take(8) {
            self.out(format!("    {} chart={} members={} ready={} playing={} round={}",
                c::bold(&room.id), room.chart_id, room.member_ids.len(), room.ready_count,
                room.playing, room.round_id.as_deref().unwrap_or("-")));
        }
        self.out(format!("  {} sample users", c::cyan("▸")));
        for user in world.sample_users.iter().take(8) {
            self.out(format!("    {} id={} room={} ready={} playing={}",
                c::bold(&user.name), user.id, user.room_id.as_deref().unwrap_or("-"), user.ready, user.playing));
        }
        if !world.sample_rounds.is_empty() {
            self.out(format!("  {} sample rounds", c::cyan("▸")));
            for round in world.sample_rounds.iter().take(6) {
                self.out(format!("    {} room={} chart={} players={} score={} touch={} judge={}",
                    c::bold(&round.round_id), round.room_id, round.chart_id, round.players,
                    round.sample_score, round.sample_touches, round.sample_judges));
            }
        }
        if !world.recent_events.is_empty() {
            self.out(format!("  {} recent events", c::cyan("▸")));
            for event in world.recent_events.iter().take(8) {
                self.out(format!("    #{:<4} {:<10} {}", event.seq, event.kind, event.message));
            }
        }
    }

    pub(in crate::cli) async fn print_simulation_sample(&self) {
        let status = self.state.simulation.status().await;
        let touches = crate::simulation::SimulationManager::sample_touches(status.seed);
        let judges = crate::simulation::SimulationManager::sample_judges(status.seed);
        self.out(format!("  {} sample touches: {} 条；sample judges: {} 条；seed={}", c::green("◆"), touches.len(), judges.len(), status.seed));
        if let Some(first) = touches.first() {
            self.out(format!("  {} first touch: t={}ms lane={} pressed={}", c::dim("│"), first.time_ms, first.lane, first.pressed));
        }
        if let Some(first) = judges.first() {
            self.out(format!("  {} first judge: t={}ms {} +{}", c::dim("│"), first.time_ms, first.judge, first.score_delta));
        }
        self.out(format!("  {} Step 3 示例数据由 shadow world 复用，仍不写入真实数据库/房间", c::dim("▸")));
    }
}

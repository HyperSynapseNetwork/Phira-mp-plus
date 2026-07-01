use super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn dispatch_runtime_command(&self, args: &[&str]) {
        self.runtime_command(args).await;
    }

    async fn runtime_command(&self, args: &[&str]) {
        let sub = args.first().copied().unwrap_or("status");
        match sub {
            "status" | "" => {
                let sim = self.state.simulation.status().await;
                let persistence = self.state.persistence_worker.stats().await;
                self.out(format!("  {} Runtime v2 skeleton", c::green("◆")));
                let event_stats = self.state.event_bus.stats(5);
                self.out(format!("  {} command specs:      {}", c::dim("│"), self.state.command_registry.iter().count()));
                self.out(format!("  {} event subscribers:  {}", c::dim("│"), event_stats.receiver_count));
                self.out(format!("  {} events published:   {}", c::dim("│"), event_stats.published));
                let actors = self.state.actor_runtime.stats().await;
                let room_commands = self.state.room_commands.stats();
                self.out(format!("  {} simulation running: {}", c::dim("│"), sim.running));
                self.out(format!("  {} persistence queue:  queued={} processed={} dropped={}", c::dim("│"), persistence.queued, persistence.processed, persistence.dropped));
                self.out(format!("  {} telemetry cutover:  {}", c::dim("│"), persistence.telemetry_cutover_mode));
                let phira = self.state.phira_client.stats();
                let plan = self.state.runtime_plan.snapshot();
                self.out(format!("  {} room command gw:    routed={} ok={} failed={} mailbox={}", c::dim("│"), room_commands.routed, room_commands.succeeded, room_commands.failed, room_commands.mailbox_enabled));
                self.out(format!("  {} phira http:         requests={} retry={} failures={}", c::dim("│"), phira.requests, phira.retry_attempts, phira.failures));
                self.out(format!("  {} runtime plan:       total={} active={} planned={} blocked={}", c::dim("│"), plan.total, plan.active, plan.planned, plan.blocked));
                self.out(format!("  {} actor blueprint:    {} boundaries", c::dim("│"), actors.boundaries.len()));
                self.out(format!("  {} web management API: {}", c::dim("│"), actors.web_management_api));
                self.out(format!("  {} 现有 Room/Session/DB 主逻辑仍未完全迁移；Actor 模型是最终架构，Web 管理 API 不做", c::dim("▸")));
            }
            "roadmap" => {
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
            "phira" => {
                let stats = self.state.phira_client.stats();
                self.out(format!("  {} Phira HTTP RetryClient", c::green("◆")));
                self.out(format!("  {} requests:       {}", c::dim("│"), stats.requests));
                self.out(format!("  {} successes:      {}", c::dim("│"), stats.successes));
                self.out(format!("  {} retry_attempts: {}", c::dim("│"), stats.retry_attempts));
                self.out(format!("  {} retry_notices:  {}", c::dim("│"), stats.retry_notices));
                self.out(format!("  {} failures:       {}", c::dim("│"), stats.failures));
                self.out(format!("  {} last_error:     {}", c::dim("│"), stats.last_error.unwrap_or_else(|| "-".to_string())));
                self.out(format!("  {} policy: timeout={}ms retries={} backoff={}..{}ms",
                    c::dim("│"), stats.policy.timeout_ms, stats.policy.max_retries,
                    stats.policy.base_backoff_ms, stats.policy.max_backoff_ms));
                self.out(format!("  {} breaker: {} enabled={} opened={} rejected={} threshold={} open={}ms",
                    c::dim("│"), stats.circuit_breaker.state, stats.circuit_breaker.enabled,
                    stats.circuit_breaker.opened, stats.circuit_breaker.rejected,
                    stats.circuit_breaker.failure_threshold, stats.circuit_breaker.open_duration_ms));
                self.out(format!("  {} Phira HTTP 策略来自 server_config.yml 的 runtime_v2.phira_http；Simulation 默认不访问 Phira", c::dim("▸")));
            }
            "commands" => {
                self.out(format!("  {} Command Registry", c::green("◆")));
                self.out(format!("  {} groups: {}", c::dim("│"), self.state.command_registry.groups().join(", ")));
                self.out(format!("  {} specs:  {}", c::dim("│"), self.state.command_registry.iter().count()));
                self.out(format!("  {} roots:  {}", c::dim("│"), self.state.command_registry.root_commands().join(", ")));
            }
            "events" => {
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
            "rooms" => {
                let stats = self.state.room_commands.stats();
                self.out(format!("  {} RoomCommandGateway", c::green("◆")));
                self.out(format!("  {} phase:     {}", c::dim("│"), stats.phase));
                self.out(format!("  {} routed:    {}", c::dim("│"), stats.routed));
                self.out(format!("  {} succeeded: {}", c::dim("│"), stats.succeeded));
                self.out(format!("  {} failed:    {}", c::dim("│"), stats.failed));
                self.out(format!("  {} mailbox:   enabled={} active_rooms={} created={} enqueued={} completed={} failed={} fallback={} closed={}",
                    c::dim("│"), stats.mailbox_enabled, stats.room_mailboxes, stats.mailbox_created,
                    stats.mailbox_enqueued, stats.mailbox_completed, stats.mailbox_failed,
                    stats.mailbox_fallback, stats.mailbox_closed));
                self.out(format!("  {} registry:  hit={} miss={}", c::dim("│"), stats.mailbox_registry_hit, stats.mailbox_registry_miss));
                let avg_us = if stats.audited > 0 { stats.latency_total_us / stats.audited } else { 0 };
                self.out(format!("  {} audit:     commands={} avg_us={} max_us={}", c::dim("│"), stats.audited, avg_us, stats.latency_max_us));
                if !stats.recent_commands.is_empty() {
                    self.out(format!("  {} recent commands", c::cyan("▸")));
                    for item in stats.recent_commands.iter().take(8) {
                        let status = if item.ok { c::green("ok") } else { c::red("err") };
                        let err = item.error.as_deref().unwrap_or("");
                        self.out(format!(
                            "    #{:<4} {:<9} room={} {:>6}us {} {}",
                            item.command_id, item.action, item.room_id, item.latency_us, status, err
                        ));
                    }
                }
                self.out(format!("  {} note:      {}", c::dim("│"), stats.note));
                self.out(format!("  {} set_lock/set_cycle/set_host/close/kick/start/cancel 已穿过 per-room mailbox registry", c::dim("▸")));
            }
            "actors" => {
                let stats = self.state.actor_runtime.stats().await;
                self.out(format!("  {} Runtime v2 Actor Model Blueprint", c::green("◆")));
                self.out(format!("  {} phase:              {}", c::dim("│"), stats.phase));
                self.out(format!("  {} web management API: {}", c::dim("│"), stats.web_management_api));
                self.out(format!("  {} rule:               {}", c::dim("│"), stats.rule));
                let room_commands = self.state.room_commands.stats();
                self.out(format!("  {} room gateway:       phase={} routed={} ok={} failed={} mailbox={} audited={} max_us={}", c::dim("│"), room_commands.phase, room_commands.routed, room_commands.succeeded, room_commands.failed, room_commands.mailbox_enabled, room_commands.audited, room_commands.latency_max_us));
                self.out(format!("  {} boundaries", c::cyan("▸")));
                for boundary in stats.boundaries {
                    self.out(format!(
                        "    {:<20} {:<12} {}",
                        c::bold(&boundary.name),
                        boundary.status.as_str(),
                        boundary.responsibility
                    ));
                    self.out(format!("      {} next: {}", c::dim("▸"), boundary.next_step));
                    self.out(format!("      {} files: {}", c::dim("▸"), boundary.source_files.join(", ")));
                }
                self.out(format!("  {} 迁移节奏：先镜像事件，再迁移读路径，再迁移写路径，最后删旧直连调用", c::dim("▸")));
            }

            "cutover" => {
                if let Some(raw_mode) = args.get(1) {
                    match crate::telemetry_batcher::TelemetryCutoverMode::parse(raw_mode) {
                        Some(mode) => {
                            let mode = self.state.persistence_worker.set_telemetry_cutover_mode(mode).await;
                            self.out(format!("  {} telemetry cutover mode set to {}", c::green("✓"), c::bold(mode.as_str())));
                            self.out(format!("  {} {}", c::dim("▸"), mode.description()));
                        }
                        None => {
                            self.out(format!("  {} unknown telemetry cutover mode: {}", c::red("✗"), c::yellow(raw_mode)));
                            self.out("  available: direct_only | dual_write | worker_only | fallback_only".to_string());
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
                        self.out(format!("    {} {:<14} {}", marker, mode.as_str(), mode.description()));
                    }
                    self.out(format!("  {} examples", c::cyan("▸")));
                    self.out("    runtime cutover dual_write".to_string());
                    self.out("    runtime cutover worker_only".to_string());
                    self.out("    runtime cutover direct_only".to_string());
                }
            }
            "schema" => {
                self.out(format!("  {} Runtime v2 persistence schema", c::green("◆")));
                self.out(format!("  {} telemetry schema version: 2", c::dim("│")));
                self.out(format!("  {} batch table: mp_runtime_telemetry_batches", c::dim("│")));
                self.out(format!("  {} item table:  mp_runtime_telemetry_items", c::dim("│")));
                self.out(format!("  {} meta table:  mp_runtime_persistence_meta", c::dim("│")));
                self.out(format!("  {} policy table: mp_runtime_retention_policies", c::dim("│")));
                self.out(format!("  {} important columns", c::cyan("▸")));
                self.out("    batch_uuid, run_id, scope, pipeline, source, dual_write, schema_version, flush_reason".to_string());
                self.out("    round_uuid, room_id, player_id, item_count, payload, created_at".to_string());
                self.out(format!("  {} mode", c::cyan("▸")));
                let stats = self.state.persistence_worker.stats().await;
                self.out(format!("    production Touch/Judge cutover: {}", stats.telemetry_cutover_mode));
                self.out("    modes: direct_only | dual_write | worker_only | fallback_only".to_string());
                self.out("    read path: direct mp_round_player_data first, Runtime v2 item table fallback".to_string());
                self.out("    persist.touches / persist.judges also fall back to Runtime v2 batch table when direct batches are absent".to_string());
                self.out("    simulation: mp_sim_events + Runtime v2 simulation telemetry path".to_string());
                self.out(format!("  {} 项目仍处测试阶段，schema 和持久化路径可继续自由演进；runtime cutover <mode> 可切换", c::dim("▸")));
            }
            "persistence" => {
                let stats = self.state.persistence_worker.stats().await;
                self.out(format!("  {} Persistence Worker", c::green("◆")));
                self.out(format!("  {} capacity:  {}", c::dim("│"), stats.capacity));
                self.out(format!("  {} queued:    {}", c::dim("│"), stats.queued));
                self.out(format!("  {} processed: {}", c::dim("│"), stats.processed));
                self.out(format!("  {} pending:   {}", c::dim("│"), stats.pending));
                self.out(format!("  {} dropped:   {}", c::dim("│"), stats.dropped));
                self.out(format!("  {} mirrored:  {}", c::dim("│"), stats.mirrored_from_event_bus));
                self.out(format!("  {} skipped:   {}", c::dim("│"), stats.skipped_event_bus_events));
                self.out(format!("  {} lagged:    {}", c::dim("│"), stats.bridge_lagged));
                self.out(format!("  {} sim_db_req:{}", c::dim("│"), stats.simulation_persist_requests));
                self.out(format!("  {} prod_db_req:{}", c::dim("│"), stats.production_persist_requests));
                self.out(format!("  {} prod_skip: {}", c::dim("│"), stats.production_persist_skipped));
                self.out(format!("  {} telemetry_staged: {}", c::dim("│"), stats.production_telemetry_staged));
                self.out(format!("  {} telemetry_mode:   {} (changes={})", c::dim("│"), stats.telemetry_cutover_mode, stats.telemetry_cutover_changes));
                self.out(format!("  {} telemetry batcher", c::cyan("▸")));
                self.out(format!("    enabled={} dry_run={} cutover={} queued={} accepted={} dropped={} pending={} flushes={} flushed_items={}",
                    stats.telemetry.enabled, stats.telemetry.dry_run, stats.telemetry.cutover_mode, stats.telemetry.queued,
                    stats.telemetry.accepted, stats.telemetry.dropped, stats.telemetry.pending,
                    stats.telemetry.flushed_batches, stats.telemetry.flushed_items));
                self.out(format!("    db_write_batches={} db_write_items={} item_rows={} db_write_errors={}",
                    stats.telemetry.write_batches, stats.telemetry.write_items,
                    stats.telemetry.write_item_rows, stats.telemetry.write_errors));
                self.out(format!("    schema_v={} last_batch={} touch_items={} judge_items={}",
                    stats.telemetry.schema_version,
                    stats.telemetry.last_batch_uuid.clone().unwrap_or_else(|| "-".to_string()),
                    stats.telemetry.touch_items, stats.telemetry.judge_items));
                self.out(format!("    max_batch={} interval={}ms storage=mp_runtime_telemetry_batches + mp_runtime_telemetry_items",
                    stats.telemetry.max_items_per_batch, stats.telemetry.flush_interval_ms));
                self.out(format!("    telemetry_last_err={}", stats.telemetry.last_error.clone().unwrap_or_else(|| "-".to_string())));
                self.out(format!("  {} last_err:  {}", c::dim("│"), stats.last_error.clone().unwrap_or_else(|| "-".to_string())));
                if !stats.by_kind.is_empty() {
                    self.out(format!("  {} by kind", c::cyan("▸")));
                    for (kind, count) in stats.by_kind.iter().rev().take(16) {
                        self.out(format!("    {:<28} {}", kind, count));
                    }
                }
                if !stats.recent.is_empty() {
                    self.out(format!("  {} recent", c::cyan("▸")));
                    for event in stats.recent.iter().rev().take(12) {
                        self.out(format!("    #{:<4} {:<9} {:<24} sim={} {}", event.seq, event.action, event.kind, event.simulation, event.summary));
                    }
                }
                self.out(format!("  {} 低频生产事件已 EventBus → Worker → mp_events 双写；现有 db.rs 直接写入路径仍保持不变", c::dim("▸")));
                self.out(format!("  {} 生产 Touch/Judge cutover={}；EventBus 只保留计数观测，完整 payload 走 Session → Worker", c::dim("▸"), stats.telemetry_cutover_mode));
                self.out(format!("  {} Step 23 schema: batch header 表 + raw item 表 + persistence meta/retention policy，测试阶段可继续自由演进", c::dim("▸")));
            }
            _ => {
                self.out(format!("  {} 未知 runtime 子命令: {}", c::red("✗"), c::yellow(sub)));
                self.out(format!("  {} 可用: runtime status | roadmap | phira | commands | events | persistence | schema | cutover | actors | rooms", c::dim("▸")));
            }
        }
    }

}

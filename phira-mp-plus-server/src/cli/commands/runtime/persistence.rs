//! PersistenceWorker diagnostics.

use super::super::super::*;

impl CliHandler {
    pub(in crate::cli) async fn print_runtime_persistence(&self) {
        let stats = self.state.persistence_worker.stats().await;
        self.out(format!("  {} Persistence Worker", c::green("◆")));
        self.out(format!("  {} capacity:  {}", c::dim("│"), stats.capacity));
        self.out(format!("  {} queued:    {}", c::dim("│"), stats.queued));
        self.out(format!("  {} processed: {}", c::dim("│"), stats.processed));
        self.out(format!("  {} pending:   {}", c::dim("│"), stats.pending));
        self.out(format!(
            "  {} health:    {} (pending={}%)",
            c::dim("│"),
            stats.queue_health,
            stats.pending_ratio_percent
        ));
        self.out(format!(
            "  {} advice:    {}",
            c::dim("│"),
            stats.backpressure_advice
        ));
        self.out(format!("  {} dropped:   {}", c::dim("│"), stats.dropped));
        self.out(format!(
            "  {} sim_db_req:{}",
            c::dim("│"),
            stats.simulation_persist_requests
        ));
        self.out(format!(
            "  {} prod_db_req:{}",
            c::dim("│"),
            stats.production_persist_requests
        ));
        self.out(format!(
            "  {} prod_skip: {}",
            c::dim("│"),
            stats.production_persist_skipped
        ));
        self.out(format!(
            "  {} telemetry_staged: {}",
            c::dim("│"),
            stats.production_telemetry_staged
        ));
        self.out(format!(
            "  {} telemetry_failed: {}",
            c::dim("│"),
            stats.production_telemetry_stage_failed
        ));
        self.out(format!(
            "  {} dead_letter: path={} written={} failed_or_disabled={}",
            c::dim("│"),
            stats.dead_letter_path.as_deref().unwrap_or("disabled"),
            stats.dead_letter_written,
            stats.dead_letter_failed
        ));
        self.out(format!(
            "  {} benchmark_reports: queued={} skipped_no_db={}",
            c::dim("│"),
            stats.benchmark_report_persist_requests,
            stats.benchmark_report_persist_skipped
        ));
        self.out(format!("  {} telemetry batcher", c::cyan("▸")));
        self.out(format!(
            "    enabled={} dry_run={} queued={} accepted={} dropped={} pending={} flushes={} flushed_items={}",
            stats.telemetry.enabled, stats.telemetry.dry_run, stats.telemetry.cutover_mode, stats.telemetry.queued,
            stats.telemetry.accepted, stats.telemetry.dropped, stats.telemetry.pending,
            stats.telemetry.flushed_batches, stats.telemetry.flushed_items
        ));
        self.out(format!(
            "    db_write_batches={} db_write_items={} item_rows={} db_write_errors={}",
            stats.telemetry.write_batches,
            stats.telemetry.write_items,
            stats.telemetry.write_item_rows,
            stats.telemetry.write_errors
        ));
        self.out(format!(
            "    db_dispatch_ms(avg/max/last)={}/{}/{} samples={} db_ack_ms(avg/max/last)={}/{}/{} samples={}",
            stats.telemetry.db_dispatch_avg_ms, stats.telemetry.db_dispatch_max_ms,
            stats.telemetry.db_dispatch_last_ms, stats.telemetry.db_dispatch_samples,
            stats.telemetry.db_ack_avg_ms, stats.telemetry.db_ack_max_ms,
            stats.telemetry.db_ack_last_ms, stats.telemetry.db_ack_samples
        ));
        self.out(format!(
            "    schema_v={} last_batch={} touch_items={} judge_items={}",
            stats.telemetry.schema_version,
            stats
                .telemetry
                .last_batch_uuid
                .clone()
                .unwrap_or_else(|| "-".to_string()),
            stats.telemetry.touch_items,
            stats.telemetry.judge_items
        ));
        self.out(format!(
            "    max_batch={} interval={}ms storage=mp_runtime_telemetry_batches + mp_runtime_telemetry_items",
            stats.telemetry.max_items_per_batch, stats.telemetry.flush_interval_ms
        ));
        self.out(format!(
            "    telemetry_last_err={}",
            stats
                .telemetry
                .last_error
                .clone()
                .unwrap_or_else(|| "-".to_string())
        ));
        self.out(format!(
            "  {} last_err:  {}",
            c::dim("│"),
            stats.last_error.clone().unwrap_or_else(|| "-".to_string())
        ));
        if !stats.db_dispatch.is_empty() {
            self.out(format!("  {} db ack latency by pipeline", c::cyan("▸")));
            for (pipeline, latency) in &stats.db_dispatch {
                let failures = stats
                    .db_dispatch_failures
                    .get(pipeline)
                    .copied()
                    .unwrap_or(0);
                self.out(format!(
                    "    {:<22} samples={} avg={}ms max={}ms last={}ms failures={}",
                    pipeline, latency.samples, latency.avg_ms, latency.max_ms, latency.last_ms, failures
                ));
            }
        }
        if !stats.by_kind.is_empty() {
            self.out(format!("  {} by kind", c::cyan("▸")));
            for (kind, count) in stats.by_kind.iter().rev().take(16) {
                self.out(format!("    {:<28} {}", kind, count));
            }
        }
        if !stats.recent.is_empty() {
            self.out(format!("  {} recent", c::cyan("▸")));
            for event in stats.recent.iter().rev().take(12) {
                self.out(format!(
                    "    #{:<4} {:<9} {:<24} sim={} {}",
                    event.seq, event.action, event.kind, event.simulation, event.summary
                ));
            }
        }
        self.out(format!("  {} 低频生产事件由 PersistenceWorker 写入；数据库重试耗尽后写入本地 JSONL dead-letter，不再静默丢弃", c::dim("▸")));
        self.out(format!("  {} Touch/Judge 统一通过 TelemetryBatcher 路径写入, EventBus 只保留计数观测，完整 payload 走 Session → Worker", c::dim("▸")));
        self.out(format!(
            "  {} Touch/Judge 持久化不依赖 active monitor；active monitor 只控制实时 monitor 广播",
            c::dim("▸")
        ));
        self.out(format!("  {} BenchmarkReport 通过 PersistenceWorker 幂等写入 mp_runtime_benchmark_reports，供 CLI 内部查询读取", c::dim("▸")));
        self.out(format!("  {} PersistenceWorker 对普通事件施加有界背压；数据库重试耗尽后进入 dead-letter，dead-letter 再失败会触发 degraded", c::dim("▸")));
        self.out(format!("  {} TelemetryBatcher 的 Flush/Shutdown 返回真实数据库结果", c::dim("▸")));
    }
}

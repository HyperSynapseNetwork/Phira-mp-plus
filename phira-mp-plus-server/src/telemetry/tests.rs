
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutover_decisions_match_mode_contract() {
        let direct = TelemetryCutoverMode::DirectOnly.cutover_decision();
        assert!(!direct.enqueue_worker);
        assert!(direct.should_write_direct_after_worker_enqueue(false));
        assert!(direct.should_write_direct_after_worker_enqueue(true));

        let worker = TelemetryCutoverMode::WorkerPreferred.cutover_decision();
        assert!(worker.enqueue_worker);
        assert!(worker.should_write_direct_after_worker_enqueue(false));
        assert!(worker.should_write_direct_after_worker_enqueue(true));
    }

    #[test]
    fn cutover_helpers_delegate_to_decision_contract() {
        for &mode in TelemetryCutoverMode::variants() {
            let decision = mode.cutover_decision();
            assert_eq!(mode.should_enqueue_worker(), decision.enqueue_worker);
            assert_eq!(
                mode.should_write_direct(),
                decision.write_direct_before_worker_result
            );
        }
    }

    #[test]
    fn db_dispatch_latency_uses_constant_size_aggregates() {
        let mut stats = TelemetryBatcherStats::default();
        record_db_dispatch_latency(&mut stats, 3);
        record_db_dispatch_latency(&mut stats, 7);
        assert_eq!(stats.db_dispatch_samples, 2);
        assert_eq!(stats.db_dispatch_avg_ms, 5);
        assert_eq!(stats.db_dispatch_max_ms, 7);
        assert_eq!(stats.db_dispatch_last_ms, 7);
    }
}

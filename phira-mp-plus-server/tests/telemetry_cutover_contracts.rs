//! Telemetry cutover safety contracts.
//!
//! `direct_only` remains the safe default. `worker_preferred` attempts direct
//! first, mirrors acknowledged direct writes and lets an accepted Worker event
//! compensate a direct failure. `worker_authoritative` is explicit opt-in and
//! writes direct only when Worker enqueue is known to have been rejected.

use phira_mp_plus_server::telemetry::TelemetryCutoverMode;

#[test]
fn default_cutover_mode_is_safe() {
    let mode = TelemetryCutoverMode::default();
    assert_eq!(
        mode,
        TelemetryCutoverMode::DirectOnly,
        "default must be DirectOnly to prevent data loss"
    );
}

#[test]
fn worker_preferred_is_not_default() {
    assert_ne!(
        TelemetryCutoverMode::default(),
        TelemetryCutoverMode::WorkerPreferred,
        "WorkerPreferred must not be the default"
    );
}

#[test]
fn cutover_mode_as_str_round_trip() {
    for mode in TelemetryCutoverMode::variants() {
        let s = mode.as_str();
        let parsed = TelemetryCutoverMode::parse(s);
        assert_eq!(parsed, Some(*mode), "parse round-trip failed for {s}");
    }
}

#[test]
fn cutover_mode_description_is_non_empty() {
    for mode in TelemetryCutoverMode::variants() {
        let desc = mode.description();
        assert!(
            !desc.is_empty(),
            "description for {mode:?} should not be empty"
        );
    }
}

#[test]
fn direct_only_writes_direct_not_worker() {
    let mode = TelemetryCutoverMode::DirectOnly;
    assert!(mode.should_write_direct());
    assert!(!mode.should_enqueue_worker());
}

#[test]
fn worker_preferred_writes_direct_and_enqueues_worker() {
    let mode = TelemetryCutoverMode::WorkerPreferred;
    assert!(
        mode.should_write_direct(),
        "WorkerPreferred must always write direct as safety path"
    );
    assert!(
        mode.should_enqueue_worker(),
        "WorkerPreferred must enqueue Worker for mirror or canonical compensation"
    );
}

#[test]
fn worker_authoritative_is_single_writer_after_acceptance() {
    let mode = TelemetryCutoverMode::WorkerAuthoritative;
    let decision = mode.cutover_decision();
    assert!(decision.enqueue_worker);
    assert!(!decision.write_direct_before_worker_result);
    assert!(!decision.should_write_direct_after_worker_enqueue(true));
    assert!(decision.should_write_direct_after_worker_enqueue(false));
}

#[test]
fn cutover_decision_contract() {
    for mode in TelemetryCutoverMode::variants() {
        let decision = mode.cutover_decision();
        assert_eq!(decision.mode, *mode);
        assert_eq!(decision.enqueue_worker, mode.should_enqueue_worker());
        assert_eq!(
            decision.write_direct_before_worker_result,
            mode.should_write_direct()
        );
    }
}

#[test]
fn variants_cover_all_modes() {
    let variants = TelemetryCutoverMode::variants();
    assert_eq!(variants.len(), 3, "should have exactly 3 modes");
    assert!(variants.contains(&TelemetryCutoverMode::DirectOnly));
    assert!(variants.contains(&TelemetryCutoverMode::WorkerPreferred));
    assert!(variants.contains(&TelemetryCutoverMode::WorkerAuthoritative));
}

#[test]
fn only_supported_mode_names_are_exposed() {
    let variants = TelemetryCutoverMode::variants();
    for v in variants {
        assert!(
            v.as_str() == "direct_only"
                || v.as_str() == "worker_preferred"
                || v.as_str() == "worker_authoritative",
            "unexpected mode: {}",
            v.as_str()
        );
    }
}

#[test]
fn worker_preferred_parse_rejects_legacy_names() {
    // Legacy config names are no longer accepted
    assert_eq!(
        TelemetryCutoverMode::parse("worker_only"),
        None,
        "parse('worker_only') should be rejected"
    );
    assert_eq!(
        TelemetryCutoverMode::parse("worker"),
        None,
        "parse('worker') should be rejected"
    );
}

#[test]
fn worker_preferred_description_matches_compensation_contract() {
    let mode = TelemetryCutoverMode::WorkerPreferred;
    let desc = mode.description();
    assert!(
        desc.contains("direct"),
        "WorkerPreferred description must mention the direct path: {desc}"
    );
    assert!(
        desc.contains("mirror"),
        "WorkerPreferred description must mention mirror: {desc}"
    );
    assert!(
        desc.contains("compensat"),
        "WorkerPreferred description must mention direct-failure compensation: {desc}"
    );
}

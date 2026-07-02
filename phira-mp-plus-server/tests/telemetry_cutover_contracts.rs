//! Telemetry cutover safety contracts.
//!
//! These tests verify that the default telemetry cutover mode is safe
//! (won't silently drop Touches/Judges), that WorkerPreferred requires
//! explicit opt-in, and that parse/display are consistent.
//!
//! WorkerPreferred semantics: direct RoundStore/db.rs (authoritative) +
//! best-effort enqueue to Runtime v2 worker as async mirror / batch
//! observation. Worker failure never blocks data persistence.

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
        "WorkerPreferred must enqueue worker as batch mirror"
    );
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
fn variants_cover_both_modes() {
    let variants = TelemetryCutoverMode::variants();
    assert_eq!(variants.len(), 2, "should have exactly 2 modes");
    assert!(variants.contains(&TelemetryCutoverMode::DirectOnly));
    assert!(variants.contains(&TelemetryCutoverMode::WorkerPreferred));
}

#[test]
fn no_dual_write_or_fallback_in_simplified_modes() {
    let variants = TelemetryCutoverMode::variants();
    for v in variants {
        assert!(
            v.as_str() == "direct_only" || v.as_str() == "worker_preferred",
            "unexpected mode: {}",
            v.as_str()
        );
    }
}

#[test]
fn worker_preferred_parse_accepts_legacy_names() {
    // Old config names should still parse to WorkerPreferred for backward compatibility
    assert_eq!(
        TelemetryCutoverMode::parse("worker_only"),
        Some(TelemetryCutoverMode::WorkerPreferred),
        "parse('worker_only') should return WorkerPreferred"
    );
    assert_eq!(
        TelemetryCutoverMode::parse("worker"),
        Some(TelemetryCutoverMode::WorkerPreferred)
    );
}

#[test]
fn worker_preferred_decision_matches_description() {
    let mode = TelemetryCutoverMode::WorkerPreferred;
    let desc = mode.description();
    assert!(
        desc.contains("direct write"),
        "WorkerPreferred description must mention direct write: {desc}"
    );
    assert!(
        desc.contains("mirror"),
        "WorkerPreferred description must mention mirror: {desc}"
    );
}

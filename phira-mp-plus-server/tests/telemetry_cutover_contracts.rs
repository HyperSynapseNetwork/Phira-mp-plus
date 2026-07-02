//! Telemetry cutover safety contracts.
//!
//! These tests verify that the default telemetry cutover mode is safe
//! (won't silently drop Touches/Judges), that WorkerOnly requires
//! explicit opt-in, and that parse/display are consistent.

use phira_mp_plus_server::telemetry::TelemetryCutoverMode;

#[test]
fn default_cutover_mode_is_safe() {
    let mode = TelemetryCutoverMode::default();
    assert_eq!(mode, TelemetryCutoverMode::DirectOnly,
        "default must be DirectOnly to prevent data loss");
}

#[test]
fn worker_only_is_not_default() {
    assert_ne!(TelemetryCutoverMode::default(), TelemetryCutoverMode::WorkerOnly,
        "WorkerOnly must not be the default");
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
        assert!(!desc.is_empty(), "description for {mode:?} should not be empty");
    }
}

#[test]
fn direct_only_should_write_direct() {
    let mode = TelemetryCutoverMode::DirectOnly;
    assert!(mode.should_write_direct());
    assert!(!mode.should_enqueue_worker());
}

#[test]
fn worker_only_should_not_write_direct() {
    let mode = TelemetryCutoverMode::WorkerOnly;
    assert!(!mode.should_write_direct());
    assert!(mode.should_enqueue_worker());
}

#[test]
fn cutover_decision_contract() {
    for mode in TelemetryCutoverMode::variants() {
        let decision = mode.cutover_decision();
        assert_eq!(decision.mode, *mode);
        assert_eq!(decision.enqueue_worker, mode.should_enqueue_worker());
    }
}

#[test]
fn variants_cover_both_modes() {
    let variants = TelemetryCutoverMode::variants();
    assert_eq!(variants.len(), 2, "should have exactly 2 modes");
    assert!(variants.contains(&TelemetryCutoverMode::DirectOnly));
    assert!(variants.contains(&TelemetryCutoverMode::WorkerOnly));
}

#[test]
fn no_dual_write_or_fallback_in_simplified_modes() {
    // These modes were removed in the simplification; ensure they don't exist
    let variants = TelemetryCutoverMode::variants();
    for v in variants {
        assert!(v.as_str() == "direct_only" || v.as_str() == "worker_only",
            "unexpected mode: {}", v.as_str());
    }
}

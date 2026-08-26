//! T029 — ComplexityClassifier integration test.
//!
//! Verify tier classification: Economical for "write a JUnit test", Frontier
//! for "refactor to Streams + fix @Transactional boundary", AmbiguousDefault
//! for ambiguous input, the pin/unpin override, and scope fan-out.

use std::path::PathBuf;

use joey_neurocode::classifier::{ComplexityClassifier, ComplexityTier};
use joey_neurocode::engine::CodingRequest;

fn make_request(text: &str) -> CodingRequest {
    CodingRequest {
        text: text.into(),
        active_file: None,
        active_symbols: vec![],
        project_root: PathBuf::from("."),
        token_budget_hint: 0,
    }
}

fn make_request_with_symbols(text: &str, symbols: &[&str]) -> CodingRequest {
    CodingRequest {
        text: text.into(),
        active_file: None,
        active_symbols: symbols.iter().map(|s| s.to_string()).collect(),
        project_root: PathBuf::from("."),
        token_budget_hint: 0,
    }
}

#[test]
fn junit_test_is_economical() {
    let clf = ComplexityClassifier::default();
    let route = clf.classify(&make_request("Write a JUnit test for UserServiceImpl.findById"));
    assert_eq!(
        route.tier,
        ComplexityTier::Economical,
        "tier: {}, reasoning: {}",
        route.tier,
        route.reasoning
    );
    assert!(!route.overridden);
    assert!(route.override_tier.is_none());
}

#[test]
fn refactor_streams_transactional_is_frontier() {
    let clf = ComplexityClassifier::default();
    let route = clf.classify(
        &make_request("Refactor to Streams + fix @Transactional boundary"),
    );
    assert_eq!(
        route.tier,
        ComplexityTier::Frontier,
        "tier: {}, reasoning: {}",
        route.tier,
        route.reasoning
    );
    assert!(!route.overridden);
}

#[test]
fn ambiguous_input_is_ambiguous_default() {
    let clf = ComplexityClassifier::default();
    let route = clf.classify(&make_request("Help me with this code"));
    assert_eq!(route.tier, ComplexityTier::AmbiguousDefault);
    assert!(!route.overridden);
}

#[test]
fn completely_empty_text_is_ambiguous() {
    let clf = ComplexityClassifier::default();
    let route = clf.classify(&make_request(""));
    assert_eq!(route.tier, ComplexityTier::AmbiguousDefault);
}

#[test]
fn pin_overrides_classification() {
    let clf = ComplexityClassifier::default();

    // Before pin: "write a test" → Economical.
    let before = clf.classify(&make_request("write a JUnit test"));
    assert_eq!(before.tier, ComplexityTier::Economical);

    // Pin to Frontier.
    clf.pin_tier(ComplexityTier::Frontier);
    assert_eq!(clf.pinned_tier(), Some(ComplexityTier::Frontier));

    let pinned = clf.classify(&make_request("write a JUnit test"));
    assert_eq!(pinned.tier, ComplexityTier::Frontier);
    assert!(pinned.overridden, "route should be marked overridden");
    assert_eq!(pinned.override_tier, Some(ComplexityTier::Frontier));
    assert!(
        pinned.signals.is_empty(),
        "pinned route should have no signals"
    );

    // Unpin → back to automatic.
    clf.unpin_tier();
    assert!(clf.pinned_tier().is_none());
    let after = clf.classify(&make_request("write a JUnit test"));
    assert_eq!(after.tier, ComplexityTier::Economical);
    assert!(!after.overridden);
}

#[test]
fn pin_economical_overrides_frontier_request() {
    let clf = ComplexityClassifier::default();
    clf.pin_tier(ComplexityTier::Economical);

    // Normally this would be Frontier.
    let route = clf.classify(&make_request("refactor architecture concurrency redesign"));
    assert_eq!(route.tier, ComplexityTier::Economical);
    assert!(route.overridden);

    clf.unpin_tier();
}

#[test]
fn scope_fan_out_leans_frontier() {
    let clf = ComplexityClassifier::default();
    // 6 symbols > threshold (4), no economical keyword to cancel it out.
    let route = clf.classify(&make_request_with_symbols(
        "update these methods",
        &["UserService", "UserRepository", "AuditLogger", "Config", "Auth", "Session"],
    ));
    assert_eq!(
        route.tier,
        ComplexityTier::Frontier,
        "6 symbols > threshold 4 → Frontier"
    );

    // There should be a ScopeFanOut signal.
    assert!(
        route
            .signals
            .iter()
            .any(|s| matches!(s.kind, joey_neurocode::classifier::SignalKind::ScopeFanOut)),
        "expected a ScopeFanOut signal, got: {:?}",
        route.signals
    );
}

#[test]
fn below_scope_threshold_is_not_frontier_from_scope() {
    let clf = ComplexityClassifier::default();
    // 3 symbols ≤ threshold (4) and no frontier keyword.
    let route = clf.classify(&make_request_with_symbols(
        "update these",
        &["A", "B", "C"],
    ));
    assert_eq!(route.tier, ComplexityTier::AmbiguousDefault);
}

#[test]
fn ambiguous_default_resolves_to_economical() {
    // AmbiguousDefault.resolve_ambiguous(Economical) == Economical.
    assert_eq!(
        ComplexityTier::AmbiguousDefault.resolve_ambiguous(ComplexityTier::Economical),
        ComplexityTier::Economical
    );
    // But can resolve to Frontier if configured.
    assert_eq!(
        ComplexityTier::AmbiguousDefault.resolve_ambiguous(ComplexityTier::Frontier),
        ComplexityTier::Frontier
    );
    // A concrete tier resolves to itself.
    assert_eq!(
        ComplexityTier::Frontier.resolve_ambiguous(ComplexityTier::Economical),
        ComplexityTier::Frontier
    );
}

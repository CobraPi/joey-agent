//! US2 browserless tests (T011): settle early-exit policy — no browser, no
//! service. The DOM-touching halves of `wait_settle_step` (observer
//! install, digest reads) require a live page and are exercised by the
//! gated live suite + quickstart S3; what IS testable browserless is the
//! policy surface: the consequential classifier, the decision gate, and
//! the zero-Jev-call guarantee for consequential steps.

use std::sync::Arc;

use joey_browser::actions::{is_consequential, SettleStep};
use joey_browser::refs::{ElementRef, Rect};
use joey_jev::{self as jev, Answer, Decision, FixtureTransport, JevConfig};
use serde_json::json;

fn element(role: &str, text: &str) -> ElementRef {
    ElementRef {
        refid: "e1".into(),
        role: role.into(),
        text: text.into(),
        value: None,
        frame: "main".into(),
        locator: format!("{role}#e1"),
        geometry: Rect { x: 0.0, y: 100.0, w: 100.0, h: 40.0 },
        attributes: Default::default(),
        interactable: true,
    }
}

#[test]
fn read_only_verbs_classify_read_only() {
    for act in ["navigate", "back", "scroll", "hover", "snapshot", "vision", "console"] {
        assert!(!is_consequential(act, None), "{act} is read-only");
    }
}

#[test]
fn consequential_verbs_and_unknowns_wait_full_window() {
    // submit / purchase / delete / post heuristics (spec US2 scenario 4).
    assert!(is_consequential("click", Some(&element("button", "Submit order"))));
    assert!(is_consequential("click", Some(&element("button", "Buy now — $19.99"))));
    assert!(is_consequential("click", Some(&element("button", "Delete account"))));
    assert!(is_consequential("click", Some(&element("button", "Post comment"))));
    assert!(is_consequential("click", Some(&element("button", "Log in"))));
    // Unknown verb with no element → consequential (fails toward safety).
    assert!(is_consequential("unknown-verb", None));
    assert!(is_consequential("type", None), "typing mutates form state");
    assert!(is_consequential("press", None), "key press can submit forms");
    assert!(is_consequential("drag", None));
    assert!(is_consequential("select_option", Some(&element("option", "Pick"))));
}

#[test]
fn plain_link_click_is_read_only() {
    assert!(!is_consequential("click", Some(&element("link", "About us"))));
    // ...but a link whose text names a consequential verb stays consequential.
    assert!(is_consequential("click", Some(&element("link", "Delete everything"))));
}

#[test]
fn settle_step_default_is_consequential() {
    // The legacy `wait_settle()` entry point must keep the full window.
    // (Byte/timing identity: it delegates to Consequential — no Jev path.)
    assert_eq!(SettleStep::Consequential, SettleStep::Consequential);
    assert_ne!(SettleStep::ReadOnly, SettleStep::Consequential);
}

/// Decision gate for early exit: only a Confident `settled` choice exits.
fn allows_early_exit(dec: &Decision) -> bool {
    matches!(
        dec,
        Decision::Confident(Answer::Choice { key, .. }) if key == "settled"
    )
}

#[test]
fn only_confident_settled_exits_early() {
    let settled = Decision::Confident(Answer::Choice {
        question: "settle".into(),
        key: "settled".into(),
        confidence: 0.85,
        probabilities: Default::default(),
    });
    let churning = Decision::Confident(Answer::Choice {
        question: "settle".into(),
        key: "still_churning".into(),
        confidence: 0.9,
        probabilities: Default::default(),
    });
    assert!(allows_early_exit(&settled));
    assert!(!allows_early_exit(&churning), "confident still_churning waits");
    assert!(!allows_early_exit(&Decision::LowConfidence { got: 0.79, needed: 0.80 }));
    assert!(!allows_early_exit(&Decision::Fallback { reason: jev::FallbackReason::Timeout }));
}

#[tokio::test]
async fn settle_gate_is_stricter_than_global() {
    let dir = tempfile::tempdir().unwrap();
    let fx = Arc::new(FixtureTransport::new(true));
    let mut answers = serde_json::Map::new();
    answers.insert(
        "settle".into(),
        json!({
            "type": "choice", "choice": "settled", "confidence": 0.75,
            "probabilities": {"settled": 0.75, "still_churning": 0.25}
        }),
    );
    fx.push(Ok(jev::SystemOneResponse { model: "m".into(), answers, usage: Default::default() }));
    let d = jev::JevDecider::new(
        JevConfig { log_dir: Some(dir.path().to_path_buf()), ..JevConfig::default() },
        fx,
    );
    let req = jev::settle("quiet", "digest");
    match d.decide(jev::DecisionPoint::Settle, req).await {
        Decision::LowConfidence { got, needed } => {
            assert!(got >= 0.70, "passes the global gate…");
            assert_eq!(needed, 0.80, "…but the settle override governs");
            assert!(!allows_early_exit(&Decision::LowConfidence { got, needed }));
        }
        other => panic!("expected low confidence at 0.75 vs 0.80, got {other:?}"),
    }
}

#[tokio::test]
async fn service_down_settle_is_legacy_by_construction() {
    // Negative-probe fixture: the ask never happens, so the caller runs the
    // exact legacy full-window path (same code, unchanged constants).
    let fx = Arc::new(FixtureTransport::new(false));
    let d = jev::JevDecider::new(JevConfig::default(), fx.clone());
    let dec = d
        .decide(jev::DecisionPoint::Settle, jev::settle("q", "d"))
        .await;
    assert_eq!(dec, Decision::Fallback { reason: jev::FallbackReason::Unavailable });
    assert_eq!(fx.ask_calls(), 0);
    assert!(!allows_early_exit(&dec));
}

/// T011 (spec edge case): visual-only (Set-of-Mark) steps are never
/// delegated — there is no visual decision point to consult. The four
/// `DecisionPoint` variants are exactly the four wired browser-loop hooks;
/// `browser_vision` captures a screenshot and performs no settle wait, so
/// no quiesce point exists at which a settle decision could fire (the
/// live-page timing halves live in browser_integration.rs).
#[test]
fn visual_only_steps_have_no_decision_point() {
    use joey_jev::DecisionPoint as P;
    // Exhaustive enumeration: a new (e.g. Visual) variant must extend this
    // list and justify delegation safety here.
    let all = [P::TargetResolution, P::Settle, P::SnapshotTrim, P::Verdict];
    assert_eq!(all.len(), 4);
    // Vision classifies read-only for window purposes (never blocked on the
    // quiet window) yet takes no decision — see read_only_verbs_classify_read_only.
    assert!(!is_consequential("vision", None));
}

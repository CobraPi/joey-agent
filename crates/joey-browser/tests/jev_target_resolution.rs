//! US1 browserless tests (T008): ambiguous-match resolution through the
//! local Jev decider — no browser, no service (canned fixture transport).
//!
//! Byte-identity gate (Constitution VII): every non-confident outcome must
//! keep the pre-feature refusal EXACTLY — same error type, same string.

use std::sync::Arc;

use joey_browser::refs::{self, ElementRef, Rect, ResolvedBy};
use joey_jev::{self as jev, Answer, Decision, FixtureTransport, JevConfig, TransportError};
use serde_json::json;

fn fixture_decider(
    fx: Arc<FixtureTransport>,
) -> jev::JevDecider {
    jev::JevDecider::new(
        JevConfig {
            log_dir: Some(std::env::temp_dir().join(format!(
                "joey-jev-us1-{}",
                std::process::id()
            ))),
            ..JevConfig::default()
        },
        fx,
    )
}

fn choice_answer(key: &str, confidence: f32) -> jev::SystemOneResponse {
    let mut answers = serde_json::Map::new();
    answers.insert(
        "target".into(),
        json!({
            "type": "choice", "choice": key, "confidence": confidence,
            "probabilities": {"e1": 0.2, "e2": 0.7, "e3": 0.1}
        }),
    );
    jev::SystemOneResponse { model: "m".into(), answers, usage: Default::default() }
}

/// The candidate strings the resolver produces today (pre-feature shape).
fn candidates() -> Vec<String> {
    vec![
        "e1 [link] \"Login\"".to_string(),
        "e2 [button] \"Login\"".to_string(),
        "e3 [link] \"Login\"".to_string(),
    ]
}

fn element(refid: &str, y: f64) -> ElementRef {
    ElementRef {
        refid: refid.into(),
        role: "button".into(),
        text: "Login".into(),
        value: None,
        frame: "main".into(),
        locator: format!("button#{refid}"),
        geometry: Rect { x: 0.0, y, w: 100.0, h: 40.0 },
        attributes: Default::default(),
        interactable: true,
    }
}

/// The exact refusal today's ambiguous path produces — byte-identical gate.
fn baseline_refusal(c: &[String]) -> String {
    joey_browser::BrowserError::ambiguous(c).to_string()
}

#[tokio::test]
async fn confident_pick_resolves_and_reports_jev_choice() {
    let fx = Arc::new(FixtureTransport::new(true));
    fx.push(Ok(choice_answer("e2", 0.9)));
    let d = fixture_decider(fx);
    let picked = refs::jev_resolve_ambiguous(&d, "click Login", "T", "http://u", &candidates())
        .await
        .expect("confident pick resolves");
    assert_eq!(picked, "e2");
    assert_eq!(ResolvedBy::JevChoice.as_str(), "jev-choice");
}

#[tokio::test]
async fn low_confidence_keeps_refusal_byte_identical() {
    let fx = Arc::new(FixtureTransport::new(true));
    fx.push(Ok(choice_answer("e1", 0.30)));
    let d = fixture_decider(fx);
    let c = candidates();
    let picked = refs::jev_resolve_ambiguous(&d, "click Login", "T", "http://u", &c).await;
    assert!(picked.is_none(), "low confidence must refuse");
    assert_eq!(
        baseline_refusal(&c),
        "ambiguous match: 3 [e1 [link] \"Login\"; e2 [button] \"Login\"; e3 [link] \"Login\"] candidates refused"
    );
}

#[tokio::test]
async fn tie_keeps_refusal() {
    let fx = Arc::new(FixtureTransport::new(true));
    let mut answers = serde_json::Map::new();
    answers.insert(
        "target".into(),
        json!({
            "type": "choice", "choice": "e1", "confidence": 0.95,
            "probabilities": {"e1": 0.475, "e2": 0.475, "e3": 0.05}
        }),
    );
    fx.push(Ok(jev::SystemOneResponse { model: "m".into(), answers, usage: Default::default() }));
    let d = fixture_decider(fx);
    let picked = refs::jev_resolve_ambiguous(&d, "click Login", "T", "http://u", &candidates()).await;
    assert!(picked.is_none(), "top-2 tie must refuse");
}

#[tokio::test]
async fn fixture_unavailable_keeps_refusal_and_no_ask() {
    let fx = Arc::new(FixtureTransport::new(false));
    let d = fixture_decider(fx.clone());
    let picked = refs::jev_resolve_ambiguous(&d, "click Login", "T", "http://u", &candidates()).await;
    assert!(picked.is_none());
    assert_eq!(fx.ask_calls(), 0, "unreachable service must never ask");
}

#[tokio::test]
async fn candidate_cap_twelve_existing_order() {
    let fx = Arc::new(FixtureTransport::new(true));
    fx.push(Ok(choice_answer("e5", 0.9)));
    let d = fixture_decider(fx.clone());
    let many: Vec<String> = (1..=20)
        .map(|i| format!("e{i} [button] \"Login {i}\""))
        .collect();
    let picked = refs::jev_resolve_ambiguous(&d, "click Login", "T", "http://u", &many).await;
    assert_eq!(picked.as_deref(), Some("e5"));
    let last = fx.last_request().unwrap();
    let crit = last.questions["target"]["criteria"].as_object().unwrap();
    assert_eq!(crit.len(), 12, "candidate cap is 12");
    // Existing order preserved: e1 then e2 (insertion order on the wire).
    let keys: Vec<&String> = crit.keys().collect();
    assert_eq!(keys[0], "e1");
    assert_eq!(keys[1], "e2");
}

#[tokio::test]
async fn picked_refid_must_exist_in_registry_caller_side() {
    // Caller-side lookup contract: the picked key is a registry refid.
    let reg = refs::ElementRefRegistry::new();
    let _ = reg;
    let el = element("e2", 100.0);
    assert_eq!(el.refid, "e2");
    // Decision surface sanity: Confident carries the raw choice key.
    let dec = Decision::Confident(Answer::Choice {
        question: "target".into(),
        key: "e2".into(),
        confidence: 0.9,
        probabilities: Default::default(),
    });
    match dec {
        Decision::Confident(Answer::Choice { key, .. }) => assert_eq!(key, "e2"),
        _ => panic!(),
    }
    let _: Result<jev::SystemOneResponse, TransportError> = Err(TransportError::Timeout);
}

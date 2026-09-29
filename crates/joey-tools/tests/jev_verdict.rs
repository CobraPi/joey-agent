//! US4 browserless tests (T016): verdict injection on mutating action
//! results — no browser, no service. The verdict mapping is pure
//! (`verdict_value`); the DOM halves (pre/post digests) are exercised by
//! the live suite + quickstart S5.
//!
//! Cross-cutting invariants covered here:
//! - Fallback → NO `verdict` key (body byte-identical to pre-feature).
//! - Verdict values are additive JSON objects — never instructions; they
//!   ride tool output already wrapped `<untrusted_tool_result>` upstream.
//! - Decision `state` strings are redacted/length-capped before transport
//!   (joey-jev `clean`); asserted on the wire request here.

use std::sync::Arc;

use joey_jev::{self as jev, Answer, Decision, FixtureTransport, JevConfig};
use joey_tools::tools::browser_tools::verdict_value;
use serde_json::json;

fn confident(key: &str, conf: f32) -> Decision {
    Decision::Confident(Answer::Choice {
        question: "verdict".into(),
        key: key.into(),
        confidence: conf,
        probabilities: Default::default(),
    })
}

#[test]
fn confident_verdict_is_additive_key() {
    let v = verdict_value(confident("achieved", 0.9)).unwrap();
    assert_eq!(v["outcome"], "achieved");
    assert_eq!(v["confidence"], 0.9);

    let v = verdict_value(confident("not_achieved", 0.82)).unwrap();
    assert_eq!(v["outcome"], "not_achieved");
}

#[test]
fn below_threshold_marks_inconclusive() {
    let v = verdict_value(Decision::LowConfidence { got: 0.55, needed: 0.70 }).unwrap();
    assert_eq!(v["outcome"], "inconclusive");
    assert_eq!(v["confidence"], 0.55);
}

#[test]
fn fallback_has_no_verdict_key_byte_identical() {
    // Fallback → None → the tool body stays exactly the pre-feature JSON:
    // { "ok": true, "resolved_by": ..., "detail": ... }
    assert!(verdict_value(Decision::Fallback { reason: jev::FallbackReason::Unavailable }).is_none());
    assert!(verdict_value(Decision::Fallback { reason: jev::FallbackReason::Timeout }).is_none());
    assert!(verdict_value(Decision::Fallback { reason: jev::FallbackReason::Disabled }).is_none());

    // Byte-identity: injecting None leaves the body untouched.
    let mut body = json!({ "ok": true, "resolved_by": "text", "detail": "clicked \"Login\"" });
    let before = serde_json::to_string(&body).unwrap();
    if let Some(v) = verdict_value(Decision::Fallback { reason: jev::FallbackReason::WireError }) {
        body["verdict"] = v;
    }
    assert_eq!(serde_json::to_string(&body).unwrap(), before);
}

#[test]
fn verdict_is_value_object_never_instruction_surface() {
    // The injected value is a plain object with exactly outcome+confidence —
    // nothing derived from page text reaches the model as prose.
    let v = verdict_value(confident("achieved", 0.9)).unwrap();
    let obj = v.as_object().unwrap();
    assert_eq!(obj.len(), 2);
    assert!(obj.contains_key("outcome") && obj.contains_key("confidence"));
}

#[tokio::test]
async fn state_strings_are_capped_and_redacted_before_transport() {
    let dir = tempfile::tempdir().unwrap();
    let fx = Arc::new(FixtureTransport::new(true));
    let mut answers = serde_json::Map::new();
    answers.insert(
        "verdict".into(),
        json!({
            "type": "choice", "choice": "achieved", "confidence": 0.9,
            "probabilities": {"achieved": 0.9, "not_achieved": 0.1}
        }),
    );
    fx.push(Ok(jev::SystemOneResponse { model: "m".into(), answers, usage: Default::default() }));
    let d = jev::JevDecider::new(
        JevConfig { log_dir: Some(dir.path().to_path_buf()), ..JevConfig::default() },
        fx.clone(),
    );
    let long_pre: String = "a sk-abcdefghijklmnopqrstuvwxy ".repeat(40);
    let req = jev::verdict("click", &long_pre, "title=T url=http://u");
    let dec = d.decide(jev::DecisionPoint::Verdict, req).await;
    assert!(matches!(dec, Decision::Confident(_)));
    let sent = fx.last_request().unwrap();
    let pre = sent.state["pre_digest"].as_str().unwrap();
    assert!(pre.chars().count() <= 200, "length-capped");
    assert!(pre.contains("«"), "secret-shaped substrings masked");
}

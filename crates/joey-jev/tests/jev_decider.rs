//! joey-jev unit/contract tests (T007): confidence gating, tie refusal,
//! timeout/422 fallbacks, disabled-decider, log-row completeness, and
//! re-probe hysteresis — all against the canned fixture transport with NO
//! service running (Constitution IV).
//!
//! By-construction guarantees exercised here:
//! - Judgment-only question kinds (`choice`/`score`) per FR-006 — every
//!   builder emits exactly those kinds.
//! - The decider never selects models per FR-012 — `model` comes solely
//!   from `JevConfig` and is echoed verbatim on the wire.

use std::sync::Arc;

use joey_jev::{
    self as jev, Answer, Decision, DecisionPoint, FallbackReason, FixtureTransport, JevConfig,
    Question, QuestionKind, TransportError,
};
use serde_json::json;

fn config() -> JevConfig {
    JevConfig {
        log_dir: Some(std::env::temp_dir().join(format!("joey-jev-test-{}", std::process::id()))),
        ..JevConfig::default()
    }
}

fn choice_response(key: &str, confidence: f32, probs: serde_json::Value) -> jev::SystemOneResponse {
    let mut answers = serde_json::Map::new();
    answers.insert(
        "target".into(),
        json!({
            "type": "choice",
            "choice": key,
            "confidence": confidence,
            "probabilities": probs
        }),
    );
    jev::SystemOneResponse {
        model: "laya-mlx".into(),
        answers,
        usage: Default::default(),
    }
}

fn target_req() -> jev::DecisionRequest {
    jev::target_resolution(
        "click Login",
        "Page",
        "http://x",
        &[
            ("e1".into(), "Login header link".into()),
            ("e2".into(), "Login button".into()),
            ("e3".into(), "Login footer link".into()),
        ],
    )
}

async fn decider_with(cfg: JevConfig, fx: Arc<FixtureTransport>) -> jev::JevDecider {
    jev::JevDecider::new(cfg, fx)
}

#[tokio::test]
async fn confident_pick_passes_global_gate() {
    let fx = Arc::new(FixtureTransport::new(true));
    fx.push(Ok(choice_response(
        "e2",
        0.92,
        json!({"e1": 0.05, "e2": 0.92, "e3": 0.03}),
    )));
    let d = decider_with(config(), fx.clone()).await;
    let dec = d.decide(DecisionPoint::TargetResolution, target_req()).await;
    match dec {
        Decision::Confident(Answer::Choice { key, confidence, .. }) => {
            assert_eq!(key, "e2");
            assert!(confidence >= 0.70);
        }
        other => panic!("expected confident, got {other:?}"),
    }
    assert_eq!(fx.probe_calls(), 1, "first decision probes once");
    assert_eq!(fx.ask_calls(), 1);
}

#[tokio::test]
async fn low_confidence_falls_back_to_caller() {
    let fx = Arc::new(FixtureTransport::new(true));
    fx.push(Ok(choice_response(
        "e1",
        0.40,
        json!({"e1": 0.40, "e2": 0.35, "e3": 0.25}),
    )));
    let d = decider_with(config(), fx).await;
    let dec = d.decide(DecisionPoint::TargetResolution, target_req()).await;
    match dec {
        Decision::LowConfidence { got, needed } => {
            assert!(got < needed);
        }
        other => panic!("expected low confidence, got {other:?}"),
    }
}

#[tokio::test]
async fn top2_tie_refuses_even_at_high_confidence() {
    let fx = Arc::new(FixtureTransport::new(true));
    fx.push(Ok(choice_response(
        "e1",
        0.95,
        json!({"e1": 0.475, "e2": 0.475, "e3": 0.05}),
    )));
    let d = decider_with(config(), fx).await;
    let dec = d.decide(DecisionPoint::TargetResolution, target_req()).await;
    match dec {
        Decision::LowConfidence { got, needed } => {
            assert!(got >= needed, "tie must refuse independent of confidence");
        }
        other => panic!("expected low confidence on tie, got {other:?}"),
    }
}

#[tokio::test]
async fn timeout_falls_back() {
    let fx = Arc::new(FixtureTransport::new(true));
    fx.push(Err(TransportError::Timeout));
    let d = decider_with(config(), fx).await;
    let dec = d.decide(DecisionPoint::TargetResolution, target_req()).await;
    assert_eq!(
        dec,
        Decision::Fallback { reason: FallbackReason::Timeout }
    );
}

#[tokio::test]
async fn oversize_422_shrinks_once_then_falls_back() {
    let fx = Arc::new(FixtureTransport::new(true));
    fx.push(Err(TransportError::Rejected422 { detail: "oversize".into() }));
    fx.push(Ok(choice_response(
        "e2",
        0.9,
        json!({"e1": 0.1, "e2": 0.9}),
    )));
    let d = decider_with(config(), fx.clone()).await;
    let dec = d.decide(DecisionPoint::TargetResolution, target_req()).await;
    match &dec {
        Decision::Confident(Answer::Choice { key, .. }) => assert_eq!(key, "e2"),
        other => panic!("expected confident after shrink retry, got {other:?}"),
    }
    assert_eq!(fx.ask_calls(), 2, "422 → exactly one shrink retry");

    // Shrink actually halved the candidates on the retry wire.
    let last = fx.last_request().unwrap();
    let crit = &last.questions["target"]["criteria"];
    assert_eq!(crit.as_object().unwrap().len(), 2, "3 candidates → first half kept (ceil)");

    // A 422 that cannot shrink further falls back.
    fx.push(Err(TransportError::Rejected422 { detail: "still oversize".into() }));
    fx.push(Err(TransportError::Rejected422 { detail: "still oversize".into() }));
    let dec = d.decide(DecisionPoint::TargetResolution, target_req()).await;
    assert_eq!(
        dec,
        Decision::Fallback { reason: FallbackReason::Rejected422 }
    );
    assert_eq!(fx.ask_calls(), 4, "one retry only, then fallback");
}

#[tokio::test]
async fn unavailable_service_short_circuits_before_ask() {
    let fx = Arc::new(FixtureTransport::new(false));
    let d = decider_with(config(), fx.clone()).await;
    let dec = d.decide(DecisionPoint::TargetResolution, target_req()).await;
    assert_eq!(
        dec,
        Decision::Fallback { reason: FallbackReason::Unavailable }
    );
    assert_eq!(fx.ask_calls(), 0, "negative probe must gate all asks");
}


/// Build a Config from YAML via a temp file (the only public constructor
/// that parses user YAML — same pattern as joey-browser config tests).
fn cfg_from_yaml(y: &str) -> joey_core::config::Config {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.yaml");
    std::fs::write(&path, y).expect("write yaml");
    joey_core::config::Config::load_from(path).expect("config load")
}

#[tokio::test]
async fn all_points_disabled_yields_none_decider() {
    // from_config path: all four dotted toggles false → None (zero footprint).
    let yaml = "jev:\n  target_resolution:\n    enabled: false\n  settle:\n    enabled: false\n  trim:\n    enabled: false\n  verdict:\n    enabled: false\n";
    let config = cfg_from_yaml(yaml);
    assert!(joey_jev::JevDecider::from_config(&config).is_none());
    // any point enabled → Some
    let yaml_on = "jev:\n  verdict:\n    enabled: true\n";
    assert!(joey_jev::JevDecider::from_config(&cfg_from_yaml(yaml_on)).is_some());
}

#[tokio::test]
async fn every_decision_appends_exactly_one_log_row() {
    let dir = tempfile::tempdir().unwrap();
    let fx = Arc::new(FixtureTransport::new(true));
    fx.push(Ok(choice_response("e1", 0.9, json!({"e1": 0.9, "e2": 0.1}))));
    fx.push(Ok(choice_response("e1", 0.3, json!({"e1": 0.3, "e2": 0.7}))));
    fx.push(Err(TransportError::Timeout));
    let cfg = JevConfig { log_dir: Some(dir.path().to_path_buf()), ..JevConfig::default() };
    let d = jev::JevDecider::new(cfg, fx);
    d.decide(DecisionPoint::TargetResolution, target_req()).await; // confident
    d.decide(DecisionPoint::TargetResolution, target_req()).await; // low confidence
    d.decide(DecisionPoint::TargetResolution, target_req()).await; // timeout fallback
    let content = std::fs::read_to_string(dir.path().join("jev-decisions.jsonl")).unwrap();
    let rows: Vec<serde_json::Value> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), 3, "one row per decide(), fallbacks included");
    let required = ["ts", "point", "question_kind", "answer", "confidence", "latency_ms", "fallback_used"];
    for row in &rows {
        for key in required {
            assert!(row.get(key).is_some(), "row missing {key}: {row}");
        }
        assert_eq!(row["point"], "target_resolution");
        assert_eq!(row["question_kind"], "choice");
    }
    assert_eq!(rows[0]["answer"], "e1");
    assert_eq!(rows[0]["fallback_used"], false);
    assert_eq!(rows[1]["fallback_used"], false);
    assert_eq!(rows[2]["fallback_used"], true);
    assert_eq!(rows[2]["error"], "timeout");
    assert_eq!(rows[2]["answer"], serde_json::Value::Null);
}

#[tokio::test]
async fn three_consecutive_fallbacks_force_reprobe() {
    let fx = Arc::new(FixtureTransport::new(true));
    let d = decider_with(config(), fx.clone()).await;
    let req = target_req();
    // Prime the positive cache.
    fx.push(Ok(choice_response("e1", 0.9, json!({"e1": 0.9, "e2": 0.1}))));
    let _ = d.decide(DecisionPoint::TargetResolution, req.clone()).await;
    assert_eq!(fx.probe_calls(), 1);
    // Three consecutive transport failures while "available".
    for _ in 0..3 {
        fx.push(Err(TransportError::Wire("boom".into())));
        let dec = d.decide(DecisionPoint::TargetResolution, req.clone()).await;
        assert!(matches!(dec, Decision::Fallback { .. }));
    }
    // Within the TTL window, so no re-probe yet.
    assert_eq!(fx.probe_calls(), 1);
    // 4th call: hysteresis cleared the positive cache → re-probe.
    fx.push(Ok(choice_response("e2", 0.9, json!({"e1": 0.1, "e2": 0.9}))));
    let dec = d.decide(DecisionPoint::TargetResolution, req.clone()).await;
    assert!(matches!(dec, Decision::Confident(_)));
    assert_eq!(fx.probe_calls(), 2, "3 fallbacks → forced re-probe");
}

#[tokio::test]
async fn score_batch_min_confidence_gates_on_weakest_entry() {
    let dir = tempfile::tempdir().unwrap();
    let fx = Arc::new(FixtureTransport::new(true));
    let mut answers = serde_json::Map::new();
    for (name, conf) in [("e1", 0.9), ("e2", 0.5)] {
        answers.insert(
            name.into(),
            json!({
                "type": "score", "score": 3, "confidence": conf, "probabilities": {}
            }),
        );
    }
    fx.push(Ok(jev::SystemOneResponse {
        model: "m".into(),
        answers,
        usage: Default::default(),
    }));
    let cfg = JevConfig { log_dir: Some(dir.path().to_path_buf()), ..JevConfig::default() };
    let d = jev::JevDecider::new(cfg, fx);
    let req = jev::snapshot_trim(
        "goal",
        "T",
        "http://u",
        &[("e1".into(), "a".into()), ("e2".into(), "b".into())],
    );
    match d.decide(DecisionPoint::SnapshotTrim, req).await {
        Decision::LowConfidence { got, .. } => assert!((got - 0.5).abs() < 1e-6),
        other => panic!("expected low confidence, got {other:?}"),
    }
    // The score batch is logged with kind=score.
    let content = std::fs::read_to_string(dir.path().join("jev-decisions.jsonl")).unwrap();
    assert!(content.contains("\"question_kind\":\"score\""));
}

#[tokio::test]
async fn settle_point_uses_stricter_threshold() {
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
    let d = decider_with(config(), fx).await;
    let req = jev::settle("quiet", "digest");
    // 0.75 ≥ global 0.70 but < settle 0.80 → LowConfidence.
    match d.decide(DecisionPoint::Settle, req).await {
        Decision::LowConfidence { needed, .. } => assert!((needed - 0.80).abs() < 1e-6),
        other => panic!("expected low confidence, got {other:?}"),
    }
}

/// T019: the settle-point override (`jev.settle.min_confidence`, 0.80)
/// governs Settle even when the global `jev.min_confidence` is looser, and
/// the looser global still governs every other point.
#[tokio::test]
async fn settle_min_confidence_override_beats_global() {
    let mut cfg = config();
    cfg.min_confidence = 0.50; // global loosened
    cfg.settle_min_confidence = 0.80; // settle stays strict
    // (a) settle at 0.75: passes 0.50 global, fails 0.80 override → LowConfidence.
    let fx = Arc::new(FixtureTransport::new(true));
    fx.push(Ok(choice_response(
        "settled",
        0.75,
        json!({ "settled": 0.75, "still_churning": 0.25 }),
    )));
    let d = jev::JevDecider::new(cfg.clone(), fx);
    match d.decide(DecisionPoint::Settle, jev::settle("q", "d")).await {
        Decision::LowConfidence { got, needed } => {
            assert!((got - 0.75).abs() < 1e-6);
            assert!((needed - 0.80).abs() < 1e-6);
        }
        other => panic!("settle must gate at 0.80, got {other:?}"),
    }
    // (b) the same 0.75 answer on a non-settle point clears the 0.50 global.
    let fx = Arc::new(FixtureTransport::new(true));
    fx.push(Ok(choice_response("e2", 0.75, json!({ "e1": 0.25, "e2": 0.75 }))));
    let d = jev::JevDecider::new(cfg, fx);
    assert!(matches!(
        d.decide(DecisionPoint::TargetResolution, target_req()).await,
        Decision::Confident(_)
    ));
}

#[tokio::test]
async fn disabled_point_falls_back_without_network() {
    let fx = Arc::new(FixtureTransport::new(true));
    let cfg = JevConfig { settle_enabled: false, ..config() };
    let d = jev::JevDecider::new(cfg, fx.clone());
    let dec = d.decide(DecisionPoint::Settle, jev::settle("q", "d")).await;
    assert_eq!(dec, Decision::Fallback { reason: FallbackReason::Disabled });
    assert_eq!(fx.probe_calls(), 0, "disabled point must not even probe");
    assert_eq!(fx.ask_calls(), 0);
}

#[tokio::test]
async fn model_is_config_echo_only() {
    let fx = Arc::new(FixtureTransport::new(true));
    fx.push(Ok(choice_response("e1", 0.9, json!({"e1": 0.9, "e2": 0.1}))));
    let cfg = JevConfig { model: "jev-preview".into(), ..config() };
    let d = jev::JevDecider::new(cfg, fx.clone());
    d.decide(DecisionPoint::TargetResolution, target_req()).await;
    assert_eq!(fx.last_request().unwrap().model, "jev-preview");
}

#[test]
fn question_kinds_are_judgment_only() {
    // FR-006 by construction: every builder emits only choice/score.
    let reqs = vec![
        target_req(),
        jev::settle("s", "d"),
        jev::snapshot_trim("g", "t", "u", &[("e1".into(), "d".into())]),
        jev::verdict("click", "pre", "post"),
    ];
    for req in reqs {
        for (_, q) in &req.questions.questions {
            assert!(
                matches!(q.kind(), QuestionKind::Choice | QuestionKind::Score),
                "noul/free-text questions are unreachable by construction"
            );
            if let Question::Score { criteria, .. } = q {
                assert!(criteria.len() <= 10, "score rubric ≤ 10 levels");
            }
        }
    }
}

/// T019: each point toggles independently — disabling one never disables
/// the others (probe+ask still happen for the enabled point).
#[tokio::test]
async fn per_point_toggles_are_independent() {
    use joey_jev::DecisionPoint as P;
    for (off, on_req) in [
        (P::TargetResolution, target_req() as jev::DecisionRequest),
        (P::Settle, jev::settle("q", "d")),
        (P::SnapshotTrim, jev::snapshot_trim("g", "t", "u", &[("e1".into(), "d".into())])),
        (P::Verdict, jev::verdict("a", "pre", "post")),
    ] {
        let fx = Arc::new(FixtureTransport::new(true));
        let mut cfg = config();
        match off {
            P::TargetResolution => cfg.target_resolution_enabled = false,
            P::Settle => cfg.settle_enabled = false,
            P::SnapshotTrim => cfg.trim_enabled = false,
            P::Verdict => cfg.verdict_enabled = false,
        }
        let d = jev::JevDecider::new(cfg, fx.clone());
        // The disabled point falls back with zero network…
        assert_eq!(
            d.decide(off, on_req.clone()).await,
            Decision::Fallback { reason: FallbackReason::Disabled }
        );
        assert_eq!(fx.probe_calls(), 0);
        // …while any other enabled point still works end-to-end: answer the
        // exact follow-up request we are about to send (always P::Verdict here).
        let v_req = jev::verdict("a", "pre", "post");
        let mut a = serde_json::Map::new();
        for (qn, _) in &v_req.questions.questions {
            a.insert(
                qn.clone(),
                json!({
                    "type": "choice", "choice": "achieved", "confidence": 0.9,
                    "probabilities": {"achieved": 0.9, "not_achieved": 0.1}
                }),
            );
        }
        fx.push(Ok(jev::SystemOneResponse {
            model: "m".into(),
            answers: a,
            usage: Default::default(),
        }));
        // Any OTHER point (always Verdict here) must reach the transport.
        let dec = d.decide(P::Verdict, v_req).await;
        assert!(
            matches!(dec, Decision::Confident(_)),
            "verdict point stays live while {off:?} is off"
        );
        assert_eq!(fx.ask_calls(), 1);
        // Settle-only exception: settle disabled + verdict enabled is fine.
        // (When Verdict itself is the disabled one, use Settle as the live one.)
        if off == P::Verdict {
            let fx2 = Arc::new(FixtureTransport::new(true));
            let mut cfg2 = config();
            cfg2.verdict_enabled = false;
            let d2 = jev::JevDecider::new(cfg2, fx2.clone());
            assert_eq!(
                d2.decide(P::Verdict, jev::verdict("a", "p", "q")).await,
                Decision::Fallback { reason: FallbackReason::Disabled }
            );
            let mut a = serde_json::Map::new();
            a.insert(
                "settle".into(),
                json!({
                    "type": "choice", "choice": "settled", "confidence": 0.85,
                    "probabilities": {"settled": 0.85, "still_churning": 0.15}
                }),
            );
            fx2.push(Ok(jev::SystemOneResponse { model: "m".into(), answers: a, usage: Default::default() }));
            assert!(matches!(
                d2.decide(P::Settle, jev::settle("q", "d")).await,
                Decision::Confident(_)
            ));
        }
    }
}

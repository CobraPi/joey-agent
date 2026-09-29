//! Live-service integration tests (T022): round-trips against the real
//! local Jev service at `http://127.0.0.1:8765`. AUTO-SKIP when the
//! service is unreachable (500 ms TCP probe) — `cargo test --workspace`
//! stays green offline.

use std::sync::Arc;
use std::time::Duration;

use joey_jev::{self as jev, Decision, JevConfig};

fn service_up() -> bool {
    std::net::TcpStream::connect_timeout(
        &"127.0.0.1:8765".parse().unwrap(),
        Duration::from_millis(500),
    )
    .is_ok()
}

fn live_decider() -> jev::JevDecider {
    jev::JevDecider::new(
        JevConfig {
            log_dir: Some(std::env::temp_dir().join(format!(
                "joey-jev-live-{}",
                std::process::id()
            ))),
            ..JevConfig::default()
        },
        Arc::new(jev::ReqwestTransport::new(
            "http://127.0.0.1:8765",
            "local",
            Duration::from_millis(1500),
        )),
    )
}

#[tokio::test]
async fn probe_and_ask_round_trip() {
    if !service_up() {
        eprintln!("skipping: jev service not reachable on 127.0.0.1:8765");
        return;
    }
    let d = live_decider();
    // Target resolution with two clearly distinct candidates.
    let req = jev::target_resolution(
        "click the Login button",
        "Fixture",
        "http://127.0.0.1/",
        &[
            ("e1".into(), "Login header navigation link".into()),
            ("e2".into(), "Login primary submit button".into()),
        ],
    );
    match d.decide(jev::DecisionPoint::TargetResolution, req).await {
        Decision::Confident(jev::Answer::Choice { key, confidence, .. }) => {
            assert!(key == "e1" || key == "e2", "pick must be a sent key: {key}");
            assert!(confidence > 0.0 && confidence <= 1.0);
        }
        Decision::Confident(jev::Answer::Scores { .. }) => {
            panic!("target_resolution must answer a choice, not scores");
        }
        Decision::LowConfidence { got, needed } => {
            // A live model may legitimately be uncalibrated-unsure; the
            // gate must still behave: either pass or refuse coherently.
            assert!(got < needed);
        }
        Decision::Fallback { reason } => panic!("live service up but fell back: {reason:?}"),
    }
}

#[tokio::test]
async fn oversize_input_is_422_then_fallback() {
    if !service_up() {
        eprintln!("skipping: jev service not reachable on 127.0.0.1:8765");
        return;
    }
    let d = live_decider();
    // ~40KB instruction dwarfs the Laya context window → the service MUST
    // reject with 422 (never silently truncate); client shrinks once then
    // falls back.
    let huge = "x".repeat(40_000);
    let req = jev::target_resolution(
        &huge,
        "T",
        "http://u",
        &[("e1".into(), "a".into()), ("e2".into(), "b".into())],
    );
    match d.decide(jev::DecisionPoint::TargetResolution, req).await {
        Decision::Fallback { reason } => {
            assert!(
                reason == jev::FallbackReason::Rejected422
                    || reason == jev::FallbackReason::WireError,
                "oversize must collapse to a fallback, got {reason:?}"
            );
        }
        other => panic!("oversize input must not answer: {other:?}"),
    }
}

#[tokio::test]
async fn wrong_bearer_key_is_unauthorized_unavailable() {
    if !service_up() {
        eprintln!("skipping: jev service not reachable on 127.0.0.1:8765");
        return;
    }
    let d = jev::JevDecider::new(
        JevConfig {
            log_dir: Some(std::env::temp_dir().join(format!(
                "joey-jev-live-{}",
                std::process::id()
            ))),
            ..JevConfig::default()
        },
        Arc::new(jev::ReqwestTransport::new(
            "http://127.0.0.1:8765",
            "definitely-not-the-key",
            Duration::from_millis(1500),
        )),
    );
    // The PROBE itself 401s → unavailable before any ask; misconfig never
    // reaches the loop (contract: 401 → treat as unavailable).
    let dec = d
        .decide(jev::DecisionPoint::Verdict, jev::verdict("click", "pre", "post"))
        .await;
    assert_eq!(
        dec,
        Decision::Fallback { reason: jev::FallbackReason::Unavailable }
    );
}

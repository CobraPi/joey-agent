//! US5 regression test (T018): all `jev.*` points disabled → the decider is
//! never constructed, the transport records ZERO invocations, and every
//! decision surface degrades to byte-identical pre-feature output
//! (Constitution VII coverage for the new public config surface).

use std::sync::Arc;

use joey_browser::refs::{self, ElementRef, Rect};
use joey_browser::snapshot::{self, SalienceCtx, SnapshotBudgets, Viewport};
use joey_jev::{self as jev, Decision, FixtureTransport, JevConfig};

fn all_disabled() -> JevConfig {
    JevConfig {
        target_resolution_enabled: false,
        settle_enabled: false,
        trim_enabled: false,
        verdict_enabled: false,
        log_dir: Some(std::env::temp_dir().join(format!(
            "joey-jev-us5-{}",
            std::process::id()
        ))),
        ..JevConfig::default()
    }
}

#[test]
fn from_config_none_when_all_points_disabled() {
    let yaml = "jev:\n  target_resolution:\n    enabled: false\n  settle:\n    enabled: false\n  trim:\n    enabled: false\n  verdict:\n    enabled: false\n";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(&path, yaml).unwrap();
    let config = joey_core::config::Config::load_from(path).unwrap();
    assert!(
        joey_jev::JevDecider::from_config(&config).is_none(),
        "all four points disabled → decider never constructed (zero footprint)"
    );
}

#[tokio::test]
async fn disabled_decider_makes_zero_transport_calls() {
    let fx = Arc::new(FixtureTransport::new(true));
    let d = jev::JevDecider::new(all_disabled(), fx.clone());
    // Hammer all four points.
    let reqs = vec![
        (jev::DecisionPoint::TargetResolution, jev::target_resolution("i", "t", "u", &[("e1".into(), "d".into())])),
        (jev::DecisionPoint::Settle, jev::settle("q", "d")),
        (jev::DecisionPoint::SnapshotTrim, jev::snapshot_trim("g", "t", "u", &[("e1".into(), "d".into())])),
        (jev::DecisionPoint::Verdict, jev::verdict("a", "pre", "post")),
    ];
    for (point, req) in reqs {
        assert_eq!(
            d.decide(point, req).await,
            Decision::Fallback { reason: jev::FallbackReason::Disabled }
        );
    }
    assert_eq!(fx.probe_calls(), 0, "no probes");
    assert_eq!(fx.ask_calls(), 0, "no asks — zero network footprint");
}

#[tokio::test]
async fn disabled_resolution_keeps_refusal_byte_identical() {
    // The browserless helper returns None → the caller emits today's
    // BrowserError::ambiguous refusal unchanged.
    let fx = Arc::new(FixtureTransport::new(true));
    let d = jev::JevDecider::new(all_disabled(), fx);
    let c = vec![
        "e1 [link] \"Login\"".to_string(),
        "e2 [button] \"Login\"".to_string(),
    ];
    let picked = refs::jev_resolve_ambiguous(&d, "click Login", "T", "http://u", &c).await;
    assert!(picked.is_none());
    let refusal = joey_browser::BrowserError::ambiguous(&c).to_string();
    assert_eq!(
        refusal,
        "ambiguous match: 2 [e1 [link] \"Login\"; e2 [button] \"Login\"] candidates refused"
    );
}

#[tokio::test]
async fn disabled_trim_keeps_legacy_snapshot_byte_identical() {
    let els: Vec<ElementRef> = (0..40)
        .map(|i| ElementRef {
            refid: format!("e{i}"),
            role: "button".into(),
            text: format!("element-{i}-{}", "x".repeat(60)),
            value: None,
            frame: "main".into(),
            locator: format!("button:nth-of-type({i})"),
            geometry: Rect { x: 0.0, y: 100.0 + (i as f64) * 10.0, w: 100.0, h: 40.0 },
            attributes: Default::default(),
            interactable: true,
        })
        .collect();
    let budgets = SnapshotBudgets { max_step_bytes: 4096, cumulative_cap_bytes: 65_536, viewport_margin: 1.0 };
    let vp = Viewport { x: 0.0, y: 0.0, w: 1280.0, h: 800.0, scroll_y: 0.0 };

    let fx = Arc::new(FixtureTransport::new(true));
    let d = jev::JevDecider::new(all_disabled(), fx.clone());
    let ctx = SalienceCtx { decider: &d, goal: "any goal" };
    let with_ctx = snapshot::structural_snapshot_with_jev(
        "http://u".into(), "T".into(), 1, vp, els.clone(), vec![], &budgets, 1.0, Some(ctx),
    )
    .await;
    let plain = snapshot::structural_snapshot_with_jev(
        "http://u".into(), "T".into(), 1, vp, els, vec![], &budgets, 1.0, None,
    )
    .await;
    assert_eq!(
        serde_json::to_string(&with_ctx).unwrap(),
        serde_json::to_string(&plain).unwrap(),
        "disabled point → byte-identical snapshot"
    );
    assert_eq!(fx.ask_calls(), 0);
}

//! US3 browserless tests (T014): goal-aware snapshot trimming — no browser,
//! no service. Exercises `structural_snapshot_with_jev` + `TrimPlan`
//! directly with the fixture decider; budget invariants and byte-identity
//! of the fallback path are the gate.

use std::sync::Arc;

use joey_browser::refs::{ElementRef, Rect};
use joey_browser::snapshot::{
    self, SalienceCtx, Snapshot, SnapshotBudgets, TrimPlan, Viewport,
};
use joey_jev::{self as jev, FixtureTransport, JevConfig};
use serde_json::json;

fn vp() -> Viewport {
    Viewport { x: 0.0, y: 0.0, w: 1280.0, h: 800.0, scroll_y: 0.0 }
}

fn el(y: f64, role: &str, text: &str) -> ElementRef {
    ElementRef {
        refid: format!("e{}", y as i64),
        role: role.into(),
        text: text.into(),
        value: None,
        frame: "main".into(),
        locator: format!("{role}:nth-of-type({})", y as i64),
        geometry: Rect { x: 0.0, y, w: 100.0, h: 40.0 },
        attributes: Default::default(),
        interactable: true,
    }
}

/// Elements with long texts → far over any small budget.
fn cluttered(n: usize) -> Vec<ElementRef> {
    (0..n)
        .map(|i| {
            el(
                100.0 + (i as f64) * 10.0,
                "button",
                &format!("element-{i}-{}", "x".repeat(60)),
            )
        })
        .collect()
}

fn tiny_budgets() -> SnapshotBudgets {
    SnapshotBudgets { max_step_bytes: 4096, cumulative_cap_bytes: 65_536, viewport_margin: 1.0 }
}

fn fixture_decider(fx: Arc<FixtureTransport>) -> jev::JevDecider {
    jev::JevDecider::new(
        JevConfig {
            log_dir: Some(
                std::env::temp_dir().join(format!("joey-jev-us3-{}", std::process::id())),
            ),
            ..JevConfig::default()
        },
        fx,
    )
}

fn score_response(scores: &[(&str, u32, f32)]) -> jev::SystemOneResponse {
    let mut answers = serde_json::Map::new();
    for (name, score, conf) in scores {
        answers.insert(
            (*name).into(),
            json!({
                "type": "score", "score": score, "confidence": conf,
                "probabilities": {}
            }),
        );
    }
    jev::SystemOneResponse { model: "m".into(), answers, usage: Default::default() }
}

async fn snap(
    elements: Vec<ElementRef>,
    budgets: &SnapshotBudgets,
    trim: Option<SalienceCtx<'_>>,
) -> Snapshot {
    snapshot::structural_snapshot_with_jev(
        "http://u".into(),
        "T".into(),
        1,
        vp(),
        elements,
        vec![],
        budgets,
        1.0,
        trim,
    )
    .await
}

fn serialized_len(s: &Snapshot) -> usize {
    serde_json::to_string(&s.elements).map(|x| x.len()).unwrap_or(0)
}

#[tokio::test]
async fn under_budget_page_makes_no_jev_call() {
    let fx = Arc::new(FixtureTransport::new(true));
    let d = fixture_decider(fx.clone());
    let ctx = SalienceCtx { decider: &d, goal: "find pricing" };
    let s = snap(vec![el(100.0, "button", "ok")], &tiny_budgets(), Some(ctx)).await;
    assert_eq!(fx.ask_calls(), 0, "small page → no Jev call");
    assert!(!s.truncation.applied);
}

#[tokio::test]
async fn relevant_entries_survive_and_budget_holds() {
    let names: Vec<String> = (1..=32).map(|i| format!("e{i}")).collect();
    // e1 essential (kept), e2 irrelevant (drops first), rest background.
    let scores: Vec<(&str, u32, f32)> = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let score = match i {
                0 => 3,
                1 => 0,
                _ => 1,
            };
            (n.as_str(), score, 0.95f32)
        })
        .collect();
    let fx = Arc::new(FixtureTransport::new(true));
    fx.push(Ok(score_response(&scores)));
    let d = fixture_decider(fx.clone());
    let ctx = SalienceCtx { decider: &d, goal: "find pricing" };
    let s = snap(cluttered(40), &tiny_budgets(), Some(ctx)).await;
    assert_eq!(fx.ask_calls(), 1, "one batched request");
    assert!(s.truncation.applied, "over-budget page trims");
    assert!(s.truncation.omitted.unwrap() > 0, "dropped count announced");
    assert!(
        serialized_len(&s) <= tiny_budgets().max_step_bytes,
        "output never exceeds the byte budget"
    );
    assert!(
        s.elements.iter().any(|e| e.text.starts_with("element-0-")),
        "relevant-marked entry survives"
    );
    assert!(
        !s.elements.iter().any(|e| e.text.starts_with("element-1-")),
        "lowest-scored entry drops first"
    );
}

#[tokio::test]
async fn low_confidence_trims_with_legacy_order_byte_identical() {
    // Identical inputs, two builds: low-confidence fallback vs no decider
    // at all — the serialized snapshot must be byte-identical.
    let names: Vec<String> = (1..=32).map(|i| format!("e{i}")).collect();
    let scores: Vec<(&str, u32, f32)> =
        names.iter().map(|n| (n.as_str(), 1u32, 0.4f32)).collect(); // below gate
    let fx = Arc::new(FixtureTransport::new(true));
    fx.push(Ok(score_response(&scores)));
    let d = fixture_decider(fx);
    let ctx = SalienceCtx { decider: &d, goal: "g" };
    let els = cluttered(40);
    let s_low = snap(els.clone(), &tiny_budgets(), Some(ctx)).await;
    let s_plain = snap(els, &tiny_budgets(), None).await;
    assert_eq!(
        serde_json::to_string(&s_low).unwrap(),
        serde_json::to_string(&s_plain).unwrap(),
        "low-confidence fallback must be byte-identical to legacy trimming"
    );
}

#[tokio::test]
async fn unavailable_service_trims_with_legacy_order() {
    let els = cluttered(40);
    let fx = Arc::new(FixtureTransport::new(false));
    let d = fixture_decider(fx);
    let ctx = SalienceCtx { decider: &d, goal: "g" };
    let s_down = snap(els.clone(), &tiny_budgets(), Some(ctx)).await;
    let s_plain = snap(els, &tiny_budgets(), None).await;
    assert_eq!(
        serde_json::to_string(&s_down).unwrap(),
        serde_json::to_string(&s_plain).unwrap(),
        "service down → byte-identical legacy trim"
    );
}

#[test]
fn trim_plan_priority_order_pure() {
    // Pure plan application: drop order follows ascending score, retained
    // indices survive, budget invariant holds.
    let els = cluttered(12);
    let budgets = SnapshotBudgets { max_step_bytes: 700, ..tiny_budgets() };
    let plan = TrimPlan {
        drop_first: vec![2, 0, 4], // index 2 lowest-scored FIRST
        retain: [1usize, 3].into_iter().collect(),
    };
    let mut oov = Vec::new();
    let (kept, trunc) = Snapshot::enforce_step_budget(els, &mut oov, &budgets, Some(&plan));
    assert!(trunc.applied);
    assert!(serde_json::to_string(&kept).unwrap().len() <= 700);
    // Retained indices 1 and 3 still present (by their unique texts).
    assert!(kept.iter().any(|e| e.text.starts_with("element-1-")));
    assert!(kept.iter().any(|e| e.text.starts_with("element-3-")));
    // Drop-priority indices gone.
    for i in [0usize, 2, 4] {
        assert!(
            !kept.iter().any(|e| e.text.starts_with(&format!("element-{i}-"))),
            "index {i} must drop in priority order"
        );
    }
}

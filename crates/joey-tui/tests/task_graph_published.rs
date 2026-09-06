//! TUI wiring for `AgentEvent::TaskGraphPublished` + explorer-without-indexer.
//!
//! The live orchestrator publishes task graphs as `TaskGraphPublished`
//! events (the `task_graph` tool) — previously only the `/hypercode run`
//! engine pipeline (`EngineEvent::TaskGraphSnapshot`, fed by joey-cli)
//! ever reached `App::set_task_graph`. These tests pin the new wiring:
//! the event feeds the graph, selects the Tasks tab when the NeuroCode
//! indexer is inactive, keeps the previous graph on malformed payloads,
//! opens the explorer without the indexer, and drives the header ⚑ badge
//! and the docked sidebar panel — all through the PUBLIC `joey_tui`
//! surface (same conventions as `tests/neurocode_tasks_tab.rs` and
//! `tests/indicator_widgets.rs`).

use joey_agent_core::events::AgentEvent;
use joey_orchestration::evaluator::VerificationPlanView;
use joey_orchestration::task_graph::{
    AcceptanceCriterion, TaskGraph, TaskNode, TaskStatus,
};
use joey_tui::anim::{HeaderFlow, Pulse, Spinner};
use joey_tui::neurocode_viz::VizTab;
use joey_tui::state::App;
use joey_tui::theme::Theme;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

// ── fixtures (pattern reused verbatim from neurocode_tasks_tab.rs) ─────────

/// One TaskNode with minimal-but-real fields (same construction shape as
/// the task_graph.rs unit tests at ~L900/L1340).
fn node(id: &str, deps: &[&str], objective: &str, status: TaskStatus) -> TaskNode {
    let deps: Vec<_> = deps.iter().map(|d| d.to_string().into()).collect();
    TaskNode {
        id: id.to_string().into(),
        objective: objective.to_string(),
        dependencies: deps,
        read_set: vec!["src/lib.rs".into(), "src/main.rs".into()],
        write_set: vec!["src/generated.rs".into()],
        artifact_ids: vec![],
        role: joey_orchestration::task_graph::WorkerRole::Implementor,
        model_tier: joey_orchestration::task_graph::ModelTier::Frontier,
        risk: joey_orchestration::task_graph::RiskLevel::Low,
        acceptance: vec![AcceptanceCriterion {
            criterion: "tests pass".to_string(),
            kind: "command".to_string(),
        }],
        verification: VerificationPlanView::default(),
        isolation: joey_orchestration::task_graph::IsolationMode::SharedCheckout,
        status,
        attempts: 1,
    }
}

/// t1 (root, Completed) → t2 (dep t1, Dispatched) → t3 (dep t2, Pending).
fn chain_graph() -> TaskGraph {
    let mut g = TaskGraph::default();
    let t1 = node("t1", &[], "fetch pages", TaskStatus::Completed);
    let t2 = node("t2", &["t1"], "parse html", TaskStatus::Dispatched);
    let t3 = node("t3", &["t2"], "index docs", TaskStatus::Pending);
    g.nodes.insert(t1.id.clone(), t1);
    g.nodes.insert(t2.id.clone(), t2);
    g.nodes.insert(t3.id.clone(), t3);
    g
}

/// A single Completed task (badge reads ⚑1/1 — all done).
fn done_graph() -> TaskGraph {
    let mut g = TaskGraph::default();
    let t1 = node("t1", &[], "fetch pages", TaskStatus::Completed);
    g.nodes.insert(t1.id.clone(), t1);
    g
}

/// Publish a graph through the real event path the orchestrator uses.
fn publish(app: &mut App, graph: &TaskGraph) {
    app.apply(AgentEvent::TaskGraphPublished {
        graph: serde_json::to_value(graph).unwrap(),
    });
}

/// Render one frame at `w×h` with the given draw closure and return the
/// whole buffer as a flat symbol string (same harness as
/// indicator_widgets.rs / expanded_view_formatting.rs).
fn buffer_text<F>(w: u16, h: u16, draw: F) -> String
where
    F: FnOnce(&mut ratatui::Frame),
{
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw(f)).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol().to_string())
        .collect()
}

// ── a. event feeds the graph and selects the Tasks tab ─────────────────────

#[test]
fn published_event_feeds_graph_and_selects_tasks_tab() {
    let mut app = App::new("s", "m"); // default state: neurocode_active = false
    assert!(!app.neurocode_active, "indexer inactive by default");
    assert_eq!(app.neurocode_viz.current_tab(), VizTab::Graph, "Graph default");

    publish(&mut app, &chain_graph());

    assert!(app.task_graph.is_some(), "graph landed from the event");
    assert_eq!(
        app.task_graph.as_ref().unwrap().nodes.len(),
        3,
        "all three tasks deserialized"
    );
    assert!(app.task_graph_updated_at.is_some());
    assert_eq!(
        app.neurocode_viz.current_tab(),
        VizTab::Tasks,
        "graph without the indexer selects the only meaningful tab"
    );
}

// ── b. malformed payload keeps the previous graph ──────────────────────────

#[test]
fn published_event_with_malformed_payload_keeps_previous() {
    let mut app = App::new("s", "m");
    publish(&mut app, &chain_graph());
    let before = app.task_graph.clone().expect("first graph landed");

    app.apply(AgentEvent::TaskGraphPublished {
        graph: serde_json::json!({"garbage": true}),
    });

    let after = app.task_graph.as_ref().expect("previous graph kept");
    assert_eq!(
        &before, after,
        "malformed payload must not blank a good graph"
    );
}

// ── c. explorer toggle works without the indexer ───────────────────────────

#[test]
fn explorer_toggle_works_without_indexer() {
    // Graph published + indexer inactive: the toggle flips expanded state.
    let mut app = App::new("s", "m");
    publish(&mut app, &chain_graph());
    assert!(!app.neurocode_active);
    app.toggle_neurocode_expanded();
    assert!(app.neurocode_expanded, "toggle opens without the indexer");
    app.toggle_neurocode_expanded();
    assert!(!app.neurocode_expanded, "second toggle docks back");

    // No graph AND indexer inactive: still a no-op.
    let mut bare = App::new("s", "m");
    assert!(!bare.neurocode_active && bare.task_graph.is_none());
    bare.toggle_neurocode_expanded();
    assert!(!bare.neurocode_expanded, "no-op without indexer or graph");
}

// ── d. header ⚑ badge renders from the published event ─────────────────────

#[test]
fn badge_renders_from_published_event() {
    let mut app = App::new("s", "m");
    publish(&mut app, &done_graph()); // all tasks Completed

    let theme = Theme::aurora();
    let spinner = Spinner::dots();
    let pulse = Pulse::new();
    let flow = HeaderFlow::new();
    let text = buffer_text(120, 24, |f| {
        joey_tui::widgets::draw_header(
            f,
            Rect::new(0, 0, 120, 2),
            &app,
            theme,
            &spinner,
            &pulse,
            Some(&flow),
        );
    });
    assert!(text.contains('⚑'), "badge glyph rendered: {text:?}");
    assert!(text.contains("⚑1/1"), "done/total for the fixture: {text:?}");
}

// ── e. docked panel visible without the indexer ────────────────────────────

#[test]
fn docked_panel_visible_without_indexer() {
    let mut app = App::new("s", "m");
    publish(&mut app, &chain_graph());
    assert!(!app.neurocode_active);

    // 100x30 — same dimensions as the sidebar-rendering tests
    // (expanded_view_formatting.rs render_main); sidebar shows at >=72 cols
    // and the docked feed needs >=16 sidebar rows.
    let text = buffer_text(100, 30, |f| {
        let area = f.area();
        joey_tui::app::render_body_for_test(
            f,
            area,
            &app,
            Theme::aurora(),
            false,
            0.5,
        );
    });
    assert!(
        text.contains("neurocode · tasks"),
        "docked panel drawn (panel title): {text:?}"
    );
    assert!(text.contains("neurocode"), "panel chrome present: {text:?}");
}

// ── f. regression: deep DAG clips rows instead of underflowing ────────────

/// 12-task linear chain (topological depths 0..11 — one DAG row per task).
/// On a 30-row terminal only the first rows fit the canvas; before the
/// saturating-geometry fix `area.y + area.height - y` underflowed (debug
/// panic; release wrapped to h=3), pushing boxes past the buffer, and
/// clipped boxes left phantom (0, 0) hit-cells at the origin.
fn deep_chain_graph() -> TaskGraph {
    let mut g = TaskGraph::default();
    let mut prev: Option<String> = None;
    for i in 1..=12 {
        let deps: Vec<&str> = match &prev {
            Some(p) => vec![p.as_str()],
            None => vec![],
        };
        let id = format!("t{i}");
        let objective = format!("step {:02}", i);
        let t = node(&id, &deps, &objective, TaskStatus::Pending);
        prev = Some(id);
        g.nodes.insert(t.id.clone(), t);
    }
    g
}

#[test]
fn tasks_tab_deep_dag_clipped_rows_neither_panic_nor_draw() {
    let mut app = App::new("s", "m");
    publish(&mut app, &deep_chain_graph());
    app.toggle_neurocode_expanded();
    assert_eq!(app.neurocode_viz.current_tab(), VizTab::Tasks);

    // 100x30 explorer render: rows past the canvas bottom are clipped.
    // (The old underflow panicked here — debug subtraction overflow — and
    // the release wrap made the border loop index past the buffer.)
    let text = buffer_text(100, 30, |f| {
        let area = f.area();
        joey_tui::neurocode_viz::draw_explorer(f, area, &app, Theme::aurora());
    });
    assert!(text.contains("step 01"), "first DAG row drawn: {text:?}");
    assert!(
        !text.contains("step 11") && !text.contains("step 12"),
        "fully-clipped rows must not draw: {text:?}"
    );

    // Hit-test cells: one entry per task, None for clipped boxes — no
    // phantom (0, 0) centers that could select invisible tasks.
    let cells = app.neurocode_viz.task_cells.borrow();
    assert_eq!(cells.len(), 12, "one hit-cell slot per task, in order");
    assert!(cells[0].is_some(), "drawn box carries a hit-cell");
    assert!(cells[11].is_none(), "clipped box carries NO hit-cell");
}


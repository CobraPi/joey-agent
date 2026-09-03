//! T4 (NeuroCode explorer Tasks tab): live task-DAG visualization tests.
//!
//! Integration-style per the joey-tui tests convention — exercise only the
//! PUBLIC `joey_tui` surface: build a real `TaskGraph`, feed it through
//! `App::set_task_graph` (the same serde_json path the planner event uses),
//! open the explorer via `App::toggle_neurocode_expanded`, select
//! `VizTab::Tasks`, and render through the real explorer renderer into a
//! TestBackend buffer. The Tasks tab must work WITHOUT a neurocode
//! snapshot (it reads `App::task_graph`, not the context-graph payload).

use joey_agent_core::events::AgentEvent;
use joey_orchestration::evaluator::VerificationPlanView;
use joey_orchestration::task_graph::{
    AcceptanceCriterion, TaskGraph, TaskNode, TaskStatus,
};
use joey_tui::neurocode_viz::{draw_explorer, VizState, VizTab};
use joey_tui::state::App;
use joey_tui::theme::Theme;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

// ── fixtures ────────────────────────────────────────────────────────────────

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
    // Objectives stay ≤ 12 chars: DAG boxes truncate objective lines at
    // ~20 cols by design, so snippets must be short + unique per task.
    let t1 = node("t1", &[], "fetch pages", TaskStatus::Completed);
    let t2 = node("t2", &["t1"], "parse html", TaskStatus::Dispatched);
    let t3 = node("t3", &["t2"], "index docs", TaskStatus::Pending);
    g.nodes.insert(t1.id.clone(), t1);
    g.nodes.insert(t2.id.clone(), t2);
    g.nodes.insert(t3.id.clone(), t3);
    g
}

/// An App with NeuroCode ACTIVE (so the explorer can open) but NO context
/// snapshot — the Tasks tab must still render from the task graph alone.
fn tasks_app() -> App {
    let mut app = App::new("s", "m");
    app.apply(AgentEvent::NeuroCodeActive { active: true });
    app.set_task_graph(serde_json::to_value(chain_graph()).unwrap());
    app.neurocode_viz.tab = VizTab::Tasks;
    app
}

/// Render the explorer (same direct-draw pattern the module's own visual
/// tests use) and return the whole buffer as a flat symbol string.
fn render_explorer_text(app: &App, width: u16, height: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
    let area = Rect::new(0, 0, width, height);
    term.draw(|f| draw_explorer(f, area, app, Theme::aurora())).unwrap();
    term.backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol().to_string())
        .collect()
}

// ── the DAG renders from the task graph alone ──────────────────────────────

/// Boxes, glyphs, ids, objectives, and the task-counts stats line all
/// land in the buffer — with NO neurocode snapshot (cold explorer).
#[test]
fn tasks_tab_renders_dag_without_snapshot() {
    let app = tasks_app();
    assert!(app.neurocode_snapshot.is_none(), "cold: no graph payload");
    let text = render_explorer_text(&app, 100, 30);

    assert!(text.contains("neurocode explorer"), "chrome present");
    assert!(text.contains("tasks"), "title says tasks");
    assert!(text.contains("✓"), "Completed glyph");
    assert!(text.contains("►"), "Dispatched glyph");
    assert!(text.contains("○"), "Pending glyph");
    assert!(text.contains("t1"), "task id t1");
    assert!(text.contains("t2"), "task id t2");
    assert!(text.contains("t3"), "task id t3");
    assert!(text.contains("fetch pages"), "objective snippet");
    assert!(text.contains("parse html"), "objective snippet");
    assert!(text.contains("index docs"), "objective snippet");
    assert!(text.contains("tasks 1/3"), "stats: 1 done of 3");
    assert!(text.contains("1 active"), "stats: t2 dispatched");
    assert!(text.contains("0 blocked"), "stats: none blocked");
}

/// The tab bar renders in all cases (fallback-feed restructure): the
/// 4-tab strip is present with or without a snapshot, and feed moved to 4.
#[test]
fn tab_bar_shows_four_tabs_without_snapshot() {
    let app = tasks_app();
    let text = render_explorer_text(&app, 100, 30);
    assert!(text.contains("1 graph"), "tab strip graph");
    assert!(text.contains("2 nodes"), "tab strip nodes");
    assert!(text.contains("3 tasks"), "tab strip tasks");
    assert!(text.contains("4 feed"), "feed rebound to 4");
}

/// The detail pane carries the selected task's fields.
#[test]
fn tasks_tab_detail_pane_fields() {
    let mut app = tasks_app();
    // Selected = index 0 → (depth 0, t1) in the (depth, id) ordering.
    let text = render_explorer_text(&app, 100, 30);
    assert!(text.contains("completed"), "status word");
    assert!(text.contains("fetch pages"), "full objective in detail");

    // Move the selection down one → t2, which has deps + read/write sets.
    let n = app.task_graph.as_ref().unwrap().nodes.len();
    joey_tui::neurocode_viz::explorer_key(
        &mut app,
        &crossterm::event::KeyEvent {
            code: crossterm::event::KeyCode::Down,
            modifiers: crossterm::event::KeyModifiers::NONE,
            kind: crossterm::event::KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        },
    );
    let _ = n;
    let text = render_explorer_text(&app, 100, 30);
    assert!(text.contains("dispatched"), "t2 status word");
    assert!(text.contains("t1"), "dependency id listed");
    assert!(text.contains("src/lib.rs"), "read-set path");
    assert!(text.contains("src/generated.rs"), "write-set path");
}

/// hjkl moves the selection through the (depth, id) ordering, clamped.
#[test]
fn tasks_tab_selection_moves_and_clamps() {
    let mut app = tasks_app();
    for key in ['j', 'j', 'j', 'j'] {
        joey_tui::neurocode_viz::explorer_key(
            &mut app,
            &crossterm::event::KeyEvent {
                code: crossterm::event::KeyCode::Char(key),
                modifiers: crossterm::event::KeyModifiers::NONE,
                kind: crossterm::event::KeyEventKind::Press,
                state: crossterm::event::KeyEventState::NONE,
            },
        );
    }
    assert_eq!(app.neurocode_viz.selected, 2, "clamped at last task");
    for key in ['k'] {
        joey_tui::neurocode_viz::explorer_key(
            &mut app,
            &crossterm::event::KeyEvent {
                code: crossterm::event::KeyCode::Char(key),
                modifiers: crossterm::event::KeyModifiers::NONE,
                kind: crossterm::event::KeyEventKind::Press,
                state: crossterm::event::KeyEventState::NONE,
            },
        );
    }
    assert_eq!(app.neurocode_viz.selected, 1, "k moves up");
}

/// Digit '3' switches to the Tasks tab (digit→tab binding), Tab cycles
/// through the new four-tab order.
#[test]
fn digit_and_tab_keys_reach_tasks_tab() {
    let mut app = App::new("s", "m");
    app.apply(AgentEvent::NeuroCodeActive { active: true });
    app.neurocode_expanded = true;

    let key = |code| crossterm::event::KeyEvent {
        code,
        modifiers: crossterm::event::KeyModifiers::NONE,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    };
    assert!(joey_tui::neurocode_viz::explorer_key(
        &mut app,
        &key(crossterm::event::KeyCode::Char('3'))
    ));
    assert_eq!(app.neurocode_viz.tab, VizTab::Tasks);
    assert!(joey_tui::neurocode_viz::explorer_key(
        &mut app,
        &key(crossterm::event::KeyCode::Tab)
    ));
    assert_eq!(app.neurocode_viz.tab, VizTab::Feed);
    assert!(joey_tui::neurocode_viz::explorer_key(
        &mut app,
        &key(crossterm::event::KeyCode::Char('4'))
    ));
    assert_eq!(app.neurocode_viz.tab, VizTab::Feed, "digit 4 = feed");
}

// ── empty state ─────────────────────────────────────────────────────────────

/// No task graph at all: centered dim message, tab bar still reachable,
/// no panic.
#[test]
fn tasks_tab_empty_state_without_graph() {
    let mut app = App::new("s", "m");
    app.apply(AgentEvent::NeuroCodeActive { active: true });
    app.toggle_neurocode_expanded();
    assert!(app.neurocode_expanded, "explorer opens without snapshot");
    app.neurocode_viz.tab = VizTab::Tasks;
    let text = render_explorer_text(&app, 100, 30);
    assert!(text.contains("no task graph"), "empty-state message");
    assert!(text.contains("3 tasks"), "tab bar reachable while empty");
    // Hit-test state stays empty (no phantom boxes).
    assert!(app.neurocode_viz.task_cells.borrow().is_empty());
}

/// The explorer is openable cold via the public API: the toggle gates on
/// neurocode_active only — never on the snapshot (verify-by-reading fact
/// for the T4 report).
#[test]
fn explorer_opens_without_neurocode_snapshot() {
    let mut app = App::new("s", "m");
    assert!(!app.neurocode_expanded);
    app.apply(AgentEvent::NeuroCodeActive { active: true });
    app.toggle_neurocode_expanded();
    assert!(app.neurocode_expanded);
    assert!(app.neurocode_snapshot.is_none());
}

/// VizState default/reset sanity for the new tab (pub-surface regression:
/// reset() must restore Graph and clear task_cells).
#[test]
fn viz_state_reset_restores_graph_and_clears_task_cells() {
    let mut v = VizState::default();
    v.tab = VizTab::Tasks;
    v.selected = 7;
    *v.task_cells.borrow_mut() = vec![(1, 1)];
    v.reset();
    assert_eq!(v.tab, VizTab::Graph);
    assert_eq!(v.selected, 0);
    assert!(v.task_cells.borrow().is_empty());
}

//! Feature-indicator widget tests (T5): status chips, header task badge,
//! and the pinned OMO goal line. Renders the real pub widget functions
//! against a TestBackend — same conventions as `tests/smoke.rs`.

use std::time::{Duration, Instant};

use joey_orchestration::evaluator::VerificationPlanView;
use joey_orchestration::task_graph::{
    IsolationMode, ModelTier, RiskLevel, TaskGraph, TaskId, TaskNode, TaskStatus, WorkerRole,
};
use joey_tui::anim::{Equalizer, HeaderFlow, Pulse, Spinner};
use joey_tui::theme::Theme;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

/// Render one frame at `w×h` with the given draw closure and return the
/// whole buffer as a flat symbol string (substring assertions anywhere in
/// the frame). Mirrors `tests/common/mod.rs::render_frame`.
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

/// A minimal valid TaskNode with the given runtime status.
fn node(id: &str, status: TaskStatus) -> TaskNode {
    TaskNode {
        id: TaskId::new(id).expect("valid id"),
        objective: format!("task {id}"),
        dependencies: vec![],
        read_set: vec![],
        write_set: vec![],
        artifact_ids: vec![],
        role: WorkerRole::Implementor,
        model_tier: ModelTier::Economical,
        risk: RiskLevel::Low,
        acceptance: vec![],
        verification: VerificationPlanView::default(),
        isolation: IsolationMode::SharedCheckout,
        status,
        attempts: 0,
    }
}

/// Status chips render (and only render) when their indicator is active.
#[test]
fn status_chips_present_when_active() {
    let theme = Theme::aurora();
    let mut app = joey_tui::AppState::new("s1234567", "test-model");
    app.provider = "prov".to_string();
    app.retries_this_turn = 2;
    app.compression_count = 3;
    app.mcp_servers = vec!["ctx".to_string()];
    app.browser_connected = true;

    let text = buffer_text(160, 30, |f| {
        joey_tui::widgets::draw_status(
            f,
            Rect::new(0, 29, 160, 1),
            &app,
            theme,
            Duration::from_secs(2),
        );
    });
    assert!(text.contains("↻2"), "retry chip: {text:?}");
    assert!(text.contains("⟳3"), "compression chip: {text:?}");
    assert!(text.contains("◐ web"), "browser chip: {text:?}");
    assert!(text.contains("⚙1"), "mcp chip: {text:?}");
}

/// Zeroed / empty / disconnected indicators emit none of the chips.
#[test]
fn status_chips_absent_when_inactive() {
    let theme = Theme::aurora();
    let app = joey_tui::AppState::new("s1234567", "test-model");

    let text = buffer_text(160, 30, |f| {
        joey_tui::widgets::draw_status(
            f,
            Rect::new(0, 29, 160, 1),
            &app,
            theme,
            Duration::from_secs(2),
        );
    });
    assert!(!text.contains("↻"), "no retry chip: {text:?}");
    assert!(!text.contains("⟳"), "no compression chip: {text:?}");
    assert!(!text.contains("◐ web"), "no browser chip: {text:?}");
    assert!(!text.contains("⚙"), "no mcp chip: {text:?}");
}

/// Header shows the ⚑done/total task badge for a live graph (2 Completed +
/// 1 Pending → ⚑2/3, accent — work remains).
#[test]
fn header_task_badge_counts_done_over_total() {
    let theme = Theme::aurora();
    let mut app = joey_tui::AppState::new("s1234567", "test-model");
    let mut graph = TaskGraph::default();
    graph.nodes.insert(TaskId::new("a").unwrap(), node("a", TaskStatus::Completed));
    graph.nodes.insert(TaskId::new("b").unwrap(), node("b", TaskStatus::Completed));
    graph.nodes.insert(TaskId::new("c").unwrap(), node("c", TaskStatus::Pending));
    app.task_graph = Some(graph);

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
    assert!(text.contains("⚑2/3"), "task badge: {text:?}");
}

/// The OMO panel pins the active goal (◎ objective) at its top.
#[test]
fn omo_panel_pins_goal_line() {
    let theme = Theme::aurora();
    let mut app = joey_tui::AppState::new("s1234567", "test-model");
    app.omo_goal = Some("ship the release".to_string());
    app.goal_set_at = Some(Instant::now());

    let spinner = Spinner::dots();
    let equalizer = Equalizer::new(10);
    let text = buffer_text(96, 24, |f| {
        joey_tui::widgets::draw_omo_panel(
            f,
            Rect::new(62, 0, 34, 24),
            &app,
            theme,
            &spinner,
            &equalizer,
        );
    });
    assert!(text.contains("◎ ship the release"), "goal line: {text:?}");
    assert!(text.contains("set "), "goal-set stamp: {text:?}");
}

//! Subagent-section text overflow: display-width-aware truncation.
//!
//! Wide (double-cell) glyphs in subagent goals used to overflow the OMO
//! sidebar roster and the subagent rail, clobbering the panel border and
//! clipping the elapsed suffix. These tests pin the fixed behavior on the
//! REAL rendered buffer (TestBackend): inner right-margin cell stays blank,
//! suffix text survives, and borders stay intact. ASCII rendering parity
//! is pinned too (truncate_width == truncate_str for pure ASCII).

use joey_agent_core::events::AgentEvent;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn app() -> joey_tui::state::App {
    joey_tui::state::App::new("s", "m")
}

fn spawn(id: u64, goal: &str) -> AgentEvent {
    AgentEvent::SubagentSpawn {
        id,
        goal: goal.to_string(),
        model: "test-model".to_string(),
        toolset_summary: "file, web".to_string(),
        depth: 0,
    }
}

/// ≥30 double-cell CJK chars — wide enough to overflow every budget here.
fn long_cjk_goal() -> String {
    // 22 chars + 13 chars = 35 chars = 70 display cells.
    "任务分析并执行代码修改工作并持续迭代直到完成".to_string() + "任务分析并执行代码修改工作"
}

fn render(a: &joey_tui::state::App, width: u16, height: u16) -> Terminal<TestBackend> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let area = f.area();
            joey_tui::app::render_body_for_test(
                f,
                area,
                a,
                joey_tui::theme::Theme::aurora(),
                false,
                0.5,
            );
        })
        .unwrap();
    terminal
}

#[test]
fn roster_wide_glyphs_stay_inside_panel() {
    let mut a = app();
    a.apply(spawn(1, "short"));
    a.apply(spawn(2, &long_cjk_goal()));
    let terminal = render(&a, 120, 30);
    let buf = terminal.backend().buffer();

    // Find the roster row: some cell in the sidebar inner area (x 87..=118)
    // containing the first CJK glyph of the goal.
    let mut row: Option<u16> = None;
    for y in 0..30u16 {
        for x in 87..=118u16 {
            if buf[(x, y)].symbol() == "任" {
                row = Some(y);
            }
        }
    }
    let y = row.expect("roster row with CJK goal not found in sidebar");

    // (a) At least one inner right-margin cell before the border.
    assert_eq!(buf[(118, y)].symbol(), " ", "roster row must keep inner right-margin cell blank");
    // (b) The elapsed suffix is not clipped by the wide label.
    let line: String = (87..=118u16).map(|x| buf[(x, y)].symbol().to_string()).collect();
    assert!(line.contains("0s"), "elapsed suffix missing from roster row: {:?}", line);
    // (c) The panel border is intact.
    assert_eq!(buf[(119, y)].symbol(), "│", "sidebar border clobbered by roster row");
}

#[test]
fn rail_collapsed_wide_goal_keeps_margin() {
    let mut a = app();
    a.apply(spawn(1, "first task"));
    a.apply(spawn(2, &long_cjk_goal()));
    // Collapsed rail by default (orchestrator view, no focus change):
    // rail x0..18, border col 18; pane 2's tab is at row 3.
    let terminal = render(&a, 120, 30);
    let buf = terminal.backend().buffer();
    assert_eq!(buf[(17, 3)].symbol(), " ", "collapsed tab must keep margin cell before border");
    assert_eq!(buf[(18, 3)].symbol(), "│", "rail border clobbered by collapsed wide tab");
}

#[test]
fn rail_expanded_wide_title_keeps_margin() {
    let mut a = app();
    a.apply(spawn(1, "first task"));
    a.apply(spawn(2, &long_cjk_goal()));
    a.toggle_subagent_rail();
    // Expanded rail = 48 cols, inner x0..46, border col 47; pane 2's card
    // title line is at row 5.
    let terminal = render(&a, 120, 30);
    let buf = terminal.backend().buffer();
    assert_eq!(buf[(46, 5)].symbol(), " ", "expanded card title must keep margin cell before border");
    assert_eq!(buf[(47, 5)].symbol(), "│", "rail border clobbered by expanded wide title");
}

#[test]
fn rail_ascii_labels_unchanged() {
    let mut a = app();
    a.apply(spawn(1, "alpha task"));
    a.apply(spawn(2, "beta task"));
    let terminal = render(&a, 120, 30);
    let frame: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol().to_string())
        .collect();
    assert!(frame.contains("alpha task"), "ASCII tab label must render unchanged");
    assert!(frame.contains("beta task"), "ASCII tab label must render unchanged");
}

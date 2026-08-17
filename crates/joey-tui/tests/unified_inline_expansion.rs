//! Unified inline-expansion tests: every expandable transcript kind (tool,
//! terminal tool, file diff) follows the REASONING-HISTORY expand format —
//! a three-state inline cycle (collapsed → tail window → full) — and never
//! opens a separate viewer window on click/space.

use joey_agent_core::AgentEvent;
use joey_tui::state::{ReasoningExpandState, TranscriptItem};
use joey_tui::widgets;

fn theme() -> joey_tui::theme::Theme {
    joey_tui::theme::Theme::aurora()
}

fn long_result(lines: usize) -> String {
    (0..lines)
        .map(|i| format!("output line {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A tool item with a 300-line result: the cycle must visit
/// Collapsed → TailWindow → Full → Collapsed (no skips — every state is
/// meaningful for a long payload).
#[test]
fn long_tool_result_cycles_through_all_three_states() {
    let item = TranscriptItem::Tool {
        name: "terminal".into(),
        emoji: "💻".into(),
        summary: "make build".into(),
        status: joey_tui::state::ToolStatus::Done,
        duration_secs: Some(1.0),
        result_preview: long_result(300),
        expand_state: ReasoningExpandState::Collapsed,
        full_args: None,
        full_result: Some(long_result(300)),
        is_terminal: true,
        exit_code: Some(0),
        live_output: String::new(),
        live_output_capacity: joey_tui::state::LIVE_OUTPUT_CAPACITY,
    };

    let mut it = item.clone();
    // Collapsed → TailWindow
    joey_tui::state::toggle_expand_for_test(&mut it);
    assert!(matches!(
        it,
        TranscriptItem::Tool { expand_state: ReasoningExpandState::TailWindow, .. }
    ));
    // TailWindow → Full
    joey_tui::state::toggle_expand_for_test(&mut it);
    assert!(matches!(
        it,
        TranscriptItem::Tool { expand_state: ReasoningExpandState::Full, .. }
    ));
    // Full → Collapsed
    joey_tui::state::toggle_expand_for_test(&mut it);
    assert!(matches!(
        it,
        TranscriptItem::Tool { expand_state: ReasoningExpandState::Collapsed, .. }
    ));
}

/// Render-level: collapsed shows the tight cap with an expand affordance;
/// the tail window shows 200 lines with a "full view" affordance; full
/// shows every line.
#[test]
fn rendered_states_match_reasoning_format() {
    let mk = |state| TranscriptItem::Tool {
        name: "terminal".into(),
        emoji: "💻".into(),
        summary: "make build".into(),
        status: joey_tui::state::ToolStatus::Done,
        duration_secs: Some(1.0),
        result_preview: long_result(300),
        expand_state: state,
        full_args: None,
        full_result: Some(long_result(300)),
        is_terminal: true,
        exit_code: Some(0),
        live_output: String::new(),
        live_output_capacity: joey_tui::state::LIVE_OUTPUT_CAPACITY,
    };

    let text = |item: &TranscriptItem| {
        widgets::item_lines_for_test(item, 100, theme())
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // Collapsed: affordance present, early lines hidden.
    let collapsed = text(&mk(ReasoningExpandState::Collapsed));
    assert!(collapsed.contains("output line 0"), "collapsed shows the head");
    assert!(
        collapsed.contains("[click or space to expand]"),
        "collapsed affordance: {collapsed}"
    );

    // Tail window: last 200 lines, head hidden, full-view affordance.
    let tail = text(&mk(ReasoningExpandState::TailWindow));
    assert!(tail.contains("output line 299"), "tail shows the end");
    assert!(
        tail.contains("[click or space for full view]"),
        "tail affordance: {tail}"
    );

    // Full: every line, no truncation affordance.
    let full = text(&mk(ReasoningExpandState::Full));
    assert!(full.contains("output line 0"), "full shows the head");
    assert!(full.contains("output line 299"), "full shows the tail");
}

/// Interaction-level: a mouse click on a tool item expands it INLINE —
/// the maximized output viewer must NOT open (that path is Ctrl+O only).
#[test]
fn clicking_a_tool_expands_inline_not_viewer() {
    let mut app = joey_tui::state::App::new("s", "m");
    app.apply(AgentEvent::ToolStart {
        name: "terminal".into(),
        emoji: "💻".into(),
        summary: "make build".into(),
    });
    app.apply(AgentEvent::ToolEnd {
        name: "terminal".into(),
        is_error: false,
        result_preview: long_result(300),
        duration_secs: 1.0,
        exit_code: Some(0),
        full_result: long_result(300),
    });

    // Click the tool item via the same hit-test the real handler uses.
    // Bottom-anchored render: probe rows from the bottom until the tool
    // item resolves (its collapsed block is a few rows).
    app.last_text_area.set((0, 0, 80, 40));
    let mut hit = None;
    for row in (0..40u16).rev() {
        if let Some(i) = widgets::transcript_hit_test(&app, theme(), row, 4) {
            hit = Some(i);
            break;
        }
    }
    let hit = hit.expect("click resolves to the tool item");

    // The unified behavior: expand inline (toggle the state)…
    app.toggle_item_expand_by_index(hit);
    assert!(
        !app.output_viewer_open,
        "click must NOT open the maximized viewer"
    );
    assert!(matches!(
        app.transcript[hit],
        TranscriptItem::Tool { expand_state: ReasoningExpandState::TailWindow | ReasoningExpandState::Full, .. }
    ));
}

/// FileDiff follows the same three-state cycle (collapsed 50 → tail 200 →
/// full), matching the reasoning format.
#[test]
fn file_diff_cycles_three_states() {
    let diff_lines: Vec<String> = (0..300)
        .map(|i| format!("+ added line {i}"))
        .collect();
    let mut item = TranscriptItem::FileDiff {
        path: "src/big.rs".into(),
        stat: "+300 -0".into(),
        lines: diff_lines,
        is_binary: false,
        expand_state: ReasoningExpandState::Collapsed,
    };
    assert_eq!(joey_tui::state::expand_state_for_test(&item), ReasoningExpandState::Collapsed);
    joey_tui::state::toggle_expand_for_test(&mut item);
    assert_eq!(joey_tui::state::expand_state_for_test(&item), ReasoningExpandState::TailWindow);
    joey_tui::state::toggle_expand_for_test(&mut item);
    assert_eq!(joey_tui::state::expand_state_for_test(&item), ReasoningExpandState::Full);
    joey_tui::state::toggle_expand_for_test(&mut item);
    assert_eq!(joey_tui::state::expand_state_for_test(&item), ReasoningExpandState::Collapsed);
}

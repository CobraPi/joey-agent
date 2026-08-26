//! delegate_task expandability-from-ToolStart tests (Feature 2).
//!
//! A Running delegate_task block must carry the expand affordance from the
//! moment it starts (empty result_preview), and its expanded view must show
//! the delegated goal (via the args/summary fallback) or a running…
//! placeholder when no text is available. Generic tools keep the
//! completion-gated affordance (regression-pinned below).

use joey_agent_core::AgentEvent;
use joey_tui::state::{ReasoningExpandState, TranscriptItem};
use joey_tui::widgets;

fn theme() -> joey_tui::theme::Theme {
    joey_tui::theme::Theme::aurora()
}

fn render(item: &TranscriptItem) -> String {
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
}

fn running_delegate_item(summary: &str) -> TranscriptItem {
    TranscriptItem::Tool {
        name: "delegate_task".into(),
        emoji: "🤝".into(),
        summary: summary.into(),
        status: joey_tui::state::ToolStatus::Running,
        duration_secs: None,
        result_preview: String::new(),
        expand_state: ReasoningExpandState::Collapsed,
        full_args: None,
        full_result: None,
        is_terminal: false,
        exit_code: None,
        live_output: String::new(),
        live_output_capacity: joey_tui::state::LIVE_OUTPUT_CAPACITY,
    }
}

/// ToolStart for delegate_task → the collapsed Running block already shows
/// the expand affordance (result area is empty until ToolEnd).
#[test]
fn running_delegate_task_shows_expand_affordance_from_start() {
    let mut app = joey_tui::state::App::new("s", "m");
    app.apply(AgentEvent::ToolStart {
        name: "delegate_task".into(),
        emoji: "🤝".into(),
        summary: r#"{"goal": "investigate the flaky cron test"}"#.into(),
    });
    // The transcript item exists and is Running with an empty preview.
    assert!(matches!(
        &app.transcript[0],
        TranscriptItem::Tool { name, status, result_preview, .. }
            if name == "delegate_task"
                && *status == joey_tui::state::ToolStatus::Running
                && result_preview.is_empty()
    ));
    let text = render(&app.transcript[0]);
    assert!(
        text.contains("[click or space to expand]"),
        "Running delegate_task affordance: {text}"
    );
}

/// Expanding a Running delegate_task shows the delegated goal (args JSON
/// rides in via the summary fallback at ToolStart).
#[test]
fn expanded_running_delegate_task_shows_goal() {
    let mut app = joey_tui::state::App::new("s", "m");
    app.apply(AgentEvent::ToolStart {
        name: "delegate_task".into(),
        emoji: "🤝".into(),
        summary: r#"{"goal": "refactor the auth middleware"}"#.into(),
    });
    // Toggle via the same index path the click/space handler uses.
    app.toggle_item_expand_by_index(0);
    assert!(matches!(
        &app.transcript[0],
        TranscriptItem::Tool { expand_state: ReasoningExpandState::TailWindow | ReasoningExpandState::Full, .. }
    ));
    let text = render(&app.transcript[0]);
    assert!(text.contains("refactor the auth middleware"), "goal visible: {text}");
}

/// Expanded Running delegate_task with NO args/summary text falls back to a
/// running… placeholder line instead of an empty body.
#[test]
fn expanded_running_delegate_task_empty_args_shows_running_placeholder() {
    let mut item = running_delegate_item("");
    joey_tui::state::toggle_expand_for_test(&mut item);
    let text = render(&item);
    assert!(text.contains("running…"), "placeholder shown: {text}");
}

/// Regression: a generic (non-delegate_task) Running tool block does NOT
/// get the affordance — the completion gate stays intact.
#[test]
fn running_generic_tool_shows_no_affordance() {
    let mut app = joey_tui::state::App::new("s", "m");
    app.apply(AgentEvent::ToolStart {
        name: "search_files".into(),
        emoji: "🔍".into(),
        summary: "pattern=manager".into(),
    });
    assert!(matches!(
        &app.transcript[0],
        TranscriptItem::Tool { name, status, .. }
            if name == "search_files" && *status == joey_tui::state::ToolStatus::Running
    ));
    let text = render(&app.transcript[0]);
    assert!(
        !text.contains("[click or space to expand]"),
        "generic Running tool must not show the affordance: {text}"
    );
}

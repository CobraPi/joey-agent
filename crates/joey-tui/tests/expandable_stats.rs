//! Expandable-stats feature tests: the live context window (stats page
//! stream) renders every entry expandable — click/Space toggles reveal the
//! full content inline with a gutter, identical to the main transcript's
//! expansion affordance. Covers both the orchestrator's stats page and the
//! per-subagent pane stats page.

use joey_agent_core::events::{AgentEvent, ContextEntry};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn app() -> joey_tui::state::App {
    joey_tui::state::App::new("s", "m")
}

fn entry(role: &str, tokens: u64, preview: &str, full: &str) -> ContextEntry {
    ContextEntry {
        role: role.into(),
        tokens,
        preview: preview.into(),
        has_tool_calls: false,
        is_compressed_summary: false,
        full_content: full.into(),
    }
}

fn snapshot(entries: Vec<ContextEntry>) -> AgentEvent {
    AgentEvent::ContextSnapshot {
        entries,
        system_tokens: 500,
        history_tokens: 100,
        context_window: 100_000,
        compression_threshold: 80_000,
        compactions: 0,
        model: "m".into(),
    }
}

/// Render the stats page body through the REAL layout and return the
/// buffer text.
fn render_stats(a: &mut joey_tui::state::App) -> String {
    let backend = TestBackend::new(110, 34);
    let mut terminal = Terminal::new(backend).unwrap();
    a.stats_open = true;
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
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol().to_string())
        .collect()
}

#[test]
fn collapsed_stream_shows_previews_and_arrows() {
    let mut a = app();
    a.apply(snapshot(vec![
        entry("user", 10, "fix the bug", "fix the bug in auth.rs"),
        entry(
            "tool",
            200,
            "grep result",
            "line one\nline two\nline three",
        ),
    ]));
    let text = render_stats(&mut a);
    assert!(text.contains("fix the bug"), "preview visible");
    assert!(text.contains('▸'), "collapsed affordance arrow");
    assert!(!text.contains("line three"), "full content hidden while collapsed");
}

#[test]
fn expanded_entry_renders_full_content_inline() {
    let mut a = app();
    a.apply(snapshot(vec![entry(
        "tool",
        200,
        "grep result",
        "line one\nline two\nline three",
    )]));
    a.toggle_context_entry(0);
    assert!(a.expanded_context.contains(&0));
    let text = render_stats(&mut a);
    assert!(text.contains("line one"), "expanded shows content");
    assert!(text.contains("line three"), "expanded shows ALL lines");
    assert!(text.contains('▾'), "expanded affordance arrow");
    // Gutter present (output-viewer style "N │ ").
    assert!(text.contains("│"), "line-number gutter rendered");
}

#[test]
fn toggle_roundtrips_and_out_of_range_is_noop() {
    let mut a = app();
    a.apply(snapshot(vec![entry("user", 1, "p", "full")]));
    a.toggle_context_entry(0);
    assert!(a.expanded_context.contains(&0));
    a.toggle_context_entry(0);
    assert!(!a.expanded_context.contains(&0));
    a.toggle_context_entry(99); // out of range: no-op, no panic
    assert!(a.expanded_context.is_empty());
}

#[test]
fn expansion_survives_append_renumbering() {
    let mut a = app();
    a.apply(snapshot(vec![
        entry("user", 10, "first", "first full"),
        entry("tool", 20, "second", "second full"),
    ]));
    a.toggle_context_entry(1); // expand "second"
    // New snapshot: one message PREPENDED → indices shift by 1.
    a.apply(snapshot(vec![
        entry("user", 5, "zeroth", "zeroth full"),
        entry("user", 10, "first", "first full"),
        entry("tool", 20, "second", "second full"),
    ]));
    assert!(
        a.expanded_context.contains(&2),
        "expansion followed its entry to the new index: {:?}",
        a.expanded_context
    );
    assert!(!a.expanded_context.contains(&1));
}

#[test]
fn expansion_dropped_when_entry_compacted_away() {
    let mut a = app();
    a.apply(snapshot(vec![
        entry("user", 10, "old", "old full"),
        entry("user", 20, "keep", "keep full"),
    ]));
    a.toggle_context_entry(0); // expand "old"
    // Compaction removes "old" entirely.
    a.apply(snapshot(vec![entry("user", 20, "keep", "keep full")]));
    assert!(a.expanded_context.is_empty(), "dropped entry loses expansion");
}

#[test]
fn oversized_expansion_is_bounded_with_affordance() {
    let mut a = app();
    let huge: String = (0..300)
        .map(|i| format!("row-{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    a.apply(snapshot(vec![entry("tool", 9_999, "huge", &huge)]));
    a.toggle_context_entry(0);
    let text = render_stats(&mut a);
    // The window is tail-anchored: the affordance line (bottom of the
    // expansion) plus the tail of the capped 40 lines are visible.
    assert!(
        text.contains("more lines — too large to expand inline"),
        "bounding affordance shown"
    );
    assert!(text.contains("row-"), "capped content lines render");
    assert!(!text.contains("row-299"), "tail beyond cap hidden");
}

#[test]
fn pane_stats_stream_is_expandable_too() {
    let mut a = app();
    a.apply(AgentEvent::SubagentSpawn {
        id: 1,
        goal: "child".into(),
        model: "m".into(),
        toolset_summary: "all".into(),
        depth: 0,
    });
    a.apply(AgentEvent::SubagentEvent {
        id: 1,
        event: Box::new(snapshot(vec![entry(
            "assistant",
            42,
            "thinking about it",
            "the full child answer\nwith two lines",
        )])),
    });
    a.focus_subagent(Some(0));
    a.toggle_pane_context_entry(0);
    let pane = a.focused_pane().unwrap();
    assert!(pane.expanded_context.contains(&0));

    // Render the pane stats page via the real layout.
    a.stats_open = true;
    let backend = TestBackend::new(110, 34);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let area = f.area();
            joey_tui::app::render_body_for_test(
                f,
                area,
                &a,
                joey_tui::theme::Theme::aurora(),
                false,
                0.5,
            );
        })
        .unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol().to_string())
        .collect();
    assert!(
        text.contains("the full child answer"),
        "pane stats shows expanded content"
    );
    assert!(text.contains("subagent stats"), "pane stats page rendered");
}

#[test]
fn stats_context_entry_hit_resolves_rows() {
    let mut a = app();
    a.apply(snapshot(vec![
        entry("user", 10, "one", "full one"),
        entry("user", 10, "two", "full two"),
    ]));
    a.stats_open = true;
    // Render once so geometry is recorded, then hit-test.
    let _ = render_stats(&mut a);
    let (inner_y, _start) = a.last_stats_window.get();
    // The first entry's header row is the first stream row; entries start
    // after the dashboard header (~6 rows). Scan for a row that resolves.
    let mut resolved = None;
    for probe in inner_y..inner_y + 20 {
        if let Some(idx) = a.stats_context_entry_hit(probe) {
            resolved = Some((probe, idx));
            break;
        }
    }
    let (row, idx) = resolved.expect("some row resolves to an entry");
    assert_eq!(idx, 0, "first hit is entry 0 (row {row})");
    // Toggle through the hit path (what a click does).
    a.toggle_context_entry(idx);
    assert!(a.expanded_context.contains(&idx));
}

#[test]
fn pane_item_expansion_toggles() {
    let mut a = app();
    a.apply(AgentEvent::SubagentSpawn {
        id: 1,
        goal: "child".into(),
        model: "m".into(),
        toolset_summary: "all".into(),
        depth: 0,
    });
    a.apply(AgentEvent::SubagentEvent {
        id: 1,
        event: Box::new(AgentEvent::ToolStart {
            name: "read_file".into(),
            emoji: "📖".into(),
            summary: "x".into(),
        }),
    });
    a.focus_subagent(Some(0));
    let pane = a.focused_pane().unwrap();
    assert!(!matches!(
        pane.transcript.back(),
        Some(joey_tui::state::TranscriptItem::Tool { expanded: true, .. })
    ));
    // Pane item toggle via the SubagentPane method.
    let idx = pane.transcript.len() - 1;
    a.focused_pane_mut().unwrap().toggle_item_expand(idx);
    let pane = a.focused_pane().unwrap();
    assert!(matches!(
        pane.transcript.back(),
        Some(joey_tui::state::TranscriptItem::Tool { expanded: true, .. })
    ));
}

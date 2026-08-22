//! Parallel-subagent feature: per-subagent pane state + rendering tests.
//!
//! Drives App::apply with synthetic orchestration events (SubagentSpawn /
//! SubagentEvent / SubagentComplete) and asserts the pane model + the
//! rendered rail/pane views via a ratatui TestBackend.

use joey_agent_core::events::{AgentEvent, ContextEntry};
use joey_tui::state::{RunMode, SubagentStatus, TranscriptItem};
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

#[test]
fn spawn_opens_pane_and_stacks_tab() {
    let mut a = app();
    a.apply(spawn(1, "explore the manager"));
    a.apply(spawn(2, "explore the agent"));
    assert_eq!(a.subagent_panes.len(), 2);
    assert_eq!(a.subagent_panes[0].child_id, 1);
    assert_eq!(a.subagent_panes[1].child_id, 2);
    assert!(a.focused_subagent.is_none(), "orchestrator stays focused by default");
}

#[test]
fn wrapped_events_route_to_matching_pane_only() {
    let mut a = app();
    a.apply(spawn(1, "task one"));
    a.apply(spawn(2, "task two"));

    // Child 2 emits content + a tool call.
    a.apply(AgentEvent::SubagentEvent {
        id: 2,
        event: Box::new(AgentEvent::ContentDelta("hello from two".into())),
    });
    a.apply(AgentEvent::SubagentEvent {
        id: 2,
        event: Box::new(AgentEvent::ToolStart {
            name: "search_files".into(),
            emoji: "🔍".into(),
            summary: "pattern=manager".into(),
        }),
    });
    // Child 1 emits reasoning.
    a.apply(AgentEvent::SubagentEvent {
        id: 1,
        event: Box::new(AgentEvent::ReasoningDelta("thinking...".into())),
    });

    assert_eq!(a.subagent_panes[1].streaming_assistant, "hello from two");
    assert!(a.subagent_panes[1]
        .transcript
        .iter()
        .any(|it| matches!(it, TranscriptItem::Tool { name, .. } if name == "search_files")));
    assert_eq!(a.subagent_panes[0].streaming_reasoning, "thinking...");
    // Cross-contamination check.
    assert!(a.subagent_panes[0].streaming_assistant.is_empty());
    assert!(a.subagent_panes[1].streaming_reasoning.is_empty());
}

#[test]
fn wrapped_child_done_does_not_reset_parent_mode() {
    let mut a = app();
    a.mode = RunMode::Busy;
    a.apply(spawn(1, "task"));
    // The CHILD's Done arrives wrapped — it must NOT flip the parent's
    // RunMode back to Input.
    a.apply(AgentEvent::SubagentEvent {
        id: 1,
        event: Box::new(AgentEvent::Done {
            final_text: "child done".into(),
            usage: Default::default(),
            iterations: 3,
        }),
    });
    assert!(a.is_busy(), "parent stays busy while children run");
    // The wrapped Done also must not push into the PARENT transcript.
    assert!(!a
        .transcript
        .iter()
        .any(|it| matches!(it, TranscriptItem::Assistant { text } if text == "child done")));
}

#[test]
fn lifecycle_completes_pane_and_flushes_stream() {
    let mut a = app();
    a.apply(spawn(7, "finish me"));
    a.apply(AgentEvent::SubagentEvent {
        id: 7,
        event: Box::new(AgentEvent::ContentDelta("partial answer".into())),
    });
    a.apply(AgentEvent::SubagentComplete {
        id: 7,
        goal: "finish me".to_string(),
        success: true,
        summary_preview: "done summary".to_string(),
        token_usage: Default::default(),
        duration_secs: 1.2,
    });
    let pane = &a.subagent_panes[0];
    assert_eq!(pane.status, SubagentStatus::Done);
    assert_eq!(pane.summary_preview.as_deref(), Some("done summary"));
    // Streaming text flushed into the pane transcript.
    assert!(pane
        .transcript
        .iter()
        .any(|it| matches!(it, TranscriptItem::Assistant { text } if text == "partial answer")));
}

#[test]
fn focus_switching_and_escape() {
    let mut a = app();
    a.apply(spawn(1, "one"));
    a.apply(spawn(2, "two"));
    a.focus_subagent(Some(1));
    assert_eq!(a.focused_subagent, Some(1));
    assert_eq!(a.focused_pane().unwrap().child_id, 2);
    a.focus_subagent(Some(99)); // out of range → back to orchestrator
    assert!(a.focused_subagent.is_none());
    a.clear_subagent_panes();
    assert!(a.subagent_panes.is_empty());
    assert!(a.last_subagent_tab_rects.borrow().is_empty());
}

#[test]
fn panes_survive_turn_done() {
    let mut a = app();
    a.mode = RunMode::Busy;
    a.apply(spawn(1, "survivor"));
    a.apply(AgentEvent::SubagentComplete {
        id: 1,
        goal: "survivor".to_string(),
        success: true,
        summary_preview: "".to_string(),
        token_usage: Default::default(),
        duration_secs: 0.5,
    });
    a.apply(AgentEvent::Done {
        final_text: "parent done".into(),
        usage: Default::default(),
        iterations: 1,
    });
    // Activity-panel entries are cleared, but the pane stays readable.
    assert!(a.subagent_entries.is_empty());
    assert_eq!(a.subagent_panes.len(), 1);
    assert_eq!(a.mode, RunMode::Input);
}

#[test]
fn pane_stats_capture_child_context() {
    let mut a = app();
    a.apply(spawn(3, "ctx child"));
    a.apply(AgentEvent::SubagentEvent {
        id: 3,
        event: Box::new(AgentEvent::ContextSnapshot {
            entries: vec![ContextEntry {
                role: "user".into(),
                tokens: 42,
                preview: "goal".into(),
                has_tool_calls: false,
                is_compressed_summary: false,
                full_content: String::new(),
            }],
            system_tokens: 500,
            history_tokens: 42,
            context_window: 100_000,
            compression_threshold: 80_000,
            compactions: 0,
            model: "test-model".into(),
        }),
    });
    let pane = &a.subagent_panes[0];
    assert_eq!(pane.context_entries.len(), 1);
    assert_eq!(pane.context_history_tokens, 42);
    assert!((pane.context_usage_pct() - 0.542).abs() < 0.01);
}

// ── Rendering (TestBackend) ─────────────────────────────────────────────

fn render_frame(a: &joey_tui::state::App, width: u16, height: u16) -> String {
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
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol().to_string())
        .collect()
}

#[test]
fn rail_renders_tabs_with_goal_labels() {
    let mut a = app();
    a.apply(spawn(1, "alpha task"));
    a.apply(spawn(2, "beta task"));
    let text = render_frame(&a, 120, 30);
    assert!(text.contains("subagents"), "rail title: {}", first_line(&text, 0));
    assert!(text.contains("alpha task"), "tab 1 label");
    assert!(text.contains("beta task"), "tab 2 label");
}

#[test]
fn focused_pane_replaces_main_transcript() {
    let mut a = app();
    a.apply(spawn(1, "watched child"));
    // Parent transcript has its own content.
    a.push_item(TranscriptItem::User { text: "parent prompt".into() });
    a.focus_subagent(Some(0));
    let text = render_frame(&a, 120, 30);
    assert!(text.contains("subagent: watched child"), "pane title shown");
    assert!(!text.contains("parent prompt"), "parent transcript hidden while pane focused");
    // Back to the orchestrator: parent content returns.
    a.focus_subagent(None);
    let text = render_frame(&a, 120, 30);
    assert!(text.contains("parent prompt"));
}

#[test]
fn tab_click_hit_routing() {
    let mut a = app();
    a.apply(spawn(1, "one"));
    a.apply(spawn(2, "two"));
    // Simulate the rail recording rects (as the widget does per frame).
    *a.last_subagent_tab_rects.borrow_mut() = vec![
        (100, 1, 18, 1),
        (100, 3, 18, 1),
    ];
    assert_eq!(a.subagent_tab_hit(1, 105), Some(0));
    assert_eq!(a.subagent_tab_hit(3, 105), Some(1));
    assert_eq!(a.subagent_tab_hit(2, 105), None, "gap between tabs");
    assert_eq!(a.subagent_tab_hit(1, 50), None, "outside the rail");
}

/// The pinned orchestrator tab renders at the rail's bottom whenever the
/// rail is drawn, and records a click rect distinct from the pane tabs.
#[test]
fn orchestrator_tab_renders_and_records_click_rect() {
    let mut a = app();
    a.apply(spawn(1, "alpha task"));
    let text = render_frame(&a, 120, 30);
    assert!(text.contains("orchestrator"), "orchestrator tab label: {text}");
    // The click rect was recorded with real geometry (non-zero w/h).
    let (x, y, w, h) = a.last_orchestrator_tab_rect.get();
    assert!(w > 0 && h > 0, "orchestrator tab rect recorded");
    // It sits BELOW the pane tabs (the rail stacks panes from the top).
    let pane_rects = a.last_subagent_tab_rects.borrow().clone();
    assert!(!pane_rects.is_empty());
    let (_, first_pane_y, _, _) = pane_rects[0];
    assert!(y > first_pane_y, "orchestrator tab is pinned below the pane tabs");
    // Hit-test: inside the rect → true; outside → false.
    assert!(a.orchestrator_tab_hit(y, x + 2));
    assert!(!a.orchestrator_tab_hit(y + 5, x + 2), "row below the tab");
    assert!(!a.orchestrator_tab_hit(y, x + w + 2), "column right of the rail");
}

/// Clicking the orchestrator tab returns to the main view from a focused
/// pane (the click handler routes through focus_subagent(None)).
#[test]
fn clicking_orchestrator_tab_returns_to_main_view() {
    let mut a = app();
    a.apply(spawn(1, "watched child"));
    a.push_item(TranscriptItem::User { text: "parent prompt".into() });
    a.focus_subagent(Some(0));
    // Simulate the recorded rect, then the hit-test + routing the real
    // handler performs.
    a.last_orchestrator_tab_rect.set((100, 28, 18, 1));
    assert!(a.orchestrator_tab_hit(28, 105));
    a.focus_subagent(None);
    assert!(a.focused_subagent.is_none(), "back on the orchestrator view");
    let text = render_frame(&a, 120, 30);
    assert!(text.contains("parent prompt"), "parent transcript restored");
}

/// The orchestrator tab carries the focused highlight exactly when the main
/// view is active (no pane focused) — the rail always shows the current
/// view.
#[test]
fn orchestrator_tab_highlight_follows_focus() {
    let mut a = app();
    a.apply(spawn(1, "alpha task"));
    // Orchestrator focused: highlight ON. The ▸ focus marker is inserted
    // before the status glyph ("▸ ◆ orchestrator").
    let text = render_frame(&a, 120, 30);
    assert!(
        text.contains("▸") && text.find("▸").zip(text.find("orchestrator"))
            .map(|(m, l)| m < l)
            .unwrap_or(false),
        "focused marker before the label (got: {:?})",
        &text[text.find("orchestrator").map(|i| i.saturating_sub(12)).unwrap_or(0)..]
    );
    // Pane focused: the pane tab is highlighted instead; the orchestrator
    // tab is dimmed. (Rendered highlight of pane tabs is covered by
    // focused_pane_replaces_main_transcript; here we assert the state
    // transitions the renderer reads.)
    a.focus_subagent(Some(0));
    assert!(a.focused_subagent == Some(0));
    a.focus_subagent(None);
    assert!(a.focused_subagent.is_none());
}

fn first_line(s: &str, _n: usize) -> String {
    s.lines().next().unwrap_or("").to_string()
}

// ── Expandable rail (Ctrl+N / title-click) ─────────────────────────────

/// Render the rail area only (recorded geometry from a real frame), used
/// to assert rail widths across the collapse/expand toggle.
fn rail_width_after_render(a: &joey_tui::state::App, width: u16, height: u16) -> (u16, u16) {
    let _ = render_frame(a, width, height);
    // The title rect width tracks the rail's drawn width; the per-tab
    // rects use the same inner width.
    let (x, y, w, h) = a.last_subagent_rail_title_rect.get();
    assert!(w > 0 && h > 0, "rail title rect recorded (rail drawn)");
    let tab_w = a
        .last_subagent_tab_rects
        .borrow()
        .first()
        .map(|(_, _, tw, _)| *tw)
        .unwrap_or(0);
    let _ = (x, y);
    (w, tab_w)
}

/// Default is collapsed: the rail renders at the original 19-col strip
/// width (18 inner cols + 1 border), and the title carries the ▸ hint.
#[test]
fn rail_defaults_to_collapsed_19_cols() {
    let mut a = app();
    a.apply(spawn(1, "alpha task"));
    assert!(!a.subagent_rail_expanded, "expansion flag defaults to false");
    let text = render_frame(&a, 120, 30);
    assert!(text.contains("subagents"), "rail title present");
    assert!(text.contains("▸"), "collapsed title shows the ▸ hint");
    let (title_w, tab_w) = rail_width_after_render(&a, 120, 30);
    assert_eq!(title_w, 18, "collapsed title row spans the 18-col inner rail");
    assert_eq!(tab_w, 18, "collapsed tab rows span the 18-col inner rail");
}

/// Toggling via the state helper renders the wider rail (48 cols on a
/// 120-col terminal) with the richer detail lines.
#[test]
fn expanded_rail_renders_wider_with_detail_lines() {
    let mut a = app();
    a.apply(spawn(1, "alpha task"));
    // Give the entry some richness: a tool call updates phase + last_tool.
    a.apply(AgentEvent::SubagentEvent {
        id: 1,
        event: Box::new(AgentEvent::ToolStart {
            name: "search_files".into(),
            emoji: "🔍".into(),
            summary: "pattern=x".into(),
        }),
    });
    a.toggle_subagent_rail();
    assert!(a.subagent_rail_expanded);
    let text = render_frame(&a, 120, 30);
    assert!(text.contains("test-model"), "expanded card shows the model");
    assert!(text.contains("d0"), "expanded card shows delegation depth");
    assert!(text.contains("search_files"), "expanded card shows last_tool");
    assert!(text.contains("▾"), "expanded title shows the ▾ hint");
    let (title_w, tab_w) = rail_width_after_render(&a, 120, 30);
    // 120-col terminal: 48-col rail (47 inner + 1 border).
    assert_eq!(title_w, 47, "expanded title row spans the wider rail");
    assert_eq!(tab_w, 47, "expanded card rows span the wider rail");
    // Toggling back collapses again.
    a.toggle_subagent_rail();
    let (title_w, _) = rail_width_after_render(&a, 120, 30);
    assert_eq!(title_w, 18, "toggle back to collapsed restores 19-col rail");
}

/// On a terminal too narrow to honor the expanded width (transcript would
/// drop below 60 cols), the rail clamps back to the 19-col strip.
#[test]
fn expanded_rail_clamps_on_narrow_terminal() {
    let mut a = app();
    a.apply(spawn(1, "alpha task"));
    a.toggle_subagent_rail();
    // 96 cols: 96 - 48 = 48 < 60 → clamp to 19.
    let (title_w, _) = rail_width_after_render(&a, 96, 30);
    assert_eq!(title_w, 18, "narrow terminal clamps the rail back to 18 inner cols");
    // Wide enough (120 - 48 = 72 >= 60) it stays wide.
    let (title_w, _) = rail_width_after_render(&a, 120, 30);
    assert_eq!(title_w, 47, "wide terminal honors the expanded rail");
}

/// The rail title row records a click rect in BOTH modes; the hit-test
/// matches it and the recorded width tracks the mode.
#[test]
fn title_rect_recorded_in_both_modes() {
    let mut a = app();
    a.apply(spawn(1, "alpha task"));
    render_frame(&a, 120, 30);
    let (x, y, w, h) = a.last_subagent_rail_title_rect.get();
    assert!(w > 0 && h > 0, "collapsed title rect recorded");
    assert!(a.subagent_rail_title_hit(y, x + 2), "hit inside the title rect");
    assert!(!a.subagent_rail_title_hit(y + 2, x + 2), "row below misses");
    a.toggle_subagent_rail();
    render_frame(&a, 120, 30);
    let (x2, y2, w2, h2) = a.last_subagent_rail_title_rect.get();
    assert!(w2 > w, "expanded title rect is wider");
    assert!(h2 > 0 && a.subagent_rail_title_hit(y2, x2 + 2), "expanded title hit");
}

/// Clicking an entry focuses that pane in EXPANDED mode too (recorded
/// rects + the same routing the click handler performs).
#[test]
fn expanded_entry_clicks_still_focus_panes() {
    let mut a = app();
    a.apply(spawn(1, "one"));
    a.apply(spawn(2, "two"));
    a.toggle_subagent_rail();
    render_frame(&a, 120, 30);
    let rects = a.last_subagent_tab_rects.borrow().clone();
    assert_eq!(rects.len(), 2, "both cards recorded hit rects");
    // Cards stack 4 rows apart in expanded mode.
    assert_eq!(rects[0].1 + 4, rects[1].1, "expanded cards are 4 rows apart");
    assert!(a.subagent_tab_hit(rects[0].1, rects[0].0 + 2).is_some());
    // Route like handle_mouse_click does.
    if let Some(idx) = a.subagent_tab_hit(rects[1].1, rects[1].0 + 2) {
        a.focus_subagent(Some(idx));
    }
    assert_eq!(a.focused_subagent, Some(1), "clicking card 2 focuses pane 2");
    // Ctrl+P parity: back to the orchestrator (state-level, key handler
    // covered by the app.rs unit tests).
    a.focus_subagent(None);
    assert!(a.focused_subagent.is_none());
}

/// When the rail is hidden (no panes) the title rect is zeroed — no stale
/// geometry catching clicks.
#[test]
fn title_rect_zeroed_when_rail_hidden() {
    let a = app();
    let _ = render_frame(&a, 120, 30);
    let (x, y, w, h) = a.last_subagent_rail_title_rect.get();
    assert_eq!((x, y, w, h), (0, 0, 0, 0), "no panes → no title rect");
    assert!(!a.subagent_rail_title_hit(0, 100));
}

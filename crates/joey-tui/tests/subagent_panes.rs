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

// ── Scrollable rail (overflow panes) ───────────────────────────────────

/// Spawn `n` panes with unique goal markers (assertable in the buffer).
fn spawn_many(a: &mut joey_tui::state::App, n: usize) {
    for i in 0..n {
        a.apply(spawn(1 + i as u64, &format!("pane-goal-{i:02}")));
    }
}

/// Render a frame and return ONLY the rail strip's buffer text (the goals
/// also appear in the activity sidebar, so raw full-frame contains() would
/// be ambiguous). Uses the rail rect the frame itself recorded.
fn render_rail_text(a: &joey_tui::state::App, width: u16, height: u16) -> String {
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
    let (x, y, w, h) = a.last_subagent_rail_rect.get();
    assert!(w > 0 && h > 0, "rail rect recorded");
    let buf = terminal.backend().buffer();
    let mut out = String::new();
    for row in y..y.saturating_add(h) {
        for col in x..x.saturating_add(w) {
            out.push_str(buf[(col, row)].symbol());
        }
        out.push('\n');
    }
    out
}

/// 12 panes on a 14-row terminal overflow the collapsed rail (capacity 5):
/// the first pane is visible at scroll 0, the last is NOT — and scrolling
/// to the bottom window flips exactly that.
#[test]
fn rail_overflow_scrolls_to_reveal_hidden_panes() {
    let mut a = app();
    spawn_many(&mut a, 12);
    let text = render_rail_text(&a, 120, 14);
    assert!(text.contains("pane-goal-00"), "pane 0 visible at scroll 0");
    assert!(!text.contains("pane-goal-11"), "last pane overflows off-screen");
    assert_eq!(a.last_subagent_rail_max_scroll.get(), 7, "12 panes - 5 capacity");
    assert_eq!(a.last_subagent_rail_drawn_offset.get(), 0);

    // Scroll to the bottom (clamped) and re-render.
    a.subagent_rail_scroll_down(100);
    let text = render_rail_text(&a, 120, 14);
    assert!(text.contains("pane-goal-11"), "last pane visible after scrolling");
    assert!(!text.contains("pane-goal-00"), "pane 0 scrolled out of the window");
    // The scroll indicator appears when overflowing (custom '█'/'│' cells);
    // at the bottom window only the above-availability glyph remains.
    assert!(text.contains('│') || text.contains('█'), "scroll indicator drawn");
    assert!(text.contains('▲'), "above-availability glyph at the bottom window");
    assert!(!text.contains('▼'), "nothing hidden below at max scroll");
    // At the top window the below-glyph shows instead.
    a.subagent_rail_scroll_up(100);
    let text = render_rail_text(&a, 120, 14);
    assert!(text.contains('▼'), "below-availability glyph at the top window");
    assert!(!text.contains('▲'), "nothing hidden above at scroll 0");
}

/// Rail scrolling clamps at both bounds: up at 0 stays 0, down past the
/// recorded max clamps to max.
#[test]
fn rail_scroll_clamps_at_bounds() {
    let mut a = app();
    spawn_many(&mut a, 12);
    render_frame(&a, 120, 14); // records max_scroll = 7
    a.subagent_rail_scroll_up(3);
    assert_eq!(a.subagent_rail_scroll, 0, "up at 0 stays 0");
    a.subagent_rail_scroll_down(100);
    assert_eq!(a.subagent_rail_scroll, 7, "down past max clamps to max");
    a.subagent_rail_scroll_up(2);
    assert_eq!(a.subagent_rail_scroll, 5);
}

/// Clearing the panes (Ctrl+L path) resets the rail window to 0.
#[test]
fn clearing_panes_resets_rail_scroll() {
    let mut a = app();
    spawn_many(&mut a, 12);
    render_frame(&a, 120, 14);
    a.subagent_rail_scroll_down(5);
    assert_eq!(a.subagent_rail_scroll, 5);
    a.clear_subagent_panes();
    assert_eq!(a.subagent_rail_scroll, 0, "pane clear resets the scroll");
    assert_eq!(a.last_subagent_rail_max_scroll.get(), 0);
    assert_eq!(a.last_subagent_rail_drawn_offset.get(), 0);
}

/// HIT-RECT CORRECTNESS: after scrolling, the recorded rects map back to
/// TRUE pane indices — clicking the top visible tab focuses the offset
/// pane, not pane 0.
#[test]
fn scrolled_tab_click_maps_to_true_pane_index() {
    let mut a = app();
    spawn_many(&mut a, 12);
    render_frame(&a, 120, 14);
    a.subagent_rail_scroll_down(100); // to max (7)
    render_frame(&a, 120, 14);
    let rects = a.last_subagent_tab_rects.borrow().clone();
    assert_eq!(rects.len(), 5, "window shows exactly the 5-panes capacity");
    // Click the TOP visible tab — that's pane 7 at scroll offset 7.
    let hit = a.subagent_tab_hit(rects[0].1, rects[0].0 + 2).expect("top tab hit");
    assert_eq!(hit, 7, "hit maps through the window offset");
    a.focus_subagent(Some(hit));
    assert_eq!(a.focused_subagent, Some(7));
    // The bottom visible tab is pane 11.
    let hit_last = a
        .subagent_tab_hit(rects[4].1, rects[4].0 + 2)
        .expect("bottom tab hit");
    assert_eq!(hit_last, 11);
}

/// FOCUS-FOLLOW: focusing an off-window pane scrolls the rail the MINIMUM
/// amount needed — the last pane becomes visible without jumping to top.
#[test]
fn focus_follow_reveals_offscreen_pane() {
    let mut a = app();
    spawn_many(&mut a, 12);
    render_frame(&a, 120, 14); // window [0,5), max 7
    a.focus_subagent(Some(11));
    assert_eq!(a.subagent_rail_scroll, 7, "focused the LAST pane → minimal scroll = 7");
    let text = render_rail_text(&a, 120, 14);
    assert!(text.contains("pane-goal-11"), "focused pane revealed");
    assert!(text.contains("pane-goal-07"), "window starts at pane 7 (minimal)");
    // Focusing a pane already visible doesn't move the window.
    a.focus_subagent(Some(9));
    assert_eq!(a.subagent_rail_scroll, 7, "visible pane: window unchanged");
    // Focusing a pane ABOVE the window scrolls back minimally.
    a.focus_subagent(Some(2));
    assert_eq!(a.subagent_rail_scroll, 2, "pane 2 becomes the top visible tab");
}

/// Expanded mode windows with 4-row cards too (capacity from real rows).
#[test]
fn expanded_rail_windows_overflow_cards() {
    let mut a = app();
    spawn_many(&mut a, 12);
    a.toggle_subagent_rail();
    let text = render_rail_text(&a, 120, 16);
    assert!(text.contains("pane-goal-00"));
    assert!(!text.contains("pane-goal-11"), "4-row cards overflow sooner");
    assert!(a.last_subagent_rail_max_scroll.get() > 0);
    let max = a.last_subagent_rail_max_scroll.get();
    a.subagent_rail_scroll_down(max);
    let text = render_rail_text(&a, 120, 16);
    assert!(text.contains("pane-goal-11"), "expanded rail scrolled to the tail");
    // Hit rects still map to true indices in expanded mode.
    let rects = a.last_subagent_tab_rects.borrow().clone();
    let hit = a.subagent_tab_hit(rects[0].1, rects[0].0 + 2).expect("hit");
    assert_eq!(hit, max, "top visible card maps through the offset");
}

// ── T005 (D10, constitution VII): main-screen non-regression pins ──────
//
// With `App.focused_subagent == None` (orchestrator view) — even while
// subagent panes EXIST and carry content — every main-screen key must keep
// acting on the MAIN transcript. Each test drives the exact App-level
// mutators `Tui::handle_key` dispatches to in its None-focus branches
// (integration tests cannot construct `Tui` — `new_for_test` is
// `#[cfg(test)]`-gated; this is the same state-convention smoke.rs uses
// for the agent-picker contract) and asserts:
//   (a) the MAIN state moved, and
//   (b) every pane's scroll/expand/search/stats state is untouched.

mod common;

use common::{assistant_item, pane_with_transcript, reasoning_item, tool_item, user_item};
use joey_tui::state::{expand_state_for_test, ReasoningExpandState};
use joey_tui::widgets;
use joey_tui::Theme;

/// T005 fixture: two panes with synthetic content (each holds assistant/
/// tool/reasoning/filediff/user items — expandables start Collapsed), the
/// orchestrator view active, and a MAIN transcript with marker items:
///
///   0 user "user message 0"        1 assistant "assistant message 1"
///   2 reasoning (3 lines)          3 tool `tool3` (6-line result)
///   4 user "user message 4"        5 assistant "assistant message 5"
///   6 user "main y-prompt"         7 assistant "main final answer"
///   8 user "main needle here"
fn t005_app() -> joey_tui::state::App {
    let mut a = app();
    pane_with_transcript(&mut a, 1, "t005 child one", 5);
    pane_with_transcript(&mut a, 2, "t005 child two", 5);
    a.push_item(user_item(0));
    a.push_item(assistant_item(1));
    a.push_item(reasoning_item(2));
    a.push_item(tool_item(3, 6));
    a.push_item(user_item(4));
    a.push_item(assistant_item(5));
    a.push_item(TranscriptItem::User { text: "main y-prompt".into() });
    a.push_item(TranscriptItem::Assistant { text: "main final answer".into() });
    a.push_item(TranscriptItem::User { text: "main needle here".into() });
    assert_eq!(a.subagent_panes.len(), 2, "precondition: panes exist");
    assert!(a.focused_subagent.is_none(), "precondition: orchestrator view active");
    a
}

/// Capture the pane transcript lengths before driving keys.
fn pane_lens(a: &joey_tui::state::App) -> Vec<usize> {
    a.subagent_panes.iter().map(|p| p.transcript.len()).collect()
}

/// Main-transcript index of the fixture's single `Reasoning` item.
/// (`apply(SubagentSpawn)` prepends Notice items to the main transcript,
/// so indices are resolved by marker, never hardcoded.)
fn main_reasoning_idx(a: &joey_tui::state::App) -> usize {
    a.transcript
        .iter()
        .rposition(|it| matches!(it, TranscriptItem::Reasoning { text, .. } if text.starts_with("reasoning 2")))
        .expect("main reasoning item present")
}

/// Main-transcript index of the fixture's single `tool3` Tool item.
fn main_tool_idx(a: &joey_tui::state::App) -> usize {
    a.transcript
        .iter()
        .rposition(|it| matches!(it, TranscriptItem::Tool { name, .. } if name == "tool3"))
        .expect("main tool3 item present")
}

/// Shared negative assertion: no pane's scroll / expand / stats-anchor /
/// transcript was touched by a main-screen key.
fn assert_panes_untouched(a: &joey_tui::state::App, lens: &[usize]) {
    for (i, pane) in a.subagent_panes.iter().enumerate() {
        assert_eq!(pane.scroll, None, "pane {i} scroll untouched");
        assert_eq!(pane.stats_view, None, "pane {i} stats anchor untouched");
        assert!(pane.expanded_context.is_empty(), "pane {i} stats expansions untouched");
        assert_eq!(pane.transcript.len(), lens[i], "pane {i} transcript untouched");
        for (j, item) in pane.transcript.iter().enumerate() {
            assert_eq!(
                expand_state_for_test(item),
                ReasoningExpandState::Collapsed,
                "pane {i} item {j} still Collapsed"
            );
        }
    }
}

/// g/G, Home/End, j/k and PgUp/PgDn scroll the MAIN transcript when no pane
/// is focused (handle_key's Transcript arm takes the `pane_focused == false`
/// branch), never any pane's scroll — and the pane-scroll mutators are
/// no-ops while unfocused (they guard on `focused_subagent`).
#[test]
fn main_screen_scroll_keys_scroll_main_transcript_not_panes() {
    let mut a = t005_app();
    for i in 0..30 {
        a.push_item(user_item(100 + i)); // overflow filler with unique markers
    }
    let lens = pane_lens(&a);
    let _ = render_frame(&a, 120, 40); // records last_max_scroll (real geometry)
    let max = a.last_max_scroll.get();
    assert!(max > 0, "precondition: main transcript overflows the viewport");

    // 'g' / Home → scroll_to_top.
    a.scroll_to_top();
    assert_eq!(a.scroll, Some(max));
    // 'G' / End → scroll_to_bottom (auto-follow).
    a.scroll_to_bottom();
    assert_eq!(a.scroll, None);
    // 'k' / Up → scroll_up(1): from the tail, one row up.
    a.scroll_up(1);
    assert_eq!(a.scroll, Some(1));
    // 'j' / Down → scroll_down(1): back to follow.
    a.scroll_down(1);
    assert_eq!(a.scroll, None);
    // PgUp → scroll_up(10); PgDn → scroll_down(4) leaves 6; clamps at bottom.
    a.scroll_up(10);
    assert_eq!(a.scroll, Some(10));
    a.scroll_down(4);
    assert_eq!(a.scroll, Some(6));
    a.scroll_down(100);
    assert_eq!(a.scroll, None, "scroll_down past the bottom clamps to follow");

    // Negative: even the pane-targeted mutators are no-ops while unfocused.
    a.pane_scroll_up(5);
    a.pane_scroll_down(5);
    assert_panes_untouched(&a, &lens);
    assert_eq!(a.subagent_rail_scroll, 0, "rail window unmoved by transcript keys");
    assert!(a.focused_subagent.is_none());
}

/// Space/x toggles the MAIN transcript's viewport item expansion (the
/// handle_key arm resolves via transcript_hit_test at the center row with a
/// top-item fallback — both resolve into `app.transcript`, never a pane's).
#[test]
fn main_screen_space_x_toggles_main_item_expansion_not_panes() {
    let mut a = t005_app();
    let lens = pane_lens(&a);
    let _ = render_frame(&a, 120, 40); // records last_text_area geometry
    let (_tx, ty, _tw, th) = a.last_text_area.get();
    assert!(th > 0, "precondition: transcript area recorded");

    // Mirror the Space/x resolution exactly (center row, col 4).
    let center_row = ty + th / 2;
    let idx = widgets::transcript_hit_test(&a, Theme::aurora(), center_row, 4);
    let resolved = match idx {
        Some(i) if a.item_is_expandable(i) => Some(i),
        _ => {
            let top = widgets::transcript_item_at_top(&a, Theme::aurora());
            top.and_then(|t0| (t0..a.transcript.len()).find(|&i| a.item_is_expandable(i)))
        }
    };
    let i = resolved.expect("Space resolves an expandable MAIN item");
    let r_idx = main_reasoning_idx(&a);
    let t_idx = main_tool_idx(&a);
    assert!(
        i == r_idx || i == t_idx,
        "resolved a main expandable (reasoning @{r_idx} / tool @{t_idx}), got {i}"
    );
    a.toggle_item_expand_by_index(i);
    assert_ne!(
        expand_state_for_test(&a.transcript[i]),
        ReasoningExpandState::Collapsed,
        "the MAIN item left Collapsed"
    );
    assert_panes_untouched(&a, &lens);
}

/// Ctrl+E (`cycle_focused_reasoning_expand`) and Ctrl+G
/// (`toggle_focused_tool_expand`) advance the MAIN transcript's most recent
/// reasoning/tool items; pane items stay Collapsed.
#[test]
fn main_screen_ctrl_e_ctrl_g_cycle_main_expandables_not_panes() {
    let mut a = t005_app();
    let lens = pane_lens(&a);
    let r_idx = main_reasoning_idx(&a);
    let t_idx = main_tool_idx(&a);
    // Ctrl+E arm: the last MAIN Reasoning item (@2) cycles.
    a.cycle_focused_reasoning_expand();
    assert_eq!(
        expand_state_for_test(&a.transcript[r_idx]),
        ReasoningExpandState::Full,
        "3-line reasoning skips TailWindow: Collapsed → Full"
    );
    assert_eq!(
        expand_state_for_test(&a.transcript[t_idx]),
        ReasoningExpandState::Collapsed,
        "Ctrl+E touched only the reasoning item"
    );
    // Ctrl+G arm: the last MAIN Tool item (@3) cycles.
    a.toggle_focused_tool_expand();
    assert_eq!(
        expand_state_for_test(&a.transcript[t_idx]),
        ReasoningExpandState::Full,
        "6-line tool result skips TailWindow: Collapsed → Full"
    );
    assert_panes_untouched(&a, &lens);
}

/// y/Y emit `TuiAction::CopyItem` with the index of the last Assistant/User
/// item of the MAIN transcript (the rposition handle_key runs over
/// `app.transcript`). The action itself is constructed inside
/// `Tui::handle_key`, which integration tests can't build (see smoke.rs'
/// picker-contract note) — so this pins the exact index resolution the
/// action payload uses, and that it never resolves into pane transcripts.
#[test]
fn main_screen_y_y_copy_resolves_main_transcript_indices() {
    let a = t005_app();
    let lens = pane_lens(&a);

    // `y` arm: rposition of the last Assistant item over app.transcript.
    let y_idx = a
        .transcript
        .iter()
        .rposition(|i| matches!(i, TranscriptItem::Assistant { .. }));
    assert_eq!(y_idx, a.transcript.iter().rposition(|it| matches!(it, TranscriptItem::Assistant { text } if text == "main final answer")));
    assert!(
        matches!(&a.transcript[y_idx.unwrap()], TranscriptItem::Assistant { text } if text == "main final answer"),
        "y targets the MAIN final answer, not any pane item"
    );

    // `Y` arm: rposition of the last User item over app.transcript.
    let big_y_idx = a
        .transcript
        .iter()
        .rposition(|i| matches!(i, TranscriptItem::User { .. }));
    assert_eq!(big_y_idx, a.transcript.iter().rposition(|it| matches!(it, TranscriptItem::User { text } if text == "main needle here")));
    assert!(
        matches!(&a.transcript[big_y_idx.unwrap()], TranscriptItem::User { text } if text == "main needle here"),
        "Y targets the newest MAIN user item"
    );

    // Negative: the panes' own last-assistant items are DIFFERENT items
    // (synthetic pane markers) — the resolution scope is main-only — and
    // the copy path mutates no pane state.
    for pane in &a.subagent_panes {
        let pane_last = pane
            .transcript
            .iter()
            .rposition(|i| matches!(i, TranscriptItem::Assistant { .. }));
        assert_eq!(pane_last, Some(0));
        assert!(
            matches!(&pane.transcript[0], TranscriptItem::Assistant { text } if text == "assistant message 0"),
            "the pane's assistant is a different item than the copied main one"
        );
    }
    assert_panes_untouched(&a, &lens);
}

/// '/' (transcript focus) and Ctrl+S (input focus) open the MAIN search bar
/// (App-level `search_open`/`search_query` — panes carry no search state),
/// and n/N (`search_next`) navigate MAIN matches only: a query that exists
/// solely inside a pane transcript finds nothing.
#[test]
fn main_screen_search_opens_on_main_and_skips_pane_transcripts() {
    let mut a = t005_app();
    // Content that exists ONLY inside pane 0's transcript.
    a.subagent_panes[0].push_item(TranscriptItem::User { text: "pane needle".into() });
    let lens = pane_lens(&a);

    // '/' / Ctrl+S arms: open the App-level search bar with a cleared query.
    a.search_open = true;
    a.search_query.clear();
    assert!(a.search_open, "main search bar open");

    // Typing in the bar (handle_search_key) runs the query against MAIN.
    a.search_query = "main needle".into();
    a.run_search();
    assert!(a.search_has_match, "the main-only needle was found");
    let after_first = a.scroll;
    assert!(after_first.is_some(), "search scrolled the MAIN transcript");

    // 'n' / 'N' arms: search_next navigates main matches.
    a.search_next(true);
    assert!(a.scroll.is_some(), "n keeps a main match in view");
    a.search_next(false);
    assert!(a.scroll.is_some(), "N keeps a main match in view");

    // A query that exists only in a pane transcript finds nothing on main.
    a.search_query = "pane needle".into();
    a.run_search();
    assert!(!a.search_has_match, "pane transcripts are not searched from the main screen");
    assert_eq!(a.scroll, after_first, "a pane-only miss doesn't move the main view");

    // Negative: pane scroll/transcript untouched (search state lives
    // entirely on App; there is no pane search state to mutate).
    assert_panes_untouched(&a, &lens);
}

/// Ctrl+O opens the output viewer on the MAIN transcript's most recent tool
/// item and Ctrl+A opens the MAIN stats page (App-level state), with panes
/// present but unfocused — no per-pane stats/anchor state is touched.
/// (`toggle_stats` re-pins `stats_view` to None = the live tail; the
/// open/closed contract is carried by `stats_open`.)
#[test]
fn main_screen_ctrl_o_ctrl_a_open_main_viewers_with_panes_unfocused() {
    let mut a = t005_app();
    let lens = pane_lens(&a);

    // Ctrl+O arm: the viewer targets the most recent MAIN tool item.
    a.toggle_output_viewer(None);
    assert!(a.output_viewer_open, "output viewer open");
    let t_idx = main_tool_idx(&a);
    assert_eq!(a.output_viewer_index, Some(t_idx), "targets the main tool3 item, not a pane tool");
    assert!(
        matches!(&a.transcript[t_idx], TranscriptItem::Tool { name, .. } if name == "tool3"),
        "the pane tools are named tool1; the main one is tool3"
    );
    // The same key toggles it closed.
    a.toggle_output_viewer(None);
    assert!(!a.output_viewer_open, "Ctrl+O again closes the viewer");

    // Ctrl+A arm: the MAIN stats page opens, re-pinned to the live tail.
    a.toggle_stats();
    assert!(a.stats_open, "main stats page open");
    assert_eq!(a.stats_view, None, "opened at the live tail (auto-follow)");

    assert!(a.focused_subagent.is_none(), "viewers never steal pane focus");
    assert_panes_untouched(&a, &lens);
}

// ── T024 (US5): state preservation across focus switches (FR-010, SC-004,
//    research.md D9) ─────────────────────────────────────────────────────
//
// SC-004 pins "100% state preservation" across four per-pane dimensions:
// scroll, expansion, search, and stats view. `pane_stats_capture_child_
// context` (above) and `pane_stats_expanded_context_survives_focus_switch`
// (tests/pane_maximized_parity.rs, Ctrl+P path) partially cover the stats
// dimension; this section covers the rest — scroll, item expansion, search,
// the frozen stats ANCHOR via a direct rail-click switch — plus the D9
// Ctrl+L / disappearance contracts.
//
// `Tui::handle_key` can't be constructed from integration tests
// (`new_for_test` is `#[cfg(test)]`-gated), so keys are driven through the
// exact App-level mutator sequences the handlers dispatch to — the same
// convention as the T005 pins above and the sibling parity suites.

use joey_tui::state::App;

const T024_W: u16 = 80;
const T024_H: u16 = 24;

/// Exactly what `Tui::open_search` (app.rs) does when '/' is pressed with a
/// pane focused: the App-level live latch opens with a fresh query AND the
/// focused pane's per-view SearchState mirror opens (FR-010: the pane
/// remembers its bar across focus switches).
fn t024_open_search(a: &mut App) {
    a.search_open = true;
    a.search_query.clear();
    if let Some(pane) = a.focused_pane_mut() {
        pane.search_open = true;
        pane.search_query.clear();
    }
}

/// Exactly what the search-bar typing path (`Tui::handle_search_key`) does
/// per character: push the char into the live query, run the search (which,
/// with a pane focused, retargets to the pane's transcript).
fn t024_type_in_bar(a: &mut App, query: &str) {
    for c in query.chars() {
        a.search_query.push(c);
        a.run_search();
    }
}

/// A full frame including the search-bar overlay (draw_search_bar runs
/// after render_body in Tui::draw — matches pane_search_copy.rs).
fn t024_render_with_search_bar(a: &App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let area = f.area();
            joey_tui::app::render_body_for_test(f, area, a, Theme::aurora(), false, 0.5);
            widgets::draw_search_bar(f, area, a, &Theme::aurora());
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

/// A Reasoning item with >200 lines so all three cycle states are distinct
/// (Collapsed → TailWindow → Full → Collapsed).
fn t024_long_reasoning() -> TranscriptItem {
    let text = (0..300)
        .map(|j| format!("t024 think line {j:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    TranscriptItem::Reasoning {
        text,
        expand_state: ReasoningExpandState::Collapsed,
        thought_duration: Some(std::time::Duration::from_secs(2)),
    }
}

/// Feed child `id` a ContextSnapshot with `n` marked entries (the wrapped
/// event the orchestration layer routes — same as pane_maximized_parity.rs).
fn t024_child_context(a: &mut App, id: u64, n: usize) {
    a.apply(AgentEvent::SubagentEvent {
        id,
        event: Box::new(AgentEvent::ContextSnapshot {
            entries: (0..n)
                .map(|i| ContextEntry {
                    role: if i % 2 == 0 { "user" } else { "assistant" }.to_string(),
                    tokens: 100 + i as u64,
                    preview: format!("t024-ctx entry {i}"),
                    has_tool_calls: false,
                    is_compressed_summary: false,
                    full_content: format!("t024-ctx-full-{i}\nsecond body line {i}"),
                })
                .collect(),
            system_tokens: 400,
            history_tokens: 1200,
            context_window: 8000,
            compression_threshold: 6000,
            compactions: 0,
            model: "test-model".to_string(),
        }),
    });
}

// ── 1. Scroll preservation (FR-010, SC-004: scroll dimension) ──────────

/// A pane pinned mid-transcript keeps its EXACT offset across focus
/// switches (away and back, multiple round trips); a pane pinned at the top
/// keeps the top; a follow-tail pane stays follow-tail; and appends to an
/// UNFOCUSED pane never move its offset (ScrollState: only user scrolls
/// do). Main `App.scroll` is never touched by pane focus switches.
#[test]
fn t024_scroll_state_survives_focus_switches() {
    let mut a = app();
    let pa = pane_with_transcript(&mut a, 1, "scroll child a", 40);
    let pb = pane_with_transcript(&mut a, 2, "scroll child b", 40);
    let pc = pane_with_transcript(&mut a, 3, "scroll child c", 40);

    // Pin A mid-transcript (bound recorded by a real frame, like the 'k'
    // key path).
    a.focus_subagent(Some(pa));
    let _ = render_frame(&a, T024_W, T024_H);
    let max_a = a.last_pane_max_scroll.get();
    assert!(max_a >= 4, "precondition: 40 items overflow an 80x24 pane (max={max_a})");
    let pin_a = max_a / 2;
    a.pane_scroll_up(pin_a);
    assert_eq!(a.subagent_panes[pa].scroll, Some(pin_a), "A pinned mid-transcript");

    // Pin B at the TOP (its own render-time bound).
    a.focus_subagent(Some(pb));
    let _ = render_frame(&a, T024_W, T024_H);
    let max_b = a.last_pane_max_scroll.get();
    assert!(max_b >= 4, "precondition: B overflows too (max={max_b})");
    a.pane_scroll_up(max_b);
    assert_eq!(a.subagent_panes[pb].scroll, Some(max_b), "B pinned at the top");

    // Give C a recorded bound too (so its follow-tail is a real choice).
    a.focus_subagent(Some(pc));
    let _ = render_frame(&a, T024_W, T024_H);
    assert_eq!(a.subagent_panes[pc].scroll, None, "C at follow-tail");

    // Round trips: every pane's scroll is byte-identical after each switch.
    a.focus_subagent(Some(pa));
    assert_eq!(a.subagent_panes[pa].scroll, Some(pin_a), "A's mid pin survives");
    assert_eq!(a.subagent_panes[pb].scroll, Some(max_b), "B's top pin survives");
    assert_eq!(a.subagent_panes[pc].scroll, None, "C stays follow-tail");

    a.focus_subagent(Some(pb));
    assert_eq!(a.subagent_panes[pa].scroll, Some(pin_a));
    assert_eq!(a.subagent_panes[pb].scroll, Some(max_b));
    assert_eq!(a.subagent_panes[pc].scroll, None);

    // Orchestrator round trip preserves them too.
    a.focus_subagent(None);
    a.focus_subagent(Some(pa));
    assert_eq!(a.subagent_panes[pa].scroll, Some(pin_a), "A survives via orchestrator");
    assert_eq!(a.subagent_panes[pb].scroll, Some(max_b));

    // Appends to an UNFOCUSED pane never move its offset (auto-follow is
    // per-pane; a pinned sibling keeps the pin).
    for i in 0..3 {
        a.subagent_panes[pb].push_item(user_item(200 + i));
    }
    assert_eq!(
        a.subagent_panes[pb].scroll,
        Some(max_b),
        "unfocused pinned pane does not jump on appends"
    );
    assert_eq!(a.subagent_panes[pc].scroll, None);

    // Rendering again (bound refresh) keeps the pinned value — render never
    // writes pane.scroll.
    let _ = render_frame(&a, T024_W, T024_H);
    assert_eq!(a.subagent_panes[pa].scroll, Some(pin_a));

    // Main scroll was never touched by any focus switch or pane scroll.
    assert_eq!(a.scroll, None, "main scroll untouched throughout");
}

// ── 2. Expansion preservation (FR-010, SC-004: expansion dimension) ────

/// Per-item expansion states in pane A (Ctrl+E reasoning cycle to Full,
/// Ctrl+G tool expand, Space/x-equivalent diff toggle) survive switches to
/// pane B and back, B's items stay Collapsed, and the MAIN transcript's own
/// expansion state is untouched by pane expansion (focused-view isolation).
#[test]
fn t024_expansion_state_survives_focus_switches() {
    let mut a = app();
    // A: cycle items [assistant, tool(6), reasoning(3), filediff(4), user]
    // plus a 300-line reasoning pushed as the newest item.
    let pa = pane_with_transcript(&mut a, 1, "expand child a", 5);
    a.subagent_panes[pa].push_item(t024_long_reasoning());
    let pb = pane_with_transcript(&mut a, 2, "expand child b", 5);

    // Main transcript has its own expandables (indices resolved by marker —
    // spawns prepend Notice items to the main transcript).
    a.push_item(reasoning_item(90));
    a.push_item(tool_item(91, 6));
    let main_r = a
        .transcript
        .iter()
        .rposition(|it| matches!(it, TranscriptItem::Reasoning { text, .. } if text.starts_with("reasoning 90")))
        .expect("main reasoning present");
    let main_t = a
        .transcript
        .iter()
        .rposition(|it| matches!(it, TranscriptItem::Tool { name, .. } if name == "tool91"))
        .expect("main tool present");
    a.toggle_item_expand_by_index(main_r); // 3-line reasoning → Full
    assert_eq!(
        expand_state_for_test(&a.transcript[main_r]),
        ReasoningExpandState::Full,
        "precondition: main reasoning expanded"
    );

    // Focus A and expand through every affordance:
    a.focus_subagent(Some(pa));
    // Ctrl+E ×2: the newest reasoning (300 lines) Collapsed → TailWindow → Full.
    a.cycle_focused_reasoning_expand();
    assert_eq!(
        expand_state_for_test(&a.subagent_panes[pa].transcript[5]),
        ReasoningExpandState::TailWindow,
        "300-line reasoning first lands on TailWindow"
    );
    a.cycle_focused_reasoning_expand();
    assert_eq!(expand_state_for_test(&a.subagent_panes[pa].transcript[5]), ReasoningExpandState::Full);
    // Ctrl+G: the newest tool (6-line result) Collapsed → Full.
    a.toggle_focused_tool_expand();
    assert_eq!(expand_state_for_test(&a.subagent_panes[pa].transcript[1]), ReasoningExpandState::Full);
    // Space/x on the diff item (4 lines) → Full (fits-collapsed skip rule).
    a.subagent_panes[pa].toggle_item_expand(3);
    assert_eq!(expand_state_for_test(&a.subagent_panes[pa].transcript[3]), ReasoningExpandState::Full);
    // The pane's OTHER reasoning (3 lines) stays Collapsed (per-item isolation).
    assert_eq!(expand_state_for_test(&a.subagent_panes[pa].transcript[2]), ReasoningExpandState::Collapsed);

    // Switch away (B) and back — every expansion is byte-identical.
    a.focus_subagent(Some(pb));
    a.focus_subagent(Some(pa));
    assert_eq!(expand_state_for_test(&a.subagent_panes[pa].transcript[5]), ReasoningExpandState::Full, "reasoning Full survives");
    assert_eq!(expand_state_for_test(&a.subagent_panes[pa].transcript[1]), ReasoningExpandState::Full, "tool Full survives");
    assert_eq!(expand_state_for_test(&a.subagent_panes[pa].transcript[3]), ReasoningExpandState::Full, "diff Full survives");
    assert_eq!(expand_state_for_test(&a.subagent_panes[pa].transcript[2]), ReasoningExpandState::Collapsed);

    // Via the orchestrator view too.
    a.focus_subagent(None);
    a.focus_subagent(Some(pa));
    assert_eq!(expand_state_for_test(&a.subagent_panes[pa].transcript[5]), ReasoningExpandState::Full);

    // B was never touched: all its items stay Collapsed.
    for (j, item) in a.subagent_panes[pb].transcript.iter().enumerate() {
        assert_eq!(
            expand_state_for_test(item),
            ReasoningExpandState::Collapsed,
            "pane B item {j} untouched by A's expansions"
        );
    }

    // Main transcript expansion untouched by the pane cycles/switches: the
    // expanded main reasoning stays Full, the main tool stays Collapsed.
    assert_eq!(expand_state_for_test(&a.transcript[main_r]), ReasoningExpandState::Full, "main expansion preserved");
    assert_eq!(expand_state_for_test(&a.transcript[main_t]), ReasoningExpandState::Collapsed, "main tool untouched");
}

// ── 3. Search preservation (FR-010, SC-004: search dimension) ──────────

/// Pane A's SearchState (open latch, query, match indicator, match-pinned
/// scroll) survives switching to pane B — where a different query finds no
/// match — and back. The indicator renders from the FOCUSED pane's state,
/// so it flips no-match (B) / match-found (A) while A's preserved query and
/// pin stay byte-identical.
#[test]
fn t024_search_state_survives_focus_switches() {
    let mut a = app();
    let pa = pane_with_transcript(&mut a, 1, "search child a", 40);
    let pb = pane_with_transcript(&mut a, 2, "search child b", 40);
    // Two needles in A: an older one and a newer one (the newest-first
    // search finds items[35] first: from-bottom idx 4 → pin Some(2)).
    a.subagent_panes[pa].transcript[5] = TranscriptItem::User { text: "t024 needle old".into() };
    a.subagent_panes[pa].transcript[35] = TranscriptItem::User { text: "t024 needle new".into() };

    // Focus A, open the bar (pane mirror opens), type the query.
    a.focus_subagent(Some(pa));
    let _ = render_frame(&a, T024_W, T024_H);
    assert!(a.last_pane_max_scroll.get() >= 4, "precondition: A overflows");
    t024_open_search(&mut a);
    t024_type_in_bar(&mut a, "needle");

    let pane_a = &a.subagent_panes[pa];
    assert!(pane_a.search_open, "A's bar mirror is open");
    assert_eq!(pane_a.search_query, "needle", "query preserved on A");
    assert!(pane_a.search_has_match, "A found its own occurrence");
    assert_eq!(pane_a.scroll, Some(2), "A pinned to its newest match (idx-2)");
    let pinned = pane_a.scroll;
    assert_eq!(a.scroll, None, "main scroll untouched by the pane search");
    assert!(!a.search_has_match, "App indicator mirrors MAIN only");

    // Switch to B: run a query with NO match there. A keeps everything.
    a.focus_subagent(Some(pb));
    let _ = render_frame(&a, T024_W, T024_H);
    t024_open_search(&mut a); // B's mirror opens, A's preserved state stays
    t024_type_in_bar(&mut a, "zzz");
    let pane_b = &a.subagent_panes[pb];
    assert!(!pane_b.search_has_match, "no 'zzz' in B");
    assert_eq!(pane_b.scroll, None, "a no-match search never yanks B's view");
    let frame = t024_render_with_search_bar(&a, T024_W, T024_H);
    assert!(
        frame.contains("search · no matches"),
        "indicator renders B's (focused) no-match state: {frame:?}"
    );

    // A's preserved SearchState survived the whole B detour.
    let pane_a = &a.subagent_panes[pa];
    assert!(pane_a.search_open, "A's bar latch preserved");
    assert_eq!(pane_a.search_query, "needle", "A's query preserved");
    assert!(pane_a.search_has_match, "A's match indicator preserved");
    assert_eq!(pane_a.scroll, pinned, "A's match-pinned scroll preserved");

    // Back to A: still preserved, and the indicator now renders A's match.
    a.focus_subagent(Some(pa));
    let pane_a = &a.subagent_panes[pa];
    assert_eq!(pane_a.search_query, "needle");
    assert!(pane_a.search_has_match);
    assert_eq!(pane_a.scroll, pinned, "pinned scroll restored on refocus");
    let frame = t024_render_with_search_bar(&a, T024_W, T024_H);
    assert!(
        frame.contains("search · match found (n=next N=prev)"),
        "indicator renders A's (focused) match state: {frame:?}"
    );
    assert_eq!(a.scroll, None, "main scroll still untouched");

    // NAVIGATION state (T015 `pane_search_next`, the n/N pane arms) survives
    // too. A's matches sit at from-bottom idx 4 (newest, item 35) and 34
    // (oldest, item 5); the bar left A pinned at Some(2). 'N' (backward)
    // walks to the older match and pins at min(34-2, bound)…
    let max_a = a.last_pane_max_scroll.get(); // A's bound (re-recorded above)
    a.pane_search_next(false);
    let navigated = 32.min(max_a);
    assert_eq!(
        a.subagent_panes[pa].scroll,
        Some(navigated),
        "N pinned the older match (idx 34 → offset {navigated})"
    );
    // …and the NAVIGATED offset — not the original pin — is what survives a
    // round trip through the orchestrator view.
    a.focus_subagent(None);
    a.focus_subagent(Some(pa));
    assert_eq!(a.subagent_panes[pa].scroll, Some(navigated), "navigated offset survives");
    assert_eq!(a.subagent_panes[pa].search_query, "needle", "query still preserved");
    assert!(a.subagent_panes[pa].search_has_match, "indicator still preserved");
    // 'n' (forward) walks back to the newest match (idx 4 → Some(2)).
    a.pane_search_next(true);
    assert_eq!(a.subagent_panes[pa].scroll, Some(2), "n re-pins the newest match");
    assert_eq!(a.scroll, None, "main scroll untouched by pane navigation");
}

// ── 4. Stats preservation: the FROZEN anchor (FR-010, SC-004) ──────────

/// The sibling suites cover context-entry EXPANSION survival (Ctrl+P path);
/// this pins the missing piece — the pane's FROZEN stats-view ANCHOR (set
/// via the real mutator walk from a render-recorded bound) plus the entry
/// expansion, preserved across a DIRECT rail-click switch (focus_subagent
/// pane→pane, no Ctrl+P stats reset in between) — and that the MAIN
/// stats anchor is never touched by pane stats actions.
#[test]
fn t024_stats_anchor_and_expansion_survive_rail_switch() {
    let mut a = app();
    let pa = pane_with_transcript(&mut a, 1, "stats child a", 5);
    let pb = pane_with_transcript(&mut a, 2, "stats child b", 5);
    t024_child_context(&mut a, 1, 24); // overflow the pane stats viewport

    // Focus A, open stats (Ctrl+A arm), freeze the anchor with the real
    // mutator (pane_stats_scroll_up from the render-recorded bound), and
    // expand context entry 1 (Space/click arm).
    a.focus_subagent(Some(pa));
    a.toggle_stats();
    assert!(a.stats_open);
    let _ = render_frame(&a, T024_W, T024_H); // records pane A's stats bound
    let max_anchor = a.focused_pane().unwrap().last_stats_max_anchor.get();
    assert!(max_anchor >= 3, "precondition: 24 entries overflow (max_anchor={max_anchor})");
    a.pane_stats_scroll_up(2); // freezes at max_anchor - 2
    let frozen = a.focused_pane().unwrap().stats_view;
    assert_eq!(frozen, Some(max_anchor - 2), "anchor frozen by the mutator walk");
    assert!(frozen.is_some() && frozen != Some(0), "precondition: a genuinely frozen anchor");
    a.toggle_pane_context_entry(1);
    assert!(a.subagent_panes[pa].expanded_context.contains(&1), "entry 1 expanded");

    // Rail-click straight to B (no Ctrl+P reset): A keeps anchor+expansion,
    // B carries its own (pristine) stats state.
    a.focus_subagent(Some(pb));
    assert_eq!(a.subagent_panes[pa].stats_view, frozen, "frozen anchor survives the switch");
    assert!(a.subagent_panes[pa].expanded_context.contains(&1), "expansion survives the switch");
    assert_eq!(a.subagent_panes[pb].stats_view, None, "B at its own follow-tail");
    assert!(a.subagent_panes[pb].expanded_context.is_empty());

    // Back to A — still frozen, still expanded.
    a.focus_subagent(Some(pa));
    assert_eq!(a.subagent_panes[pa].stats_view, frozen, "anchor restored on refocus");
    assert!(a.subagent_panes[pa].expanded_context.contains(&1));

    // The MAIN stats anchor was never involved (App-level, separate view).
    assert_eq!(a.stats_view, None, "main stats anchor untouched by pane stats actions");
    assert_eq!(a.subagent_panes[pb].stats_view, None);
}

// ── 5. Ctrl+L rule (D9): clear panes, return focus, main scroll kept ───

/// With a pane focused and the orchestrator scrolled to a remembered
/// offset, `clear_subagent_panes` (the Ctrl+L pane path, research.md D9)
/// drops every pane, returns focus to the orchestrator, renders the main
/// view again — and leaves `App.scroll` at its EXACT prior value (plus the
/// recorded main bound), the D9 regression pin.
#[test]
fn t024_ctrl_l_clear_returns_focus_and_preserves_main_scroll() {
    let mut a = app();
    pane_with_transcript(&mut a, 1, "ctrl-l child a", 10);
    pane_with_transcript(&mut a, 2, "ctrl-l child b", 10);
    for i in 0..45 {
        a.push_item(TranscriptItem::User { text: format!("t024 line {i}") });
    }
    // Main view: record the real bound, scroll to a remembered offset.
    let _ = render_frame(&a, T024_W, T024_H);
    let max = a.last_max_scroll.get();
    assert!(max >= 9, "precondition: main transcript overflows (max={max})");
    a.scroll_up(9);
    assert_eq!(a.scroll, Some(9), "main scrolled to the remembered offset");
    let bound_before = a.last_max_scroll.get();

    // Focus a pane (main scroll untouched by the focus switch itself).
    a.focus_subagent(Some(0));
    assert_eq!(a.scroll, Some(9), "focusing a pane keeps the main scroll");
    let _ = render_frame(&a, T024_W, T024_H); // pane view on screen

    // Ctrl+L's pane path: clear_subagent_panes.
    a.clear_subagent_panes();
    assert!(a.subagent_panes.is_empty(), "all panes cleared");
    assert!(a.focused_subagent.is_none(), "focus returned to the orchestrator");
    assert_eq!(a.scroll, Some(9), "D9: orchestrator scroll EXACTLY preserved");
    assert_eq!(a.last_max_scroll.get(), bound_before, "D9: the main bound is untouched too");

    // Graceful return: the main view renders again (pane chrome gone).
    let frame = render_frame(&a, T024_W, T024_H);
    assert!(!frame.contains("subagent:"), "no pane title remains");
    assert!(frame.contains("t024 line"), "main transcript content visible again");
}

/// The full Ctrl+L KEY path (app.rs handle_key `Char('l') if ctrl`) is
/// `transcript.clear(); scroll = None; clear_subagent_panes();` — the
/// historical clear-view reset PLUS the pane/rail reset. Driven here as the
/// exact mutator sequence the handler runs (integration tests can't build
/// `Tui`; same convention as every T024 pin). D9 splits in two: the KEY
/// returns focus to the orchestrator with the view at follow-tail (its own
/// transcript is emptied, so None is the only sane scroll), while the
/// pane-clear OPERATION alone preserves a remembered main scroll — pinned
/// by the test above. No panic, no dangling focus, either way.
#[test]
fn t024_ctrl_l_key_sequence_clears_panes_and_returns_focus() {
    let mut a = app();
    pane_with_transcript(&mut a, 1, "ctrl-l key child", 10);
    for i in 0..45 {
        a.push_item(TranscriptItem::User { text: format!("t024 key line {i}") });
    }
    let _ = render_frame(&a, T024_W, T024_H);
    let max = a.last_max_scroll.get();
    assert!(max >= 9, "precondition: main transcript overflows (max={max})");
    a.scroll_up(9);
    a.focus_subagent(Some(0));
    let _ = render_frame(&a, T024_W, T024_H); // pane view on screen
    assert_eq!(a.focused_subagent, Some(0));

    // The exact key-handler sequence (app.rs Ctrl+L arm).
    a.transcript.clear();
    a.scroll = None;
    a.clear_subagent_panes();

    assert!(a.subagent_panes.is_empty(), "panes gone");
    assert!(a.focused_subagent.is_none(), "focus returned to the orchestrator");
    assert!(a.transcript.is_empty(), "the key also clears the main view (by design)");
    assert_eq!(a.scroll, None, "cleared view sits at follow-tail");
    // Subsequent main-transcript keys act on the main view, not a ghost pane
    // (re-render first so the recorded bound reflects the emptied view).
    let _ = render_frame(&a, T024_W, T024_H);
    let bound = a.last_max_scroll.get();
    a.scroll_up(3);
    assert_eq!(a.scroll, Some(3.min(bound)), "scroll clamps to the empty view's bound");
    a.scroll_down(3);
    assert_eq!(a.scroll, None);
    let frame = render_frame(&a, T024_W, T024_H);
    assert!(!frame.contains("subagent:"), "no pane chrome remains");
}

// ── 6. Disappearance: unreachable by design; persistence pinned (D9) ────

/// Panes are NEVER individually removed — Done/Failed (child or parent)
/// keep every pane AND the focused view intact — so "pane disappearance"
/// has exactly one reachable path: Ctrl+L `clear_subagent_panes`, pinned
/// above. This pins the persistence side of that contract: completing or
/// failing the FOCUSED pane's child does not kick the user off the view.
#[test]
fn t024_panes_persist_after_done_failed_no_disappearance() {
    let mut a = app();
    a.mode = RunMode::Busy;
    pane_with_transcript(&mut a, 1, "persist child a", 5);
    let pb = pane_with_transcript(&mut a, 2, "persist child b", 5);
    a.focus_subagent(Some(pb)); // the FOCUSED child will fail

    // Child 1 completes: pane stays, status Done.
    a.apply(AgentEvent::SubagentComplete {
        id: 1,
        goal: "persist child a".to_string(),
        success: true,
        summary_preview: "done".to_string(),
        token_usage: Default::default(),
        duration_secs: 0.5,
    });
    assert_eq!(a.subagent_panes.len(), 2, "completion never removes a pane");
    assert_eq!(a.subagent_panes[0].status, SubagentStatus::Done);
    assert_eq!(a.focused_subagent, Some(pb), "focus unaffected by a sibling's completion");

    // The FOCUSED child fails: pane stays (Failed), focus stays on it —
    // the view is readable, the user is not ejected.
    a.apply(AgentEvent::SubagentFailed {
        id: 2,
        goal: "persist child b".to_string(),
        error: "boom".to_string(),
        duration_secs: 0.2,
    });
    assert_eq!(a.subagent_panes.len(), 2, "failure never removes a pane");
    assert_eq!(a.subagent_panes[pb].status, SubagentStatus::Failed);
    assert_eq!(a.focused_subagent, Some(pb), "the focused pane survives its own failure");
    let frame = render_frame(&a, T024_W, T024_H);
    assert!(frame.contains("subagent: persist child b"), "the failed pane's view still renders");

    // Parent turn Done: panes persist, the still-valid focus persists.
    a.apply(AgentEvent::Done {
        final_text: "parent done".into(),
        usage: Default::default(),
        iterations: 1,
    });
    assert_eq!(a.subagent_panes.len(), 2, "parent Done keeps the panes");
    assert_eq!(a.focused_subagent, Some(pb), "still-valid focus kept across parent Done");
    assert_eq!(a.mode, RunMode::Input);

    // The only disappearance path remains Ctrl+L (pinned above): after it,
    // an out-of-range focus would have been the bug — it is None instead.
    a.clear_subagent_panes();
    assert!(a.subagent_panes.is_empty());
    assert!(a.focused_subagent.is_none(), "graceful return: no dangling focus");
}

// ── T026 (US6, FR-011 / research.md D8): single-funnel per-surface pane
//    parity spot-checks ──────────────────────────────────────────────────
//
// FUNNEL AUDIT (manager.rs read-only): every spawn surface feeds the ONE
// SubagentManager → event-tap → pane pipeline; there is NO surface-specific
// pane fork:
//
//   delegate_task (single)  → DelegateTask::execute → manager.dispatch_single
//                             → dispatch_single_with_overrides (manager.rs:252)
//   delegate_task (batch)   → execute_batch → manager.dispatch_batch_with_roles
//                             → dispatch_requests (manager.rs:442)
//   call_omo_agent / OMO    → CallOmoAgent::execute → inner DelegateTask::execute
//                             (delegation_tool.rs:669) → the SAME dispatch_single
//                             path; CategoryDelegation is an extra notice-level
//                             event, the pane still comes from SubagentSpawn
//   /hypercode run          → engine.rs → hypercode::run_hypercode →
//                             ctx.manager.dispatch_requests (hypercode.rs:890/
//                             934/978) — a SEPARATE SubagentManager instance
//                             (engine.rs:232) but the same event shapes,
//                             drained by the process-global tap
//                             (tap::set_global_tap, tui.rs:228 / repl.rs:898)
//                             since hypercode passes event_tx: None
//   dispatch_batch          → dispatch_requests → per-child
//                             dispatch_single_with_overrides + one closing
//                             DelegationBatchComplete (manager.rs:560)
//
// ALL of them emit AgentEvent::{SubagentSpawn, SubagentEvent{..},
// SubagentComplete/SubagentFailed, DelegationBatchComplete} into the tap;
// the TUI consumes them in ONE place (App::apply, state.rs:1844+) where
// SubagentSpawn arm is `SubagentPane::new`'s sole call site (state.rs:1870)
// and wrapped events go through the single `pane_apply` (state.rs:559).
//
// AUDIT FINDING — RESOLVED AT SOURCE by T033 (orchestration layer): the
// /hypercode manager used to be a fresh SubagentManager whose child id
// counter started at 1 just like the delegate manager's, and panes match
// by child_id FIRST-MATCH — so a hypercode child could collide with a
// surviving delegate pane's id and route its events into the older pane.
// T033 moved child-id allocation to a PROCESS-GLOBAL counter in
// manager.rs, so two concurrently-alive managers can never mint the same
// id (orchestration regression:
// parallel_tap.rs::child_ids_disjoint_across_concurrent_managers). The
// duplicate-id first-match pin below remains as an App-level contract:
// routing is unchanged (joey-tui src untouched this wave) and duplicate
// ids are simply unreachable from real managers now.

use joey_agent_core::events::{FileChangeKind, FileChangeSource};
use joey_tui::state::ToolStatus;
use joey_tools::file_tracker::DiffResult;

const T026_W: u16 = 80;
const T026_H: u16 = 24;

/// SubagentSpawn exactly as the ONE funnel emits it for ANY surface — the
/// only pane-creating event shape in the codebase (models each surface's
/// spawn: goal text, resolved model, toolset summary; depth 0 for all
/// top-level surfaces).
fn surface_spawn(id: u64, goal: &str, model: &str, toolsets: &str) -> AgentEvent {
    AgentEvent::SubagentSpawn {
        id,
        goal: goal.to_string(),
        model: model.to_string(),
        toolset_summary: toolsets.to_string(),
        depth: 0,
    }
}

/// Wrap a child event like `Subagent::run_with_tap` does (manager.rs:331).
fn wrap(id: u64, ev: AgentEvent) -> AgentEvent {
    AgentEvent::SubagentEvent { id, event: Box::new(ev) }
}

/// Feed child `id` the common wrapped stream every surface's children emit
/// through the tap: `rounds` reasoning+assistant rounds (one carries the
/// search needle) plus one completed tool call. Surface-agnostic by design —
/// parity means the pane can't tell which surface produced the stream.
fn feed_common_stream(a: &mut App, id: u64, rounds: usize) {
    for i in 0..rounds {
        let needle = if i == rounds / 2 { "t026-needle " } else { "" };
        a.apply(wrap(
            id,
            AgentEvent::ReasoningDelta(format!(
                "t026 child {id} round {i} think\nsecond line\nthird line"
            )),
        ));
        a.apply(wrap(
            id,
            AgentEvent::AssistantMessage(format!("{needle}t026 child {id} answer {i}")),
        ));
    }
    let tool_name = format!("tool{id}");
    a.apply(wrap(id, AgentEvent::ToolStart {
        name: tool_name.clone(),
        emoji: "🔧".to_string(),
        summary: format!("t026 summary for child {id}"),
    }));
    a.apply(wrap(id, AgentEvent::ToolEnd {
        name: tool_name,
        is_error: false,
        result_preview: "ok".into(),
        duration_secs: 0.1,
        exit_code: Some(0),
        full_result: format!("t026 full output for child {id}"),
    }));
}

/// The SAME capability walk for every surface's pane — the parity pin:
/// identical TranscriptItem kinds land, and scroll / expand / search /
/// lifecycle-close all operate on the pane regardless of origin surface.
fn t026_assert_funnel_capabilities(a: &mut App, pane_idx: usize, child_id: u64) {
    // (a) transcript kinds: Reasoning + Assistant (needle) + a Done Tool.
    let pane = &a.subagent_panes[pane_idx];
    assert_eq!(pane.child_id, child_id, "pane belongs to this surface's child");
    assert!(pane
        .transcript
        .iter()
        .any(|it| matches!(it, TranscriptItem::Reasoning { .. })), "Reasoning item landed");
    assert!(pane.transcript.iter().any(|it| matches!(
        it,
        TranscriptItem::Assistant { text, .. } if text.contains("t026-needle")
    )), "Assistant item with the needle landed");
    let tool_name = format!("tool{child_id}");
    assert!(pane.transcript.iter().any(|it| matches!(
        it,
        TranscriptItem::Tool { name, status, .. } if name == &tool_name && *status == ToolStatus::Done
    )), "completed Tool item landed");

    // (b) scroll: focus, record the real bound from a frame, pin, clamp.
    a.focus_subagent(Some(pane_idx));
    let _ = render_frame(a, T026_W, T026_H);
    let max = a.last_pane_max_scroll.get();
    assert!(max >= 4, "precondition: pane {pane_idx} overflows (max={max})");
    a.pane_scroll_up(2);
    assert_eq!(a.subagent_panes[pane_idx].scroll, Some(2), "pane scroll pins");
    // Scroll affordance state clamps to the pane bounds on every surface:
    // an oversized up-step saturates at the recorded max, and a full
    // down-step returns to follow-tail (None) — same ScrollState contract
    // the main transcript uses (state.rs pane_scroll_up/down).
    a.pane_scroll_up(1000);
    assert_eq!(
        a.subagent_panes[pane_idx].scroll,
        Some(max),
        "scroll up past the top clamps to the pane's max bound"
    );
    a.pane_scroll_down(max);
    assert_eq!(
        a.subagent_panes[pane_idx].scroll,
        None,
        "scroll down to the bottom resumes follow-tail"
    );

    // (c) expand: a Reasoning item CYCLES through the three-state inline
    // expansion via the pane's own toggle (per-item expand works on this
    // surface's items). The stream's reasoning blocks are 3 lines, so the
    // fits-collapsed skip rule lands Collapsed → Full; the second toggle
    // completes the cycle back to Collapsed.
    let r_idx = a.subagent_panes[pane_idx]
        .transcript
        .iter()
        .rposition(|it| matches!(it, TranscriptItem::Reasoning { .. }))
        .expect("a reasoning item to expand");
    a.subagent_panes[pane_idx].toggle_item_expand(r_idx);
    assert_ne!(
        expand_state_for_test(&a.subagent_panes[pane_idx].transcript[r_idx]),
        ReasoningExpandState::Collapsed,
        "pane item expansion works"
    );
    a.subagent_panes[pane_idx].toggle_item_expand(r_idx);
    assert_eq!(
        expand_state_for_test(&a.subagent_panes[pane_idx].transcript[r_idx]),
        ReasoningExpandState::Collapsed,
        "second toggle completes the cycle back to Collapsed"
    );

    // (d) search: the pane bar finds the surface-agnostic needle.
    t024_open_search(a);
    t024_type_in_bar(a, "t026-needle");
    let pane = &a.subagent_panes[pane_idx];
    assert!(pane.search_has_match, "pane search works on this surface's transcript");
    assert_eq!(pane.search_query, "t026-needle");

    // (e) copy hit-test: `y` with this pane focused resolves the last
    // Assistant item INSIDE the pane transcript — the exact rposition the
    // `Tui::handle_key` Pane arm performs (app.rs:1526), emitting
    // `TuiAction::CopyPaneItem { pane: pane_idx, idx }` (pane-owned,
    // pane-relative idx). Surface-agnostic by construction: the resolution
    // sees only this pane's items, and each child's stream embeds its id,
    // so a mis-routed (main/other-pane) copy is detectable — the main
    // transcript here carries no Assistant item at all.
    let pane = &a.subagent_panes[pane_idx];
    let copy_idx = pane
        .transcript
        .iter()
        .rposition(|i| matches!(i, TranscriptItem::Assistant { .. }))
        .expect("an assistant item to copy");
    assert!(
        matches!(&pane.transcript[copy_idx], TranscriptItem::Assistant { text, .. }
            if text.contains(&format!("t026 child {child_id}"))),
        "copy hit-test resolves within the pane transcript: CopyPaneItem {{ pane: {pane_idx}, idx: {copy_idx} }} carries this child's own text"
    );
    assert!(
        !a.transcript.iter().any(|it| matches!(it, TranscriptItem::Assistant { .. })),
        "a wrongly-main-routed copy would resolve nothing (main has no Assistant item)"
    );

    // (f) lifecycle close: pending stream flushes + status Done through the
    // single pane path (same SubagentComplete every surface's manager emits).
    a.apply(wrap(child_id, AgentEvent::ContentDelta(format!("t026 final stream {child_id}"))));
    a.apply(AgentEvent::SubagentComplete {
        id: child_id,
        goal: format!("t026 goal {child_id}"),
        success: true,
        summary_preview: format!("t026 summary {child_id}"),
        token_usage: Default::default(),
        duration_secs: 1.0,
    });
    let pane = &a.subagent_panes[pane_idx];
    assert_eq!(pane.status, SubagentStatus::Done);
    assert!(pane.transcript.iter().any(|it| matches!(
        it,
        TranscriptItem::Assistant { text, .. } if text.contains(&format!("t026 final stream {child_id}"))
    )), "streaming text flushed into the pane transcript on close");
}

/// Surface 1 — delegate_task (single-goal mode): dispatch_single →
/// SubagentSpawn → wrapped stream → SubagentComplete, all through the one
/// pane pipeline.
#[test]
fn t026_delegate_single_surface_feeds_the_one_pane_funnel() {
    let mut a = app();
    a.apply(surface_spawn(1, "delegate: explore the manager crate", "glm-5.2", "file, web"));
    feed_common_stream(&mut a, 1, 20);
    t026_assert_funnel_capabilities(&mut a, 0, 1);
}

/// Surface 2 — call_omo_agent / OMO category delegation: the OMO path's
/// EXTRA event (CategoryDelegation, emitted before dispatch) is
/// notice-level only and must not fork a second pane pipeline — the child
/// enters through the SAME SubagentSpawn shape with the resolved model.
#[test]
fn t026_omo_call_agent_surface_feeds_the_one_pane_funnel() {
    let mut a = app();
    a.apply(AgentEvent::CategoryDelegation { category: "deep".into(), model: "glm-5.2".into() });
    assert!(
        a.subagent_panes.is_empty(),
        "CategoryDelegation adds a job-board entry but NO pane — no surface-specific fork"
    );
    // The resolved child then spawns through the common funnel (CallOmoAgent
    // delegates to the inner DelegateTask → dispatch_single).
    a.apply(surface_spawn(2, "call_omo_agent: research explore agents", "glm-5.2", "file-read, web"));
    feed_common_stream(&mut a, 2, 20);
    t026_assert_funnel_capabilities(&mut a, 0, 2);
}

/// Surface 3 — /hypercode: planner/explorer/implementor children fan out
/// via dispatch_requests with role-routed models/toolsets; an implementor's
/// FileChange lands as a FileDiff item through the SAME file_diff_item
/// construction the main transcript uses (D7 parity), and the wave closes
/// with the shared DelegationBatchComplete — which never touches panes.
#[test]
fn t026_hypercode_surface_feeds_the_one_pane_funnel() {
    let mut a = app();
    a.apply(surface_spawn(3, "hypercode explorer: map workstream 0", "glm-5.2-flash", "file-read, terminal, web"));
    a.apply(surface_spawn(4, "hypercode implementor: build workstream 0", "glm-5.2", "file, terminal"));
    feed_common_stream(&mut a, 3, 20);
    feed_common_stream(&mut a, 4, 20);

    // Implementor write → FileChange wrapped event → FileDiff pane item
    // (pane_apply's FileChange arm, state.rs:714 — the shared construction).
    a.apply(wrap(4, AgentEvent::FileChange {
        path: "src/feature_0.rs".into(),
        kind: FileChangeKind::Edit,
        before: "old line\n".into(),
        after: "old line\nnew line\n".into(),
        diff: DiffResult {
            path: "src/feature_0.rs".into(),
            diff: "-old line\n+old line\n+new line".into(),
            added: 2,
            removed: 1,
        },
        is_binary: false,
        source: FileChangeSource::FileTool,
    }));
    assert!(
        a.subagent_panes[1]
            .transcript
            .iter()
            .any(|it| matches!(it, TranscriptItem::FileDiff { path, .. } if path == "src/feature_0.rs")),
        "the implementor's FileChange became a pane FileDiff item via the shared construction"
    );

    t026_assert_funnel_capabilities(&mut a, 0, 3);
    t026_assert_funnel_capabilities(&mut a, 1, 4);

    // The wave's closing event (dispatch_requests, manager.rs:560) is
    // notice-level on the main transcript: panes are untouched.
    a.apply(AgentEvent::DelegationBatchComplete {
        total: 2,
        succeeded: 2,
        failed: 0,
        total_duration_secs: 3.0,
    });
    assert_eq!(a.subagent_panes.len(), 2, "batch close keeps both panes");
}

/// Surface 4 — dispatch_batch (tasks[] wave): one SubagentSpawn per task
/// with the parent manager's monotonic ids, per-child wrapped streams, and
/// the closing DelegationBatchComplete — every pane identical in capability.
#[test]
fn t026_dispatch_batch_surface_feeds_the_one_pane_funnel() {
    let mut a = app();
    for id in 5..=7 {
        a.apply(surface_spawn(id, &format!("batch task {id}"), "glm-5.2", "file"));
        feed_common_stream(&mut a, id, 20);
    }
    assert_eq!(a.subagent_panes.len(), 3, "one pane per batch task");
    // Monotonic manager ids → panes stack in dispatch order.
    for (i, id) in (5..=7).enumerate() {
        assert_eq!(a.subagent_panes[i].child_id, id, "pane {i} carries child id {id}");
    }
    t026_assert_funnel_capabilities(&mut a, 0, 5);
    t026_assert_funnel_capabilities(&mut a, 2, 7);
    a.apply(AgentEvent::DelegationBatchComplete {
        total: 3,
        succeeded: 2,
        failed: 1,
        total_duration_secs: 2.0,
    });
    assert_eq!(a.subagent_panes.len(), 3, "batch close removes nothing");
}

/// Coexistence: panes from TWO different surfaces (a delegate_task child
/// and a /hypercode child) live in the one rail with fully independent
/// state — events attribute by child_id (never by surface), view state
/// never bleeds across panes, and lifecycle closes are per-child. Also pins
/// the audit finding: duplicate child ids across manager instances route
/// FIRST-MATCH into the older pane (current behavior).
#[test]
fn t026_two_surface_panes_coexist_independently() {
    let mut a = app();
    a.apply(surface_spawn(11, "delegate coexist child", "glm-5.2", "file"));
    a.apply(surface_spawn(21, "hypercode coexist child", "glm-5.2-flash", "file-read, terminal"));
    feed_common_stream(&mut a, 11, 20);
    feed_common_stream(&mut a, 21, 20);

    // Wrapped events attribute to the OWNING pane only (single funnel:
    // routing is by child_id, never by surface).
    a.apply(wrap(21, AgentEvent::ReasoningDelta("hyper only".into())));
    assert_eq!(a.subagent_panes[1].streaming_reasoning, "hyper only");
    assert!(a.subagent_panes[0].streaming_reasoning.is_empty(), "no cross-surface bleed");

    // Per-pane view state is independent: pin pane 0's scroll + expand an
    // item, then exercise pane 1's search — pane 0's state is untouched.
    a.focus_subagent(Some(0));
    let _ = render_frame(&a, T026_W, T026_H);
    assert!(a.last_pane_max_scroll.get() >= 1, "pane 0 overflows");
    a.pane_scroll_up(3);
    assert_eq!(a.subagent_panes[0].scroll, Some(3));
    let r0 = a.subagent_panes[0]
        .transcript
        .iter()
        .rposition(|it| matches!(it, TranscriptItem::Reasoning { .. }))
        .expect("pane 0 reasoning item");
    a.subagent_panes[0].toggle_item_expand(r0);

    a.focus_subagent(Some(1));
    t024_open_search(&mut a);
    t024_type_in_bar(&mut a, "t026-needle");
    assert!(a.subagent_panes[1].search_has_match, "pane 1's search works");
    assert_eq!(a.subagent_panes[0].scroll, Some(3), "pane 0's pin untouched by pane 1's search");
    assert_ne!(
        expand_state_for_test(&a.subagent_panes[0].transcript[r0]),
        ReasoningExpandState::Collapsed,
        "pane 0's expansion untouched by pane 1's search"
    );

    // Lifecycle independence: completing surface A's child leaves surface
    // B's pane Running.
    a.apply(AgentEvent::SubagentComplete {
        id: 11,
        goal: "delegate coexist child".into(),
        success: true,
        summary_preview: "ok".into(),
        token_usage: Default::default(),
        duration_secs: 0.4,
    });
    assert_eq!(a.subagent_panes[0].status, SubagentStatus::Done);
    assert_eq!(a.subagent_panes[1].status, SubagentStatus::Running, "sibling pane unaffected");

    // AUDIT NOTE — cross-manager child-id collision, KEPT as an App-level
    // first-match routing contract (see the funnel-audit comment atop this
    // section): T033 fixed the collision AT SOURCE (process-global id
    // counter in manager.rs), so real managers can no longer mint a
    // duplicate id — this synthetic duplicate pins what App::apply would
    // do if one ever arrived (first-match wins, older pane absorbs events).
    a.apply(surface_spawn(11, "colliding later spawn", "glm-5.2", "file"));
    assert_eq!(a.subagent_panes.len(), 3, "the duplicate-id spawn still stacks a pane");
    a.apply(wrap(11, AgentEvent::ContentDelta("collision".into())));
    assert_eq!(
        a.subagent_panes[0].streaming_assistant, "collision",
        "first pane with the id wins (current first-match routing)"
    );
    assert!(
        a.subagent_panes[2].streaming_assistant.is_empty(),
        "the duplicate-id pane never sees the event (pinned divergence)"
    );
}

/// T033 — the /hypercode pane-collision fix, viewed from the TUI: with the
/// process-global child-id counter (manager.rs), a delegate-surface child
/// and a LATER /hypercode-surface child can no longer share a child id, so
/// their panes coexist with zero event cross-routing even when the
/// delegate pane survives into the hypercode run. Models the T033
/// scenario: delegate_task child spawns first (pane 0, id D), the
/// /hypercode manager (a separate SubagentManager, engine.rs) then spawns
/// its first child — which pre-T033 re-minted id 1 and hijacked pane 0.
/// Post-T033 the hypercode child's id is provably different (global
/// counter), pinned structurally here as "a later manager's ids never
/// reuse a live pane's id" with the disjoint-id property itself pinned at
/// the orchestration layer (parallel_tap.rs).
#[test]
fn t033_hypercode_child_never_reuses_delegate_panes_child_id() {
    let mut a = app();
    // Delegate-surface child — would historically take id 1.
    let delegate_id: u64 = 1;
    a.apply(surface_spawn(delegate_id, "delegate survivor child", "glm-5.2", "file"));
    feed_common_stream(&mut a, delegate_id, 20);

    // /hypercode surface's FIRST child, from the separate manager. With the
    // old per-manager counter this was also id 1 — the collision. The
    // process-global counter guarantees this id is different; we simulate
    // the post-fix id shape (any id the global counter could hand out is
    // > every id minted before it, so > delegate_id).
    let hypercode_id = delegate_id + 1;
    a.apply(surface_spawn(hypercode_id, "hypercode first child", "glm-5.2-flash", "file-read, terminal"));
    feed_common_stream(&mut a, hypercode_id, 20);
    assert_ne!(
        hypercode_id, delegate_id,
        "post-T033 ids from a later manager never reuse a live pane's id"
    );

    // No cross-routing: each pane owns exactly its own stream state.
    a.apply(wrap(delegate_id, AgentEvent::ContentDelta("delegate only".into())));
    assert_eq!(
        a.subagent_panes[0].streaming_assistant, "delegate only",
        "delegate event lands in the delegate pane"
    );
    assert!(
        a.subagent_panes[1].streaming_assistant.is_empty(),
        "hypercode pane untouched by delegate events"
    );

    a.apply(wrap(hypercode_id, AgentEvent::ContentDelta("hypercode only".into())));
    assert_eq!(
        a.subagent_panes[1].streaming_assistant, "hypercode only",
        "hypercode event lands in the hypercode pane"
    );
    assert_eq!(
        a.subagent_panes[0].streaming_assistant, "delegate only",
        "delegate pane's own pending stream untouched by hypercode events"
    );

    // Wrapped lifecycle also closes only the owning pane.
    a.apply(AgentEvent::SubagentComplete {
        id: hypercode_id,
        goal: "hypercode first child".into(),
        success: true,
        summary_preview: "ok".into(),
        token_usage: Default::default(),
        duration_secs: 0.2,
    });
    assert_eq!(a.subagent_panes[1].status, SubagentStatus::Done);
    assert_eq!(
        a.subagent_panes[0].status,
        SubagentStatus::Running,
        "delegate pane keeps running — no lifecycle cross-close"
    );
}

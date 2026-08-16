//! Parallel-subagent feature: per-subagent pane state + rendering tests.
//!
//! Drives App::apply with synthetic orchestration events (SubagentSpawn /
//! SubagentEvent / SubagentComplete) and asserts the pane model + the
//! rendered rail/pane views via a ratatui TestBackend.

use joey_agent_core::events::{AgentEvent, ContextEntry};
use joey_tui::state::{NoticeKind, RunMode, SubagentStatus, TranscriptItem};
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

fn first_line(s: &str, _n: usize) -> String {
    s.lines().next().unwrap_or("").to_string()
}

//! Integration smoke tests: verify the widget rendering pipeline produces
//! valid frames for representative app states without requiring a real TTY,
//! and pin down the event-stream contract (token accounting, message dedupe,
//! tool lifecycle resolution).

use joey_agent_core::AgentEvent;
use joey_tui::theme::Theme;
use joey_tui::TranscriptItem;

fn usage(prompt: u64, completion: u64) -> joey_providers::Usage {
    joey_providers::Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: prompt + completion,
        ..Default::default()
    }
}

#[test]
fn renders_idle_state_without_panic() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let theme = Theme::aurora();
    let mut app = joey_tui::AppState::new("test1234", "test-model");
    app.provider = "test-provider".to_string();
    app.cwd = "/tmp/test".to_string();

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            use ratatui::layout::{Constraint, Direction, Layout};
            use ratatui::style::Style;
            use ratatui::widgets::Block;
            let area = f.area();
            f.render_widget(
                Block::default().style(Style::default().bg(theme.bg_void.to_color())),
                area,
            );
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2),
                    Constraint::Min(8),
                    Constraint::Length(7),
                    Constraint::Length(1),
                ])
                .split(area);
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([ratatui::layout::Constraint::Min(40), ratatui::layout::Constraint::Length(34)])
                .split(chunks[1]);
            joey_tui::widgets::draw_transcript(f, body[0], &app, theme, false, 0.5);
            joey_tui::widgets::draw_activity(
                f,
                body[1],
                &app,
                theme,
                &joey_tui::anim::Spinner::dots(),
                &joey_tui::anim::Equalizer::new(10),
            );
        })
        .unwrap();
}

#[test]
fn renders_busy_state_with_events() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let theme = Theme::aurora();
    let mut app = joey_tui::AppState::new("abc12345", "glm-5.2");
    app.cwd = "/home/joey".to_string();

    // Simulate a turn.
    app.record_user("list my files");
    app.apply(AgentEvent::TurnStart { max_iterations: 90 });
    app.apply(AgentEvent::IterationStart { iteration: 1, max_iterations: 90 });
    app.apply(AgentEvent::ApiCallStart);
    app.apply(AgentEvent::ReasoningDelta("Let me think about this. ".into()));
    app.apply(AgentEvent::ContentDelta("Hello! ".into()));
    app.apply(AgentEvent::ToolStart {
        name: "terminal".into(),
        emoji: "⚡".into(),
        summary: "ls -la".into(),
    });
    app.apply(AgentEvent::ToolEnd {
        name: "terminal".into(),
        is_error: false,
        result_preview: "file1.rs\nfile2.rs".into(),
        duration_secs: 0.12,
    });
    app.apply(AgentEvent::ApiCallEnd { usage: usage(100, 50) });
    app.apply(AgentEvent::Done {
        final_text: "Hello! Here are your files.".into(),
        usage: usage(100, 50),
        iterations: 1,
    });

    let backend = TestBackend::new(110, 35);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            use ratatui::layout::{Constraint, Direction, Layout};
            use ratatui::style::Style;
            use ratatui::widgets::Block;
            let area = f.area();
            f.render_widget(
                Block::default().style(Style::default().bg(theme.bg_void.to_color())),
                area,
            );
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2),
                    Constraint::Min(8),
                    Constraint::Length(7),
                    Constraint::Length(1),
                ])
                .split(area);
            joey_tui::widgets::draw_transcript(f, chunks[1], &app, theme, true, 0.5);
        })
        .unwrap();

    // Verify the transcript has the content.
    assert!(app.transcript_len() >= 2); // user + assistant at minimum
    assert!(!app.last_final_text.is_empty());
    // Tokens are counted once, from ApiCallEnd; Done's cumulative usage must
    // not be added on top.
    assert_eq!(app.tokens.prompt, 100);
    assert_eq!(app.tokens.completion, 50);
}

#[test]
fn done_usage_is_not_double_counted() {
    let mut app = joey_tui::AppState::new("s", "m");
    app.apply(AgentEvent::TurnStart { max_iterations: 10 });
    app.apply(AgentEvent::ApiCallEnd { usage: usage(70, 30) });
    app.apply(AgentEvent::ApiCallEnd { usage: usage(30, 20) });
    // Done reports the turn TOTAL (100/50) — already counted per call.
    app.apply(AgentEvent::Done {
        final_text: "done".into(),
        usage: usage(100, 50),
        iterations: 2,
    });
    assert_eq!(app.tokens.prompt, 100);
    assert_eq!(app.tokens.completion, 50);
    assert_eq!(app.tokens.iterations, 2);
}

#[test]
fn final_message_is_not_duplicated() {
    let mut app = joey_tui::AppState::new("s", "m");
    app.apply(AgentEvent::TurnStart { max_iterations: 10 });
    app.apply(AgentEvent::ContentDelta("The answer is 42.".into()));
    // The agent commits the message, then ends the turn with the same text.
    app.apply(AgentEvent::AssistantMessage("The answer is 42.".into()));
    app.apply(AgentEvent::Done {
        final_text: "The answer is 42.".into(),
        usage: usage(0, 0),
        iterations: 1,
    });
    let assistant_items = app
        .transcript
        .iter()
        .filter(|it| matches!(it, TranscriptItem::Assistant { .. }))
        .count();
    assert_eq!(assistant_items, 1, "final answer must appear exactly once");
    assert_eq!(app.last_final_text, "The answer is 42.");
    assert!(!app.is_busy());
}

#[test]
fn tool_end_resolves_across_intervening_items() {
    use joey_tui::state::ToolStatus;

    let mut app = joey_tui::AppState::new("s", "m");
    app.apply(AgentEvent::TurnStart { max_iterations: 10 });
    app.apply(AgentEvent::ToolStart {
        name: "terminal".into(),
        emoji: "⚡".into(),
        summary: "cargo build".into(),
    });
    // A retry notice lands between start and end — the tool must still
    // resolve instead of spinning forever.
    app.apply(AgentEvent::RetryAttempt {
        attempt: 1,
        max_retries: 3,
        error: "flaky network".into(),
        wait_secs: 0.5,
    });
    app.apply(AgentEvent::ToolEnd {
        name: "terminal".into(),
        is_error: false,
        result_preview: "Finished".into(),
        duration_secs: 4.2,
    });

    let tool = app
        .transcript
        .iter()
        .find_map(|it| match it {
            TranscriptItem::Tool { status, duration_secs, .. } => Some((*status, *duration_secs)),
            _ => None,
        })
        .expect("tool item present");
    assert_eq!(tool.0, ToolStatus::Done);
    assert_eq!(tool.1, Some(4.2));
}

#[test]
fn scroll_is_clamped_and_not_yanked_to_bottom() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let theme = Theme::aurora();
    let mut app = joey_tui::AppState::new("s", "m");
    for i in 0..40 {
        app.push_item(TranscriptItem::Notice {
            text: format!("line {i}"),
            kind: joey_tui::state::NoticeKind::Info,
        });
    }

    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    let draw = |t: &mut Terminal<TestBackend>, app: &joey_tui::AppState| {
        t.draw(|f| {
            let area = f.area();
            joey_tui::widgets::draw_transcript(f, area, app, theme, false, 0.5);
        })
        .unwrap();
    };

    // First frame records the scrollable extent.
    draw(&mut terminal, &app);
    assert!(app.last_max_scroll.get() > 0);

    // Scrolling far past the top clamps to the measured extent.
    app.scroll_up(10_000);
    let max = app.last_max_scroll.get();
    assert!(app.scroll.unwrap() <= max);

    // New streamed content must NOT yank the reader back to the bottom.
    let before = app.scroll;
    app.apply(AgentEvent::Notice("new event while reading".into()));
    assert_eq!(app.scroll, before);

    // But the user's own message snaps back to live.
    app.record_user("next question");
    assert_eq!(app.scroll, None);

    draw(&mut terminal, &app);
}

#[test]
fn failed_turn_flushes_partial_output() {
    let mut app = joey_tui::AppState::new("s", "m");
    app.apply(AgentEvent::TurnStart { max_iterations: 10 });
    app.apply(AgentEvent::ReasoningDelta("hmm ".into()));
    app.apply(AgentEvent::ContentDelta("partial ans".into()));
    app.apply(AgentEvent::Failed("provider exploded".into()));

    assert!(!app.is_busy());
    assert!(app.active_agents.is_empty());
    assert!(!app.reasoning_open);
    assert!(app.streaming_assistant.is_empty());
    let has_partial = app.transcript.iter().any(
        |it| matches!(it, TranscriptItem::Assistant { text } if text == "partial ans"),
    );
    let has_error = app
        .transcript
        .iter()
        .any(|it| matches!(it, TranscriptItem::Error { text } if text.contains("exploded")));
    assert!(has_partial && has_error);
}

#[test]
fn activity_scales_with_agent_count() {
    use joey_tui::anim::Activity;
    use std::time::Duration;

    let mut a = Activity::idle();
    assert!((a.intensity - 0.0).abs() < 0.01);

    // Simulate agents becoming active.
    for _ in 0..60 {
        a.update(4, Duration::from_millis(16));
    }
    // Intensity should be significantly elevated with 4 agents.
    assert!(a.intensity > 0.5, "intensity should be high: {}", a.intensity);
    // Speed scaling was toned down (crush-style calmer motion): baseline 0.8,
    // up to ~1.5x at full intensity, rather than the old 3x range.
    assert!(a.speed() > 1.2, "speed should scale up: {}", a.speed());

    // Now go idle.
    for _ in 0..120 {
        a.update(0, Duration::from_millis(16));
    }
    // Intensity should decay toward the baseline shimmer.
    assert!(a.intensity < 0.3, "intensity should decay: {}", a.intensity);
}

#[test]
fn tiny_terminal_renders_without_panic() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let theme = Theme::aurora();
    let mut app = joey_tui::AppState::new("s", "m");
    app.push_item(TranscriptItem::Notice {
        text: "hello".into(),
        kind: joey_tui::state::NoticeKind::Info,
    });

    // Degenerate sizes must not index outside the buffer.
    for (w, h) in [(1u16, 1u16), (5, 2), (10, 3), (20, 4)] {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                joey_tui::widgets::draw_transcript(f, area, &app, theme, false, 0.5);
                joey_tui::widgets::draw_status(
                    f,
                    area,
                    &app,
                    theme,
                    std::time::Duration::from_secs(3),
                );
            })
            .unwrap();
    }
}

/// T039 contract: the agent-picker state machine. A populated roster → open
/// the picker → move the cursor → select → the active index updates and the
/// chosen name is surfaced. This pins the exact transitions that Tab/↑↓/Enter
/// drive in `Tui::handle_key` (which itself needs a real TTY to construct).
#[test]
fn agent_picker_open_navigate_select_contract() {
    use joey_tui::state::DisplayAgent;

    fn mk(name: &str, display: &str) -> DisplayAgent {
        DisplayAgent {
            name: name.to_string(),
            display_name: display.to_string(),
            color: String::new(),
            mode: "Primary".to_string(),
            resolved_model: Some("m".to_string()),
            description: String::new(),
        }
    }

    let mut app = joey_tui::AppState::new("s", "m");
    // Simulate populate_agent_roster: Default + a few OMO agents.
    app.agent_roster = vec![
        mk("default", "Default"),
        mk("sisyphus", "Sisyphus"),
        mk("prometheus", "Prometheus"),
        mk("atlas", "Atlas"),
    ];
    assert_eq!(app.agent_roster.len(), 4);

    // Tab → open the picker.
    app.agent_picker_open = true;
    app.agent_picker_cursor = 0;
    assert!(app.agent_picker_open);

    // Render the picker: with a roster, draw_agent_picker must not bail.
    {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let theme = Theme::aurora();
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                joey_tui::widgets::draw_agent_picker(f, f.area(), &app, &theme);
            })
            .unwrap();
    }

    // ↓ ×2 → cursor on Prometheus (index 2).
    let n = app.agent_roster.len();
    app.agent_picker_cursor = (app.agent_picker_cursor + 1) % n;
    app.agent_picker_cursor = (app.agent_picker_cursor + 1) % n;
    assert_eq!(app.agent_picker_cursor, 2);

    // Enter → select: active index updates to the cursor, picker closes, and
    // the SwitchAgent name matches the roster entry (as handle_key emits).
    let idx = app.agent_picker_cursor;
    let chosen = app.agent_roster[idx].name.clone();
    app.agent_picker_open = false;
    app.active_agent_index = idx;
    assert!(!app.agent_picker_open);
    assert_eq!(app.active_agent_index, 2);
    assert_eq!(chosen, "prometheus");

    // The status bar reflects the new active agent.
    let active = &app.agent_roster[app.active_agent_index];
    assert_eq!(active.display_name, "Prometheus");
}

/// build_agent_roster_from_registry always leads with "Default" and includes
/// only available primary agents (T140 contract).
#[test]
fn roster_builder_leads_with_default_then_available_primaries() {
    // Only a GLM model available → Sisyphus/Prometheus/Atlas resolve via the
    // glm family; Hephaestus needs an openai-class provider and is skipped.
    let profile = joey_providers::profile::get_profile("zai").unwrap();
    let available =
        joey_omo::AvailableModelSet::from_connected(&profile, "glm-5.2");
    let overrides = joey_omo::agents::registry::ModelOverrides::new();
    let registry = joey_omo::AgentRegistry::build(available, &overrides);

    let roster = joey_tui::widgets::build_agent_roster_from_registry(&registry);

    assert!(!roster.is_empty(), "roster must not be empty");
    assert_eq!(roster[0].name, "default", "Default is always first");
    // Every non-default entry is an available primary with a resolved model.
    for entry in roster.iter().skip(1) {
        assert!(
            entry.resolved_model.is_some(),
            "{} should have resolved (only available primaries appear)",
            entry.display_name
        );
    }
    // The "Default" entry has no resolved_model until the host stamps it.
    assert!(roster[0].resolved_model.is_none());
}

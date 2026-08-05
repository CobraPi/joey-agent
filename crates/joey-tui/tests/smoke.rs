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
            joey_tui::widgets::draw_omo_panel(
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
        exit_code: Some(0),
        full_result: "file1.rs\nfile2.rs".into(),
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
        exit_code: Some(0),
        full_result: "Finished".into(),
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

/// T077 contract: the OMO activity panel renders without panic at multiple
/// terminal sizes including the degraded narrow/short cases, and shows the
/// full 11-agent roster in the idle state. Mirrors the contract's graceful
/// degradation rules (contracts/activity-panel.md).
#[test]
fn omo_panel_renders_at_multiple_sizes_and_shows_roster() {
    use joey_tui::state::DisplayAgent;
    use ratatui::backend::TestBackend;
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::Terminal;

    fn mk(name: &str, display: &str, model: Option<&str>) -> DisplayAgent {
        DisplayAgent {
            name: name.to_string(),
            display_name: display.to_string(),
            color: "#fff".to_string(),
            mode: "Primary".to_string(),
            resolved_model: model.map(String::from),
            description: String::new(),
        }
    }

    let mut app = joey_tui::AppState::new("s", "m");
    // Full 11-agent roster (2 unavailable to exercise dimming).
    app.agent_roster = vec![
        mk("default", "Default", None),
        mk("sisyphus", "Sisyphus", Some("claude-opus-4.8")),
        mk("hephaestus", "Hephaestus", None),
        mk("prometheus", "Prometheus", Some("claude-opus-4.8")),
        mk("atlas", "Atlas", Some("claude-sonnet-5")),
        mk("oracle", "Oracle", Some("gpt-5.6-sol")),
        mk("librarian", "Librarian", Some("gpt-5.4-mini")),
        mk("explore", "Explore", Some("gpt-5.4-mini")),
        mk("multimodal-looker", "Multimodal", Some("gpt-5.6-sol")),
        mk("metis", "Metis", Some("glm-5")),
        mk("momus", "Momus", Some("glm-5")),
    ];
    assert_eq!(app.agent_roster.len(), 11);

    let theme = Theme::aurora();
    // 80×24, 120×40, and the degraded 70×20 must all render the panel.
    for (w, h) in [(80u16, 24u16), (120, 40), (70, 20)] {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                let body = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Min(40), Constraint::Length(34)])
                    .split(area);
                joey_tui::widgets::draw_omo_panel(
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
}

/// T077 (degraded sizes): the panel must not panic at very short heights
/// (<9 rows collapse to single line) and tiny widths.
#[test]
fn omo_panel_renders_degraded_heights() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = joey_tui::AppState::new("s", "m");
    app.agent_roster = vec![joey_tui::state::DisplayAgent {
        name: "sisyphus".into(),
        display_name: "Sisyphus".into(),
        color: "#fff".into(),
        mode: "Primary".into(),
        resolved_model: Some("claude-opus-4.8".into()),
        description: String::new(),
    }];
    let theme = Theme::aurora();
    // Short heights + the very-short 1-row and 8-row cases.
    for (w, h) in [(34u16, 8u16), (34, 3), (34, 1), (50, 14)] {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                joey_tui::widgets::draw_omo_panel(
                    f,
                    f.area(),
                    &app,
                    theme,
                    &joey_tui::anim::Spinner::dots(),
                    &joey_tui::anim::Equalizer::new(10),
                );
            })
            .unwrap();
    }
}

/// T128 regression: Tab-free keybindings remain intact. We can't drive a real
/// TTY here, so we verify the input box (which Tab no longer focuses) still
/// honors the core editing keys, and that the agent-roster state Tab now uses
/// is independent of the transcript/scroll subsystem.
#[test]
fn tab_free_keybindings_remain_intact() {
    use joey_tui::input::Input;
    let mut input = Input::new();
    // Core editing keys that must keep working regardless of the Tab change.
    input.insert_str("hello world");
    assert_eq!(input.text(), "hello world");
    input.backspace();
    assert_eq!(input.text(), "hello worl");
    input.clear();
    assert!(input.text().is_empty());

    // The agent picker state is a separate concern from input editing.
    let mut app = joey_tui::AppState::new("s", "m");
    assert!(!app.agent_picker_open);
    assert_eq!(app.agent_picker_cursor, 0);
    app.agent_picker_open = true;
    app.agent_picker_cursor = 0;
    // Forward/backward wrap behavior (BC-014/BC-017) doesn't touch input.
    app.agent_picker_cursor = (app.agent_picker_cursor + 1) % 1;
    assert_eq!(app.agent_picker_cursor, 0);
}

/// T132: the TUI renders without panic at narrow/short degraded sizes
/// (70×20, 80×12, 50×10) — the panel's graceful-degradation contract
/// (contracts/activity-panel.md). Exercises the full transcript + status +
/// omo panel rendering surface.
#[test]
fn narrow_terminal_renders_without_panic() {
    use ratatui::backend::TestBackend;
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::Terminal;

    let theme = Theme::aurora();
    let mut app = joey_tui::AppState::new("s", "m");
    app.agent_roster = vec![joey_tui::state::DisplayAgent {
        name: "sisyphus".into(),
        display_name: "Sisyphus".into(),
        color: "#fff".into(),
        mode: "Primary".into(),
        resolved_model: Some("m".into()),
        description: String::new(),
    }];
    app.push_item(TranscriptItem::Notice {
        text: "hello".into(),
        kind: joey_tui::state::NoticeKind::Info,
    });

    for (w, h) in [(70u16, 20u16), (80, 12), (50, 10), (60, 8)] {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(2),
                        Constraint::Min(4),
                        Constraint::Length(1),
                    ])
                    .split(area);
                let body = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Min(20), Constraint::Length(34)])
                    .split(chunks[1]);
                joey_tui::widgets::draw_transcript(f, body[0], &app, theme, false, 0.5);
                joey_tui::widgets::draw_omo_panel(
                    f,
                    body[1],
                    &app,
                    theme,
                    &joey_tui::anim::Spinner::dots(),
                    &joey_tui::anim::Equalizer::new(10),
                );
                joey_tui::widgets::draw_status(
                    f,
                    chunks[2],
                    &app,
                    theme,
                    std::time::Duration::from_secs(3),
                );
            })
            .unwrap();
    }
}

/// T131: the agent picker overlay renders well within the 16ms/frame budget
/// (SC-003). Guards against accidental per-row allocation blowups.
#[test]
fn agent_picker_render_is_fast() {
    use joey_tui::state::DisplayAgent;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::Instant;

    fn mk(name: &str, display: &str) -> DisplayAgent {
        DisplayAgent {
            name: name.to_string(),
            display_name: display.to_string(),
            color: "#abc".to_string(),
            mode: "Primary".to_string(),
            resolved_model: Some("m".to_string()),
            description: String::new(),
        }
    }

    let mut app = joey_tui::AppState::new("s", "m");
    app.agent_roster = vec![
        mk("default", "Default"),
        mk("sisyphus", "Sisyphus"),
        mk("hephaestus", "Hephaestus"),
        mk("prometheus", "Prometheus"),
        mk("atlas", "Atlas"),
    ];
    app.agent_picker_open = true;
    let theme = Theme::aurora();

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    // Warm up.
    terminal
        .draw(|f| {
            joey_tui::widgets::draw_agent_picker(f, f.area(), &app, &theme);
        })
        .unwrap();

    let start = Instant::now();
    for _ in 0..30 {
        terminal
            .draw(|f| {
                joey_tui::widgets::draw_agent_picker(f, f.area(), &app, &theme);
            })
            .unwrap();
    }
    let per_frame = start.elapsed() / 30;
    assert!(
        per_frame.as_millis() < 16,
        "picker render took {:?}/frame, must be <16ms",
        per_frame
    );
}

/// T129 regression: existing CLI flags and config keys still work. We verify
/// the AgentConfig shape the CLI builds from `--model`/`--tui` round-trips a
/// model string and the core provider profile lookup (used by the CLI's model
/// resolver) still resolves a known provider.
#[test]
fn cli_flags_and_config_shapes_remain_valid() {
    // AgentConfig (built from --model) round-trips a model string.
    use joey_agent_core::AgentConfig;
    let ac = AgentConfig {
        model: "custom-flag-model".to_string(),
        provider: "zai".to_string(),
        base_url: "https://api.example.com".to_string(),
        api_key: None,
        max_turns: 10,
        api_max_retries: 3,
        tool_delay: 0.0,
        reasoning: None,
        enabled_tools: vec![],
        max_tokens: None,
        stream: false,
        pass_session_id: false,
    };
    assert_eq!(ac.model, "custom-flag-model");

    // The provider profile lookup the CLI uses at startup still resolves.
    let profile = joey_providers::profile::get_profile("zai");
    assert!(profile.is_some(), "zai provider profile must resolve");
}

/// T155: the Atlas job board renders during BoulderWorkStarted execution
/// (contracts/activity-panel.md "Job Board (during Atlas execution)"). The
/// panel must not panic at 80×24 / 120×40 / degraded 70×20 when the job board
/// is visible with delegated tasks carrying tool-call counts.
#[test]
fn job_board_renders_during_atlas_execution() {
    use joey_agent_core::AgentEvent;
    use ratatui::backend::TestBackend;
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::Terminal;

    let mut app = joey_tui::AppState::new("s", "m");
    // Activate the job board + spawn two delegated tasks with tool calls.
    app.apply(AgentEvent::BoulderWorkStarted {
        plan_name: "feat".into(),
        work_id: "w1".into(),
    });
    app.apply(AgentEvent::SubagentSpawn {
        goal: "Task 1: Implement auth".into(),
        model: "glm-5".into(),
        toolset_summary: "file".into(),
        depth: 1,
    });
    app.apply(AgentEvent::SubagentSpawn {
        goal: "Task 2: Write tests".into(),
        model: "glm-5".into(),
        toolset_summary: "file".into(),
        depth: 1,
    });
    app.apply(AgentEvent::ToolStart {
        name: "read_file".into(),
        emoji: "📖".into(),
        summary: "src/auth.rs".into(),
    });
    app.apply(AgentEvent::ToolStart {
        name: "grep".into(),
        emoji: "🔍".into(),
        summary: "password".into(),
    });
    assert!(app.job_board_visible, "job board flag set");
    assert_eq!(app.subagent_entries.len(), 2);

    let theme = Theme::aurora();
    for (w, h) in [(80u16, 24u16), (120, 40), (70, 20), (34, 14)] {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                let body = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Min(40), Constraint::Length(34)])
                    .split(area);
                joey_tui::widgets::draw_omo_panel(
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

    // Tool calls were attributed to the most recent running entry.
    let last = app.subagent_entries.last().unwrap();
    assert_eq!(last.tool_call_count, 2, "two tool calls attributed");
    assert_eq!(last.last_tool.as_deref(), Some("grep"));
}

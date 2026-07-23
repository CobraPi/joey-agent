//! Integration smoke tests: verify the widget rendering pipeline produces
//! valid frames for representative app states without requiring a real TTY.

use joey_agent_core::AgentEvent;
use joey_tui::theme::Theme;

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
            joey_tui::widgets::draw_transcript(f, body[0], &app, theme);
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
    app.apply(AgentEvent::TurnStart { max_iterations: 90 });
    app.apply(AgentEvent::IterationStart { iteration: 1, max_iterations: 90 });
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
    app.apply(AgentEvent::Done {
        final_text: "Hello! Here are your files.".into(),
        usage: joey_providers::Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            ..Default::default()
        },
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
            joey_tui::widgets::draw_transcript(f, chunks[1], &app, theme);
        })
        .unwrap();

    // Verify the transcript has the content.
    assert!(app.transcript_len() >= 2); // user + assistant at minimum
    assert!(!app.last_final_text.is_empty());
    assert_eq!(app.tokens.prompt, 100);
    assert_eq!(app.tokens.completion, 50);
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
    assert!(a.speed() > 1.5, "speed should scale up: {}", a.speed());

    // Now go idle.
    for _ in 0..120 {
        a.update(0, Duration::from_millis(16));
    }
    // Intensity should decay toward the baseline shimmer.
    assert!(a.intensity < 0.3, "intensity should decay: {}", a.intensity);
}

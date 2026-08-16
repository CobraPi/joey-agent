//! Main-window expanded-view formatting tests: expanded sections in the
//! transcript render tool results/args as text-editor-like views — embedded
//! newlines appear as REAL line breaks in the numbered gutter, never as
//! literal `\n` escape runs.

use joey_tui::state::{App, ToolStatus, TranscriptItem, LIVE_OUTPUT_CAPACITY};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

/// Render the full app frame (main window) and return the joined buffer.
fn render_main(app: &mut App) -> String {
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let area = f.area();
            joey_tui::app::render_body_for_test(
                f,
                area,
                app,
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

fn tool_item(full_result: &str, expanded: bool) -> TranscriptItem {
    TranscriptItem::Tool {
        name: "search_files".into(),
        emoji: "🔍".into(),
        summary: "pattern=manager".into(),
        status: ToolStatus::Done,
        duration_secs: Some(0.4),
        result_preview: "…".into(),
        expanded,
        full_args: None,
        full_result: Some(full_result.to_string()),
        is_terminal: false,
        exit_code: None,
        live_output: String::new(),
        live_output_capacity: LIVE_OUTPUT_CAPACITY,
    }
}

#[test]
fn expanded_generic_tool_renders_real_newlines_not_escapes() {
    // Tool result whose JSON payload embeds a newline inside a string.
    let raw = r#"{"output":"line one\nline two\nline three","matches":2}"#;
    let mut app = App::new("s", "m");
    app.push_item(tool_item(raw, true));
    let text = render_main(&mut app);

    // Each embedded \n became its own guttered row.
    assert!(text.contains("line one"), "first payload line visible");
    assert!(text.contains("line two"), "second payload line visible");
    assert!(text.contains("line three"), "third payload line visible");
    // No literal escape runs anywhere in the frame.
    assert!(
        !text.contains("\\n"),
        "expanded view must not show literal \\n escapes"
    );
    // Gutter present on the payload rows.
    assert!(text.contains("│"), "numbered gutter rendered");
}

#[test]
fn expanded_terminal_tool_renders_real_newlines_not_escapes() {
    let raw = r#"{"output":"$ build\nok in 3 steps","exit_code":0,"error":null}"#;
    let mut app = App::new("s", "m");
    app.push_item(TranscriptItem::Tool {
        name: "terminal".into(),
        emoji: "💻".into(),
        summary: "make build".into(),
        status: ToolStatus::Done,
        duration_secs: Some(1.2),
        result_preview: "…".into(),
        expanded: true,
        full_args: None,
        full_result: Some(raw.to_string()),
        is_terminal: true,
        exit_code: Some(0),
        live_output: String::new(),
        live_output_capacity: LIVE_OUTPUT_CAPACITY,
    });
    let text = render_main(&mut app);
    assert!(text.contains("$ build"), "payload line 1");
    assert!(text.contains("ok in 3 steps"), "payload line 2");
    assert!(!text.contains("\\n"), "no literal \\n escape runs");
}

#[test]
fn expanded_view_pretty_prints_flat_json_and_splits_lines() {
    // Compact JSON (single line, no spaces) still pretty-prints AND any
    // embedded newline in a value splits into real rows.
    let raw = r#"{"matches":[{"path":"a.rs","note":"multi\nline note"}]}"#;
    let mut app = App::new("s", "m");
    app.push_item(tool_item(raw, true));
    let text = render_main(&mut app);
    assert!(text.contains("path"), "pretty-printed structure visible");
    assert!(text.contains("a.rs"));
    assert!(text.contains("multi"), "embedded value part 1");
    assert!(text.contains("line note"), "embedded value part 2");
    assert!(!text.contains("\\n"), "no literal \\n escape runs");
}

#[test]
fn collapsed_tool_header_stays_single_line() {
    // The collapsed header keeps its one-line summary (newlines in the
    // summary collapse to spaces — the header is not an expanded view).
    let raw = r#"{"output":"line one\nline two"}"#;
    let mut app = App::new("s", "m");
    app.push_item(tool_item(raw, false));
    let text = render_main(&mut app);
    // Collapsed: bounded preview rows may appear, but no raw JSON with
    // escapes is dumped into the frame.
    assert!(text.contains("search_files"), "header present");
    assert!(
        !text.contains("\"output\""),
        "collapsed view doesn't dump the raw JSON envelope"
    );
}

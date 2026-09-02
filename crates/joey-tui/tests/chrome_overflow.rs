//! Regression tests for main-view chrome overflow (orchestrator screen):
//! popups must stay inside the frame, the status bar's left content must
//! yield to the right-aligned keymap hint instead of colliding with it,
//! and the header's right status must never overwrite the logo.

use joey_tui::anim::{Pulse, Spinner};
use joey_tui::state::{App, DisplayAgent, RunMode, SlashCommandInfo, TokenStats};
use joey_tui::theme::Theme;
use joey_tui::widgets;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use std::time::Duration;

fn row_string(terminal: &Terminal<TestBackend>, y: u16) -> String {
    let buf = terminal.backend().buffer();
    (0..buf.area.width)
        .map(|x| buf[(x, y)].symbol().to_string())
        .collect()
}

fn agent(display_name: &str) -> DisplayAgent {
    DisplayAgent {
        name: "agent".to_string(),
        display_name: display_name.to_string(),
        color: "#00ffff".to_string(),
        mode: "Primary".to_string(),
        resolved_model: None,
        description: String::new(),
    }
}

#[test]
fn slash_popup_stays_inside_narrow_screen() {
    let mut app = App::new("abcdefgh", "m");
    app.slash_commands = vec![SlashCommandInfo {
        name: "help".to_string(),
        aliases: Vec::new(),
        description: "Show help".to_string(),
        args_hint: String::new(),
        implemented: true,
    }];
    app.slash_menu_open = true;
    let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
    terminal
        .draw(|f| {
            widgets::draw_slash_popup(f, f.area(), &app, "/", Theme::aurora());
        })
        .unwrap();
    let mut saw_title = false;
    for y in 0..24 {
        let row = row_string(&terminal, y);
        if row.contains("Commands") {
            saw_title = true;
        }
        let rightmost = row
            .chars()
            .collect::<Vec<_>>()
            .iter()
            .rposition(|c| !c.is_whitespace());
        if let Some(i) = rightmost {
            assert!(i < 60, "row {y} draws past the screen: {row:?}");
        }
    }
    assert!(saw_title, "popup rendered its title");
}

#[test]
fn completion_popup_stays_inside_narrow_screen() {
    let mut app = App::new("abcdefgh", "m");
    app.completion_menu_open = true;
    app.completion_items = vec![
        joey_tools::completion::CompletionItem {
            replacement: "read_file".to_string(),
            display: "read_file".to_string(),
            meta: "read a file".to_string(),
        },
        joey_tools::completion::CompletionItem {
            replacement: "write_file".to_string(),
            display: "write_file".to_string(),
            meta: "write a file".to_string(),
        },
    ];
    let mut terminal = Terminal::new(TestBackend::new(70, 24)).unwrap();
    terminal
        .draw(|f| {
            widgets::draw_completion_popup(f, f.area(), &app, Theme::aurora());
        })
        .unwrap();
    let mut saw_title = false;
    for y in 0..24 {
        let row = row_string(&terminal, y);
        if row.contains("Completions") {
            saw_title = true;
        }
        let rightmost = row
            .chars()
            .collect::<Vec<_>>()
            .iter()
            .rposition(|c| !c.is_whitespace());
        if let Some(i) = rightmost {
            assert!(i < 70, "row {y} draws past the screen: {row:?}");
        }
    }
    assert!(saw_title, "popup rendered its title");
}

#[test]
fn status_bar_drops_overlong_agent_name_instead_of_colliding() {
    let mut app = App::new("abcdefgh", "m");
    app.mode = RunMode::Busy;
    app.agent_roster = vec![agent(&"N".repeat(40))];
    app.neurocode_active = true;
    app.cwd = "/very/long/working/directory/path/that/never/ends".to_string();
    app.provider = "long-provider-example".to_string();
    app.tokens = TokenStats {
        prompt: 123_456,
        completion: 789,
        iterations: 3,
    };
    let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
    terminal
        .draw(|f| {
            widgets::draw_status(
                f,
                f.area(),
                &app,
                Theme::aurora(),
                Duration::from_secs(5),
            );
        })
        .unwrap();
    let row = row_string(&terminal, 0);
    assert!(row.contains("BUSY"), "mode badge always kept: {row:?}");
    assert!(
        !row.contains('◆'),
        "over-long agent name is dropped, not run under the hint: {row:?}"
    );
    assert!(row.contains("? help"), "keymap hint intact: {row:?}");
}

#[test]
fn status_bar_keeps_fitting_spans_and_drops_the_rest() {
    let mut app = App::new("abcdefgh", "m");
    app.mode = RunMode::Busy;
    app.agent_roster = vec![agent("Nova")];
    app.neurocode_active = true;
    app.cwd = "/very/long/working/directory/path/that/never/ends".to_string();
    app.provider = "long-provider-example".to_string();
    app.tokens = TokenStats {
        prompt: 123_456,
        completion: 789,
        iterations: 3,
    };
    let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
    terminal
        .draw(|f| {
            widgets::draw_status(
                f,
                f.area(),
                &app,
                Theme::aurora(),
                Duration::from_secs(5),
            );
        })
        .unwrap();
    let row = row_string(&terminal, 0);
    assert!(row.contains("BUSY"), "mode badge kept: {row:?}");
    assert!(row.contains("◆ Nova"), "short agent name kept: {row:?}");
    assert!(
        !row.contains("NEUROCODE"),
        "lower-priority badge dropped before colliding: {row:?}"
    );
    assert!(row.contains("? help"), "keymap hint intact: {row:?}");
}

#[test]
fn header_long_model_never_overwrites_logo() {
    let mut app = App::new("abcdefgh", &"M".repeat(70));
    app.mode = RunMode::Busy;
    app.hypercode_enabled = false;
    let mut terminal = Terminal::new(TestBackend::new(100, 2)).unwrap();
    terminal
        .draw(|f| {
            widgets::draw_header(
                f,
                f.area(),
                &app,
                Theme::aurora(),
                &Spinner::dots(),
                &Pulse::new(),
                None,
            );
        })
        .unwrap();
    let row = row_string(&terminal, 0);
    assert!(row.contains("joey"), "logo preserved: {row:?}");
    assert!(row.contains("abcdefgh"), "session id kept (tail): {row:?}");
    let underline = row_string(&terminal, 1);
    assert!(underline.contains('─'), "underline row drawn");
}

#[test]
fn header_short_model_still_right_aligned() {
    let mut app = App::new("abcdefgh", "gpt-small");
    app.mode = RunMode::Busy;
    app.hypercode_enabled = false;
    let mut terminal = Terminal::new(TestBackend::new(100, 2)).unwrap();
    terminal
        .draw(|f| {
            widgets::draw_header(
                f,
                f.area(),
                &app,
                Theme::aurora(),
                &Spinner::dots(),
                &Pulse::new(),
                None,
            );
        })
        .unwrap();
    let row = row_string(&terminal, 0);
    assert!(row.contains("joey"), "logo preserved: {row:?}");
    assert!(row.contains("gpt-small"), "short model name visible: {row:?}");
}

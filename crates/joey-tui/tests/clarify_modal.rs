//! Clarify-modal integration tests: drive `App::clarify_key` directly (no
//! terminal needed), mirroring the App construction in tests/smoke.rs.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use joey_tui::AppState;
use std::sync::{Arc, Mutex};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn quick_pick_digit_answers_choice() {
    let answers = Arc::new(Mutex::new(Vec::new()));
    let sink = answers.clone();
    let mut app = AppState::new("test1234", "test-model");
    app.open_clarify(
        "Which?".to_string(),
        vec!["A - first".to_string(), "B - second".to_string()],
        Box::new(move |a: String| sink.lock().unwrap().push(a)),
    );
    assert!(app.clarify_open());
    app.clarify_key(key(KeyCode::Char('2')));
    assert!(!app.clarify_open());
    assert_eq!(*answers.lock().unwrap(), vec!["B - second".to_string()]);
}

#[test]
fn arrow_and_enter_answers_highlighted() {
    let answers = Arc::new(Mutex::new(Vec::new()));
    let sink = answers.clone();
    let mut app = AppState::new("test1234", "test-model");
    app.open_clarify(
        "Which?".to_string(),
        vec!["A - first".to_string(), "B - second".to_string()],
        Box::new(move |a: String| sink.lock().unwrap().push(a)),
    );
    app.clarify_key(key(KeyCode::Down));
    app.clarify_key(key(KeyCode::Enter));
    assert!(!app.clarify_open());
    assert_eq!(*answers.lock().unwrap(), vec!["B - second".to_string()]);
}

#[test]
fn esc_cancels_with_empty_answer() {
    let answers = Arc::new(Mutex::new(Vec::new()));
    let sink = answers.clone();
    let mut app = AppState::new("test1234", "test-model");
    app.open_clarify(
        "Which?".to_string(),
        vec!["A - first".to_string(), "B - second".to_string()],
        Box::new(move |a: String| sink.lock().unwrap().push(a)),
    );
    app.clarify_key(key(KeyCode::Esc));
    assert!(!app.clarify_open());
    assert_eq!(*answers.lock().unwrap(), vec!["".to_string()]);
}

#[test]
fn other_row_free_text() {
    let answers = Arc::new(Mutex::new(Vec::new()));
    let sink = answers.clone();
    let mut app = AppState::new("test1234", "test-model");
    app.open_clarify(
        "Which?".to_string(),
        vec!["A - first".to_string(), "B - second".to_string()],
        Box::new(move |a: String| sink.lock().unwrap().push(a)),
    );
    app.clarify_key(key(KeyCode::Down)); // cursor 0 -> 1
    app.clarify_key(key(KeyCode::Down)); // cursor 1 -> 2 (Other)
    app.clarify_key(key(KeyCode::Enter)); // enter typing mode
    assert!(app.clarify.as_ref().unwrap().typing);
    app.clarify_key(key(KeyCode::Char('x')));
    app.clarify_key(key(KeyCode::Char('y')));
    app.clarify_key(key(KeyCode::Enter));
    assert!(!app.clarify_open());
    assert_eq!(*answers.lock().unwrap(), vec!["xy".to_string()]);
}

#[test]
fn open_ended_immediate_typing() {
    let answers = Arc::new(Mutex::new(Vec::new()));
    let sink = answers.clone();
    let mut app = AppState::new("test1234", "test-model");
    app.open_clarify(
        "Name?".to_string(),
        vec![],
        Box::new(move |a: String| sink.lock().unwrap().push(a)),
    );
    assert!(app.clarify.as_ref().unwrap().typing);
    app.clarify_key(key(KeyCode::Char('h')));
    app.clarify_key(key(KeyCode::Char('i')));
    app.clarify_key(key(KeyCode::Enter));
    assert!(!app.clarify_open());
    assert_eq!(*answers.lock().unwrap(), vec!["hi".to_string()]);
}

#[test]
fn esc_in_typing_with_choices_returns_to_rows() {
    let answers = Arc::new(Mutex::new(Vec::new()));
    let sink = answers.clone();
    let mut app = AppState::new("test1234", "test-model");
    app.open_clarify(
        "Which?".to_string(),
        vec!["A - first".to_string(), "B - second".to_string()],
        Box::new(move |a: String| sink.lock().unwrap().push(a)),
    );
    // Enter typing mode via the Other row.
    app.clarify_key(key(KeyCode::Down));
    app.clarify_key(key(KeyCode::Down));
    app.clarify_key(key(KeyCode::Enter));
    assert!(app.clarify.as_ref().unwrap().typing);
    // Esc returns to option rows without closing the session.
    app.clarify_key(key(KeyCode::Esc));
    assert!(app.clarify_open());
    assert!(!app.clarify.as_ref().unwrap().typing);
    // Second Esc cancels with an empty answer.
    app.clarify_key(key(KeyCode::Esc));
    assert!(!app.clarify_open());
    assert_eq!(*answers.lock().unwrap(), vec!["".to_string()]);
}

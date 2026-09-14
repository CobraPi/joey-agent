//! Interactive clarify-question modal.
//!
//! Parity: upstream Hermes ui-tui/src/components/prompts.tsx `ClarifyPrompt` —
//! numbered option rows with a `▸` cursor, an auto-appended
//! "Other (type your answer)" row, 1-N quick-pick digits, and a free-text
//! typing mode. Cancel submits an empty answer (upstream onCancel →
//! onClarifyAnswer('')). joey-tui is tokio-free: the host supplies a plain
//! Send callback that receives the final answer string.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A pending clarify question shown as a centered modal over the TUI.
pub struct ClarifySession {
    pub question: String,
    pub choices: Vec<String>,
    /// Cursor row: 0..choices.len() is an option; choices.len() is "Other".
    pub cursor: usize,
    /// Free-text typing mode (the "Other" row, or any open-ended question).
    pub typing: bool,
    /// Text typed so far in typing mode.
    pub custom: String,
    answer: Option<Box<dyn FnOnce(String) + Send>>,
}

impl ClarifySession {
    pub fn new(
        question: String,
        choices: Vec<String>,
        answer: Box<dyn FnOnce(String) + Send>,
    ) -> Self {
        let typing = choices.is_empty();
        Self { question, choices, cursor: 0, typing, custom: String::new(), answer: Some(answer) }
    }

    fn submit(&mut self, text: String) {
        if let Some(answer) = self.answer.take() {
            answer(text);
        }
    }
}

impl crate::state::App {
    /// Open the clarify modal. `answer` is invoked exactly once with the
    /// user's answer (empty string = cancelled/timed out).
    pub fn open_clarify(
        &mut self,
        question: String,
        choices: Vec<String>,
        answer: Box<dyn FnOnce(String) + Send>,
    ) {
        self.clarify = Some(ClarifySession::new(question, choices, answer));
    }

    pub fn clarify_open(&self) -> bool {
        self.clarify.is_some()
    }

    /// Handle a key while the clarify modal is open. Returns true when the
    /// session is still open after handling (caller keeps swallowing keys).
    pub fn clarify_key(&mut self, key: KeyEvent) -> bool {
        let has_choices = !self.clarify.as_ref().map(|s| s.choices.is_empty()).unwrap_or(true);

        // Ctrl+C always cancels (upstream: Esc/Ctrl+C cancel).
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if let Some(mut session) = self.clarify.take() {
                session.submit(String::new());
            }
            return false;
        }

        let typing_now = self
            .clarify
            .as_ref()
            .map(|s| s.typing || s.choices.is_empty())
            .unwrap_or(false);

        if typing_now {
            match key.code {
                KeyCode::Esc => {
                    if let Some(s) = self.clarify.as_mut() {
                        if s.typing && has_choices {
                            s.typing = false; // back to option rows
                            return true;
                        }
                    }
                    if let Some(mut session) = self.clarify.take() {
                        session.submit(String::new());
                    }
                    false
                }
                KeyCode::Enter => {
                    if let Some(mut session) = self.clarify.take() {
                        let text = session.custom.trim().to_string();
                        session.submit(text);
                    }
                    false
                }
                KeyCode::Backspace => {
                    if let Some(s) = self.clarify.as_mut() {
                        s.custom.pop();
                    }
                    true
                }
                KeyCode::Char(ch) => {
                    if let Some(s) = self.clarify.as_mut() {
                        s.custom.push(ch);
                    }
                    true
                }
                _ => true,
            }
        } else {
            let last = self.clarify.as_ref().map(|s| s.choices.len()).unwrap_or(0); // index of the "Other" row
            match key.code {
                KeyCode::Esc => {
                    if let Some(mut session) = self.clarify.take() {
                        session.submit(String::new());
                    }
                    false
                }
                KeyCode::Up => {
                    if let Some(s) = self.clarify.as_mut() {
                        if s.cursor == 0 {
                            s.cursor = last;
                        } else {
                            s.cursor -= 1;
                        }
                    }
                    true
                }
                KeyCode::Down => {
                    if let Some(s) = self.clarify.as_mut() {
                        s.cursor = (s.cursor + 1) % (last + 1);
                    }
                    true
                }
                KeyCode::Enter => {
                    let cursor = self.clarify.as_ref().map(|s| s.cursor).unwrap_or(0);
                    if cursor == last {
                        if let Some(s) = self.clarify.as_mut() {
                            s.typing = true;
                            s.custom.clear();
                        }
                        true
                    } else {
                        if let Some(mut session) = self.clarify.take() {
                            let choice = session.choices[session.cursor].clone();
                            session.submit(choice);
                        }
                        false
                    }
                }
                KeyCode::Char(ch) if ch.is_ascii_digit() => {
                    let n = ch.to_digit(10).unwrap_or(0) as usize;
                    if n >= 1 && n <= last {
                        if let Some(mut session) = self.clarify.take() {
                            let choice = session.choices[n - 1].clone();
                            session.submit(choice);
                        }
                        false
                    } else {
                        true
                    }
                }
                _ => true,
            }
        }
    }
}

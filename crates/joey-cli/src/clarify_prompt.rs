//! Line-REPL rendering of a clarify question (upstream parity:
//! hermes_cli clarify callback + ui-tui ClarifyPrompt). Renders numbered
//! option rows (with an auto-appended "Other (type your answer)" row) and
//! reads one line: a digit in range picks that option; any other non-empty
//! text is the custom answer. Open-ended questions read free text. The
//! tool side enforces the 120s timeout; a failed oneshot send simply means
//! the user answered too late and the answer is discarded.

use reedline::{DefaultPrompt, DefaultPromptSegment, Reedline};

use crate::render;

/// Prompt the terminal user for a clarify answer and deliver it through
/// `resp_tx`. Returns when an answer was sent (or the channel is dead).
pub async fn run(
    question: String,
    choices: Vec<String>,
    resp_tx: tokio::sync::oneshot::Sender<String>,
) {
    // The blocking reedline read must not stall this async task's runtime
    // thread — same treatment as the main REPL loop's checkpointing.
    let joined = tokio::task::spawn_blocking(move || prompt_sync(&question, &choices));
    let Some(answer) = joined.await.unwrap_or(None) else { return };
    let _ = resp_tx.send(answer);
}

fn prompt_sync(question: &str, choices: &[String]) -> Option<String> {
    println!();
    render::info(&format!("❓ {question}"));
    let open_ended = choices.is_empty();
    if !open_ended {
        for (i, choice) in choices.iter().enumerate() {
            println!("  {}. {}", i + 1, choice);
        }
        println!("  {}. Other (type your answer)", choices.len() + 1);
        render::info(&format!(
            "reply with 1-{} (or your own answer)",
            choices.len()
        ));
    }
    let mut editor = Reedline::create();
    let prompt = DefaultPrompt::new(DefaultPromptSegment::Empty, DefaultPromptSegment::Empty);
    loop {
        let sig = editor.read_line(&prompt);
        let Ok(reedline::Signal::Success(line)) = sig else { return None };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !open_ended {
            if let Some(n) = line.parse::<usize>().ok() {
                if n >= 1 && n <= choices.len() {
                    return Some(choices[n - 1].clone());
                }
            }
        }
        return Some(line.to_string());
    }
}

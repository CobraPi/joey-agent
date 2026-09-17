//! The `clarify` tool — ask the user a structured question with options.
//!
//! In interactive sessions, sends a ClarifyRequest event and awaits the
//! user's response via a oneshot channel. In non-interactive sessions,
//! returns an error immediately.

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

use crate::registry::{Tool, ToolResult};
use crate::ToolContext;

/// A clarification request sent to the UI layer.
#[derive(Debug)]
pub struct ClarifyRequest {
    pub question: String,
    pub choices: Vec<String>,
    pub response_tx: oneshot::Sender<String>,
}

/// Upstream parity: hermes_cli/callbacks.py `clarify` timeout (120s).
pub const CLARIFY_TIMEOUT_SECS: u64 = 120;

/// Returned in the envelope when the user does not answer in time
/// (upstream hermes_cli/callbacks.py clarify_callback timeout path).
pub const CLARIFY_TIMEOUT_RESPONSE: &str = "The user did not provide a response within the time limit. Use your best judgement to make the choice and proceed.";

/// The clarify tool.
pub struct Clarify {
    /// Channel for sending clarify requests to the UI layer.
    clarify_tx: Option<mpsc::UnboundedSender<ClarifyRequest>>,
    /// How long to wait for the user before falling back.
    timeout: std::time::Duration,
}

impl Clarify {
    pub fn new(clarify_tx: Option<mpsc::UnboundedSender<ClarifyRequest>>) -> Self {
        Self { clarify_tx, timeout: std::time::Duration::from_secs(CLARIFY_TIMEOUT_SECS) }
    }
}

#[async_trait]
impl Tool for Clarify {
    fn name(&self) -> &str {
        "clarify"
    }

    fn toolset(&self) -> &str {
        "clarify"
    }

    fn description(&self) -> &str {
        "Ask the user a question when you need clarification, feedback, or a decision before proceeding. Supports two modes:\n\n1. **Multiple choice** — provide up to 4 choices. The user picks one or types their own answer via a 5th 'Other' option.\n2. **Open-ended** — omit choices entirely. The user types a free-form response.\n\nCRITICAL: when you are offering options, put each option ONLY in the `choices` array — NEVER enumerate the options inside the `question` text. The UI renders `choices` as selectable rows; options written into the question string render as dead prose the user can't pick. Right: question='Which deployment target?', choices=['staging', 'prod']. Wrong: question='Which target? 1) staging 2) prod', choices=[].\n\nUse this tool when:\n- The task is ambiguous and you need the user to choose an approach\n- You want post-task feedback ('How did that work out?')\n- You want to offer to save a skill or update memory\n- A decision has meaningful trade-offs the user should weigh in on\n\nDo NOT use this tool for simple yes/no confirmation of dangerous commands (the terminal tool handles that). Prefer making a reasonable default choice yourself when the decision is low-stakes."
    }

    fn check(&self, ctx: &ToolContext) -> bool {
        ctx.interactive() && self.clarify_tx.is_some()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question itself, and ONLY the question (e.g. 'Which deployment target?'). Do NOT embed the answer options here — pass them as separate elements in `choices`."
                },
                "choices": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "REQUIRED whenever you are presenting selectable options: each distinct option is its own array element (up to 4). The UI renders these as pickable rows and auto-appends an 'Other (type your answer)' option. Omit this parameter entirely ONLY for a genuinely open-ended free-text question.",
                    "maxItems": 4
                }
            },
            "required": ["question"]
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        // Non-interactive sessions get an immediate error.
        if !ctx.interactive() {
            return ToolResult::Error(
                "Clarification requested but session is non-interactive.".to_string(),
            );
        }

        let Some(tx) = &self.clarify_tx else {
            return ToolResult::Error(
                "Clarification requested but no clarify channel is available.".to_string(),
            );
        };

        let question = match args.get("question").and_then(|v| v.as_str()) {
            Some(q) => q.to_string(),
            None => return ToolResult::Error("question is required".to_string()),
        };

        let choices: Vec<String> = args
            .get("choices")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let (resp_tx, resp_rx) = oneshot::channel();
        let req = ClarifyRequest {
            question: question.clone(),
            choices: choices.clone(),
            response_tx: resp_tx,
        };

        if tx.send(req).is_err() {
            return ToolResult::Error("Failed to send clarification request to UI.".to_string());
        }
        // Upstream parity: hermes_cli/callbacks.py clarify_callback blocks with a
        // timeout (default 120s) and returns a best-judgement fallback string.
        // A dropped UI channel (Ok(Err)) is treated the same as a timeout:
        // upstream has no "channel closed" concept — an unanswered clarify
        // simply runs out the clock and falls back to best judgement.
        match tokio::time::timeout(self.timeout, resp_rx).await {
            Ok(Ok(response)) => clarify_envelope(question, choices, response),
            Ok(Err(_)) | Err(_) => {
                clarify_envelope(question, choices, CLARIFY_TIMEOUT_RESPONSE.to_string())
            }
        }
    }
}

/// Upstream parity: tools/clarify_tool.py returns
/// `{"question", "choices_offered", "user_response"}` as JSON.
fn clarify_envelope(question: String, choices: Vec<String>, response: String) -> ToolResult {
    let envelope = serde_json::json!({
        "question": question,
        "choices_offered": choices,
        "user_response": response.trim(),
    });
    ToolResult::Text(serde_json::to_string(&envelope).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(interactive: bool) -> ToolContext {
        let c = ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "test");
        if interactive {
            c
        } else {
            c.with_interactive(false)
        }
    }

    #[tokio::test]
    async fn non_interactive_returns_error() {
        let tool = Clarify::new(None);
        let c = ctx(false);
        let result = tool
            .execute(json!({"question": "test?"}), &c)
            .await;
        assert!(result.is_error());
    }

    #[tokio::test]
    async fn interactive_without_channel_returns_error() {
        let tool = Clarify::new(None);
        let c = ctx(true);
        let result = tool
            .execute(json!({"question": "test?"}), &c)
            .await;
        assert!(result.is_error());
    }

    #[tokio::test]
    async fn interactive_with_channel_returns_response() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let tool = Clarify::new(Some(tx));
        let c = ctx(true);

        // Spawn a task to respond to the clarification.
        tokio::spawn(async move {
            if let Some(req) = rx.recv().await {
                assert_eq!(req.question, "Which option?");
                assert_eq!(req.choices.len(), 2);
                let _ = req.response_tx.send("option A".to_string());
            }
        });

        let result = tool
            .execute(
                json!({
                    "question": "Which option?",
                    "choices": ["option A", "option B"]
                }),
                &c,
            )
            .await;

        assert!(!result.is_error());
        let text = result.to_content_string();
        let v: serde_json::Value = serde_json::from_str(&text).expect("envelope is valid JSON");
        assert_eq!(v["user_response"], "option A");
        assert_eq!(v["question"], "Which option?");
    }

    #[tokio::test]
    async fn timeout_returns_best_judgement_envelope() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let tool = Clarify { clarify_tx: Some(tx), timeout: std::time::Duration::from_millis(50) };
        let c = ctx(true);

        let handle = tokio::spawn(async move {
            tool.execute(
                json!({
                    "question": "Pick one:",
                    "choices": ["a", "b"]
                }),
                &c,
            )
            .await
        });

        // Receive the request but NEVER answer: drop response_tx.
        if let Some(req) = rx.recv().await {
            // Keep the channel open but never answer: exercises the TIMEOUT
            // path deterministically (dropping the sender would race the
            // 50ms timeout against the channel-closed error arm).
            std::mem::forget(req.response_tx);
        }

        let result = handle.await.expect("task join");
        assert!(!result.is_error());
        let text = result.to_content_string();
        let v: serde_json::Value = serde_json::from_str(&text).expect("envelope is valid JSON");
        assert_eq!(v["user_response"], CLARIFY_TIMEOUT_RESPONSE);
    }

    #[tokio::test]
    async fn empty_choices_open_ended_roundtrip() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let tool = Clarify::new(Some(tx));
        let c = ctx(true);

        // Spawn a task to respond to the clarification.
        tokio::spawn(async move {
            if let Some(req) = rx.recv().await {
                assert!(req.choices.is_empty());
                let _ = req.response_tx.send("free text answer".to_string());
            }
        });

        let result = tool
            .execute(
                json!({
                    "question": "Any thoughts?",
                    "choices": []
                }),
                &c,
            )
            .await;

        assert!(!result.is_error());
        let text = result.to_content_string();
        let v: serde_json::Value = serde_json::from_str(&text).expect("envelope is valid JSON");
        assert_eq!(v["user_response"], "free text answer");
        assert_eq!(v["choices_offered"], serde_json::json!([]));
    }
}

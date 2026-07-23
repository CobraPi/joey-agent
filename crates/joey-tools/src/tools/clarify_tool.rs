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

/// The clarify tool.
pub struct Clarify {
    /// Channel for sending clarify requests to the UI layer.
    clarify_tx: Option<mpsc::UnboundedSender<ClarifyRequest>>,
}

impl Clarify {
    pub fn new(clarify_tx: Option<mpsc::UnboundedSender<ClarifyRequest>>) -> Self {
        Self { clarify_tx }
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
        "Ask the user a structured question when genuine ambiguity blocks progress. \
         Presents clear options (multiple-choice or open-ended) rather than guessing \
         silently. Reserved for decisions where the wrong choice has significant \
         downstream cost. Not for simple yes/no confirmation."
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
                    "description": "The question itself, and ONLY the question. Do NOT embed answer options here — pass them as the 'choices' array."
                },
                "choices": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Up to 4 distinct, mutually exclusive options. The UI renders these as selectable rows. Omit entirely for a genuinely open-ended free-text question.",
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

        match resp_rx.await {
            Ok(response) => ToolResult::Text(response),
            Err(_) => {
                ToolResult::Error("Clarification channel closed without response.".to_string())
            }
        }
    }
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
        assert_eq!(result.to_content_string(), "option A");
    }
}

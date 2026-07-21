//! Provider-agnostic message and response types (port of
//! `agent/transports/types.py` + the OpenAI/Anthropic message shapes).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A chat message in provider-neutral form. Serializes to the OpenAI wire
/// shape by default; adapters re-map for Anthropic/Bedrock/etc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    /// Text content. For multimodal user turns, use [`Message::content_parts`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Structured multimodal parts (text + images), when present this wins over
    /// `content` for wire serialization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_parts: Option<Vec<ContentPart>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Reasoning / thinking text (upstream `reasoning_content`); replayed as
    /// an unsigned thinking block on the Anthropic wire when no signed blocks
    /// exist (anthropic_adapter.py:2051-2057).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Preserved thinking / redacted_thinking blocks from a prior Anthropic
    /// response (upstream `reasoning_details`). Replayed as leading content
    /// blocks on subsequent assistant turns (anthropic_adapter.py:1828-1842).
    /// A JSON array of `{"type": "thinking"|"redacted_thinking", ...}` blocks
    /// on the Anthropic wire; opaque provider data on the chat wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<Value>,
    /// Verbatim, order-preserving Anthropic content-block list captured from a
    /// turn that interleaved SIGNED thinking with tool_use. When present the
    /// Anthropic adapter replays it unchanged (block order is signature-bound;
    /// anthropic_adapter.py:1964-2005, transports/anthropic.py:97-183).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_content_blocks: Option<Value>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self::text("system", text)
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self::text("user", text)
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self::text("assistant", text)
    }

    fn text(role: &str, text: impl Into<String>) -> Self {
        Self {
            role: role.to_string(),
            content: Some(text.into()),
            content_parts: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            reasoning: None,
            reasoning_details: None,
            anthropic_content_blocks: None,
        }
    }

    /// A tool-result message answering a specific tool call.
    pub fn tool_result(tool_call_id: impl Into<String>, name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_string(),
            content: Some(content.into()),
            content_parts: None,
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
            name: Some(name.into()),
            reasoning: None,
            reasoning_details: None,
            anthropic_content_blocks: None,
        }
    }

    /// An assistant message carrying tool calls (and optional text).
    pub fn assistant_with_tools(text: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: "assistant".to_string(),
            content: text,
            content_parts: None,
            tool_calls,
            tool_call_id: None,
            name: None,
            reasoning: None,
            reasoning_details: None,
            anthropic_content_blocks: None,
        }
    }

    /// Best-effort plain-text view of the message content.
    pub fn text_content(&self) -> String {
        if let Some(c) = &self.content {
            return c.clone();
        }
        if let Some(parts) = &self.content_parts {
            return parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
        }
        String::new()
    }
}

/// A multimodal content part.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image_url")]
    ImageUrl { image_url: ImageUrl },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
}

/// A tool/function call requested by the assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "default_tool_type")]
    pub call_type: String,
    pub function: FunctionCall,
}

fn default_tool_type() -> String {
    "function".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// Arguments as a JSON string (OpenAI wire convention).
    pub arguments: String,
}

impl ToolCall {
    pub fn new(id: impl Into<String>, name: impl Into<String>, arguments: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: name.into(),
                arguments: arguments.into(),
            },
        }
    }

    /// Parse the arguments JSON string into a value (empty object on failure).
    pub fn parsed_args(&self) -> Value {
        serde_json::from_str(&self.function.arguments)
            .unwrap_or(Value::Object(Default::default()))
    }
}

/// A tool definition sent to the model (OpenAI function-tool shape).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    #[serde(rename = "type", default = "default_tool_type")]
    pub tool_type: String,
    pub function: FunctionSchema,
    /// Optional Anthropic prompt-cache marker. Forwarded onto the Anthropic
    /// tool entry when present (anthropic_adapter.py:1716-1721); never
    /// serialized on the OpenAI wire when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionSchema {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub parameters: Value,
}

impl ToolSchema {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            tool_type: "function".to_string(),
            function: FunctionSchema {
                name: name.into(),
                description: description.into(),
                parameters,
            },
            cache_control: None,
        }
    }
}

/// Why generation stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
}

impl FinishReason {
    /// Map a wire finish/stop reason. Mirrors upstream's Anthropic
    /// `_STOP_REASON_MAP` (transports/anthropic.py:234-241) plus the OpenAI
    /// finish reasons: `refusal` → ContentFilter,
    /// `model_context_window_exceeded` → Length; unknown → Stop.
    pub fn from_wire(s: &str) -> FinishReason {
        match s {
            "tool_calls" | "tool_use" => FinishReason::ToolCalls,
            "length" | "max_tokens" | "model_context_window_exceeded" => FinishReason::Length,
            "content_filter" | "refusal" => FinishReason::ContentFilter,
            _ => FinishReason::Stop,
        }
    }
}

/// Token usage for a response.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

/// The normalized result of a provider call, regardless of wire protocol.
#[derive(Debug, Clone)]
pub struct NormalizedResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason,
    pub reasoning: Option<String>,
    pub usage: Usage,
    /// The model actually used (as reported by the provider), if known.
    pub model: Option<String>,
    /// Provider replay data: Anthropic thinking / redacted_thinking blocks
    /// (signed) or the chat wire's `reasoning_details` payload. Callers store
    /// this on the assistant [`Message`] so the next turn can replay it.
    pub reasoning_details: Option<Value>,
    /// Verbatim ordered Anthropic content blocks — populated only when the
    /// turn interleaved signed thinking with tool_use (the only shape the
    /// parallel lists reconstruct incorrectly; transports/anthropic.py:167-183).
    pub anthropic_content_blocks: Option<Value>,
}

impl NormalizedResponse {
    pub fn empty() -> Self {
        Self {
            content: String::new(),
            tool_calls: Vec::new(),
            finish_reason: FinishReason::Stop,
            reasoning: None,
            usage: Usage::default(),
            model: None,
            reasoning_details: None,
            anthropic_content_blocks: None,
        }
    }
}

/// A streaming delta emitted during a streamed generation.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A chunk of assistant text.
    ContentDelta(String),
    /// A chunk of reasoning/thinking text.
    ReasoningDelta(String),
    /// The stream finished; carries the fully-assembled response.
    Done(NormalizedResponse),
}

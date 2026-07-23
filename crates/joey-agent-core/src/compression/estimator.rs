//! Rough token estimators (port of `agent/model_metadata.py`
//! `estimate_messages_tokens_rough` / `estimate_request_tokens_rough` and the
//! compressor's per-message budget estimator, context_compressor.py:360-446).

use joey_providers::{ContentPart, Message, ToolSchema};
use serde_json::{json, Value};

/// Chars per token rough estimate (context_compressor.py:281).
pub const CHARS_PER_TOKEN: usize = 4;
/// Flat token cost per attached image part in the compressor's budget walks
/// (context_compressor.py:287 — matches Claude Code's IMAGE_TOKEN_ESTIMATE).
pub const IMAGE_TOKEN_ESTIMATE: usize = 1600;
/// The same figure in char-budget currency (context_compressor.py:291).
pub const IMAGE_CHAR_EQUIVALENT: usize = IMAGE_TOKEN_ESTIMATE * CHARS_PER_TOKEN;
/// Flat per-image cost in the request-level estimator
/// (model_metadata.py:2620 `_IMAGE_TOKEN_COST` — the Anthropic pricing model).
const REQUEST_IMAGE_TOKEN_COST: usize = 1500;

/// `estimate_tokens_rough`: ceiling division so short texts never estimate 0.
pub fn estimate_tokens_rough(text: &str) -> i64 {
    joey_core::utils::estimate_tokens(text) as i64
}

/// Effective char-length of a message's content for token budgeting
/// (`_content_length_for_budget`): text length plus a flat
/// [`IMAGE_CHAR_EQUIVALENT`] per image part.
pub fn content_length_for_budget(msg: &Message) -> usize {
    if let Some(parts) = &msg.content_parts {
        let mut total = 0usize;
        for p in parts {
            match p {
                ContentPart::Text { text } => total += text.len(),
                ContentPart::ImageUrl { .. } => total += IMAGE_CHAR_EQUIVALENT,
            }
        }
        return total;
    }
    msg.content.as_deref().map(str::len).unwrap_or(0)
}

/// Stable char-length for non-content replay/metadata fields
/// (`_serialized_length_for_budget`).
fn serialized_length_for_budget(value: Option<&Value>) -> usize {
    match value {
        None | Some(Value::Null) => 0,
        Some(Value::String(s)) => s.len(),
        Some(v) => serde_json::to_string(v).map(|s| s.len()).unwrap_or(0),
    }
}

/// Token estimate for one message in the tail-protection budget walks
/// (`_estimate_msg_budget_tokens`): content chars/4 + 10 role overhead + the
/// FULL tool_call envelope + provider replay fields (`_REPLAY_BUDGET_KEYS` —
/// the port's replay fields are `reasoning`, `reasoning_details`, and
/// `anthropic_content_blocks`).
pub fn estimate_msg_budget_tokens(msg: &Message) -> i64 {
    let content_len = content_length_for_budget(msg);
    let mut tokens = (content_len / CHARS_PER_TOKEN) as i64 + 10; // +10 for role/key overhead
    for tc in &msg.tool_calls {
        let serialized = serde_json::to_string(tc).unwrap_or_default();
        tokens += (serialized.len() / CHARS_PER_TOKEN) as i64;
    }
    if let Some(r) = &msg.reasoning {
        tokens += (r.len() / CHARS_PER_TOKEN) as i64;
    }
    tokens += (serialized_length_for_budget(msg.reasoning_details.as_ref()) / CHARS_PER_TOKEN) as i64;
    tokens +=
        (serialized_length_for_budget(msg.anthropic_content_blocks.as_ref()) / CHARS_PER_TOKEN) as i64;
    tokens
}

/// Count image-like content parts in a message (`_count_image_tokens`):
/// multimodal parts plus stashed Anthropic image blocks.
fn count_image_tokens(msg: &Message, cost_per_image: usize) -> usize {
    let mut count = 0usize;
    if let Some(parts) = &msg.content_parts {
        count += parts.iter().filter(|p| matches!(p, ContentPart::ImageUrl { .. })).count();
    }
    if let Some(Value::Array(blocks)) = &msg.anthropic_content_blocks {
        count += blocks
            .iter()
            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("image"))
            .count();
    }
    count * cost_per_image
}

/// Char count for token estimation, excluding base64 image data
/// (`_estimate_message_chars`): the message is serialized with image parts
/// replaced by a short `[stripped]` marker and the Anthropic block stash
/// excluded, then measured. (Upstream measures `len(str(dict))`; the port
/// measures the JSON serialization — the same rough size class.)
fn estimate_message_chars(msg: &Message) -> usize {
    let content: Value = if let Some(parts) = &msg.content_parts {
        Value::Array(
            parts
                .iter()
                .map(|p| match p {
                    ContentPart::Text { text } => json!({"type": "text", "text": text}),
                    ContentPart::ImageUrl { .. } => json!({"type": "image_url", "image": "[stripped]"}),
                })
                .collect(),
        )
    } else {
        msg.content.clone().map(Value::String).unwrap_or(Value::Null)
    };
    let mut shadow = serde_json::Map::new();
    shadow.insert("role".to_string(), Value::String(msg.role.clone()));
    shadow.insert("content".to_string(), content);
    if !msg.tool_calls.is_empty() {
        shadow.insert(
            "tool_calls".to_string(),
            serde_json::to_value(&msg.tool_calls).unwrap_or(Value::Null),
        );
    }
    if let Some(id) = &msg.tool_call_id {
        shadow.insert("tool_call_id".to_string(), Value::String(id.clone()));
    }
    if let Some(n) = &msg.name {
        shadow.insert("name".to_string(), Value::String(n.clone()));
    }
    if let Some(r) = &msg.reasoning {
        shadow.insert("reasoning".to_string(), Value::String(r.clone()));
    }
    if let Some(rd) = &msg.reasoning_details {
        shadow.insert("reasoning_details".to_string(), rd.clone());
    }
    serde_json::to_string(&Value::Object(shadow)).map(|s| s.len()).unwrap_or(0)
}

/// Rough token estimate for a message list (`estimate_messages_tokens_rough`):
/// serialized chars/4 (ceiling) + a flat ~1500 tokens per image.
pub fn estimate_messages_tokens_rough(messages: &[Message]) -> i64 {
    let mut total_chars = 0usize;
    let mut image_tokens = 0usize;
    for msg in messages {
        total_chars += estimate_message_chars(msg);
        image_tokens += count_image_tokens(msg, REQUEST_IMAGE_TOKEN_COST);
    }
    total_chars.div_ceil(4) as i64 + image_tokens as i64
}

/// Rough token estimate for a full chat request
/// (`estimate_request_tokens_rough`): system prompt + messages + tool schemas.
pub fn estimate_request_tokens_rough(
    messages: &[Message],
    system_prompt: &str,
    tools: Option<&[ToolSchema]>,
) -> i64 {
    let mut total: i64 = 0;
    if !system_prompt.is_empty() {
        total += system_prompt.len().div_ceil(4) as i64;
    }
    if !messages.is_empty() {
        total += estimate_messages_tokens_rough(messages);
    }
    if let Some(tools) = tools {
        total += estimate_tools_tokens_rough(tools);
    }
    total
}

/// Fast, stable rough estimate over the major schema fields
/// (`_estimate_tools_tokens_rough`; the id()-keyed memo cache is not needed —
/// this is already cheap in Rust).
pub fn estimate_tools_tokens_rough(tools: &[ToolSchema]) -> i64 {
    if tools.is_empty() {
        return 0;
    }
    let mut total_chars = 0usize;
    for tool in tools {
        total_chars += tool.function.name.len();
        total_chars += tool.function.description.len();
        total_chars += serde_json::to_string(&tool.function.parameters)
            .map(|s| s.len())
            .unwrap_or(0);
    }
    total_chars.div_ceil(4) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use joey_providers::ImageUrl;

    #[test]
    fn image_parts_count_flat() {
        let mut m = Message::user("");
        m.content = None;
        m.content_parts = Some(vec![
            ContentPart::Text { text: "hi".into() },
            ContentPart::ImageUrl {
                image_url: ImageUrl { url: format!("data:image/png;base64,{}", "A".repeat(100_000)) },
            },
        ]);
        // Budget walk: text 2 chars + IMAGE_CHAR_EQUIVALENT.
        assert_eq!(content_length_for_budget(&m), 2 + IMAGE_CHAR_EQUIVALENT);
        // Request estimate: base64 payload must NOT be counted as chars —
        // one image ≈ 1500 tokens, not ~25K.
        let est = estimate_messages_tokens_rough(&[m]);
        assert!(est < 2000, "image over-counted: {}", est);
        assert!(est >= 1500);
    }

    #[test]
    fn budget_counts_full_tool_call_envelope() {
        let mut m = Message::assistant_with_tools(
            Some(String::new()),
            vec![joey_providers::ToolCall::new("call_1", "terminal", r#"{"command":"ls"}"#)],
        );
        m.content = Some(String::new());
        let t = estimate_msg_budget_tokens(&m);
        // id + type + function name + args + JSON structure ≈ 70+ chars → >17
        // tokens; args alone would be ~4.
        assert!(t > 20, "tool_call envelope undercounted: {}", t);
    }
}

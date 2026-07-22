//! Real-user-anchor machinery (port of `agent/conversation_compression.py`
//! `_is_real_user_message` / `_insert_real_user_anchor` /
//! `_merge_anchor_into_user_message` / `_ensure_compressed_has_user_turn`).

use joey_providers::{ContentPart, Message};

/// conversation_compression.py:553-559.
pub const SYNTHETIC_USER_PREFIXES: &[&str] = &[
    "[System: Your previous response was truncated",
    "[System: The previous response was cut off",
    "[System: Your previous tool call",
    "[Your active task list was preserved across context compression]",
    "[IMPORTANT: Background process ",
];

/// `_message_text`.
pub(crate) fn message_text(message: &Message) -> String {
    if let Some(parts) = &message.content_parts {
        return parts
            .iter()
            .map(|p| match p {
                ContentPart::Text { text } => text.clone(),
                ContentPart::ImageUrl { .. } => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    message.content.clone().unwrap_or_default()
}

/// Distinguish human intent from user-role runtime scaffolding
/// (`_is_real_user_message`). A compaction summary pinned to `role="user"`
/// is scaffolding too.
pub fn is_real_user_message(message: &Message) -> bool {
    if message.role != "user" {
        return false;
    }
    if message.synthetic {
        return false;
    }
    let text = message_text(message).trim().to_string();
    if text.is_empty() {
        return false;
    }
    if SYNTHETIC_USER_PREFIXES.iter().any(|p| text.starts_with(p)) {
        return false;
    }
    !super::compressor::ContextCompressor::is_context_summary_content(&text)
}

/// Fold the human anchor into an existing user-role scaffolding turn
/// (`_merge_anchor_into_user_message`): the anchor text leads, the
/// scaffolding content follows, and the synthetic flags are cleared.
fn merge_anchor_into_user_message(target: &mut Message, anchor: &Message) {
    if anchor.content_parts.is_some() || target.content_parts.is_some() {
        let mut anchor_parts: Vec<ContentPart> = anchor
            .content_parts
            .clone()
            .unwrap_or_else(|| vec![ContentPart::Text {
                text: anchor.content.clone().unwrap_or_default(),
            }]);
        let target_parts: Vec<ContentPart> = target
            .content_parts
            .clone()
            .unwrap_or_else(|| vec![ContentPart::Text {
                text: target.content.clone().unwrap_or_default(),
            }]);
        anchor_parts.extend(target_parts);
        target.content_parts = Some(anchor_parts);
        target.content = None;
    } else {
        let merged = format!(
            "{}\n\n{}",
            anchor.content.clone().unwrap_or_default(),
            target.content.clone().unwrap_or_default()
        )
        .trim()
        .to_string();
        target.content = Some(merged);
    }
    target.synthetic = false;
}

/// Insert the latest human turn without breaking role alternation
/// (`_insert_real_user_anchor`).
pub fn insert_real_user_anchor(messages: &mut Vec<Message>, anchor: Message) {
    // Preferred: the summary boundary — before the first assistant message
    // not already preceded by a user turn.
    for index in 0..messages.len() {
        if messages[index].role != "assistant" {
            continue;
        }
        let previous_is_user = index > 0 && messages[index - 1].role == "user";
        if !previous_is_user {
            messages.insert(index, anchor);
            return;
        }
    }
    // Every assistant is user-preceded (or there are none). Appending is
    // safe whenever the transcript does not already end with a user turn.
    let ends_with_user = messages.last().map(|m| m.role == "user").unwrap_or(false);
    if !ends_with_user {
        messages.push(anchor);
        return;
    }
    let last = messages.last().unwrap();
    if super::compressor::ContextCompressor::is_context_summary_content(&message_text(last)) {
        // Never merge into a compaction summary: the summary prefix must stay
        // at the start of its message for downstream summary detection.
        messages.push(anchor);
        return;
    }
    // Trailing user-role scaffolding (e.g. the todo snapshot): merge instead
    // of inserting a consecutive same-role message (#55677).
    let last = messages.last_mut().unwrap();
    merge_anchor_into_user_message(last, &anchor);
}

/// Preserve human intent, not merely a synthetic user-role placeholder
/// (`_ensure_compressed_has_user_turn`).
pub fn ensure_compressed_has_user_turn(original_messages: &[Message], compressed: &mut Vec<Message>) {
    if compressed.iter().any(is_real_user_message) {
        return;
    }
    for message in original_messages.iter().rev() {
        if is_real_user_message(message) {
            insert_real_user_anchor(compressed, message.clone());
            return;
        }
    }
    compressed.push(Message::user(
        "Continue from the compressed conversation context above. \
This marker exists because no human user turn was available.",
    ));
}

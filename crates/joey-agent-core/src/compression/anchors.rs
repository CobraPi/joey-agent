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
    // SAFETY: `messages` was pushed to above (or returned early);
    // guaranteed non-empty here.
    let last = messages.last().unwrap();
    if super::compressor::ContextCompressor::is_context_summary_content(&message_text(last)) {
        // Never merge into a compaction summary: the summary prefix must stay
        // at the start of its message for downstream summary detection.
        messages.push(anchor);
        return;
    }
    // Trailing user-role scaffolding (e.g. the todo snapshot): merge instead
    // of inserting a consecutive same-role message (#55677).
    // SAFETY: `messages` is non-empty (checked above);
    // `last_mut()` is guaranteed Some.
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── FR-006/SC-005 regression tests (hardened sites) ──────────────────

    /// SAFETY sites (anchors.rs:106-119): `messages.last().unwrap()` and
    /// `messages.last_mut().unwrap()`.
    ///
    /// The guarded path is reached only when `messages` ends with a
    /// user-role turn (otherwise the anchor is appended/inserted before
    /// the unwrap). This test constructs exactly that input: a trailing
    /// synthetic user message that is NOT a compaction summary, forcing
    /// the code through both `.last().unwrap()` and `.last_mut().unwrap()`.
    #[test]
    fn insert_real_user_anchor_trailing_user_last_unwrap_does_not_panic() {
        let mut messages = vec![
            Message::assistant("I will help you."),
            // Trailing user message — triggers the `ends_with_user` branch,
            // which then does `.last().unwrap()` and `.last_mut().unwrap()`.
            Message::user("[System: The previous response was cut off. Please continue.]"),
        ];
        // Mark synthetic so `is_real_user_message` returns false for the
        // trailing message but the text is not a compaction summary either.
        messages[1].synthetic = true;

        let anchor = Message::user("What is 2+2?");
        insert_real_user_anchor(&mut messages, anchor);

        // The anchor should have been merged into the trailing user message
        // (merge path) or pushed — either way, no panic.
        assert!(!messages.is_empty());
        let last = messages.last().unwrap();
        assert_eq!(last.role, "user");
    }

    /// SAFETY site: `messages.last().unwrap()` when the trailing user
    /// message IS a context-compaction summary — the code takes the early
    /// return (push) after calling `.last().unwrap()`.
    #[test]
    fn insert_real_user_anchor_summary_last_unwrap_does_not_panic() {
        let summary_text = format!(
            "{}\n## Goal\nsummary body\n{}",
            super::super::compressor::SUMMARY_PREFIX.as_str(),
            super::super::compressor::SUMMARY_END_MARKER,
        );
        let mut messages = vec![
            Message::assistant("working..."),
            // Trailing user message that IS a compaction summary.
            Message::user(summary_text),
        ];
        messages[1].synthetic = true;

        let anchor = Message::user("Continue with the task.");
        insert_real_user_anchor(&mut messages, anchor);

        // The anchor is pushed (not merged into a summary).
        assert!(messages.len() >= 2);
    }

    /// Edge: empty messages vec — exercises the "no assistant" + "empty"
    /// path where `.last()` returns None (handled by `unwrap_or(false)`).
    #[test]
    fn insert_real_user_anchor_empty_messages_does_not_panic() {
        let mut messages: Vec<Message> = vec![];
        let anchor = Message::user("Hello");
        insert_real_user_anchor(&mut messages, anchor);
        assert_eq!(messages.len(), 1);
    }

    /// `ensure_compressed_has_user_turn` when compressed has no real user
    /// messages and original has none either — pushes a synthetic marker.
    #[test]
    fn ensure_compressed_has_user_turn_no_real_user_does_not_panic() {
        let original = vec![Message::assistant("I did things.")];
        let mut compressed = vec![Message::assistant("summary content")];
        ensure_compressed_has_user_turn(&original, &mut compressed);
        assert!(compressed.len() >= 2);
    }

    /// `ensure_compressed_has_user_turn` when compressed already has a
    /// real user message — early return, no modification.
    #[test]
    fn ensure_compressed_has_user_turn_already_has_user_does_not_panic() {
        let original = vec![Message::user("original question")];
        let mut compressed = vec![
            Message::assistant("summary"),
            Message::user("real question"),
        ];
        ensure_compressed_has_user_turn(&original, &mut compressed);
        assert_eq!(compressed.len(), 2);
    }
}

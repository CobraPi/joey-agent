//! Pluggable context-engine interface (port of `agent/context_engine.py`).
//!
//! A context engine controls how conversation context is managed when
//! approaching the model's token limit. The built-in [`ContextCompressor`]
//! (`super::compressor`) is the default — and currently only — implementation;
//! the trait keeps the plugin-engine shape so alternates remain possible.

use async_trait::async_trait;
use joey_providers::Message;
use serde_json::json;
use std::collections::HashMap;

/// context_engine.py:34-37.
pub const MEMORY_CONTEXT_MAX_CHARS: usize = 6_000;
const MEMORY_CONTEXT_HEAD_CHARS: usize = 4_000;
const MEMORY_CONTEXT_TAIL_CHARS: usize = 1_500;
const MEMORY_CONTEXT_TRUNCATION_MARKER: &str = "\n...[memory provider context truncated]...\n";

/// Prepare provider context for a context-engine/LLM egress boundary
/// (`sanitize_memory_context`): force-redact, then head+tail truncate.
pub fn sanitize_memory_context(memory_context: &str) -> String {
    let sanitized = joey_core::redact::redact_sensitive_text_opts(
        memory_context.trim(),
        joey_core::redact::RedactOptions {
            force: true,
            redact_url_credentials: true,
            ..Default::default()
        },
    );
    let chars: Vec<char> = sanitized.chars().collect();
    if chars.len() <= MEMORY_CONTEXT_MAX_CHARS {
        return sanitized;
    }
    let head: String = chars[..MEMORY_CONTEXT_HEAD_CHARS].iter().collect();
    let tail: String = chars[chars.len() - MEMORY_CONTEXT_TAIL_CHARS..].iter().collect();
    format!("{}{}{}", head, MEMORY_CONTEXT_TRUNCATION_MARKER, tail)
}

/// Normalized usage payload handed to `update_from_response` (the
/// context_engine.py usage-dict contract: legacy keys always present,
/// canonical buckets optional).
#[derive(Debug, Clone, Default)]
pub struct UsageUpdate {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub reasoning_tokens: i64,
}

/// Base interface all context engines must implement
/// (`ContextEngine`, context_engine.py:56-263).
///
/// Token state (`last_prompt_tokens`, `threshold_tokens`, `context_length`,
/// `compression_count`) is exposed through accessor methods instead of the
/// upstream public attributes.
#[async_trait]
pub trait ContextEngine: Send + Sync {
    /// Short identifier (e.g. "compressor", "lcm").
    fn name(&self) -> &str;

    // -- Token state (read by the loop/CLI for display + gating) --------
    fn last_prompt_tokens(&self) -> i64;
    fn threshold_tokens(&self) -> i64;
    fn context_length(&self) -> i64;
    fn compression_count(&self) -> u32;

    /// Update tracked token usage from an API response.
    fn update_from_response(&mut self, usage: &UsageUpdate);

    /// Return true if compaction should fire this turn.
    fn should_compress(&mut self, prompt_tokens: Option<i64>) -> bool;

    /// Compact the message list and return the new message list.
    async fn compress(
        &mut self,
        messages: Vec<Message>,
        current_tokens: Option<i64>,
        focus_topic: Option<&str>,
        force: bool,
        memory_context: &str,
    ) -> Vec<Message>;

    /// Quick rough check before the API call (no real token count yet).
    /// Default returns false (skip pre-flight).
    fn should_compress_preflight(&self, _messages: &[Message]) -> bool {
        false
    }

    /// Return true when preflight should trust recent real usage instead.
    fn should_defer_preflight_to_real_usage(&mut self, _rough_tokens: i64) -> bool {
        false
    }

    /// Quick check: is there anything in `messages` that can be compacted?
    /// Default returns true (always attempt).
    fn has_content_to_compress(&self, _messages: &[Message]) -> bool {
        true
    }

    // -- Optional: session lifecycle --------------------------------------

    /// Called when a new conversation session begins.
    fn on_session_start(&mut self, _session_id: &str) {}

    /// Called at real session boundaries (CLI exit, /reset, expiry).
    fn on_session_end(&mut self, _session_id: &str, _messages: &[Message]) {}

    /// Called on /new or /reset. Reset per-session state.
    fn on_session_reset(&mut self) {}

    /// Return status for display/logging (the context_engine.py `get_status`
    /// default shape, incl. the -1 sentinel clamp).
    fn get_status(&self) -> HashMap<String, serde_json::Value> {
        let last_prompt = if self.last_prompt_tokens() > 0 { self.last_prompt_tokens() } else { 0 };
        let mut out = HashMap::new();
        out.insert("last_prompt_tokens".to_string(), json!(last_prompt));
        out.insert("threshold_tokens".to_string(), json!(self.threshold_tokens()));
        out.insert("context_length".to_string(), json!(self.context_length()));
        out.insert(
            "usage_percent".to_string(),
            json!(if self.context_length() > 0 {
                (last_prompt as f64 / self.context_length() as f64 * 100.0).min(100.0)
            } else {
                0.0
            }),
        );
        out.insert("compression_count".to_string(), json!(self.compression_count()));
        out
    }

    /// Called when the user switches models or on fallback activation.
    fn update_model(
        &mut self,
        model: &str,
        context_length: i64,
        base_url: &str,
        api_key: &str,
        provider: &str,
        api_mode: &str,
        max_tokens: Option<i64>,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_context_sanitizer_truncates() {
        let short = sanitize_memory_context("  keep me  ");
        assert_eq!(short, "keep me");
        let long = "x".repeat(10_000);
        let out = sanitize_memory_context(&long);
        assert!(out.contains(MEMORY_CONTEXT_TRUNCATION_MARKER));
        assert_eq!(
            out.chars().count(),
            MEMORY_CONTEXT_HEAD_CHARS
                + MEMORY_CONTEXT_TRUNCATION_MARKER.chars().count()
                + MEMORY_CONTEXT_TAIL_CHARS
        );
    }
}

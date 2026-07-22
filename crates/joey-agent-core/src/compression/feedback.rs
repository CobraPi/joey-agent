//! User-facing summaries for manual compression commands
//! (verbatim port of `agent/manual_compression_feedback.py`, Hermes→Joey).

use super::compressor::{commafy, ContextCompressor};
use joey_providers::Message;

/// The `summarize_manual_compression` return shape.
#[derive(Debug, Clone)]
pub struct ManualCompressionSummary {
    pub noop: bool,
    pub aborted: bool,
    pub fallback_used: bool,
    pub headline: String,
    pub token_line: String,
    pub note: Option<String>,
}

/// Return consistent user-facing feedback for manual compression
/// (`summarize_manual_compression`).
pub fn summarize_manual_compression(
    before_messages: &[Message],
    after_messages: &[Message],
    before_tokens: i64,
    after_tokens: i64,
    compression_state: Option<&ContextCompressor>,
) -> ManualCompressionSummary {
    let before_count = before_messages.len();
    let after_count = after_messages.len();
    // Upstream compares list equality; the port compares serialized JSON.
    let noop = serde_json::to_string(after_messages).ok()
        == serde_json::to_string(before_messages).ok();
    let aborted = compression_state.map(|c| c.last_compress_aborted).unwrap_or(false);
    let fallback_used = compression_state.map(|c| c.last_summary_fallback_used).unwrap_or(false);
    let failure_reason: Option<String> = compression_state
        .and_then(|c| c.last_summary_error.clone())
        .filter(|r| !r.trim().is_empty());

    let headline = if aborted {
        format!("Compression aborted: {} messages preserved", before_count)
    } else if fallback_used {
        format!("Compressed with fallback: {} → {} messages", before_count, after_count)
    } else if noop {
        format!("No changes from compression: {} messages", before_count)
    } else {
        format!("Compressed: {} → {} messages", before_count, after_count)
    };

    let token_line = if noop && after_tokens == before_tokens {
        format!("Approx request size: ~{} tokens (unchanged)", commafy(before_tokens))
    } else {
        format!(
            "Approx request size: ~{} → ~{} tokens",
            commafy(before_tokens),
            commafy(after_tokens)
        )
    };

    let mut note: Option<String> = None;
    if aborted {
        note = Some("Summary generation failed; no messages were removed.".to_string());
    } else if fallback_used {
        let dropped_count = compression_state
            .map(|c| c.last_summary_dropped_count)
            .unwrap_or_else(|| before_count.saturating_sub(after_count));
        note = Some(format!(
            "Summary generation failed; Joey used limited fallback context and removed {} message(s).",
            dropped_count
        ));
    } else if !noop && after_count < before_count && after_tokens > before_tokens {
        note = Some(
            "Note: fewer messages can still raise this estimate when compression rewrites the \
             transcript into denser summaries."
                .to_string(),
        );
    }

    if let Some(reason) = failure_reason {
        if aborted || fallback_used {
            // Never let a disabled global redaction preference expose
            // credentials embedded in provider exception text.
            let safe_reason = joey_core::redact::redact_sensitive_text_opts(
                reason.trim(),
                joey_core::redact::RedactOptions { force: true, ..Default::default() },
            );
            note = Some(format!("{} Reason: {}", note.unwrap_or_default(), safe_reason));
        }
    }

    ManualCompressionSummary { noop, aborted, fallback_used, headline, token_line, note }
}

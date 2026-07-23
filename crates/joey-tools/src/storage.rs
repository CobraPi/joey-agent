//! Tool result persistence — port of `tools/tool_result_storage.py` +
//! `tools/budget_config.py`.
//!
//! Results over a tool's registered `max_result_size_chars` are written to the
//! sandbox temp dir (`<tmp>/joey-results/{id}.txt`) and replaced in-context by
//! a `<persisted-output>` preview + path envelope. A 200K per-turn aggregate
//! budget (layer 3) spills further results once exceeded; the accumulator
//! lives on [`crate::ToolContext`] (`turn_budget()`).

use sha2::{Digest, Sha256};
use std::path::PathBuf;

pub const PERSISTED_OUTPUT_TAG: &str = "<persisted-output>";
pub const PERSISTED_OUTPUT_CLOSING_TAG: &str = "</persisted-output>";

/// Defaults from `tools/budget_config.py`.
pub const DEFAULT_RESULT_SIZE_CHARS: usize = 100_000;
pub const DEFAULT_TURN_BUDGET_CHARS: usize = 200_000;
pub const DEFAULT_PREVIEW_SIZE_CHARS: usize = 1_500;

/// Pinned thresholds (`budget_config.PINNED_THRESHOLDS`): read_file is never
/// persisted — that would create infinite persist→read→persist loops.
pub fn resolve_threshold(tool_name: &str, registered: Option<usize>) -> Option<usize> {
    if tool_name == "read_file" {
        return None; // float("inf")
    }
    Some(registered.unwrap_or(DEFAULT_RESULT_SIZE_CHARS).min(DEFAULT_RESULT_SIZE_CHARS))
}

/// The storage dir: `<system temp>/joey-results` (upstream:
/// `env.get_temp_dir()/hermes-results`, default `/tmp/hermes-results`).
pub fn storage_dir() -> PathBuf {
    std::env::temp_dir().join("joey-results")
}

/// Port of `_safe_result_filename`.
pub fn safe_result_filename(tool_use_id: &str) -> String {
    const MAX_STEM: usize = 120;
    let raw_id = if tool_use_id.is_empty() { "tool_result" } else { tool_use_id };
    let safe_stem: String = raw_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' { c } else { '_' })
        .collect();
    // Collapse runs the regex `[^A-Za-z0-9_.-]+` would have merged.
    let mut collapsed = String::with_capacity(safe_stem.len());
    let mut prev_replaced = false;
    for (orig, mapped) in raw_id.chars().zip(safe_stem.chars()) {
        let was_unsafe = !(orig.is_ascii_alphanumeric() || orig == '_' || orig == '.' || orig == '-');
        if was_unsafe {
            if !prev_replaced {
                collapsed.push('_');
            }
            prev_replaced = true;
        } else {
            collapsed.push(mapped);
            prev_replaced = false;
        }
    }
    let trimmed = collapsed.trim_matches(|c| c == '.' || c == '_' || c == '-');
    let changed = trimmed != raw_id;
    let mut stem = if trimmed.is_empty() { "tool_result".to_string() } else { trimmed.to_string() };
    let mut needs_digest = changed || trimmed.is_empty();
    if stem.len() > MAX_STEM {
        needs_digest = true;
    }
    if needs_digest {
        let digest = hex::encode(Sha256::digest(raw_id.as_bytes()));
        let short = &digest[..12];
        let mut base: String = stem.chars().take(MAX_STEM).collect();
        base = base.trim_end_matches(['.', '_', '-']).to_string();
        if base.is_empty() {
            base = "tool_result".to_string();
        }
        stem = format!("{}_{}", base, short);
    }
    format!("{}.txt", stem)
}

/// Port of `generate_preview` — truncate at the last newline within
/// `max_chars`. Returns (preview, has_more).
pub fn generate_preview(content: &str, max_chars: usize) -> (String, bool) {
    if content.len() <= max_chars {
        return (content.to_string(), false);
    }
    let cut = crate::truncate::floor_char_boundary(content, max_chars);
    let mut truncated = &content[..cut];
    if let Some(last_nl) = truncated.rfind('\n') {
        if last_nl > max_chars / 2 {
            truncated = &content[..last_nl + 1];
        }
    }
    (truncated.to_string(), true)
}

/// Port of `_build_persisted_message`.
pub fn build_persisted_message(
    preview: &str,
    has_more: bool,
    original_size: usize,
    file_path: &str,
) -> String {
    let size_kb = original_size as f64 / 1024.0;
    let size_str = if size_kb >= 1024.0 {
        format!("{:.1} MB", size_kb / 1024.0)
    } else {
        format!("{:.1} KB", size_kb)
    };
    let mut msg = format!("{}\n", PERSISTED_OUTPUT_TAG);
    msg.push_str(&format!(
        "This tool result was too large ({} characters, {}).\n",
        crate::pyjson::commas(original_size as u64),
        size_str
    ));
    msg.push_str(&format!("Full output saved to: {}\n", file_path));
    msg.push_str(
        "Use the read_file tool with offset and limit to access specific sections of this output.\n\n",
    );
    msg.push_str(&format!("Preview (first {} chars):\n", preview.len()));
    msg.push_str(preview);
    if has_more {
        msg.push_str("\n...");
    }
    msg.push_str(&format!("\n{}", PERSISTED_OUTPUT_CLOSING_TAG));
    msg
}

/// Layer 2: persist an oversized result to disk, returning the preview + path
/// envelope, or the original content when it fits under `threshold`
/// (`None` = infinite, never persist).
pub fn maybe_persist_tool_result(
    content: &str,
    tool_name: &str,
    tool_use_id: &str,
    threshold: Option<usize>,
) -> String {
    let Some(threshold) = threshold else {
        return content.to_string();
    };
    if content.len() <= threshold {
        return content.to_string();
    }
    let dir = storage_dir();
    let remote_path = dir.join(safe_result_filename(tool_use_id));
    let (preview, has_more) = generate_preview(content, DEFAULT_PREVIEW_SIZE_CHARS);
    let write_ok = std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::write(&remote_path, content))
        .is_ok();
    if write_ok {
        tracing::info!(
            "Persisted large tool result: {} ({}, {} chars -> {})",
            tool_name,
            tool_use_id,
            content.len(),
            remote_path.display()
        );
        return build_persisted_message(
            &preview,
            has_more,
            content.len(),
            &remote_path.to_string_lossy(),
        );
    }
    tracing::info!(
        "Inline-truncating large tool result: {} ({} chars, no sandbox write)",
        tool_name,
        content.len()
    );
    format!(
        "{}\n\n[Truncated: tool response was {} chars. Full output could not be saved to sandbox.]",
        preview,
        crate::pyjson::commas(content.len() as u64)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_results_pass_through() {
        assert_eq!(maybe_persist_tool_result("hi", "terminal", "id1", Some(100)), "hi");
        // read_file is pinned to infinity.
        let big = "x".repeat(200);
        assert_eq!(resolve_threshold("read_file", Some(100)), None);
        assert_eq!(maybe_persist_tool_result(&big, "read_file", "id", None), big);
    }

    #[test]
    fn oversized_results_get_envelope() {
        let big = format!("line1\n{}", "y".repeat(5000));
        let out = maybe_persist_tool_result(&big, "terminal", "call_abc123", Some(1000));
        assert!(out.starts_with("<persisted-output>\n"));
        assert!(out.contains("This tool result was too large (5,006 characters, 4.9 KB)."));
        assert!(out.contains("Full output saved to: "));
        assert!(out.contains(
            "Use the read_file tool with offset and limit to access specific sections of this output."
        ));
        assert!(out.ends_with("\n</persisted-output>"));
        // The file actually exists with the full content.
        let path_line = out.lines().find(|l| l.starts_with("Full output saved to: ")).unwrap();
        let path = path_line.trim_start_matches("Full output saved to: ");
        assert_eq!(std::fs::read_to_string(path).unwrap(), big);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn filenames_are_sanitized() {
        assert_eq!(safe_result_filename("call_abc-123"), "call_abc-123.txt");
        let weird = safe_result_filename("a/b:c");
        assert!(weird.ends_with(".txt"));
        assert!(!weird.contains('/'));
        assert!(weird.starts_with("a_b_c_"));
    }

    #[test]
    fn threshold_resolution_caps_at_default() {
        assert_eq!(resolve_threshold("terminal", Some(500_000)), Some(DEFAULT_RESULT_SIZE_CHARS));
        assert_eq!(resolve_threshold("terminal", None), Some(DEFAULT_RESULT_SIZE_CHARS));
        assert_eq!(resolve_threshold("terminal", Some(50_000)), Some(50_000));
    }
}

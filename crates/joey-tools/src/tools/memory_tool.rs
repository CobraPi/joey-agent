//! Memory tool: persistent curated memory (MEMORY.md + USER.md) — port of
//! `tools/memory_tool.py`.
//!
//! Entries are joined by the `\n§\n` delimiter. Mutations re-read from disk
//! under a file lock, detect external drift (backing the file up to
//! `.bak.<ts>` and refusing), enforce the char budget, and return the
//! upstream JSON envelopes (`success`/`current_entries`/`usage` and the
//! terminal success `note`). Batches (`operations`) apply atomically against
//! the FINAL budget.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use crate::context::ToolContext;
use crate::pyjson::{commas, dumps};
use crate::registry::{Tool, ToolResult};

pub const ENTRY_DELIMITER: &str = "\n§\n";
const DEFAULT_MEMORY_CHAR_LIMIT: usize = 2200;
const DEFAULT_USER_CHAR_LIMIT: usize = 1375;
const MAX_CONSOLIDATION_FAILURES_PER_TURN: u32 = 3;

fn memory_dir() -> PathBuf {
    joey_core::constants::joey_home().join("memories")
}

fn path_for(target: &str) -> PathBuf {
    if target == "user" {
        memory_dir().join("USER.md")
    } else {
        memory_dir().join("MEMORY.md")
    }
}

fn char_limit(ctx: &ToolContext, target: &str) -> usize {
    if target == "user" {
        ctx.config().get_i64("memory.user_char_limit", DEFAULT_USER_CHAR_LIMIT as i64) as usize
    } else {
        ctx.config().get_i64("memory.memory_char_limit", DEFAULT_MEMORY_CHAR_LIMIT as i64) as usize
    }
}

fn read_entries(path: &PathBuf) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    if raw.trim().is_empty() {
        return Vec::new();
    }
    let mut seen = std::collections::HashSet::new();
    raw.split(ENTRY_DELIMITER)
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty())
        .filter(|e| seen.insert(e.clone()))
        .collect()
}

fn write_entries(path: &PathBuf, entries: &[String]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to write memory file {}: {}", path.display(), e))?;
    }
    let content = if entries.is_empty() { String::new() } else { entries.join(ENTRY_DELIMITER) };
    joey_core::utils::atomic_replace(path, content.as_bytes())
        .map_err(|e| format!("Failed to write memory file {}: {}", path.display(), e))
}

fn char_count(entries: &[String]) -> usize {
    if entries.is_empty() {
        0
    } else {
        entries.join(ENTRY_DELIMITER).chars().count()
    }
}

/// Truncated one-line previews of entries for error feedback.
fn previews(entries: &[&String]) -> Vec<String> {
    entries
        .iter()
        .map(|e| {
            let chars: Vec<char> = e.chars().collect();
            if chars.len() > 80 {
                format!("{}...", chars[..80].iter().collect::<String>())
            } else {
                e.to_string()
            }
        })
        .collect()
}

/// Best-effort exclusive file lock via a `.lock` sibling (fcntl flock on Unix).
struct FileLock {
    #[cfg(unix)]
    file: Option<std::fs::File>,
}

impl FileLock {
    fn acquire(path: &PathBuf) -> Self {
        let lock_path = {
            let mut os = path.clone().into_os_string();
            os.push(".lock");
            PathBuf::from(os)
        };
        if let Some(parent) = lock_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&lock_path)
                .ok();
            if let Some(f) = &file {
                unsafe {
                    let _ = libc::flock(f.as_raw_fd(), libc::LOCK_EX);
                }
            }
            FileLock { file }
        }
        #[cfg(not(unix))]
        {
            FileLock {}
        }
    }
}

#[cfg(unix)]
impl Drop for FileLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        if let Some(f) = &self.file {
            unsafe {
                let _ = libc::flock(f.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

/// Detect external drift: round-trip mismatch OR any parsed entry larger than
/// the whole-store char limit. On drift, snapshot to `.bak.<ts>` and return
/// the backup path.
fn detect_external_drift(path: &PathBuf, limit: usize) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    let parsed: Vec<String> = raw
        .split(ENTRY_DELIMITER)
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty())
        .collect();
    let roundtrip = parsed.join(ENTRY_DELIMITER);
    let max_entry_len = parsed.iter().map(|e| e.chars().count()).max().unwrap_or(0);
    let drift = raw.trim() != roundtrip || max_entry_len > limit;
    if !drift {
        return None;
    }
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let bak_path = {
        let mut os = path.clone().into_os_string();
        os.push(&format!(".bak.{}", ts));
        PathBuf::from(os)
    };
    match std::fs::write(&bak_path, &raw) {
        Ok(()) => Some(bak_path.to_string_lossy().into_owned()),
        Err(_) => Some(format!(
            "{} (BACKUP FAILED — file unchanged on disk)",
            bak_path.to_string_lossy()
        )),
    }
}

fn drift_error(path: &PathBuf, bak_path: &str) -> Value {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    json!({
        "success": false,
        "error": format!(
            "Refusing to write {}: file on disk has content that wouldn't round-trip through the memory tool (likely added by the patch tool, a shell append, a manual edit, or a concurrent session). A snapshot was saved to {}. Resolve the drift first — either rewrite the file as a clean §-delimited list of entries, or move the extra content out — then retry. This guard exists to prevent silent data loss (issue #26045).",
            name, bak_path
        ),
        "drift_backup": bak_path,
        "remediation": "Open the .bak file, integrate the missing entries into the memory tool one at a time via memory(action=add, content=...), then remove or rewrite the original file to a clean state.",
    })
}

fn usage_string(current: usize, limit: usize) -> String {
    format!("{}/{}", commas(current as u64), commas(limit as u64))
}

/// Success envelope — terminal, no entries echo (memory_tool.py:645-672).
fn success_response(ctx: &ToolContext, target: &str, entries: &[String], message: &str) -> Value {
    ctx.state().memory_consolidation_failures = 0;
    let current = char_count(entries);
    let limit = char_limit(ctx, target);
    let pct = if limit > 0 { ((current as f64 / limit as f64) * 100.0) as u64 } else { 0 };
    let pct = pct.min(100);
    let mut resp = Map::new();
    resp.insert("success".into(), json!(true));
    resp.insert("done".into(), json!(true));
    resp.insert("target".into(), json!(target));
    resp.insert(
        "usage".into(),
        json!(format!("{}% — {} chars", pct, usage_string(current, limit))),
    );
    resp.insert("entry_count".into(), json!(entries.len()));
    if !message.is_empty() {
        resp.insert("message".into(), json!(message));
    }
    resp.insert("note".into(), json!("Write saved. This update is complete — do not repeat it."));
    Value::Object(resp)
}

/// Count an at-capacity consolidation failure; past the per-turn cap return
/// the terminal "stop retrying" result instead (memory_tool.py #42405).
fn consolidation_failure(ctx: &ToolContext, response: Value) -> Value {
    let failures = {
        let mut st = ctx.state();
        st.memory_consolidation_failures += 1;
        st.memory_consolidation_failures
    };
    if failures <= MAX_CONSOLIDATION_FAILURES_PER_TURN {
        return response;
    }
    json!({
        "success": false,
        "done": true,
        "error": format!(
            "Memory consolidation failed {} times this turn. Stop retrying memory calls — leave memory unchanged for now and continue with your reply to the user. The fact can be saved in a later turn.",
            failures
        ),
    })
}

fn add(ctx: &ToolContext, target: &str, content: &str) -> Value {
    let content = content.trim();
    if content.is_empty() {
        return json!({"success": false, "error": "Content cannot be empty."});
    }
    let path = path_for(target);
    let _lock = FileLock::acquire(&path);
    // Re-read from disk under lock; add is append-only so drift is skipped.
    let mut entries = read_entries(&path);
    let limit = char_limit(ctx, target);

    if entries.iter().any(|e| e == content) {
        return success_response(ctx, target, &entries, "Entry already exists (no duplicate added).");
    }
    let mut new_entries = entries.clone();
    new_entries.push(content.to_string());
    let new_total = char_count(&new_entries);
    if new_total > limit {
        let current = char_count(&entries);
        return consolidation_failure(
            ctx,
            json!({
                "success": false,
                "error": format!(
                    "Memory at {}/{} chars. Adding this entry ({} chars) would exceed the limit. Consolidate now: use 'replace' to merge overlapping entries into shorter ones or 'remove' stale or less important entries (see current_entries below), then retry this add — all in this turn.",
                    commas(current as u64),
                    commas(limit as u64),
                    content.chars().count()
                ),
                "current_entries": entries,
                "usage": usage_string(current, limit),
            }),
        );
    }
    entries.push(content.to_string());
    if let Err(e) = write_entries(&path, &entries) {
        return json!({"success": false, "error": e});
    }
    success_response(ctx, target, &entries, "Entry added.")
}

fn matches_of<'a>(entries: &'a [String], old_text: &str) -> Vec<(usize, &'a String)> {
    entries.iter().enumerate().filter(|(_, e)| e.contains(old_text)).collect()
}

fn replace(ctx: &ToolContext, target: &str, old_text: &str, new_content: &str) -> Value {
    let old_text = old_text.trim();
    let new_content = new_content.trim();
    if old_text.is_empty() {
        return json!({"success": false, "error": "old_text cannot be empty."});
    }
    if new_content.is_empty() {
        return json!({"success": false, "error": "new_content cannot be empty. Use 'remove' to delete entries."});
    }
    let path = path_for(target);
    let limit = char_limit(ctx, target);
    let _lock = FileLock::acquire(&path);
    if let Some(bak) = detect_external_drift(&path, limit) {
        return drift_error(&path, &bak);
    }
    let mut entries = read_entries(&path);
    let matches = matches_of(&entries, old_text);
    if matches.is_empty() {
        return consolidation_failure(
            ctx,
            json!({
                "success": false,
                "error": format!("No entry matched '{}'. Check current_entries below and retry with the exact text of the entry you want to replace.", old_text),
                "current_entries": entries,
            }),
        );
    }
    if matches.len() > 1 {
        let unique: std::collections::HashSet<&String> = matches.iter().map(|(_, e)| *e).collect();
        if unique.len() > 1 {
            let entry_refs: Vec<&String> = matches.iter().map(|(_, e)| *e).collect();
            return json!({
                "success": false,
                "error": format!("Multiple entries matched '{}'. Be more specific.", old_text),
                "matches": previews(&entry_refs),
            });
        }
        // All identical — safe to replace just the first.
    }
    let idx = matches[0].0;
    let mut test_entries = entries.clone();
    test_entries[idx] = new_content.to_string();
    let new_total = char_count(&test_entries);
    if new_total > limit {
        let current = char_count(&entries);
        return consolidation_failure(
            ctx,
            json!({
                "success": false,
                "error": format!(
                    "Replacement would put memory at {}/{} chars. Shorten the new content, or 'remove' other stale or less important entries to make room (see current_entries below), then retry — all in this turn.",
                    commas(new_total as u64),
                    commas(limit as u64)
                ),
                "current_entries": entries,
                "usage": usage_string(current, limit),
            }),
        );
    }
    entries[idx] = new_content.to_string();
    if let Err(e) = write_entries(&path, &entries) {
        return json!({"success": false, "error": e});
    }
    success_response(ctx, target, &entries, "Entry replaced.")
}

fn remove(ctx: &ToolContext, target: &str, old_text: &str) -> Value {
    let old_text = old_text.trim();
    if old_text.is_empty() {
        return json!({"success": false, "error": "old_text cannot be empty."});
    }
    let path = path_for(target);
    let limit = char_limit(ctx, target);
    let _lock = FileLock::acquire(&path);
    if let Some(bak) = detect_external_drift(&path, limit) {
        return drift_error(&path, &bak);
    }
    let mut entries = read_entries(&path);
    let matches = matches_of(&entries, old_text);
    if matches.is_empty() {
        return consolidation_failure(
            ctx,
            json!({
                "success": false,
                "error": format!("No entry matched '{}'. Check current_entries below and retry with the exact text of the entry you want to remove.", old_text),
                "current_entries": entries,
            }),
        );
    }
    if matches.len() > 1 {
        let unique: std::collections::HashSet<&String> = matches.iter().map(|(_, e)| *e).collect();
        if unique.len() > 1 {
            let entry_refs: Vec<&String> = matches.iter().map(|(_, e)| *e).collect();
            return json!({
                "success": false,
                "error": format!("Multiple entries matched '{}'. Be more specific.", old_text),
                "matches": previews(&entry_refs),
            });
        }
    }
    let idx = matches[0].0;
    entries.remove(idx);
    if let Err(e) = write_entries(&path, &entries) {
        return json!({"success": false, "error": e});
    }
    success_response(ctx, target, &entries, "Entry removed.")
}

fn batch_error(ctx: &ToolContext, target: &str, entries: &[String], message: &str) -> Value {
    let current = char_count(entries);
    let limit = char_limit(ctx, target);
    consolidation_failure(
        ctx,
        json!({
            "success": false,
            "error": format!("{} No operations were applied (batch is all-or-nothing).", message),
            "current_entries": entries,
            "usage": usage_string(current, limit),
        }),
    )
}

fn apply_batch(ctx: &ToolContext, target: &str, operations: &[Value]) -> Value {
    if operations.is_empty() {
        return json!({"success": false, "error": "operations list is empty."});
    }
    let path = path_for(target);
    let limit = char_limit(ctx, target);
    let _lock = FileLock::acquire(&path);
    if let Some(bak) = detect_external_drift(&path, limit) {
        return drift_error(&path, &bak);
    }
    let entries = read_entries(&path);
    let mut working: Vec<String> = entries.clone();

    for (i, op) in operations.iter().enumerate() {
        let act = op.get("action").and_then(|a| a.as_str()).unwrap_or("");
        let content = op.get("content").and_then(|c| c.as_str()).unwrap_or("").trim().to_string();
        let old_text = op.get("old_text").and_then(|c| c.as_str()).unwrap_or("").trim().to_string();
        let pos = format!(
            "Operation {} ({})",
            i + 1,
            if act.is_empty() { "unknown" } else { act }
        );
        match act {
            "add" => {
                if content.is_empty() {
                    return batch_error(ctx, target, &entries, &format!("{}: content is required.", pos));
                }
                if working.iter().any(|e| *e == content) {
                    continue; // idempotent — skip duplicate, don't fail the batch
                }
                working.push(content);
            }
            "replace" => {
                if old_text.is_empty() {
                    return batch_error(ctx, target, &entries, &format!("{}: old_text is required.", pos));
                }
                if content.is_empty() {
                    return batch_error(
                        ctx,
                        target,
                        &entries,
                        &format!("{}: content is required (use action='remove' to delete).", pos),
                    );
                }
                let idxs: Vec<usize> = working
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.contains(&old_text))
                    .map(|(j, _)| j)
                    .collect();
                if idxs.is_empty() {
                    return batch_error(
                        ctx,
                        target,
                        &entries,
                        &format!("{}: no entry matched '{}'.", pos, old_text),
                    );
                }
                let unique: std::collections::HashSet<&String> =
                    idxs.iter().map(|j| &working[*j]).collect();
                if unique.len() > 1 {
                    return batch_error(
                        ctx,
                        target,
                        &entries,
                        &format!(
                            "{}: '{}' matched multiple distinct entries -- be more specific.",
                            pos, old_text
                        ),
                    );
                }
                working[idxs[0]] = content;
            }
            "remove" => {
                if old_text.is_empty() {
                    return batch_error(ctx, target, &entries, &format!("{}: old_text is required.", pos));
                }
                let idxs: Vec<usize> = working
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.contains(&old_text))
                    .map(|(j, _)| j)
                    .collect();
                if idxs.is_empty() {
                    return batch_error(
                        ctx,
                        target,
                        &entries,
                        &format!("{}: no entry matched '{}'.", pos, old_text),
                    );
                }
                let unique: std::collections::HashSet<&String> =
                    idxs.iter().map(|j| &working[*j]).collect();
                if unique.len() > 1 {
                    return batch_error(
                        ctx,
                        target,
                        &entries,
                        &format!(
                            "{}: '{}' matched multiple distinct entries -- be more specific.",
                            pos, old_text
                        ),
                    );
                }
                working.remove(idxs[0]);
            }
            _ => {
                return batch_error(
                    ctx,
                    target,
                    &entries,
                    &format!("{}: unknown action. Use add, replace, or remove.", pos),
                );
            }
        }
    }

    // Budget check against the FINAL state only.
    let new_total = char_count(&working);
    if new_total > limit {
        let current = char_count(&entries);
        return consolidation_failure(
            ctx,
            json!({
                "success": false,
                "error": format!(
                    "After applying all {} operations, memory would be at {}/{} chars -- over the limit. Remove or shorten more entries in the same batch (see current_entries below), then retry.",
                    operations.len(),
                    commas(new_total as u64),
                    commas(limit as u64)
                ),
                "current_entries": entries,
                "usage": usage_string(current, limit),
            }),
        );
    }

    if let Err(e) = write_entries(&path, &working) {
        return json!({"success": false, "error": e});
    }
    success_response(ctx, target, &working, &format!("Applied {} operation(s).", operations.len()))
}

/// Recoverable error for a replace/remove call without `old_text` — returns
/// the current entry inventory plus a retry instruction.
fn missing_old_text_error(ctx: &ToolContext, target: &str, action: &str) -> Value {
    let entries = read_entries(&path_for(target));
    let current = char_count(&entries);
    let limit = char_limit(ctx, target);
    json!({
        "success": false,
        "error": format!(
            "'{action}' needs old_text -- a short unique substring of the entry to {action}. None was provided. Reissue the {action} with old_text set to part of one of the current_entries below.",
            action = action
        ),
        "current_entries": entries,
        "usage": usage_string(current, limit),
    })
}

pub struct Memory;

#[async_trait]
impl Tool for Memory {
    fn name(&self) -> &str {
        "memory"
    }
    fn toolset(&self) -> &str {
        "memory"
    }
    fn description(&self) -> &str {
        "Save durable facts to persistent memory that survive across sessions. Memory is injected into every future turn, so keep entries compact and high-signal.\n\nHOW: make ALL your changes in ONE call via an 'operations' array (each item: {action, content?, old_text?}). The batch applies atomically and the char limit is checked only on the FINAL result — so a single call can remove/replace stale entries to free room AND add new ones, even when an add alone would overflow. The response reports current/limit chars and confirms completion; one batch call finishes the update, so don't repeat it. Use the bare action/content/old_text fields only for a single lone change.\n\nWHEN: save proactively when the user states a preference, correction, or personal detail, or you learn a stable fact about their environment, conventions, or workflow. Priority: user preferences & corrections > environment facts > procedures. The best memory stops the user repeating themselves.\n\nIF FULL: an add is rejected with the current entries shown. Reissue as ONE batch that removes or shortens enough stale entries and adds the new one together.\n\nTARGETS: 'user' = who the user is (name, role, preferences, style). 'memory' = your notes (environment, conventions, tool quirks, lessons).\n\nSKIP: trivial/obvious info, easily re-discovered facts, raw data dumps, task progress, completed-work logs, temporary TODO state (use session_search for those). Reusable procedures belong in a skill, not memory."
    }
    fn emoji(&self) -> &str {
        "🧠"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "replace", "remove"],
                    "description": "The action to perform (single-op shape). Omit when using 'operations'."
                },
                "target": {
                    "type": "string",
                    "enum": ["memory", "user"],
                    "description": "Which memory store: 'memory' for personal notes, 'user' for user profile."
                },
                "content": {
                    "type": "string",
                    "description": "The entry content. Required for 'add' and 'replace' (single-op shape)."
                },
                "old_text": {
                    "type": "string",
                    "description": "REQUIRED for 'replace' and 'remove' (single-op shape): a short unique substring identifying the existing entry to modify. Omit only for 'add'."
                },
                "operations": {
                    "type": "array",
                    "description": "Batch shape: a list of operations applied atomically in one call against the final char budget. Preferred when making multiple changes or consolidating to make room. Each item is {action, content?, old_text?}.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "action": {"type": "string", "enum": ["add", "replace", "remove"]},
                            "content": {"type": "string", "description": "Entry content for add/replace."},
                            "old_text": {"type": "string", "description": "Substring identifying the entry for replace/remove."},
                        },
                        "required": ["action"],
                    },
                },
            },
            "required": ["target"],
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        // Treat `target: null` as omitted (strict providers send JSON null).
        let target = match args.get("target") {
            None | Some(Value::Null) => "memory".to_string(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
        };
        if target != "memory" && target != "user" {
            return ToolResult::Text(dumps(&json!({
                "error": format!("Invalid target '{}'. Use 'memory' or 'user'.", target),
                "success": false,
            })));
        }

        // --- Batch path ---------------------------------------------------
        if let Some(operations) = args.get("operations") {
            if !operations.is_null() {
                let Some(ops) = operations.as_array() else {
                    return ToolResult::Text(dumps(&json!({
                        "error": "operations must be a list of {action, content?, old_text?} objects.",
                        "success": false,
                    })));
                };
                if !ops.is_empty() {
                    return ToolResult::Text(dumps(&apply_batch(ctx, &target, ops)));
                }
            }
        }

        // --- Single-op path -----------------------------------------------
        let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("");
        let content = args.get("content").and_then(|c| c.as_str()).unwrap_or("");
        let old_text = args.get("old_text").and_then(|c| c.as_str()).unwrap_or("");

        if action == "add" && content.is_empty() {
            return ToolResult::Text(dumps(&json!({
                "error": "Content is required for 'add' action.",
                "success": false,
            })));
        }
        if action == "replace" && (old_text.is_empty() || content.is_empty()) {
            if old_text.is_empty() {
                return ToolResult::Text(dumps(&missing_old_text_error(ctx, &target, "replace")));
            }
            return ToolResult::Text(dumps(&json!({
                "error": "content is required for 'replace' action.",
                "success": false,
            })));
        }
        if action == "remove" && old_text.is_empty() {
            return ToolResult::Text(dumps(&missing_old_text_error(ctx, &target, "remove")));
        }

        let result = match action {
            "add" => add(ctx, &target, content),
            "replace" => replace(ctx, &target, old_text, content),
            "remove" => remove(ctx, &target, old_text),
            other => {
                return ToolResult::Text(dumps(&json!({
                    "error": format!("Unknown action '{}'. Use: add, replace, remove", other),
                    "success": false,
                })))
            }
        };
        ToolResult::Text(dumps(&result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use joey_core::Config;

    struct HomeCtx {
        _guard: joey_core::constants::HomeOverrideGuard,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    /// The joey-home override is process-global; serialize tests that use it.
    fn ctx_with_home() -> (ToolContext, HomeCtx) {
        let lock = crate::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let guard = joey_core::constants::HomeOverrideGuard::new(dir.path().to_path_buf());
        std::mem::forget(dir);
        (
            ToolContext::new(std::env::temp_dir(), Config::defaults(), "m"),
            HomeCtx { _guard: guard, _lock: lock },
        )
    }

    fn parse(r: &ToolResult) -> Value {
        serde_json::from_str(&r.to_content_string()).unwrap()
    }

    #[tokio::test]
    async fn add_duplicate_and_usage() {
        let (c, _g) = ctx_with_home();
        let v = parse(
            &Memory
                .execute(json!({"target": "memory", "action": "add", "content": "fact one"}), &c)
                .await,
        );
        assert_eq!(v["success"], true);
        assert_eq!(v["done"], true);
        assert_eq!(v["message"], "Entry added.");
        assert_eq!(v["entry_count"], 1);
        assert_eq!(v["note"], "Write saved. This update is complete — do not repeat it.");
        assert!(v["usage"].as_str().unwrap().contains("% — "));

        let dup = parse(
            &Memory
                .execute(json!({"target": "memory", "action": "add", "content": "fact one"}), &c)
                .await,
        );
        assert_eq!(dup["message"], "Entry already exists (no duplicate added).");
        assert_eq!(dup["entry_count"], 1);
    }

    #[tokio::test]
    async fn unknown_and_missing_actions() {
        let (c, _g) = ctx_with_home();
        let v = parse(&Memory.execute(json!({"target": "memory"}), &c).await);
        assert_eq!(v["error"], "Unknown action ''. Use: add, replace, remove");
        assert_eq!(v["success"], false);

        let bad_target = parse(&Memory.execute(json!({"target": "wat", "action": "add", "content": "x"}), &c).await);
        assert_eq!(bad_target["error"], "Invalid target 'wat'. Use 'memory' or 'user'.");
    }

    #[tokio::test]
    async fn replace_remove_and_previews() {
        let (c, _g) = ctx_with_home();
        for content in ["alpha note", "beta note"] {
            Memory.execute(json!({"target": "memory", "action": "add", "content": content}), &c).await;
        }
        // Ambiguous substring across two distinct entries.
        let amb = parse(
            &Memory
                .execute(
                    json!({"target": "memory", "action": "remove", "old_text": "note"}),
                    &c,
                )
                .await,
        );
        assert_eq!(amb["error"], "Multiple entries matched 'note'. Be more specific.");
        assert_eq!(amb["matches"].as_array().unwrap().len(), 2);

        let rep = parse(
            &Memory
                .execute(
                    json!({"target": "memory", "action": "replace", "old_text": "alpha", "content": "gamma note"}),
                    &c,
                )
                .await,
        );
        assert_eq!(rep["message"], "Entry replaced.");

        let rem = parse(
            &Memory
                .execute(json!({"target": "memory", "action": "remove", "old_text": "gamma"}), &c)
                .await,
        );
        assert_eq!(rem["message"], "Entry removed.");
        assert_eq!(rem["entry_count"], 1);

        // Missing old_text → inventory response, not a dead-end.
        let miss = parse(
            &Memory.execute(json!({"target": "memory", "action": "remove"}), &c).await,
        );
        assert!(miss["error"].as_str().unwrap().starts_with("'remove' needs old_text"));
        assert!(miss["current_entries"].is_array());

        // No-match remove echoes inventory.
        let nomatch = parse(
            &Memory
                .execute(json!({"target": "memory", "action": "remove", "old_text": "zzz"}), &c)
                .await,
        );
        assert!(nomatch["error"].as_str().unwrap().starts_with("No entry matched 'zzz'."));
    }

    #[tokio::test]
    async fn overflow_and_batch() {
        let (c, _g) = ctx_with_home();
        let big = "x".repeat(3000);
        let over = parse(
            &Memory
                .execute(json!({"target": "memory", "action": "add", "content": big}), &c)
                .await,
        );
        assert_eq!(over["success"], false);
        assert!(over["error"].as_str().unwrap().contains("would exceed the limit"));
        assert!(over["usage"].as_str().unwrap().contains("/2,200"));

        // Batch: add two entries atomically; duplicate add is idempotent.
        let batch = parse(
            &Memory
                .execute(
                    json!({"target": "memory", "operations": [
                        {"action": "add", "content": "one"},
                        {"action": "add", "content": "two"},
                        {"action": "add", "content": "one"}
                    ]}),
                    &c,
                )
                .await,
        );
        assert_eq!(batch["success"], true);
        assert_eq!(batch["message"], "Applied 3 operation(s).");
        assert_eq!(batch["entry_count"], 2);

        // Batch abort is all-or-nothing.
        let abort = parse(
            &Memory
                .execute(
                    json!({"target": "memory", "operations": [
                        {"action": "remove", "old_text": "one"},
                        {"action": "remove", "old_text": "does-not-exist"}
                    ]}),
                    &c,
                )
                .await,
        );
        assert_eq!(abort["success"], false);
        assert!(abort["error"]
            .as_str()
            .unwrap()
            .ends_with("No operations were applied (batch is all-or-nothing)."));
        let after = parse(&Memory.execute(json!({"target": "memory", "action": "remove", "old_text": "nothing-here"}), &c).await);
        assert_eq!(after["current_entries"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn drift_detection_backs_up() {
        let (c, _g) = ctx_with_home();
        Memory.execute(json!({"target": "memory", "action": "add", "content": "clean entry"}), &c).await;
        // External writer appends free-form content large enough that the
        // resulting single entry exceeds the whole-store char limit — the
        // entry-size drift signal (memory_tool.py `_detect_external_drift`).
        let path = path_for("memory");
        let mut raw = std::fs::read_to_string(&path).unwrap();
        raw.push_str(&format!("\n\nrogue external append {}\n", "x".repeat(2500)));
        std::fs::write(&path, raw).unwrap();
        let v = parse(
            &Memory
                .execute(json!({"target": "memory", "action": "remove", "old_text": "clean"}), &c)
                .await,
        );
        assert_eq!(v["success"], false);
        assert!(v["error"].as_str().unwrap().starts_with("Refusing to write MEMORY.md:"));
        assert!(v["drift_backup"].as_str().unwrap().contains(".bak."));
    }
}

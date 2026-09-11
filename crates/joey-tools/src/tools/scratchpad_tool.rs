//! Feature 028 (context economy): session scratchpad tool.
//!
//! Append-only external record of findings under
//! `~/.joey/scratchpads/<sanitized-key>-<fnv1a-hex8>/scratchpad.md` so
//! details survive context cleanup. Secrets redacted before persist
//! (FR-002); entries bounded by `scratchpad.max_entry_chars` (FR-003);
//! post-session persistence + FTS discoverability via tool_calls arguments
//! in the messages index (research R1/R2).

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::context::ToolContext;
use crate::pyjson::dumps;
use crate::registry::{tool_error, Tool, ToolResult};

/// FNV-1a 64-bit of the raw session key (collision-proofing suffix),
/// rendered as 8 hex chars (low 32 bits — the dirname grammar pins an
/// 8-hex-char suffix).
fn fnv1a64(s: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", (hash & 0xffff_ffff) as u32)
}

/// Sanitize a session key to `[A-Za-z0-9._-]`, collapsing runs.
fn sanitize_key(key: &str) -> String {
    let mut out = String::new();
    let mut prev_replaced = false;
    for c in key.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            prev_replaced = false;
        } else if !prev_replaced {
            out.push('-');
            prev_replaced = true;
        }
    }
    out
}

/// Directory for a session's scratchpad under `joey_home()`.
fn scratchpad_dir(session_id: &str) -> PathBuf {
    joey_core::constants::joey_home()
        .join("scratchpads")
        .join(format!("{}-{}", sanitize_key(session_id), fnv1a64(session_id)))
}

/// A parsed scratchpad entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchpadEntry {
    pub timestamp: String,
    pub label: Option<String>,
    pub text: String,
}

/// Aggregated scratchpad stats (state-block input, research R2d).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchpadStats {
    pub entries: usize,
    pub total_chars: usize,
    pub last_entry_at: Option<String>,
}

/// Unit-struct tool; session identity from `ToolContext::session_id()`
/// (todo_tool pattern).
pub struct Scratchpad;

/// Lenient header detection: `## ` + something timestamp-shaped (starts
/// with a digit and has a `T` separator at offset 10 or a colon).
fn looks_like_header(rest: &str) -> bool {
    rest.starts_with(|c: char| c.is_ascii_digit())
        && (rest.as_bytes().get(10) == Some(&b'T') || rest.contains(':'))
}

/// Parse `## <ts> [<label>]` header remainder into (timestamp, label).
fn parse_header(rest: &str) -> (String, Option<String>) {
    match rest.split_once(' ') {
        Some((ts, remainder)) => {
            let label = if remainder.starts_with('[') && remainder.ends_with(']') {
                Some(remainder[1..remainder.len() - 1].to_string())
            } else {
                None
            };
            (ts.to_string(), label)
        }
        None => (rest.to_string(), None),
    }
}

/// Full parse: entries in file order (oldest first). Lines before the
/// first header are ignored (the file opens with a blank line).
fn parse_entries(raw: &str) -> Vec<ScratchpadEntry> {
    let mut entries: Vec<ScratchpadEntry> = Vec::new();
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            if looks_like_header(rest) {
                let (timestamp, label) = parse_header(rest);
                entries.push(ScratchpadEntry { timestamp, label, text: String::new() });
                continue;
            }
        }
        if let Some(last) = entries.last_mut() {
            if !last.text.is_empty() {
                last.text.push('\n');
            }
            last.text.push_str(line);
        }
    }
    for e in &mut entries {
        while e.text.ends_with('\n') {
            e.text.pop();
        }
    }
    entries
}

/// Read the current entries for a session (oldest first).
pub fn entries(session_id: &str) -> Vec<ScratchpadEntry> {
    let path = scratchpad_dir(session_id).join("scratchpad.md");
    match std::fs::read_to_string(&path) {
        Ok(raw) => parse_entries(&raw),
        Err(_) => Vec::new(),
    }
}

/// Aggregated stats for a session's scratchpad; `None` when no non-empty
/// scratchpad file exists.
pub fn stats(session_id: &str) -> Option<ScratchpadStats> {
    let path = scratchpad_dir(session_id).join("scratchpad.md");
    let raw = std::fs::read_to_string(&path).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    let parsed = parse_entries(&raw);
    Some(ScratchpadStats {
        entries: parsed.len(),
        total_chars: raw.chars().count(),
        last_entry_at: parsed.last().map(|e| e.timestamp.clone()),
    })
}

/// Display path of a session's scratchpad file (state-block pointer line,
/// feature 028 US2). Does not create the file.
pub fn path(session_id: &str) -> String {
    scratchpad_dir(session_id).join("scratchpad.md").display().to_string()
}

/// Best-effort exclusive file lock via a `.lock` sibling (fcntl flock on
/// Unix) — same pattern as memory_tool.
struct FileLock {
    #[cfg(unix)]
    file: Option<std::fs::File>,
}

impl FileLock {
    fn acquire(path: &std::path::Path) -> Self {
        let lock_path = {
            let mut os = path.to_path_buf().into_os_string();
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

/// Append an entry: redact first, validate, then lock + rewrite atomically.
/// Returns the new entry count.
fn append_entry(
    session_id: &str,
    text: &str,
    label: Option<&str>,
    max_entry_chars: usize,
) -> Result<usize, String> {
    // FR-002: secrets never hit disk.
    let redacted = joey_core::redact::redact_secrets(text);
    if redacted.trim().is_empty() {
        return Err(
            "Scratchpad text is empty after secret redaction; nothing to record.".to_string(),
        );
    }
    // FR-003: per-entry size bound (measured post-redaction).
    if redacted.chars().count() > max_entry_chars {
        return Err(format!(
            "Scratchpad entry too large: {} chars exceeds the configured limit of {} \
             (scratchpad.max_entry_chars). Summarize the finding or split it into multiple \
             smaller appends.",
            redacted.chars().count(),
            max_entry_chars
        ));
    }

    let dir = scratchpad_dir(session_id);
    let path = dir.join("scratchpad.md");
    let _lock = FileLock::acquire(&path);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create scratchpad dir: {e}"))?;

    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let count = parse_entries(&existing).len();
    let timestamp = joey_core::time::now_iso();
    let header = match label {
        Some(l) => format!("## {timestamp} [{l}]"),
        None => format!("## {timestamp}"),
    };
    let mut content = existing;
    content.push_str(&format!("\n{header}\n{redacted}\n"));
    joey_core::utils::atomic_replace(&path, content.as_bytes())
        .map_err(|e| format!("Failed to write scratchpad file: {e}"))?;
    tracing::info!(entries = count + 1, "scratchpad: appended entry to {}", session_id);
    Ok(count + 1)
}

/// Clear the session's scratchpad file (kept as an empty file on disk).
fn clear_entry_file(session_id: &str) -> Result<(), String> {
    let path = scratchpad_dir(session_id).join("scratchpad.md");
    let _lock = FileLock::acquire(&path);
    if path.exists() {
        joey_core::utils::atomic_replace(&path, b"")
            .map_err(|e| format!("Failed to clear scratchpad file: {e}"))?;
    }
    Ok(())
}

/// Render an entry back to its file block shape.
fn render_entry(e: &ScratchpadEntry) -> String {
    match &e.label {
        Some(l) => format!("## {} [{}]\n{}", e.timestamp, l, e.text),
        None => format!("## {}\n{}", e.timestamp, e.text),
    }
}

#[async_trait]
impl Tool for Scratchpad {
    fn name(&self) -> &str {
        "scratchpad"
    }
    fn toolset(&self) -> &str {
        "todo"
    }
    fn description(&self) -> &str {
        "Session scratchpad: an append-only external note file that survives context cleanup. \
Record findings as you discover them — file paths, identifiers, error messages, exact values, \
decisions — with an optional short label, then read recent entries back (tail_entries/offset), \
view stats, or clear the pad. Entries are redacted for secrets and size-bounded per entry; \
prefer this over re-reading large outputs: jot the key facts now and reclaim them later."
    }
    fn emoji(&self) -> &str {
        "🗒️"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["append", "read", "clear", "stats"],
                    "description": "Operation: append a new entry, read recent entries, clear the pad, or view stats"
                },
                "text": {
                    "type": "string",
                    "description": "Text to record (append only)"
                },
                "label": {
                    "type": "string",
                    "description": "Optional short label (append only)"
                },
                "tail_entries": {
                    "type": "integer",
                    "default": 20,
                    "description": "Number of most-recent entries to return (read only)"
                },
                "offset": {
                    "type": "integer",
                    "default": 0,
                    "description": "Skip the N most-recent entries before returning the tail (read only)"
                }
            },
            "required": ["action"]
        })
    }
    fn check(&self, ctx: &ToolContext) -> bool {
        ctx.config().scratchpad_enabled()
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let action = args.get("action").and_then(|v| v.as_str());
        match action {
            Some("append") => {
                let Some(text) = args.get("text").and_then(|v| v.as_str()) else {
                    return tool_error("Missing text: the append action requires a 'text' parameter");
                };
                let label = args.get("label").and_then(|v| v.as_str());
                match append_entry(
                    ctx.session_id(),
                    text,
                    label,
                    ctx.config().scratchpad_max_entry_chars(),
                ) {
                    Ok(n) => ToolResult::Text(dumps(&json!({ "ok": true, "entries": n }))),
                    Err(e) => tool_error(e),
                }
            }
            Some("read") => {
                let tail = args.get("tail_entries").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
                let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let all = entries(ctx.session_id());
                let total = all.len();
                let rem_end = total.saturating_sub(offset);
                let slice_start = rem_end.saturating_sub(tail);
                let slice = &all[slice_start..rem_end];
                let mut out = if slice.is_empty() {
                    "No scratchpad entries yet.".to_string()
                } else {
                    slice.iter().map(render_entry).collect::<Vec<_>>().join("\n")
                };
                if slice.is_empty() {
                    out.push_str(&format!("\nshowing 0-0 of {total}\n"));
                } else {
                    out.push_str(&format!("\nshowing {}-{} of {total}\n", slice_start + 1, rem_end));
                }
                ToolResult::Text(out)
            }
            Some("stats") => {
                let s = stats(ctx.session_id());
                ToolResult::Text(dumps(&json!({
                    "entries": s.as_ref().map_or(0, |s| s.entries),
                    "total_chars": s.as_ref().map_or(0, |s| s.total_chars),
                    "last_entry_at": s.and_then(|s| s.last_entry_at),
                })))
            }
            Some("clear") => match clear_entry_file(ctx.session_id()) {
                Ok(()) => ToolResult::Text(dumps(&json!({ "ok": true }))),
                Err(e) => tool_error(e),
            },
            None => tool_error(
                "Missing action: expected one of \"append\", \"read\", \"clear\", \"stats\"",
            ),
            Some(other) => tool_error(format!(
                "Invalid action {:?}: expected one of \"append\", \"read\", \"clear\", \"stats\"",
                other
            )),
        }
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

    /// The joey-home override is process-global; serialize tests that use it
    /// (sibling-tool pattern). The tempdir is intentionally leaked so the
    /// fake home outlives the guard.
    fn ctx_with_home(session: &str) -> (ToolContext, HomeCtx, PathBuf) {
        let lock = crate::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let guard = joey_core::constants::HomeOverrideGuard::new(home.clone());
        std::mem::forget(dir);
        let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), session);
        (ctx, HomeCtx { _guard: guard, _lock: lock }, home)
    }

    fn err_of(r: &ToolResult) -> String {
        match r {
            ToolResult::Error(e) => e.clone(),
            other => panic!("expected error, got {:?}", other.to_content_string()),
        }
    }

    #[tokio::test]
    async fn roundtrip_append_read() {
        let (c, _h, _home) = ctx_with_home("test-session-key with:chars");
        let r1 = Scratchpad
            .execute(json!({"action": "append", "text": "found the bug at main.rs:41", "label": "paths"}), &c)
            .await;
        let v1: Value = serde_json::from_str(&r1.to_content_string()).unwrap();
        assert_eq!(v1["ok"], true);
        assert_eq!(v1["entries"], 1);

        let r2 = Scratchpad
            .execute(json!({"action": "append", "text": "decision: retry on 429 twice"}), &c)
            .await;
        let v2: Value = serde_json::from_str(&r2.to_content_string()).unwrap();
        assert_eq!(v2["entries"], 2);

        let out = Scratchpad.execute(json!({"action": "read"}), &c).await.to_content_string();
        let p1 = out.find("found the bug at main.rs:41").expect("first text present");
        let p2 = out.find("decision: retry on 429 twice").expect("second text present");
        assert!(p1 < p2, "entries in file order");
        assert!(out.contains("[paths]"), "label rendered");
        assert!(out.contains("showing 1-2 of 2"), "out: {out}");
    }

    #[tokio::test]
    async fn redaction_on_write() {
        let (c, _h, home) = ctx_with_home("redact-session");
        let r = Scratchpad
            .execute(
                json!({"action": "append", "text": "api_key = \"sk-live-abcdefghijklmnopqrstuvwxyz0\""}),
                &c,
            )
            .await;
        assert!(!r.is_error());
        // Find the session's scratchpad file and read it raw.
        let pads = home.join("scratchpads");
        let session_dir = std::fs::read_dir(&pads)
            .unwrap()
            .find(|e| {
                e.as_ref().unwrap().file_name().to_string_lossy().starts_with("redact-session-")
            })
            .unwrap()
            .unwrap()
            .path();
        let raw = std::fs::read_to_string(session_dir.join("scratchpad.md")).unwrap();
        assert!(!raw.contains("sk-live"), "secret must not hit disk: {raw}");
        // Actual redact.rs behavior for this shape: the prefix pass masks the
        // token head-6/tail-4 (`sk-liv...xyz0`); the secret body never lands.
        assert!(raw.contains("sk-liv...xyz0"), "redaction marker present: {raw}");
    }

    #[tokio::test]
    async fn tail_pagination() {
        let (c, _h, _home) = ctx_with_home("paginate-session");
        for t in ["e01", "e02", "e03", "e04", "e05"] {
            Scratchpad.execute(json!({"action": "append", "text": t}), &c).await;
        }
        let out = Scratchpad
            .execute(json!({"action": "read", "tail_entries": 2, "offset": 0}), &c)
            .await
            .to_content_string();
        assert!(out.contains("e04"));
        assert!(out.contains("e05"));
        assert!(!out.contains("e01"));
        assert!(!out.contains("e02"));
        assert!(!out.contains("e03"));
        assert!(out.contains("showing 4-5 of 5"), "out: {out}");

        let out2 = Scratchpad
            .execute(json!({"action": "read", "tail_entries": 2, "offset": 2}), &c)
            .await
            .to_content_string();
        assert!(out2.contains("e02"));
        assert!(out2.contains("e03"));
        assert!(!out2.contains("e01"));
        assert!(!out2.contains("e04"));
        assert!(!out2.contains("e05"));
        assert!(out2.contains("showing 2-3 of 5"), "out: {out2}");
    }

    #[tokio::test]
    async fn oversized_rejection() {
        // Custom config with a 1000-char entry limit, loaded from a yaml file.
        let cfg_dir = tempfile::tempdir().unwrap();
        let cfg_path = cfg_dir.path().join("config.yaml");
        std::fs::write(&cfg_path, "scratchpad:\n  max_entry_chars: 1000\n").unwrap();
        let cfg = Config::load_from(cfg_path).unwrap();
        assert_eq!(cfg.scratchpad_max_entry_chars(), 1000);

        let lock = crate::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let guard = joey_core::constants::HomeOverrideGuard::new(home.clone());
        std::mem::forget(dir);
        let c = ToolContext::new(std::env::temp_dir(), cfg, "oversize-session");

        let r = Scratchpad
            .execute(json!({"action": "append", "text": "x".repeat(2000)}), &c)
            .await;
        let msg = err_of(&r);
        assert!(msg.contains("limit of 1000"), "msg: {msg}");
        drop(guard);
        drop(lock);
    }

    #[tokio::test]
    async fn empty_input_rejection() {
        let (c, _h, _home) = ctx_with_home("empty-session");
        // Missing key entirely.
        let r = Scratchpad.execute(json!({"action": "append"}), &c).await;
        assert!(err_of(&r).contains("Missing text"));
        // Present but empty / whitespace-only: rejected after redaction.
        let r = Scratchpad.execute(json!({"action": "append", "text": ""}), &c).await;
        assert!(err_of(&r).contains("empty after secret redaction"));
        let r = Scratchpad.execute(json!({"action": "append", "text": "   "}), &c).await;
        assert!(err_of(&r).contains("empty after secret redaction"));
    }

    #[test]
    fn session_key_dir_sanitization() {
        let dir = scratchpad_dir("telegram:chat 123/foo");
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
            "dirname charset: {name}"
        );
        assert!(!name.contains(':') && !name.contains('/') && !name.contains(' '));
        let (san, hash) = name.rsplit_once('-').unwrap();
        assert!(!san.is_empty());
        assert_eq!(hash.len(), 8, "hash suffix: {hash}");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));

        let h = fnv1a64("a");
        assert_eq!(h.len(), 8);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));

        assert_ne!(scratchpad_dir("k1"), scratchpad_dir("k2"));
    }

    #[tokio::test]
    async fn stats_missing_and_present() {
        let (c, _h, _home) = ctx_with_home("stats-session");
        assert!(stats("no-such-session-xyz").is_none());

        Scratchpad.execute(json!({"action": "append", "text": "only entry"}), &c).await;
        let s = stats(c.session_id()).unwrap();
        assert_eq!(s.entries, 1);
        assert!(s.last_entry_at.is_some());
        assert!(s.total_chars > 0);
    }

    #[tokio::test]
    async fn clear_roundtrip() {
        let (c, _h, _home) = ctx_with_home("clear-session");
        Scratchpad.execute(json!({"action": "append", "text": "a1"}), &c).await;
        Scratchpad.execute(json!({"action": "append", "text": "a2"}), &c).await;
        assert_eq!(stats(c.session_id()).unwrap().entries, 2);

        let r = Scratchpad.execute(json!({"action": "clear"}), &c).await;
        let v: Value = serde_json::from_str(&r.to_content_string()).unwrap();
        assert_eq!(v["ok"], true);

        let out = Scratchpad.execute(json!({"action": "read"}), &c).await.to_content_string();
        assert!(out.contains("showing 0-0 of 0"), "out: {out}");
        assert!(stats(c.session_id()).is_none());
    }

    #[tokio::test]
    async fn registered_under_default_config() {
        let _lock = crate::test_env_lock();
        crate::registry::invalidate_check_cache();
        let cfg = Config::defaults();
        let enabled = crate::resolve_toolsets(&cfg.get_str_list("toolsets"));
        assert!(
            enabled.contains(&"scratchpad".to_string()),
            "resolve_toolsets default list contains scratchpad"
        );
        let ctx = ToolContext::new(std::env::temp_dir(), cfg, "reg-default");
        let reg = crate::registry::ToolRegistry::with_builtins();
        let defs = reg.definitions(&enabled, &ctx);
        assert!(
            defs.iter().any(|d| d["function"]["name"] == "scratchpad"),
            "definitions contain a scratchpad entry under default config"
        );
    }

    #[tokio::test]
    async fn absent_from_registry_output_when_disabled() {
        let _lock = crate::test_env_lock();
        crate::registry::invalidate_check_cache();
        let cfg_dir = tempfile::tempdir().unwrap();
        let cfg_path = cfg_dir.path().join("config.yaml");
        std::fs::write(&cfg_path, "scratchpad:\n  enabled: false\n").unwrap();
        let cfg = Config::load_from(cfg_path).unwrap();
        assert!(!cfg.scratchpad_enabled());
        let enabled = crate::resolve_toolsets(&cfg.get_str_list("toolsets"));
        let ctx = ToolContext::new(std::env::temp_dir(), cfg, "reg-disabled");
        let reg = crate::registry::ToolRegistry::with_builtins();
        let defs = reg.definitions(&enabled, &ctx);
        assert!(
            !defs.iter().any(|d| d["function"]["name"] == "scratchpad"),
            "scratchpad hidden from definitions when disabled"
        );
        assert!(
            defs.iter().any(|d| d["function"]["name"] == "todo"),
            "todo still present when scratchpad disabled"
        );
    }

    #[tokio::test]
    async fn append_read_roundtrip_through_registry() {
        let (ctx, _h, _home) = ctx_with_home("registry-roundtrip");
        let reg = crate::registry::ToolRegistry::with_builtins();
        let r = reg
            .dispatch(
                "scratchpad",
                json!({"action": "append", "text": "path=/tmp/x id=42", "label": "find"}),
                &ctx,
            )
            .await;
        let v: Value = serde_json::from_str(&r.to_content_string()).unwrap();
        assert_eq!(v["ok"], true, "append envelope: {}", r.to_content_string());

        let out = reg
            .dispatch("scratchpad", json!({"action": "read"}), &ctx)
            .await
            .to_content_string();
        assert!(out.contains("path=/tmp/x id=42"), "read back through registry: {out}");
    }
}

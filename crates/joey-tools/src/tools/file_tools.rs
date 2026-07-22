//! File tools: read_file, write_file, patch, search_files — port of
//! `tools/file_tools.py` + `tools/file_operations.py` (schemas, result
//! envelopes, guards, dedup/loop trackers, fuzzy patch, V4A patches, and the
//! ripgrep-backed search).

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::context::ToolContext;
use crate::difflib::unified_diff;
use crate::fuzzy;
use crate::guards;
use crate::patch_parser::{self, V4aFileOps};
use crate::pyjson::{commas, dumps};
use crate::registry::{tool_error, Tool, ToolResult};
use crate::truncate;

const DEFAULT_MAX_READ_CHARS: usize = 100_000;
const LARGE_FILE_HINT_BYTES: u64 = 512_000;

fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn arg_i64(args: &Value, key: &str) -> Option<i64> {
    match args.get(key) {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

fn arg_bool(args: &Value, key: &str) -> bool {
    match args.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(s.to_lowercase().as_str(), "true" | "1" | "yes"),
        _ => false,
    }
}

fn max_read_chars(ctx: &ToolContext) -> usize {
    let v = ctx.config().get_i64("file_read_max_chars", DEFAULT_MAX_READ_CHARS as i64);
    if v > 0 {
        v as usize
    } else {
        DEFAULT_MAX_READ_CHARS
    }
}

fn mtime_of(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

// ---------------------------------------------------------------------------
// Line-ending / BOM helpers (file_operations.py:77-144)
// ---------------------------------------------------------------------------

const UTF8_BOM: &str = "\u{feff}";

fn detect_line_ending(sample: &str) -> Option<&'static str> {
    if sample.is_empty() {
        return None;
    }
    let head = &sample[..truncate::floor_char_boundary(sample, 4096)];
    if head.contains("\r\n") {
        Some("\r\n")
    } else if head.contains('\n') {
        Some("\n")
    } else {
        None
    }
}

fn normalize_line_endings(text: &str, target: &str) -> String {
    let lf = text.replace("\r\n", "\n").replace('\r', "\n");
    match target {
        "\n" => lf,
        "\r\n" => lf.replace('\n', "\r\n"),
        _ => text.to_string(),
    }
}

fn strip_bom(text: &str) -> (&str, bool) {
    match text.strip_prefix(UTF8_BOM) {
        Some(rest) => (rest, true),
        None => (text, false),
    }
}

// ---------------------------------------------------------------------------
// Line numbering + char budget (file_operations._add_line_numbers,
// file_tools._truncate_to_char_budget)
// ---------------------------------------------------------------------------

fn add_line_numbers(content: &str, start_line: usize, max_line_length: usize) -> String {
    let mut numbered = Vec::new();
    for (i, line) in content.split('\n').enumerate() {
        let line = if line.len() > max_line_length {
            let cut = truncate::floor_char_boundary(line, max_line_length);
            format!("{}... [truncated]", &line[..cut])
        } else {
            line.to_string()
        };
        numbered.push(format!("{}|{}", start_line + i, line));
    }
    numbered.join("\n")
}

/// Returns (kept_text, lines_kept, truncated).
fn truncate_to_char_budget(content: &str, max_chars: usize) -> (String, usize, bool) {
    if content.len() <= max_chars {
        let lines = if content.is_empty() { 0 } else { content.matches('\n').count() + 1 };
        return (content.to_string(), lines, false);
    }
    let lines: Vec<&str> = content.split('\n').collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut running = 0usize;
    for line in &lines {
        let addition = line.len() + if kept.is_empty() { 0 } else { 1 };
        if running + addition > max_chars {
            break;
        }
        kept.push(line);
        running += addition;
    }
    if kept.is_empty() {
        let cut = truncate::floor_char_boundary(lines[0], max_chars);
        return (lines[0][..cut].to_string(), 1, true);
    }
    let n = kept.len();
    (kept.join("\n"), n, true)
}

// ---------------------------------------------------------------------------
// read_file
// ---------------------------------------------------------------------------

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }
    fn toolset(&self) -> &str {
        "file"
    }
    fn description(&self) -> &str {
        "Read a text file with line numbers and pagination. Use this instead of cat/head/tail in terminal. Output format: 'LINE_NUM|CONTENT'. Suggests similar filenames if not found. Use offset and limit for large files. Reads exceeding ~100K characters are truncated on a line boundary and return a next_offset; continue with offset to read the rest. Jupyter notebooks (.ipynb), Word documents (.docx), and Excel workbooks (.xlsx) are auto-extracted to readable text. NOTE: Cannot read images or other binary files — use vision_analyze for images."
    }
    fn emoji(&self) -> &str {
        "📖"
    }
    fn max_result_chars(&self) -> Option<usize> {
        Some(100_000)
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to the file to read (absolute, relative, or ~/path)"},
                "offset": {"type": "integer", "description": "Line number to start reading from (1-indexed, default: 1)", "default": 1, "minimum": 1},
                "limit": {"type": "integer", "description": "Maximum number of lines to read (default: 500, max: 2000)", "default": 500, "maximum": 2000}
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let path = arg_str(&args, "path").unwrap_or_default();
        let limits = truncate::get_tool_output_limits(ctx.config());
        let (offset, limit) = truncate::normalize_read_pagination(
            arg_i64(&args, "offset"),
            arg_i64(&args, "limit"),
            limits.max_lines,
        );

        // ── Device path guard ─────────────────────────────────────────
        let device_base = if Path::new(&shellexpand::tilde(&path).to_string()).is_absolute() {
            None
        } else {
            Some(ctx.effective_cwd())
        };
        if guards::is_blocked_device(&path, device_base.as_deref()) {
            return ToolResult::Text(dumps(&json!({
                "error": format!(
                    "Cannot read '{}': this is a device file that would block or produce infinite output.",
                    path
                ),
            })));
        }

        let resolved = ctx.resolve_path(&path);
        let resolved_str = resolved.to_string_lossy().to_string();

        // ── Binary file guard (extension, no I/O) ─────────────────────
        if guards::has_binary_extension(&resolved_str) {
            let ext = resolved
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
                .unwrap_or_default();
            return ToolResult::Text(dumps(&json!({
                "error": format!(
                    "Cannot read binary file '{}' ({}). Use vision_analyze for images, or terminal to inspect binary files.",
                    path, ext
                ),
            })));
        }

        // ── Joey internal path / credential guard ─────────────────────
        if let Some(block_error) = guards::get_read_block_error(&resolved_str) {
            return ToolResult::Text(dumps(&json!({ "error": block_error })));
        }

        // ── Dedup check ───────────────────────────────────────────────
        let dedup_key = (resolved_str.clone(), offset, limit);
        let cached_mtime = ctx.state().dedup.get(&dedup_key).copied();
        if let (Some(cached), Some(current)) = (cached_mtime, mtime_of(&resolved)) {
            if cached == current {
                let hits = {
                    let mut st = ctx.state();
                    let hits = st.dedup_hits.get(&dedup_key).copied().unwrap_or(0) + 1;
                    st.dedup_hits.insert(dedup_key.clone(), hits);
                    st.cap();
                    hits
                };
                if hits >= 2 {
                    return ToolResult::Text(dumps(&json!({
                        "error": format!(
                            "BLOCKED: You have called read_file on this exact region {} times and the file has NOT changed. STOP calling read_file for this path — the content from your earlier read_file result in this conversation is still current. Proceed with your task using the information you already have.",
                            hits + 1
                        ),
                        "path": path,
                        "already_read": hits + 1,
                    })));
                }
                return ToolResult::Text(dumps(&json!({
                    "status": "unchanged",
                    "message": guards::READ_DEDUP_STATUS_MESSAGE,
                    "path": path,
                    "dedup": true,
                    "content_returned": false,
                })));
            }
        }

        // ── Perform the read ──────────────────────────────────────────
        let text = match std::fs::read_to_string(&resolved) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                let file_size =
                    std::fs::metadata(&resolved).map(|m| m.len()).unwrap_or(0);
                let mut d = Map::new();
                d.insert("content".into(), json!(""));
                d.insert("total_lines".into(), json!(0));
                d.insert("file_size".into(), json!(file_size));
                d.insert("truncated".into(), json!(false));
                d.insert("is_binary".into(), json!(true));
                d.insert("is_image".into(), json!(false));
                d.insert(
                    "error".into(),
                    json!("Binary file - cannot display as text. Use appropriate tools to handle this file type."),
                );
                return ToolResult::Text(dumps(&Value::Object(d)));
            }
            Err(_) => {
                // File not found — suggest similar files.
                return ToolResult::Text(dumps(&suggest_similar_files(&path, &resolved)));
            }
        };
        let (text, _had_bom) = strip_bom(&text);

        let file_size = std::fs::metadata(&resolved).map(|m| m.len()).unwrap_or(0);
        // wc -l semantics: count newline characters.
        let total_lines = text.matches('\n').count();
        let mut display_lines: Vec<&str> = text.split('\n').collect();
        if display_lines.last() == Some(&"") {
            display_lines.pop();
        }
        let start = offset - 1;
        let end_line = offset + limit - 1;
        let page: Vec<&str> = if start >= display_lines.len() {
            Vec::new()
        } else {
            display_lines[start..display_lines.len().min(start + limit)].to_vec()
        };
        let page_text = page.join("\n");
        let mut content = if page.is_empty() {
            String::new()
        } else {
            add_line_numbers(&page_text, offset, limits.max_line_length)
        };
        let truncated = total_lines > end_line;

        let mut result = Map::new();
        result.insert("content".into(), json!(content));
        result.insert("total_lines".into(), json!(total_lines));
        result.insert("file_size".into(), json!(file_size));
        result.insert("truncated".into(), json!(truncated));
        if truncated {
            result.insert(
                "hint".into(),
                json!(format!(
                    "Use offset={} to continue reading (showing {}-{} of {} lines)",
                    end_line + 1,
                    offset,
                    end_line,
                    total_lines
                )),
            );
        }
        result.insert("is_binary".into(), json!(false));
        result.insert("is_image".into(), json!(false));

        // ── Character-count guard (graceful char-budget truncation) ───
        let max_chars = max_read_chars(ctx);
        if content.len() > max_chars {
            let (trimmed, lines_kept, _) = truncate_to_char_budget(&content, max_chars);
            let next_offset = offset + lines_kept;
            let shown_end = offset + lines_kept - 1;
            let mut hint = format!(
                "Output truncated at the {}-char read budget after {} line(s) (showing lines {}-{} of {}). Use offset={} to continue.",
                commas(max_chars as u64),
                lines_kept,
                offset,
                shown_end,
                total_lines,
                next_offset
            );
            if trimmed.split('\n').next().map(|l| l.len()).unwrap_or(0) >= max_chars {
                hint.push_str(
                    " Note: the first line alone exceeded the budget and was clamped mid-line; its remainder is not retrievable via offset.",
                );
            }
            content = trimmed;
            result.insert("content".into(), json!(content));
            result.insert("truncated".into(), json!(true));
            result.insert("truncated_by".into(), json!("bytes"));
            result.insert("next_offset".into(), json!(next_offset));
            result.insert("hint".into(), json!(hint));
        }

        // ── Redact secrets ────────────────────────────────────────────
        if !content.is_empty() {
            content = joey_core::redact::redact_secrets(&content);
            result.insert("content".into(), json!(content));
        }

        // Large-file hint.
        if file_size > LARGE_FILE_HINT_BYTES
            && limit > 200
            && result.get("truncated").and_then(|t| t.as_bool()).unwrap_or(false)
            && !result.contains_key("_hint")
        {
            result.insert(
                "_hint".into(),
                json!(format!(
                    "This file is large ({} bytes). Consider reading only the section you need with offset and limit to keep context usage efficient.",
                    commas(file_size)
                )),
            );
        }

        // ── Track for consecutive-loop detection ──────────────────────
        let read_key = format!("read\u{0}{}\u{0}{}\u{0}{}", path, offset, limit);
        let count = {
            let mut st = ctx.state();
            st.dedup_hits.shift_remove(&dedup_key);
            st.read_history.insert((path.clone(), offset, limit));
            if st.last_key.as_deref() == Some(&read_key) {
                st.consecutive += 1;
            } else {
                st.last_key = Some(read_key);
                st.consecutive = 1;
            }
            if let Some(mtime) = mtime_of(&resolved) {
                st.dedup.insert(dedup_key.clone(), mtime);
                st.read_timestamps.insert(resolved_str.clone(), mtime);
            }
            st.cap();
            st.consecutive
        };

        if count >= 4 {
            return ToolResult::Text(dumps(&json!({
                "error": format!(
                    "BLOCKED: You have read this exact file region {} times in a row. The content has NOT changed. You already have this information. STOP re-reading and proceed with your task.",
                    count
                ),
                "path": path,
                "already_read": count,
            })));
        } else if count >= 3 {
            result.insert(
                "_warning".into(),
                json!(format!(
                    "You have read this exact file region {} times consecutively. The content has not changed since your last read. Use the information you already have. If you are stuck in a loop, stop reading and proceed with writing or responding.",
                    count
                )),
            );
        }

        ToolResult::Text(dumps(&Value::Object(result)))
    }
}

/// Port of `_suggest_similar_files` — the ENOENT scored-suggestion path.
fn suggest_similar_files(path: &str, resolved: &Path) -> Value {
    let dir_path = resolved.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let orig_dir = Path::new(path).parent().map(|p| p.to_string_lossy().to_string());
    let display_dir = match orig_dir.as_deref() {
        Some("") | None => ".".to_string(),
        Some(d) => d.to_string(),
    };
    let filename = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let basename_no_ext = Path::new(&filename)
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let ext = Path::new(&filename)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()));
    let lower_name = filename.to_lowercase();

    let mut scored: Vec<(i32, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir_path) {
        for f in entries.flatten().take(50) {
            let name = f.file_name().to_string_lossy().to_string();
            if name.is_empty() {
                continue;
            }
            let lf = name.to_lowercase();
            let cand_stem = Path::new(&name)
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let cand_ext = Path::new(&name)
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()));
            let score = if lf == lower_name {
                100
            } else if !basename_no_ext.is_empty() && cand_stem == basename_no_ext {
                90
            } else if lf.starts_with(&lower_name) || lower_name.starts_with(&lf) {
                70
            } else if lower_name.contains(&lf) && lf.len() > 2 {
                40
            } else if !lower_name.is_empty() && lf.contains(&lower_name) {
                60
            } else if ext.is_some() && cand_ext == ext {
                let common: std::collections::HashSet<char> = lower_name
                    .chars()
                    .collect::<std::collections::HashSet<_>>()
                    .intersection(&lf.chars().collect())
                    .copied()
                    .collect();
                if common.len() as f64 >= lower_name.len().max(lf.len()) as f64 * 0.4 {
                    30
                } else {
                    0
                }
            } else {
                0
            };
            if score > 0 {
                scored.push((score, format!("{}/{}", display_dir, name)));
            }
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    let similar: Vec<String> = scored.into_iter().take(5).map(|(_, p)| p).collect();

    let mut d = Map::new();
    d.insert("content".into(), json!(""));
    d.insert("total_lines".into(), json!(0));
    d.insert("file_size".into(), json!(0));
    d.insert("truncated".into(), json!(false));
    d.insert("is_binary".into(), json!(false));
    d.insert("is_image".into(), json!(false));
    d.insert("error".into(), json!(format!("File not found: {}", path)));
    if !similar.is_empty() {
        d.insert("similar_files".into(), json!(similar));
    }
    Value::Object(d)
}

// ---------------------------------------------------------------------------
// write_file
// ---------------------------------------------------------------------------

fn python_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "int"
            } else {
                "float"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Fail-closed pre-write syntax gate (file_operations.py:1393-1429).
fn syntax_gate(path: &str, content: &str) -> Option<String> {
    let ext = Path::new(path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))?;
    let lint_err: Option<String> = match ext.as_str() {
        ".json" => serde_json::from_str::<serde_json::Value>(content)
            .err()
            .map(|e| format!("JSONDecodeError: {}", e)),
        ".yaml" | ".yml" => serde_yaml::from_str::<serde_yaml::Value>(content)
            .err()
            .map(|e| format!("YAMLError: {}", e)),
        ".toml" => content
            .parse::<toml::Value>()
            .err()
            .map(|e| format!("TOMLDecodeError: {}", e)),
        _ => None,
    };
    lint_err.map(|err| {
        format!(
            "Refusing to write '{}': candidate content fails {} syntax validation ({}). The file was NOT created or modified. Fix the content and retry.",
            path, ext, err
        )
    })
}

/// Core write with CRLF/BOM preservation. Returns (bytes_written, dirs_created).
fn write_with_preservation(resolved: &Path, content: &str) -> Result<(u64, bool), String> {
    let pre_content = std::fs::read_to_string(resolved).ok();
    let mut content = content.to_string();
    // Line-ending preservation.
    if let Some(pre) = &pre_content {
        if detect_line_ending(pre) == Some("\r\n") {
            content = normalize_line_endings(&content, "\r\n");
        }
        // BOM preservation.
        if pre.starts_with(UTF8_BOM) && !content.starts_with(UTF8_BOM) {
            content = format!("{}{}", UTF8_BOM, content);
        }
    }
    let mut dirs_created = false;
    if let Some(parent) = resolved.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("Failed to write file: {}", e))?;
        dirs_created = true;
    }
    joey_core::utils::atomic_replace(resolved, content.as_bytes())
        .map_err(|e| format!("Failed to write file: {}", e))?;
    let bytes_written = std::fs::metadata(resolved).map(|m| m.len()).unwrap_or(content.len() as u64);
    Ok((bytes_written, dirs_created))
}

/// Port of `_check_file_staleness`.
fn check_file_staleness(ctx: &ToolContext, filepath: &str, resolved: &Path) -> Option<String> {
    let resolved_str = resolved.to_string_lossy().to_string();
    let read_mtime = ctx.state().read_timestamps.get(&resolved_str).copied()?;
    let current_mtime = mtime_of(resolved)?;
    if current_mtime != read_mtime {
        Some(format!(
            "Warning: {} was modified since you last read it (external edit or concurrent agent). The content you read may be stale. Consider re-reading the file to verify before writing.",
            filepath
        ))
    } else {
        None
    }
}

/// Port of `_update_read_timestamp` (+ dedup invalidation).
fn update_read_timestamp(ctx: &ToolContext, resolved: &Path) {
    let resolved_str = resolved.to_string_lossy().to_string();
    let mut st = ctx.state();
    st.invalidate_dedup_for_path(&resolved_str);
    if let Some(mtime) = mtime_of(resolved) {
        st.read_timestamps.insert(resolved_str, mtime);
        st.cap();
    }
}

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }
    fn toolset(&self) -> &str {
        "file"
    }
    fn description(&self) -> &str {
        "Write content to a file, completely replacing existing content. Use this instead of echo/cat heredoc in terminal. Creates parent directories automatically. OVERWRITES the entire file — use 'patch' for targeted edits. Auto-runs syntax checks on .py/.json/.yaml/.toml and other linted languages; only NEW errors introduced by this write are surfaced (pre-existing errors are filtered out)."
    }
    fn emoji(&self) -> &str {
        "✍\u{fe0f}"
    }
    fn max_result_chars(&self) -> Option<usize> {
        Some(100_000)
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to the file to write (will be created if it doesn't exist, overwritten if it does)"},
                "content": {"type": "string", "description": "Complete content to write to the file"},
                "cross_profile": {
                    "type": "boolean",
                    "description": "Opt out of the cross-profile soft guard. Defaults to false. Set true ONLY after explicit user direction to edit another Joey profile's skills/plugins/cron/memories — by default these writes are blocked with a warning because they affect a different profile than the one this session is running under.",
                    "default": false,
                },
            },
            "required": ["path", "content"]
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        // Arg-validation dropped-content diagnostics (file_tools.py:2055-2074).
        let path = match args.get("path") {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => {
                return tool_error(
                    "write_file: missing required field 'path'. Re-emit the tool call with both 'path' and 'content' set.",
                )
            }
        };
        let content = match args.get("content") {
            None => {
                return tool_error(
                    "write_file: missing required field 'content'. The tool call included a path but no content argument — this is almost always a dropped-arg bug under context pressure. Re-emit the tool call with the full content payload, or use execute_code with joey_tools.write_file() for very large files.",
                )
            }
            Some(Value::String(s)) => s.clone(),
            Some(other) => {
                return tool_error(format!(
                    "write_file: 'content' must be a string, got {}.",
                    python_type_name(other)
                ))
            }
        };

        let resolved = ctx.resolve_path(&path);
        if let Some(err) = guards::check_sensitive_path(&path, &resolved) {
            return tool_error(err);
        }
        // NOTE: the upstream cross-profile soft guard needs profile metadata
        // the port's ToolContext does not carry yet; the `cross_profile`
        // parameter is accepted for schema fidelity (deferred behavior).
        let _cross_profile = arg_bool(&args, "cross_profile");

        if guards::is_internal_file_tool_content(&content) {
            return tool_error(
                "Refusing to write internal read_file display text as file content. Strip read_file line-number prefixes or reconstruct the intended file contents before writing.",
            );
        }

        // Fail-closed pre-write syntax gate (JSON/YAML/TOML).
        if let Some(err) = syntax_gate(&path, &content) {
            let mut d = Map::new();
            d.insert("error".into(), json!(err));
            return ToolResult::Text(dumps(&Value::Object(d)));
        }

        let stale_warning = check_file_staleness(ctx, &path, &resolved);

        let mut result = Map::new();
        match write_with_preservation(&resolved, &content) {
            Ok((bytes_written, dirs_created)) => {
                result.insert("bytes_written".into(), json!(bytes_written));
                result.insert("dirs_created".into(), json!(dirs_created));
                if let Some(w) = stale_warning {
                    result.insert("_warning".into(), json!(w));
                }
                result.insert("resolved_path".into(), json!(resolved.to_string_lossy()));
                result.insert("files_modified".into(), json!([resolved.to_string_lossy()]));
                update_read_timestamp(ctx, &resolved);
            }
            Err(e) => {
                result.insert("error".into(), json!(e));
                if let Some(w) = stale_warning {
                    result.insert("_warning".into(), json!(w));
                }
                result.insert("resolved_path".into(), json!(resolved.to_string_lossy()));
            }
        }
        ToolResult::Text(dumps(&Value::Object(result)))
    }
}

// ---------------------------------------------------------------------------
// patch
// ---------------------------------------------------------------------------

/// Local-FS implementation of the V4A file-ops interface, resolving paths
/// against the session cwd.
struct CtxFileOps<'a> {
    ctx: &'a ToolContext,
}

impl V4aFileOps for CtxFileOps<'_> {
    fn read_file_raw(&self, path: &str) -> Result<String, String> {
        let resolved = self.ctx.resolve_path(path);
        let text = std::fs::read_to_string(&resolved)
            .map_err(|_| format!("File not found: {}", path))?;
        let (text, _) = strip_bom(&text);
        Ok(text.to_string())
    }
    fn write_file(&self, path: &str, content: &str) -> Result<(), String> {
        let resolved = self.ctx.resolve_path(path);
        write_with_preservation(&resolved, content).map(|_| ())
    }
    fn delete_file(&self, path: &str) -> Result<(), String> {
        let resolved = self.ctx.resolve_path(path);
        std::fs::remove_file(&resolved).map_err(|e| format!("Failed to delete {}: {}", path, e))
    }
    fn move_file(&self, src: &str, dst: &str) -> Result<(), String> {
        let rs = self.ctx.resolve_path(src);
        let rd = self.ctx.resolve_path(dst);
        std::fs::rename(&rs, &rd).map_err(|e| format!("Failed to move {} -> {}: {}", src, dst, e))
    }
}

fn v4a_header_paths(patch: &str) -> Result<Vec<String>, String> {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static FILE_RE: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"(?m)^\*\*\*\s*(?:Update|Add|Delete)\s+File:\s*(.+)$").unwrap()
    });
    static MOVE_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(?m)^\*\*\*\s*Move\s+File:\s*(.+?)\s*->\s*(.+)$").unwrap());
    let reject = |p: &str| -> Option<String> {
        if guards::has_traversal_component(p) {
            Some(format!(
                "V4A patch header contains '..' traversal: '{}'. Use the agent's cwd-relative path (no '..') or an absolute path in '*** Update File:' / '*** Add File:' / '*** Delete File:' / '*** Move File:' headers.",
                p
            ))
        } else {
            None
        }
    };
    let mut paths = Vec::new();
    for m in FILE_RE.captures_iter(patch) {
        let p = m[1].trim().to_string();
        if let Some(err) = reject(&p) {
            return Err(err);
        }
        paths.push(p);
    }
    for m in MOVE_RE.captures_iter(patch) {
        for p in [m[1].trim().to_string(), m[2].trim().to_string()] {
            if let Some(err) = reject(&p) {
                return Err(err);
            }
            paths.push(p);
        }
    }
    Ok(paths)
}

pub struct Patch;

#[async_trait]
impl Tool for Patch {
    fn name(&self) -> &str {
        "patch"
    }
    fn toolset(&self) -> &str {
        "file"
    }
    fn description(&self) -> &str {
        "Targeted find-and-replace edits in files. Use this instead of sed/awk in terminal. Uses fuzzy matching (9 strategies) so minor whitespace/indentation differences won't break it. Returns a unified diff. Auto-runs syntax checks after editing.\n\nREPLACE MODE (mode='replace', default): find a unique string and replace it. REQUIRED PARAMETERS: mode, path, old_string, new_string.\nPATCH MODE (mode='patch'): apply V4A multi-file patches for bulk changes. REQUIRED PARAMETERS: mode, patch."
    }
    fn emoji(&self) -> &str {
        "🔧"
    }
    fn max_result_chars(&self) -> Option<usize> {
        Some(100_000)
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "mode": {
                    "type": "string",
                    "enum": ["replace", "patch"],
                    "description": "Edit mode. 'replace' (default): requires path + old_string + new_string. 'patch': requires patch content only.",
                    "default": "replace",
                },
                "path": {
                    "type": "string",
                    "description": "REQUIRED when mode='replace'. File path to edit.",
                },
                "old_string": {
                    "type": "string",
                    "description": "REQUIRED when mode='replace'. Exact text to find and replace. Must be unique in the file unless replace_all=true. Include surrounding context lines to ensure uniqueness.",
                },
                "new_string": {
                    "type": "string",
                    "description": "REQUIRED when mode='replace'. Replacement text. Pass empty string '' to delete the matched text.",
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences instead of requiring a unique match (default: false)",
                    "default": false,
                },
                "patch": {
                    "type": "string",
                    "description": "REQUIRED when mode='patch'. V4A format patch content. Format:\n*** Begin Patch\n*** Update File: path/to/file\n@@ context hint @@\n context line\n-removed line\n+added line\n*** End Patch",
                },
                "cross_profile": {
                    "type": "boolean",
                    "description": "Opt out of the cross-profile soft guard. Defaults to false. Set true ONLY after explicit user direction to edit another Joey profile's skills/plugins/cron/memories.",
                    "default": false,
                },
            },
            "required": ["mode"],
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let mode = arg_str(&args, "mode").unwrap_or_else(|| "replace".to_string());
        let path = arg_str(&args, "path");
        let old_string = arg_str(&args, "old_string");
        let new_string = arg_str(&args, "new_string");
        let replace_all = arg_bool(&args, "replace_all");
        let patch_content = arg_str(&args, "patch");

        // Sensitive-path checks for both modes (V4A headers extracted from
        // patch CONTENT get the extra '..' traversal rejection).
        let mut paths_to_check: Vec<String> = Vec::new();
        if let Some(p) = &path {
            paths_to_check.push(p.clone());
        }
        if mode == "patch" {
            if let Some(pc) = &patch_content {
                match v4a_header_paths(pc) {
                    Ok(paths) => paths_to_check.extend(paths),
                    Err(e) => return tool_error(e),
                }
            }
        }
        for p in &paths_to_check {
            let resolved = ctx.resolve_path(p);
            if let Some(err) = guards::check_sensitive_path(p, &resolved) {
                return tool_error(err);
            }
        }

        // Staleness warnings across all touched paths.
        let mut stale_warnings: Vec<String> = Vec::new();
        let mut path_to_resolved: Vec<(String, PathBuf)> = Vec::new();
        for p in &paths_to_check {
            let resolved = ctx.resolve_path(p);
            if let Some(w) = check_file_staleness(ctx, p, &resolved) {
                stale_warnings.push(w);
            }
            path_to_resolved.push((p.clone(), resolved));
        }

        let mut result: Map<String, Value>;
        match mode.as_str() {
            "replace" => {
                let Some(path) = path.clone() else {
                    return tool_error("path required");
                };
                let (Some(old), Some(new)) = (old_string, new_string) else {
                    return tool_error("old_string and new_string required");
                };
                let resolved = ctx.resolve_path(&path);
                result = patch_replace(&resolved, &path, &old, &new, replace_all);
            }
            "patch" => {
                let Some(pc) = patch_content else {
                    return tool_error("patch content required");
                };
                result = patch_v4a(ctx, &pc);
            }
            other => {
                return tool_error(format!("Unknown mode: {}", other));
            }
        }

        if !stale_warnings.is_empty() {
            let w = if stale_warnings.len() == 1 {
                stale_warnings[0].clone()
            } else {
                stale_warnings.join(" | ")
            };
            result.insert("_warning".into(), json!(w));
        }

        let has_error = result.contains_key("error");
        if !has_error {
            let resolved_modified: Vec<String> = path_to_resolved
                .iter()
                .map(|(_, r)| r.to_string_lossy().to_string())
                .collect();
            if !resolved_modified.is_empty() {
                result.insert("files_modified".into(), json!(resolved_modified));
                if resolved_modified.len() == 1 {
                    result.insert("resolved_path".into(), json!(resolved_modified[0]));
                }
            }
            for (_, r) in &path_to_resolved {
                update_read_timestamp(ctx, r);
            }
            ctx.state().reset_patch_failures(
                &path_to_resolved
                    .iter()
                    .map(|(_, r)| r.to_string_lossy().to_string())
                    .collect::<Vec<_>>(),
            );
        } else if let Some(err) = result.get("error").and_then(|e| e.as_str()).map(str::to_string) {
            // Hint when old_string not found (file_tools.py:1809-1843).
            if err.contains("Could not find") {
                let mut failure_count = 0u32;
                if mode == "replace" {
                    if let Some(p) = &path {
                        let resolved = ctx.resolve_path(p).to_string_lossy().to_string();
                        failure_count = ctx.state().record_patch_failure(&resolved);
                    }
                }
                if failure_count >= 3 {
                    result.insert(
                        "_hint".into(),
                        json!(format!(
                            "This is failure #{} patching '{}'. Stop retrying with variations of the same old_string. Either: (1) re-read the file fresh to verify current content, (2) use a longer / more unique old_string with surrounding context lines, or (3) use write_file to replace the entire file if the targeted region is hard to anchor.",
                            failure_count,
                            path.as_deref().unwrap_or("")
                        )),
                    );
                } else if !err.contains("Did you mean one of these sections?") {
                    result.insert(
                        "_hint".into(),
                        json!("old_string not found. Use read_file to verify the current content, or search_files to locate the text."),
                    );
                }
            }
        }

        ToolResult::Text(dumps(&Value::Object(result)))
    }
}

/// Port of `ShellFileOperations.patch_replace` (result keys in PatchResult
/// order: success, diff?, files_modified?, error?).
fn patch_replace(
    resolved: &Path,
    path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Map<String, Value> {
    let mut d = Map::new();
    let raw = match std::fs::read_to_string(resolved) {
        Ok(c) => c,
        Err(_) => {
            d.insert("success".into(), json!(false));
            d.insert("error".into(), json!(format!("Failed to read file: {}", path)));
            return d;
        }
    };
    let (content, _) = strip_bom(&raw);
    let content = content.to_string();

    let outcome = fuzzy::fuzzy_find_and_replace(&content, old_string, new_string, replace_all);
    if outcome.error.is_some() || outcome.match_count == 0 {
        let mut err_msg = outcome
            .error
            .clone()
            .unwrap_or_else(|| format!("Could not find match for old_string in {}", path));
        err_msg.push_str(&fuzzy::format_no_match_hint(
            Some(&err_msg.clone()),
            outcome.match_count,
            old_string,
            &content,
        ));
        d.insert("success".into(), json!(false));
        d.insert("error".into(), json!(err_msg));
        return d;
    }

    let mut new_content = outcome.new_content;
    // Line-ending preservation for the substituted region.
    if let Some(file_ending) = detect_line_ending(&content) {
        new_content = normalize_line_endings(&new_content, file_ending);
    }

    if let Err(e) = write_with_preservation(resolved, &new_content) {
        d.insert("success".into(), json!(false));
        d.insert("error".into(), json!(format!("Failed to write changes: {}", e)));
        return d;
    }

    // Post-write verification — re-read and compare (normalized).
    let verify = match std::fs::read_to_string(resolved) {
        Ok(v) => v,
        Err(_) => {
            d.insert("success".into(), json!(false));
            d.insert(
                "error".into(),
                json!(format!("Post-write verification failed: could not re-read {}", path)),
            );
            return d;
        }
    };
    let (verify_bomless, _) = strip_bom(&verify);
    let verify_norm = verify_bomless.replace("\r\n", "\n").replace('\r', "\n");
    let new_norm = new_content.replace("\r\n", "\n").replace('\r', "\n");
    if verify_norm != new_norm {
        d.insert("success".into(), json!(false));
        d.insert(
            "error".into(),
            json!(format!(
                "Post-write verification failed for {}: on-disk content differs from intended write (wrote {} chars, read back {} chars after normalizing line endings). The patch did not persist. Re-read the file and try again.",
                path,
                new_norm.len(),
                verify_norm.len()
            )),
        );
        return d;
    }

    let diff = unified_diff(&content, &new_content, &format!("a/{}", path), &format!("b/{}", path));
    d.insert("success".into(), json!(true));
    if !diff.is_empty() {
        d.insert("diff".into(), json!(diff));
    }
    d.insert("files_modified".into(), json!([path]));
    d
}

/// Port of `patch_v4a` — parse + apply, returning the PatchResult dict shape.
fn patch_v4a(ctx: &ToolContext, patch_content: &str) -> Map<String, Value> {
    let mut d = Map::new();
    let operations = match patch_parser::parse_v4a_patch(patch_content) {
        Ok(ops) => ops,
        Err(e) => {
            d.insert("success".into(), json!(false));
            d.insert("error".into(), json!(format!("Failed to parse patch: {}", e)));
            return d;
        }
    };
    let ops = CtxFileOps { ctx };
    let result = patch_parser::apply_v4a_operations(&operations, &ops);
    d.insert("success".into(), json!(result.success));
    if !result.diff.is_empty() {
        d.insert("diff".into(), json!(result.diff));
    }
    if !result.files_modified.is_empty() {
        d.insert("files_modified".into(), json!(result.files_modified));
    }
    if !result.files_created.is_empty() {
        d.insert("files_created".into(), json!(result.files_created));
    }
    if !result.files_deleted.is_empty() {
        d.insert("files_deleted".into(), json!(result.files_deleted));
    }
    if let Some(e) = result.error {
        d.insert("error".into(), json!(e));
    }
    d
}

// ---------------------------------------------------------------------------
// search_files
// ---------------------------------------------------------------------------

pub struct SearchFiles;

#[async_trait]
impl Tool for SearchFiles {
    fn name(&self) -> &str {
        "search_files"
    }
    fn toolset(&self) -> &str {
        "file"
    }
    fn description(&self) -> &str {
        "Search file contents or find files by name. Use this instead of grep/rg/find/ls in terminal. Ripgrep-backed, faster than shell equivalents.\n\nContent search (target='content'): Regex search inside files. Output modes: full matches with line numbers, file paths only, or match counts.\n\nFile search (target='files'): Find files by glob pattern (e.g., '*.py', '*config*'). Also use this instead of ls — results sorted by modification time."
    }
    fn emoji(&self) -> &str {
        "🔎"
    }
    fn max_result_chars(&self) -> Option<usize> {
        Some(100_000)
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Regex pattern for content search, or glob pattern (e.g., '*.py') for file search"},
                "target": {"type": "string", "enum": ["content", "files"], "description": "'content' searches inside file contents, 'files' searches for files by name", "default": "content"},
                "path": {"type": "string", "description": "Directory or file to search in (default: current working directory)", "default": "."},
                "file_glob": {"type": "string", "description": "Filter files by pattern in grep mode (e.g., '*.py' to only search Python files)"},
                "limit": {"type": "integer", "description": "Maximum number of results to return (default: 50)", "default": 50},
                "offset": {"type": "integer", "description": "Skip first N results for pagination (default: 0)", "default": 0},
                "output_mode": {"type": "string", "enum": ["content", "files_only", "count"], "description": "Output format for grep mode: 'content' shows matching lines with line numbers, 'files_only' lists file paths, 'count' shows match counts per file", "default": "content"},
                "context": {"type": "integer", "description": "Number of context lines before and after each match (grep mode only)", "default": 0}
            },
            "required": ["pattern"]
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let pattern = arg_str(&args, "pattern").unwrap_or_default();
        // Target aliases: grep → content, find → files (file_tools.py:2095-2097).
        let raw_target = arg_str(&args, "target").unwrap_or_else(|| "content".to_string());
        let target = match raw_target.as_str() {
            "grep" => "content".to_string(),
            "find" => "files".to_string(),
            other => other.to_string(),
        };
        let path = arg_str(&args, "path").unwrap_or_else(|| ".".to_string());
        let file_glob = arg_str(&args, "file_glob");
        let (offset, limit) =
            truncate::normalize_search_pagination(arg_i64(&args, "offset"), arg_i64(&args, "limit"));
        let output_mode = arg_str(&args, "output_mode").unwrap_or_else(|| "content".to_string());
        let context = arg_i64(&args, "context").unwrap_or(0).max(0) as usize;

        // Consecutive repeated-search loop guard.
        let search_key = format!(
            "search\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
            pattern,
            target,
            path,
            file_glob.as_deref().unwrap_or(""),
            limit,
            offset
        );
        let count = {
            let mut st = ctx.state();
            if st.last_key.as_deref() == Some(&search_key) {
                st.consecutive += 1;
            } else {
                st.last_key = Some(search_key);
                st.consecutive = 1;
            }
            st.consecutive
        };
        if count >= 4 {
            return ToolResult::Text(dumps(&json!({
                "error": format!(
                    "BLOCKED: You have run this exact search {} times in a row. The results have NOT changed. You already have this information. STOP re-searching and proceed with your task.",
                    count
                ),
                "pattern": pattern,
                "already_searched": count,
            })));
        }

        let resolved_path = ctx.resolve_path(&path);
        if let Some(block_error) = guards::get_read_block_error(&resolved_path.to_string_lossy()) {
            return ToolResult::Text(dumps(&json!({ "error": block_error })));
        }

        let mut result = search_impl(
            ctx,
            &pattern,
            &resolved_path,
            &target,
            file_glob.as_deref(),
            limit,
            offset,
            &output_mode,
            context,
        )
        .await;

        // Filter read-blocked paths from results.
        let mut omitted = 0usize;
        result.matches.retain(|m| {
            if guards::get_read_block_error(&m.path).is_some() {
                omitted += 1;
                false
            } else {
                true
            }
        });
        result.files.retain(|f| {
            if guards::get_read_block_error(f).is_some() {
                omitted += 1;
                false
            } else {
                true
            }
        });
        result.counts.retain(|f, _| {
            if guards::get_read_block_error(f).is_some() {
                omitted += 1;
                false
            } else {
                true
            }
        });
        for m in result.matches.iter_mut() {
            if !m.content.is_empty() {
                m.content = joey_core::redact::redact_secrets(&m.content);
            }
        }

        // Newline-regex warning (line-oriented search).
        maybe_warn_line_oriented_newline_pattern(&mut result, &pattern);

        let mut dict = result.to_dict(true);
        if omitted > 0 {
            dict.insert(
                "_omitted".into(),
                json!(format!(
                    "{} result(s) omitted because they target credential, token, cache, or secret-bearing environment files.",
                    omitted
                )),
            );
        }
        if count >= 3 {
            dict.insert(
                "_warning".into(),
                json!(format!(
                    "You have run this exact search {} times consecutively. The results have not changed. Use the information you already have.",
                    count
                )),
            );
        }

        let truncated = dict.get("truncated").and_then(|t| t.as_bool()).unwrap_or(false);
        let mut result_json = dumps(&Value::Object(dict));
        if truncated {
            let next_offset = offset + limit;
            result_json.push_str(&format!(
                "\n\n[Hint: Results truncated. Use offset={} to see more, or narrow with a more specific pattern or file_glob.]",
                next_offset
            ));
        }
        ToolResult::Text(result_json)
    }
}

#[derive(Default)]
struct SearchMatch {
    path: String,
    line_number: u64,
    content: String,
}

#[derive(Default)]
struct SearchResult {
    matches: Vec<SearchMatch>,
    files: Vec<String>,
    counts: indexmap::IndexMap<String, u64>,
    total_count: u64,
    truncated: bool,
    limit_reason: Option<String>,
    warning: Option<String>,
    error: Option<String>,
}

impl SearchResult {
    const DENSIFY_MIN_MATCHES: usize = 5;

    fn densify_matches(&self) -> Option<String> {
        if self.matches.len() < Self::DENSIFY_MIN_MATCHES {
            return None;
        }
        let mut lines: Vec<String> = Vec::new();
        let mut current_path: Option<&str> = None;
        for m in &self.matches {
            if current_path != Some(m.path.as_str()) {
                lines.push(m.path.clone());
                current_path = Some(m.path.as_str());
            }
            lines.push(format!("  {}: {}", m.line_number, m.content.trim_end()));
        }
        Some(lines.join("\n"))
    }

    fn to_dict(&self, densify: bool) -> Map<String, Value> {
        let mut result = Map::new();
        result.insert("total_count".into(), json!(self.total_count));
        if !self.matches.is_empty() {
            let dense = if densify { self.densify_matches() } else { None };
            match dense {
                Some(text) => {
                    result.insert(
                        "matches_format".into(),
                        json!("path-grouped: each file path on its own line, followed by indented '<line>: <content>' rows for matches in that file"),
                    );
                    result.insert("matches_text".into(), json!(text));
                }
                None => {
                    let arr: Vec<Value> = self
                        .matches
                        .iter()
                        .map(|m| json!({"path": m.path, "line": m.line_number, "content": m.content}))
                        .collect();
                    result.insert("matches".into(), json!(arr));
                }
            }
        }
        if !self.files.is_empty() {
            result.insert("files".into(), json!(self.files));
        }
        if !self.counts.is_empty() {
            let mut counts = Map::new();
            for (k, v) in &self.counts {
                counts.insert(k.clone(), json!(v));
            }
            result.insert("counts".into(), Value::Object(counts));
        }
        if self.truncated {
            result.insert("truncated".into(), json!(true));
        }
        if let Some(r) = &self.limit_reason {
            result.insert("limit_reason".into(), json!(r));
        }
        if let Some(w) = &self.warning {
            result.insert("warning".into(), json!(w));
        }
        if let Some(e) = &self.error {
            result.insert("error".into(), json!(e));
        }
        result
    }
}

fn pattern_has_regex_newline(pattern: &str) -> bool {
    if pattern.contains('\n') {
        return true;
    }
    // Odd number of backslashes before an `n`.
    let bytes = pattern.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            let mut run = 0;
            while i < bytes.len() && bytes[i] == b'\\' {
                run += 1;
                i += 1;
            }
            if run % 2 == 1 && i < bytes.len() && bytes[i] == b'n' {
                return true;
            }
        } else {
            i += 1;
        }
    }
    false
}

fn maybe_warn_line_oriented_newline_pattern(result: &mut SearchResult, pattern: &str) {
    if result.total_count != 0 || !pattern_has_regex_newline(pattern) {
        return;
    }
    if let Some(err) = &result.error {
        let is_multiline_err = err.contains("literal \"\\n\" is not allowed") && err.contains("--multiline");
        if !is_multiline_err {
            return;
        }
    }
    result.error = None;
    result.warning = Some(
        "0 results found. Note: search_files content search is line-oriented and does not run ripgrep with -U/--multiline, so `\\n` in the regex does not match line breaks. Use context=N to inspect neighboring lines, or escape as `\\\\n` when searching for a literal backslash+n."
            .to_string(),
    );
}

fn shell_quote(arg: &str) -> String {
    format!("'{}'", arg.replace('\'', "'\"'\"'"))
}

/// Run a shell pipeline via bash with a 60s timeout, merging stderr after
/// stdout (the upstream env merges the streams; diagnostics are separated by
/// shape afterwards). Returns (combined_output, exit_code).
async fn run_search_command(script: &str, cwd: &Path) -> (String, i32) {
    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg("-c")
        .arg(script)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (format!("Search error: {}", e), 2),
    };
    match tokio::time::timeout(std::time::Duration::from_secs(60), child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
            let err = String::from_utf8_lossy(&output.stderr);
            if !err.is_empty() {
                if !combined.is_empty() && !combined.ends_with('\n') {
                    combined.push('\n');
                }
                combined.push_str(&err);
            }
            (combined, output.status.code().unwrap_or(-1))
        }
        Ok(Err(e)) => (format!("Search error: {}", e), 2),
        Err(_) => (String::new(), 124),
    }
}

/// Port of `_split_tool_diagnostics` — separate rg/grep diagnostic lines from
/// real match output by shape.
fn split_tool_diagnostics(output: &str) -> (String, String) {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static SEARCH_OUTPUT_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"^(?:[A-Za-z]:)?[^\s:][^\n]*?[:\-]\d|^[^\s:][^\s]*$").unwrap());
    let mut diagnostics: Vec<&str> = Vec::new();
    let mut payload: Vec<&str> = Vec::new();
    for line in output.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        let stripped = line.trim_start();
        if stripped.starts_with("rg: ") || stripped.starts_with("grep: ") {
            diagnostics.push(line);
            continue;
        }
        if line == "--" || SEARCH_OUTPUT_RE.is_match(line) {
            payload.push(line);
        } else {
            diagnostics.push(line);
        }
    }
    (diagnostics.join("\n"), payload.join("\n"))
}

fn parse_search_context_line(line: &str) -> Option<(String, u64, String)> {
    use once_cell::sync::Lazy;
    use regex::Regex;
    static CTX_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"-(\d+)-").unwrap());
    if line.is_empty() || line == "--" {
        return None;
    }
    let m = CTX_RE.find_iter(line).last()?;
    let path = &line[..m.start()];
    if path.is_empty() {
        return None;
    }
    let num: u64 = line[m.start() + 1..m.end() - 1].parse().ok()?;
    Some((path.to_string(), num, line[m.end()..].to_string()))
}

fn clamp_500(s: &str) -> String {
    let cut = truncate::floor_char_boundary(s, 500);
    s[..cut].to_string()
}

#[allow(clippy::too_many_arguments)]
async fn search_impl(
    ctx: &ToolContext,
    pattern: &str,
    resolved_path: &Path,
    target: &str,
    file_glob: Option<&str>,
    limit: usize,
    offset: usize,
    output_mode: &str,
    context: usize,
) -> SearchResult {
    // Validate that the path exists before searching.
    if !resolved_path.exists() {
        let mut hint_parts = vec![format!("Path not found: {}", resolved_path.display())];
        if let Some(parent) = resolved_path.parent() {
            if parent.is_dir() {
                if let Some(basename) = resolved_path.file_name().and_then(|n| n.to_str()) {
                    let lower_q = basename.to_lowercase();
                    let mut candidates: Vec<String> = Vec::new();
                    if let Ok(entries) = std::fs::read_dir(parent) {
                        for e in entries.flatten().take(20) {
                            let name = e.file_name().to_string_lossy().to_string();
                            let le = name.to_lowercase();
                            let prefix: String = lower_q.chars().take(3).collect();
                            if le.contains(&lower_q)
                                || lower_q.contains(&le)
                                || (!prefix.is_empty() && le.starts_with(&prefix))
                            {
                                candidates.push(parent.join(&name).to_string_lossy().to_string());
                            }
                        }
                    }
                    if !candidates.is_empty() {
                        hint_parts.push(format!(
                            "Similar paths: {}",
                            candidates.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
                        ));
                    }
                }
            }
        }
        return SearchResult {
            error: Some(hint_parts.join(". ")),
            total_count: 0,
            ..Default::default()
        };
    }

    let have_rg = which::which("rg").is_ok();
    if target == "files" {
        if have_rg {
            return search_files_rg(ctx, pattern, resolved_path, limit, offset).await;
        }
        return search_files_walk(resolved_path, pattern, limit, offset);
    }
    if have_rg {
        return search_with_rg(ctx, pattern, resolved_path, file_glob, limit, offset, output_mode, context)
            .await;
    }
    search_content_walk(resolved_path, pattern, file_glob, limit, offset, output_mode)
}

async fn search_files_rg(
    ctx: &ToolContext,
    pattern: &str,
    path: &Path,
    limit: usize,
    offset: usize,
) -> SearchResult {
    // Upstream `_search_files` preprocessing.
    let search_pattern = if !pattern.starts_with("**/") && !pattern.contains('/') {
        pattern.to_string()
    } else {
        pattern.split('/').next_back().unwrap_or(pattern).to_string()
    };
    // rg --files -g wrapping: bare names match at any depth.
    let glob_pattern = if !search_pattern.contains('/') && !search_pattern.starts_with('*') {
        format!("*{}", search_pattern)
    } else {
        search_pattern
    };
    let fetch_limit = limit + offset;
    let path_str = path.to_string_lossy();
    let cmd_sorted = format!(
        "rg --files --sortr=modified -g {} {} 2>/dev/null | head -n {}",
        shell_quote(&glob_pattern),
        shell_quote(&path_str),
        fetch_limit
    );
    let (stdout, code) = run_search_command(&cmd_sorted, &ctx.effective_cwd()).await;
    let limit_reason = if code == 124 { Some("search_timeout".to_string()) } else { None };
    let mut all_files: Vec<String> =
        stdout.trim().split('\n').filter(|f| !f.is_empty()).map(str::to_string).collect();

    if all_files.is_empty() && limit_reason.is_none() {
        let cmd_plain = format!(
            "rg --files -g {} {} 2>/dev/null | head -n {}",
            shell_quote(&glob_pattern),
            shell_quote(&path_str),
            fetch_limit
        );
        let (stdout, _code) = run_search_command(&cmd_plain, &ctx.effective_cwd()).await;
        all_files =
            stdout.trim().split('\n').filter(|f| !f.is_empty()).map(str::to_string).collect();
    }

    let page: Vec<String> =
        all_files.iter().skip(offset).take(limit).cloned().collect();
    SearchResult {
        truncated: all_files.len() >= fetch_limit || limit_reason.is_some(),
        total_count: all_files.len() as u64,
        files: page,
        limit_reason,
        ..Default::default()
    }
}

#[allow(clippy::too_many_arguments)]
async fn search_with_rg(
    ctx: &ToolContext,
    pattern: &str,
    path: &Path,
    file_glob: Option<&str>,
    limit: usize,
    offset: usize,
    output_mode: &str,
    context: usize,
) -> SearchResult {
    let mut cmd_parts: Vec<String> =
        vec!["rg".into(), "--line-number".into(), "--no-heading".into(), "--with-filename".into()];
    if context > 0 {
        cmd_parts.push("-C".into());
        cmd_parts.push(context.to_string());
    }
    if let Some(glob) = file_glob {
        cmd_parts.push("--glob".into());
        cmd_parts.push(shell_quote(glob));
    }
    if output_mode == "files_only" {
        cmd_parts.push("-l".into());
    } else if output_mode == "count" {
        cmd_parts.push("-c".into());
    }
    cmd_parts.push(shell_quote(pattern));
    cmd_parts.push(shell_quote(&path.to_string_lossy()));
    let fetch_limit = if context > 0 { limit + offset + 200 } else { limit + offset };
    cmd_parts.push("|".into());
    cmd_parts.push("head".into());
    cmd_parts.push("-n".into());
    cmd_parts.push(fetch_limit.to_string());
    let cmd = format!("set -o pipefail; {}", cmd_parts.join(" "));

    let (raw, code) = run_search_command(&cmd, &ctx.effective_cwd()).await;
    let limit_reason = if code == 124 { Some("search_timeout".to_string()) } else { None };
    let (diagnostics, payload) = split_tool_diagnostics(&raw);

    if code == 2 && payload.trim().is_empty() {
        let error_msg = if !diagnostics.trim().is_empty() {
            diagnostics.trim().to_string()
        } else if !raw.trim().is_empty() {
            raw.trim().to_string()
        } else {
            "Search error".to_string()
        };
        return SearchResult {
            error: Some(format!("Search failed: {}", error_msg)),
            total_count: 0,
            ..Default::default()
        };
    }

    let stdout = payload;
    if output_mode == "files_only" {
        let all_files: Vec<String> =
            stdout.trim().split('\n').filter(|f| !f.is_empty()).map(str::to_string).collect();
        let total = all_files.len() as u64;
        let page: Vec<String> = all_files.into_iter().skip(offset).take(limit).collect();
        return SearchResult {
            files: page,
            total_count: total,
            truncated: limit_reason.is_some(),
            limit_reason,
            ..Default::default()
        };
    }
    if output_mode == "count" {
        let mut counts = indexmap::IndexMap::new();
        for line in stdout.trim().split('\n') {
            if let Some((p, c)) = line.rsplit_once(':') {
                if let Ok(n) = c.parse::<u64>() {
                    counts.insert(p.to_string(), n);
                }
            }
        }
        let total: u64 = counts.values().sum();
        return SearchResult {
            counts,
            total_count: total,
            truncated: limit_reason.is_some(),
            limit_reason,
            ..Default::default()
        };
    }

    use once_cell::sync::Lazy;
    use regex::Regex;
    static MATCH_RE: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"^([A-Za-z]:)?(.*?):(\d+):(.*)$").unwrap());
    let mut matches: Vec<SearchMatch> = Vec::new();
    for line in stdout.trim().split('\n') {
        if line.is_empty() || line == "--" {
            continue;
        }
        if let Some(m) = MATCH_RE.captures(line) {
            matches.push(SearchMatch {
                path: format!("{}{}", m.get(1).map(|g| g.as_str()).unwrap_or(""), &m[2]),
                line_number: m[3].parse().unwrap_or(0),
                content: clamp_500(&m[4]),
            });
            continue;
        }
        if context > 0 {
            if let Some((p, n, c)) = parse_search_context_line(line) {
                matches.push(SearchMatch { path: p, line_number: n, content: clamp_500(&c) });
            }
        }
    }
    let total = matches.len() as u64;
    let page: Vec<SearchMatch> = matches.into_iter().skip(offset).take(limit).collect();
    SearchResult {
        matches: page,
        truncated: total as usize > offset + limit || limit_reason.is_some(),
        total_count: total,
        limit_reason,
        ..Default::default()
    }
}

/// Fallback content search when rg is unavailable (analog of the grep path):
/// an internal gitignore-aware walk.
fn search_content_walk(
    root: &Path,
    pattern: &str,
    file_glob: Option<&str>,
    limit: usize,
    offset: usize,
    output_mode: &str,
) -> SearchResult {
    let re = match regex::Regex::new(pattern) {
        Ok(r) => r,
        Err(e) => {
            return SearchResult {
                error: Some(format!("Search failed: {}", e)),
                total_count: 0,
                ..Default::default()
            }
        }
    };
    let glob = file_glob.and_then(|g| globset::Glob::new(g).ok().map(|gg| gg.compile_matcher()));
    let mut matches: Vec<SearchMatch> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut counts: indexmap::IndexMap<String, u64> = indexmap::IndexMap::new();
    let fetch_limit = limit + offset;
    let walker = ignore::WalkBuilder::new(root).build();
    'outer: for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if let Some(g) = &glob {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !g.is_match(path) && !g.is_match(name) {
                continue;
            }
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let mut file_hits = 0u64;
        for (i, line) in content.lines().enumerate() {
            if re.is_match(line) {
                file_hits += 1;
                if output_mode == "content" {
                    matches.push(SearchMatch {
                        path: path.display().to_string(),
                        line_number: (i + 1) as u64,
                        content: clamp_500(line),
                    });
                    if matches.len() >= fetch_limit {
                        break 'outer;
                    }
                }
            }
        }
        if file_hits > 0 {
            files.push(path.display().to_string());
            counts.insert(path.display().to_string(), file_hits);
            if output_mode != "content" && files.len() >= fetch_limit {
                break;
            }
        }
    }
    match output_mode {
        "files_only" => {
            let total = files.len() as u64;
            SearchResult {
                files: files.into_iter().skip(offset).take(limit).collect(),
                total_count: total,
                ..Default::default()
            }
        }
        "count" => {
            let total: u64 = counts.values().sum();
            SearchResult { counts, total_count: total, ..Default::default() }
        }
        _ => {
            let total = matches.len() as u64;
            SearchResult {
                matches: matches.into_iter().skip(offset).take(limit).collect(),
                truncated: total as usize > offset + limit,
                total_count: total,
                ..Default::default()
            }
        }
    }
}

/// Fallback file search when rg is unavailable.
fn search_files_walk(root: &Path, pattern: &str, limit: usize, offset: usize) -> SearchResult {
    let search_pattern = if !pattern.starts_with("**/") && !pattern.contains('/') {
        pattern.to_string()
    } else {
        pattern.split('/').next_back().unwrap_or(pattern).to_string()
    };
    let matcher = match globset::Glob::new(&search_pattern) {
        Ok(g) => g.compile_matcher(),
        Err(e) => {
            return SearchResult {
                error: Some(format!("Search failed: {}", e)),
                total_count: 0,
                ..Default::default()
            }
        }
    };
    let mut found: Vec<(std::time::SystemTime, String)> = Vec::new();
    let walker = ignore::WalkBuilder::new(root).build();
    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if matcher.is_match(name) {
            let mtime = mtime_of(path).unwrap_or(std::time::UNIX_EPOCH);
            found.push((mtime, path.display().to_string()));
        }
    }
    // Sorted by modification time, most recent first (rg --sortr=modified).
    found.sort_by(|a, b| b.0.cmp(&a.0));
    // Mirror the rg path's fetch cap: gather limit+offset rows, and flag
    // truncation when the cap was reached.
    let fetch_limit = limit + offset;
    let all: Vec<String> = found.into_iter().map(|(_, p)| p).take(fetch_limit).collect();
    let total = all.len() as u64;
    SearchResult {
        truncated: all.len() >= fetch_limit,
        files: all.into_iter().skip(offset).take(limit).collect(),
        total_count: total,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use joey_core::Config;

    fn ctx_in(dir: &Path) -> ToolContext {
        ToolContext::new(dir.to_path_buf(), Config::defaults(), "test")
    }

    fn parse(result: &ToolResult) -> Value {
        serde_json::from_str(&result.to_content_string()).unwrap()
    }

    #[tokio::test]
    async fn read_file_envelope() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "line1\nline2\n").unwrap();
        let ctx = ctx_in(dir.path());
        let r = ReadFile.execute(json!({"path": "a.txt"}), &ctx).await;
        let v = parse(&r);
        assert_eq!(v["content"], "1|line1\n2|line2");
        assert_eq!(v["total_lines"], 2);
        assert_eq!(v["file_size"], 12);
        assert_eq!(v["truncated"], false);
        assert_eq!(v["is_binary"], false);
        assert_eq!(v["is_image"], false);
        assert!(v.get("hint").is_none());
    }

    #[tokio::test]
    async fn read_file_pagination_hint() {
        let dir = tempfile::tempdir().unwrap();
        let content: String = (1..=10).map(|i| format!("l{}\n", i)).collect();
        std::fs::write(dir.path().join("b.txt"), content).unwrap();
        let ctx = ctx_in(dir.path());
        let r = ReadFile.execute(json!({"path": "b.txt", "offset": 1, "limit": 3}), &ctx).await;
        let v = parse(&r);
        assert_eq!(v["truncated"], true);
        assert_eq!(
            v["hint"],
            "Use offset=4 to continue reading (showing 1-3 of 10 lines)"
        );
        // offset > EOF → empty content, no error.
        let r2 = ReadFile.execute(json!({"path": "b.txt", "offset": 100}), &ctx).await;
        let v2 = parse(&r2);
        assert_eq!(v2["content"], "");
        assert_eq!(v2["total_lines"], 10);
        assert!(v2.get("error").is_none());
    }

    #[tokio::test]
    async fn read_file_dedup_and_loop_block() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("c.txt"), "x\n").unwrap();
        let ctx = ctx_in(dir.path());
        let args = json!({"path": "c.txt"});
        let first = parse(&ReadFile.execute(args.clone(), &ctx).await);
        assert!(first.get("content").is_some());
        // Second identical read: unchanged stub.
        let second = parse(&ReadFile.execute(args.clone(), &ctx).await);
        assert_eq!(second["status"], "unchanged");
        assert_eq!(second["dedup"], true);
        assert_eq!(second["content_returned"], false);
        assert_eq!(second["message"], guards::READ_DEDUP_STATUS_MESSAGE);
        // Third: escalates to hard block.
        let third = parse(&ReadFile.execute(args.clone(), &ctx).await);
        assert!(third["error"].as_str().unwrap().starts_with("BLOCKED:"));
        assert_eq!(third["already_read"], 3);
    }

    #[tokio::test]
    async fn read_file_suggests_similar() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.yaml"), "a: 1\n").unwrap();
        let ctx = ctx_in(dir.path());
        let r = ReadFile.execute(json!({"path": "config.yml"}), &ctx).await;
        let v = parse(&r);
        assert_eq!(v["error"], "File not found: config.yml");
        let sims = v["similar_files"].as_array().unwrap();
        assert!(sims[0].as_str().unwrap().ends_with("config.yaml"));
    }

    #[tokio::test]
    async fn read_file_guards() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        let dev = parse(&ReadFile.execute(json!({"path": "/dev/stdin"}), &ctx).await);
        assert!(dev["error"].as_str().unwrap().contains("device file"));
        let bin = parse(&ReadFile.execute(json!({"path": "photo.png"}), &ctx).await);
        assert!(bin["error"].as_str().unwrap().contains("Cannot read binary file"));
        std::fs::write(dir.path().join(".env"), "SECRET=1\n").unwrap();
        let env = parse(&ReadFile.execute(json!({"path": ".env"}), &ctx).await);
        assert!(env["error"].as_str().unwrap().contains("secret-bearing environment file"));
    }

    #[tokio::test]
    async fn write_file_envelope_and_guards() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        let w = parse(
            &WriteFile.execute(json!({"path": "sub/a.txt", "content": "hello\n"}), &ctx).await,
        );
        assert_eq!(w["bytes_written"], 6);
        assert_eq!(w["dirs_created"], true);
        assert!(w["resolved_path"].as_str().unwrap().ends_with("sub/a.txt"));
        assert_eq!(w["files_modified"].as_array().unwrap().len(), 1);

        // Missing content diagnostics.
        let m = parse(&WriteFile.execute(json!({"path": "x.txt"}), &ctx).await);
        assert!(m["error"].as_str().unwrap().starts_with("write_file: missing required field 'content'."));
        let t = parse(&WriteFile.execute(json!({"path": "x.txt", "content": 42}), &ctx).await);
        assert_eq!(t["error"], "write_file: 'content' must be a string, got int.");

        // Sensitive path.
        let s = parse(
            &WriteFile.execute(json!({"path": "/etc/hosts", "content": "x"}), &ctx).await,
        );
        assert!(s["error"].as_str().unwrap().starts_with("Refusing to write to sensitive system path"));

        // Internal display text refusal.
        let i = parse(
            &WriteFile
                .execute(json!({"path": "y.txt", "content": "1|a\n2|b\n3|c\n"}), &ctx)
                .await,
        );
        assert!(i["error"].as_str().unwrap().starts_with("Refusing to write internal read_file display text"));

        // Fail-closed JSON gate.
        let j = parse(
            &WriteFile.execute(json!({"path": "bad.json", "content": "{not json"}), &ctx).await,
        );
        assert!(j["error"].as_str().unwrap().contains("fails .json syntax validation"));
        assert!(!dir.path().join("bad.json").exists());
    }

    #[tokio::test]
    async fn write_preserves_crlf_and_bom() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        std::fs::write(dir.path().join("dos.txt"), "\u{feff}a\r\nb\r\n").unwrap();
        WriteFile
            .execute(json!({"path": "dos.txt", "content": "x\ny\n"}), &ctx)
            .await;
        let bytes = std::fs::read(dir.path().join("dos.txt")).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with('\u{feff}'));
        assert!(text.contains("x\r\ny\r\n"));
    }

    #[tokio::test]
    async fn patch_replace_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        std::fs::write(dir.path().join("p.txt"), "hello world\n").unwrap();
        let r = parse(
            &Patch
                .execute(
                    json!({"mode": "replace", "path": "p.txt", "old_string": "world", "new_string": "joey"}),
                    &ctx,
                )
                .await,
        );
        assert_eq!(r["success"], true);
        let diff = r["diff"].as_str().unwrap();
        assert!(diff.contains("--- a/p.txt"));
        assert!(diff.contains("+++ b/p.txt"));
        assert!(diff.contains("-hello world"));
        assert!(diff.contains("+hello joey"));
        assert!(r["resolved_path"].as_str().unwrap().ends_with("p.txt"));
        assert_eq!(std::fs::read_to_string(dir.path().join("p.txt")).unwrap(), "hello joey\n");
    }

    #[tokio::test]
    async fn patch_no_match_hint_and_escalation() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        std::fs::write(dir.path().join("q.txt"), "alpha beta\n").unwrap();
        let args = json!({"mode": "replace", "path": "q.txt", "old_string": "zzz not here zzz", "new_string": "x"});
        let r1 = parse(&Patch.execute(args.clone(), &ctx).await);
        assert_eq!(r1["success"], false);
        assert!(r1["error"].as_str().unwrap().starts_with("Could not find a match for old_string in the file"));
        let _ = parse(&Patch.execute(args.clone(), &ctx).await);
        let r3 = parse(&Patch.execute(args.clone(), &ctx).await);
        assert!(r3["_hint"].as_str().unwrap().starts_with("This is failure #3 patching 'q.txt'."), "{:?}", r3);
    }

    #[tokio::test]
    async fn patch_v4a_mode() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        std::fs::write(dir.path().join("v.txt"), "one\ntwo\nthree\n").unwrap();
        let patch = "*** Begin Patch\n*** Update File: v.txt\n one\n-two\n+2\n*** End Patch";
        let r = parse(
            &Patch.execute(json!({"mode": "patch", "patch": patch}), &ctx).await,
        );
        assert_eq!(r["success"], true, "{:?}", r);
        assert!(std::fs::read_to_string(dir.path().join("v.txt")).unwrap().contains("2\n"));

        // Traversal in headers is rejected.
        let bad = "*** Begin Patch\n*** Update File: ../evil.txt\n-x\n+y\n*** End Patch";
        let e = parse(&Patch.execute(json!({"mode": "patch", "patch": bad}), &ctx).await);
        assert!(e["error"].as_str().unwrap().contains("V4A patch header contains '..' traversal"));
    }

    #[tokio::test]
    async fn patch_unknown_mode() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        let r = parse(&Patch.execute(json!({"mode": "wat"}), &ctx).await);
        assert_eq!(r["error"], "Unknown mode: wat");
    }

    #[tokio::test]
    async fn search_envelope_and_loop_guard() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        for i in 0..3 {
            std::fs::write(
                dir.path().join(format!("f{}.txt", i)),
                "needle here\nplain\nneedle again\n",
            )
            .unwrap();
        }
        let args = json!({"pattern": "needle", "path": "."});
        let v = parse(&SearchFiles.execute(args.clone(), &ctx).await);
        assert_eq!(v["total_count"], 6);
        // ≥5 matches → densified path-grouped format.
        assert_eq!(
            v["matches_format"],
            "path-grouped: each file path on its own line, followed by indented '<line>: <content>' rows for matches in that file"
        );
        assert!(v["matches_text"].as_str().unwrap().contains("  1: needle here"));

        // Loop guard: 4th identical search blocks.
        let _ = SearchFiles.execute(args.clone(), &ctx).await;
        let third = parse(&SearchFiles.execute(args.clone(), &ctx).await);
        assert!(third.get("_warning").is_some());
        let fourth = parse(&SearchFiles.execute(args.clone(), &ctx).await);
        assert!(fourth["error"].as_str().unwrap().starts_with("BLOCKED:"));
        assert_eq!(fourth["already_searched"], 4);
    }

    #[tokio::test]
    async fn search_truncation_hint_text() {
        // Files mode: the fetch cap (limit+offset) marks truncation when hit,
        // exactly like upstream's `_search_files_rg`.
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        for i in 0..10 {
            std::fs::write(dir.path().join(format!("file{}.log", i)), "x\n").unwrap();
        }
        let r = SearchFiles
            .execute(json!({"pattern": "*.log", "target": "files", "path": ".", "limit": 5}), &ctx)
            .await
            .to_content_string();
        assert!(
            r.contains("\n\n[Hint: Results truncated. Use offset=5 to see more, or narrow with a more specific pattern or file_glob.]"),
            "{}",
            r
        );
    }

    #[tokio::test]
    async fn search_files_mode_and_alias() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        std::fs::write(dir.path().join("mod.rs"), "x\n").unwrap();
        std::fs::write(dir.path().join("lib.rs"), "y\n").unwrap();
        let v = parse(
            &SearchFiles
                .execute(json!({"pattern": "*.rs", "target": "find", "path": "."}), &ctx)
                .await,
        );
        assert_eq!(v["total_count"], 2);
        assert_eq!(v["files"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn search_path_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        let v = parse(
            &SearchFiles.execute(json!({"pattern": "x", "path": "missing_dir"}), &ctx).await,
        );
        assert!(v["error"].as_str().unwrap().starts_with("Path not found: "));
        assert_eq!(v["total_count"], 0);
    }
}

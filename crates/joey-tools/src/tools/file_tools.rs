//! File tools: read_file, write_file, patch, search_files
//! (port of `tools/file_tools.py` + `search_files`).

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::context::ToolContext;
use crate::fuzzy;
use crate::registry::{tool_error, Tool, ToolResult};
use crate::truncate;

fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

// ── read_file ────────────────────────────────────────────────────────────────

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
        "Read a text file with line numbers and pagination. Returns `LINE|CONTENT` rows."
    }
    fn emoji(&self) -> &str {
        "📖"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to the file to read."},
                "offset": {"type": "integer", "description": "1-based line to start at.", "default": 1},
                "limit": {"type": "integer", "description": "Max lines to read.", "default": 500}
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let Some(path) = arg_str(&args, "path") else {
            return tool_error("missing required parameter: path");
        };
        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1).max(1) as usize;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(500)
            .min(truncate::DEFAULT_MAX_LINES as u64) as usize;

        let resolved = ctx.resolve_path(&path);
        let text = match std::fs::read_to_string(&resolved) {
            Ok(t) => t,
            Err(e) => return tool_error(format!("cannot read {}: {}", resolved.display(), e)),
        };

        let lines: Vec<&str> = text.lines().collect();
        let total = lines.len();
        let start = offset - 1;
        if start >= total && total > 0 {
            return tool_error(format!("offset {} is past end of file ({} lines)", offset, total));
        }
        let end = (start + limit).min(total);
        let mut out = String::new();
        for (i, line) in lines[start..end].iter().enumerate() {
            let n = start + i + 1;
            let clamped = if line.chars().count() > truncate::DEFAULT_MAX_LINE_LENGTH {
                let c: String = line.chars().take(truncate::DEFAULT_MAX_LINE_LENGTH).collect();
                format!("{}… [truncated]", c)
            } else {
                (*line).to_string()
            };
            out.push_str(&format!("{:>6}|{}\n", n, clamped));
        }
        if end < total {
            out.push_str(&format!(
                "\n[showing lines {}-{} of {}; next_offset={}]",
                offset, end, total, end + 1
            ));
        }
        ToolResult::Text(joey_core::redact::redact_secrets(&out))
    }
}

// ── write_file ───────────────────────────────────────────────────────────────

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
        "Create or overwrite a file with the given content. Creates parent directories."
    }
    fn emoji(&self) -> &str {
        "✍️"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["path", "content"]
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let (Some(path), Some(content)) = (arg_str(&args, "path"), arg_str(&args, "content")) else {
            return tool_error("missing required parameters: path, content");
        };
        let resolved = ctx.resolve_path(&path);
        if let Some(parent) = resolved.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return tool_error(format!("cannot create {}: {}", parent.display(), e));
            }
        }
        match joey_core::utils::atomic_replace(&resolved, content.as_bytes()) {
            Ok(()) => {
                let bytes = content.len();
                let lines = content.lines().count();
                ToolResult::Text(format!(
                    "Wrote {} ({} bytes, {} lines).",
                    resolved.display(),
                    bytes,
                    lines
                ))
            }
            Err(e) => tool_error(format!("write failed: {}", e)),
        }
    }
}

// ── patch ────────────────────────────────────────────────────────────────────

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
        "Targeted find/replace in a file. Matches `old_string` (with fuzzy fallback) \
         and replaces it with `new_string`. Set replace_all to replace every occurrence."
    }
    fn emoji(&self) -> &str {
        "🔧"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "old_string": {"type": "string", "description": "Text to find (include enough context to be unique)."},
                "new_string": {"type": "string", "description": "Replacement text."},
                "replace_all": {"type": "boolean", "default": false}
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let (Some(path), Some(old), Some(new)) = (
            arg_str(&args, "path"),
            arg_str(&args, "old_string"),
            arg_str(&args, "new_string"),
        ) else {
            return tool_error("missing required parameters: path, old_string, new_string");
        };
        let replace_all = args.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);

        let resolved = ctx.resolve_path(&path);
        let content = match std::fs::read_to_string(&resolved) {
            Ok(c) => c,
            Err(e) => return tool_error(format!("cannot read {}: {}", resolved.display(), e)),
        };

        match fuzzy::find_and_replace(&content, &old, &new, replace_all) {
            Ok(result) => {
                if let Err(e) = joey_core::utils::atomic_replace(&resolved, result.new_content.as_bytes()) {
                    return tool_error(format!("write failed: {}", e));
                }
                ToolResult::Text(format!(
                    "Patched {} ({} replacement{}, strategy: {}).",
                    resolved.display(),
                    result.match_count,
                    if result.match_count == 1 { "" } else { "s" },
                    result.strategy
                ))
            }
            Err(e) => tool_error(e),
        }
    }
}

// ── search_files ─────────────────────────────────────────────────────────────

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
        "Search file contents (regex) or find files by glob. `target=content` greps; \
         `target=files` lists matching paths."
    }
    fn emoji(&self) -> &str {
        "🔎"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Regex (content) or glob (files)."},
                "target": {"type": "string", "enum": ["content", "files"], "default": "content"},
                "path": {"type": "string", "default": "."},
                "file_glob": {"type": "string", "description": "Restrict content search to matching files."},
                "limit": {"type": "integer", "default": 50}
            },
            "required": ["pattern"]
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let Some(pattern) = arg_str(&args, "pattern") else {
            return tool_error("missing required parameter: pattern");
        };
        let target = arg_str(&args, "target").unwrap_or_else(|| "content".to_string());
        let root = ctx.resolve_path(&arg_str(&args, "path").unwrap_or_else(|| ".".to_string()));
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
        let file_glob = arg_str(&args, "file_glob");

        // Prefer ripgrep when available (fast, gitignore-aware); else walk.
        let result = if target == "files" {
            search_files_by_name(&root, &pattern, limit)
        } else {
            search_content(&root, &pattern, file_glob.as_deref(), limit)
        };
        match result {
            Ok(out) if out.is_empty() => ToolResult::Text("No matches.".to_string()),
            Ok(out) => ToolResult::Text(truncate::bounded_head_tail(&out, truncate::DEFAULT_MAX_BYTES)),
            Err(e) => tool_error(e),
        }
    }
}

fn search_content(
    root: &std::path::Path,
    pattern: &str,
    file_glob: Option<&str>,
    limit: usize,
) -> Result<String, String> {
    let re = regex::Regex::new(pattern).map_err(|e| format!("invalid regex: {}", e))?;
    let glob = file_glob
        .map(|g| globset::Glob::new(g).map(|gg| gg.compile_matcher()))
        .transpose()
        .map_err(|e| format!("invalid file_glob: {}", e))?;

    let mut out = String::new();
    let mut hits = 0;
    let walker = ignore::WalkBuilder::new(root).build();
    for entry in walker.flatten() {
        if hits >= limit {
            break;
        }
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if let Some(g) = &glob {
            if !g.is_match(path) {
                continue;
            }
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for (i, line) in content.lines().enumerate() {
            if re.is_match(line) {
                out.push_str(&format!("{}:{}:{}\n", path.display(), i + 1, line.trim_end()));
                hits += 1;
                if hits >= limit {
                    break;
                }
            }
        }
    }
    Ok(out)
}

fn search_files_by_name(root: &std::path::Path, pattern: &str, limit: usize) -> Result<String, String> {
    let matcher = globset::Glob::new(pattern)
        .map_err(|e| format!("invalid glob: {}", e))?
        .compile_matcher();
    let mut out = String::new();
    let mut hits = 0;
    let walker = ignore::WalkBuilder::new(root).build();
    for entry in walker.flatten() {
        if hits >= limit {
            break;
        }
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if matcher.is_match(path) || matcher.is_match(name) {
            out.push_str(&format!("{}\n", path.display()));
            hits += 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use joey_core::Config;

    fn ctx_in(dir: &std::path::Path) -> ToolContext {
        ToolContext::new(dir.to_path_buf(), Config::defaults(), "test")
    }

    #[tokio::test]
    async fn write_then_read() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        let w = WriteFile
            .execute(json!({"path": "a.txt", "content": "line1\nline2\n"}), &ctx)
            .await;
        assert!(!w.is_error());
        let r = ReadFile.execute(json!({"path": "a.txt"}), &ctx).await;
        let s = r.to_content_string();
        assert!(s.contains("1|line1"));
        assert!(s.contains("2|line2"));
    }

    #[tokio::test]
    async fn patch_edits_file() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_in(dir.path());
        WriteFile
            .execute(json!({"path": "b.txt", "content": "hello world\n"}), &ctx)
            .await;
        let p = Patch
            .execute(
                json!({"path": "b.txt", "old_string": "world", "new_string": "joey"}),
                &ctx,
            )
            .await;
        assert!(!p.is_error());
        let content = std::fs::read_to_string(dir.path().join("b.txt")).unwrap();
        assert_eq!(content, "hello joey\n");
    }
}

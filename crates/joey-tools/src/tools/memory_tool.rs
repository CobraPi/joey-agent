//! Memory tool: persistent notes (MEMORY.md) + user profile (USER.md)
//! (port of `tools/memory_tool.py`). Entries joined by the `§` delimiter.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::context::ToolContext;
use crate::registry::{tool_error, Tool, ToolResult};

/// Entry delimiter (kept identical to upstream for file interop).
const ENTRY_DELIMITER: &str = "\n§\n";
const DEFAULT_MEMORY_LIMIT: usize = 2200;
const DEFAULT_USER_LIMIT: usize = 1375;

fn memories_dir() -> PathBuf {
    joey_core::constants::joey_home().join("memories")
}

fn target_path(target: &str) -> PathBuf {
    let name = if target == "user" { "USER.md" } else { "MEMORY.md" };
    memories_dir().join(name)
}

fn read_entries(path: &PathBuf) -> Vec<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| {
            s.split(ENTRY_DELIMITER)
                .map(|e| e.trim().to_string())
                .filter(|e| !e.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn write_entries(path: &PathBuf, entries: &[String]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = entries.join(ENTRY_DELIMITER);
    joey_core::utils::atomic_replace(path, body.as_bytes())?;
    Ok(())
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
        "Persist notes across sessions. target=memory for agent notes, target=user for \
         a durable user profile. Actions: add, replace, remove."
    }
    fn emoji(&self) -> &str {
        "🧠"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "target": {"type": "string", "enum": ["memory", "user"]},
                "action": {"type": "string", "enum": ["add", "replace", "remove"], "default": "add"},
                "content": {"type": "string", "description": "Entry text (for add/replace)."},
                "old_text": {"type": "string", "description": "Existing entry text to replace/remove."}
            },
            "required": ["target"]
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let target = args.get("target").and_then(|v| v.as_str()).unwrap_or("memory");
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("add");
        let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let old_text = args.get("old_text").and_then(|v| v.as_str()).unwrap_or("");

        let limit = if target == "user" {
            ctx.config().get_i64("memory.user_char_limit", DEFAULT_USER_LIMIT as i64) as usize
        } else {
            ctx.config().get_i64("memory.memory_char_limit", DEFAULT_MEMORY_LIMIT as i64) as usize
        };

        let path = target_path(target);
        let mut entries = read_entries(&path);

        match action {
            "add" => {
                if content.trim().is_empty() {
                    return tool_error("add requires content");
                }
                entries.push(content.trim().to_string());
            }
            "replace" => {
                if content.trim().is_empty() || old_text.trim().is_empty() {
                    return tool_error("replace requires content and old_text");
                }
                let Some(idx) = entries.iter().position(|e| e.contains(old_text.trim())) else {
                    return tool_error("old_text not found in memory");
                };
                entries[idx] = content.trim().to_string();
            }
            "remove" => {
                if old_text.trim().is_empty() {
                    return tool_error("remove requires old_text");
                }
                let before = entries.len();
                entries.retain(|e| !e.contains(old_text.trim()));
                if entries.len() == before {
                    return tool_error("old_text not found in memory");
                }
            }
            other => return tool_error(format!("unknown action: {}", other)),
        }

        let total_chars: usize = entries.iter().map(|e| e.chars().count()).sum::<usize>()
            + entries.len().saturating_sub(1) * ENTRY_DELIMITER.chars().count();
        if total_chars > limit {
            return tool_error(format!(
                "memory would exceed {} char limit ({} chars). Remove or condense entries first.",
                limit, total_chars
            ));
        }

        if let Err(e) = write_entries(&path, &entries) {
            return tool_error(format!("failed to write memory: {}", e));
        }
        ToolResult::Text(format!(
            "{} updated ({} entries, {} chars).",
            if target == "user" { "USER.md" } else { "MEMORY.md" },
            entries.len(),
            total_chars
        ))
    }
}

//! Live session context-window breakdown for UI surfaces
//! (port of `agent/context_breakdown.py`).
//!
//! Estimates how the next provider request is composed: system prompt,
//! tool schemas, and conversation history — same char/4 heuristic as
//! `estimate_request_tokens_rough` so numbers align with compression
//! thresholds. Upstream surfaces this through the gateway/TUI `/usage`
//! payload; the port exposes the same JSON shape for its CLI surfaces.

use joey_providers::{Message, ToolSchema};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};

use super::compressor::ContextCompressor;
use super::estimator::estimate_messages_tokens_rough;

static SKILLS_BLOCK_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)<available_skills>.*?</available_skills>").unwrap());

const SUBAGENT_TOOL_NAMES: &[&str] = &["delegate_task"];

/// context_breakdown.py `_CATEGORY_COLORS`.
fn category_color(id: &str) -> &'static str {
    match id {
        "system_prompt" => "var(--context-usage-system)",
        "tool_definitions" => "var(--context-usage-tools)",
        "rules" => "var(--context-usage-rules)",
        "skills" => "var(--context-usage-skills)",
        "mcp" => "var(--context-usage-mcp)",
        "subagent_definitions" => "var(--context-usage-subagents)",
        "memory" => "var(--context-usage-memory)",
        "conversation" => "var(--context-usage-conversation)",
        _ => "var(--ui-text-tertiary)",
    }
}

fn chars_to_tokens(text: &str) -> i64 {
    if text.is_empty() {
        return 0;
    }
    text.len().div_ceil(4) as i64
}

fn json_tokens(value: &[&ToolSchema]) -> i64 {
    if value.is_empty() {
        return 0;
    }
    serde_json::to_string(value).map(|s| chars_to_tokens(&s)).unwrap_or(0)
}

fn split_tools(tools: &[ToolSchema]) -> (Vec<&ToolSchema>, Vec<&ToolSchema>, Vec<&ToolSchema>) {
    let mut builtin = Vec::new();
    let mut mcp = Vec::new();
    let mut subagent = Vec::new();
    for tool in tools {
        let name = tool.function.name.as_str();
        if name.starts_with("mcp_") {
            mcp.push(tool);
        } else if SUBAGENT_TOOL_NAMES.contains(&name) {
            subagent.push(tool);
        } else {
            builtin.push(tool);
        }
    }
    (builtin, mcp, subagent)
}

/// Return a Cursor-style context usage breakdown for one live agent
/// (`compute_session_context_breakdown`). The port derives the tiers from
/// the assembled system prompt: the `<available_skills>` index and memory
/// blocks are carved out; everything else is "System prompt". (The upstream
/// stable/context/volatile tier split needs prompt-part plumbing the port's
/// single-string prompt does not keep.)
pub fn compute_session_context_breakdown(
    system_prompt: &str,
    memory_blocks: &[String],
    tools: &[ToolSchema],
    messages: &[Message],
    compressor: Option<&ContextCompressor>,
    model: &str,
) -> Value {
    let skills_index = SKILLS_BLOCK_RE
        .find(system_prompt)
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    let memory_text = memory_blocks
        .iter()
        .filter(|b| !b.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n\n")
        .trim()
        .to_string();

    let mut system_prompt_text = system_prompt.to_string();
    if !skills_index.is_empty() {
        system_prompt_text = system_prompt_text.replace(&skills_index, "");
    }
    for block in memory_blocks {
        if !block.is_empty() {
            system_prompt_text = system_prompt_text.replace(block, "");
        }
    }
    let system_prompt_text = system_prompt_text.trim().to_string();

    let (builtin_tools, mcp_tools, subagent_tools) = split_tools(tools);
    let conversation_tokens = estimate_messages_tokens_rough(messages);

    let categories: Vec<(&str, &str, i64)> = vec![
        ("system_prompt", "System prompt", chars_to_tokens(&system_prompt_text)),
        ("tool_definitions", "Tool definitions", json_tokens(&builtin_tools)),
        ("skills", "Skills", chars_to_tokens(&skills_index)),
        ("mcp", "MCP", json_tokens(&mcp_tools)),
        ("subagent_definitions", "Subagent definitions", json_tokens(&subagent_tools)),
        ("memory", "Memory", chars_to_tokens(&memory_text)),
        ("conversation", "Conversation", conversation_tokens),
    ];

    let estimated_total: i64 = categories.iter().map(|(_, _, t)| t).sum();

    let context_max = compressor.map(|c| c.context_length).unwrap_or(0);
    let measured_used = compressor.map(|c| c.last_prompt_tokens.max(0)).unwrap_or(0);
    let context_used = if measured_used > 0 { measured_used } else { estimated_total };
    let context_percent = if context_max > 0 {
        ((context_used as f64 / context_max as f64 * 100.0).round() as i64).clamp(0, 100)
    } else {
        0
    };

    json!({
        "categories": categories
            .iter()
            .filter(|(_, _, tokens)| *tokens > 0)
            .map(|(id, label, tokens)| json!({
                "color": category_color(id),
                "id": id,
                "label": label,
                "tokens": tokens,
            }))
            .collect::<Vec<_>>(),
        "context_max": context_max,
        "context_percent": context_percent,
        "context_used": context_used,
        "estimated_total": estimated_total,
        "model": model,
    })
}

//! `joey-tools` — the tool system for joey-agent.
//!
//! Port of `tools/` and `toolsets.py`. Provides the [`Tool`] trait, the
//! [`ToolRegistry`], the toolset resolver, schema sanitization, output
//! truncation + result persistence, the fuzzy matcher behind `patch`, the V4A
//! patch parser, SSRF/file-safety guards, and the self-contained built-in
//! tools (file, terminal, todo, memory, web, skills).

pub mod builtins;
pub mod context;
pub mod difflib;
pub mod fuzzy;
pub mod guards;
pub mod patch_parser;
pub mod pyjson;
pub mod registry;
pub mod sanitize;
pub mod storage;
pub mod toolsets;
pub mod tools;
pub mod truncate;
pub mod url_safety;
pub mod vcs;

pub use context::{SessionState, ToolContext, TurnBudget};
pub use registry::{sanitize_tool_error, tool_error, Tool, ToolRegistry, ToolResult};
pub use toolsets::{resolve as resolve_toolset, resolve_multiple as resolve_toolsets};

/// Serializes tests that mutate process-global state (the joey-home override
/// and environment variables). Test-only.
#[cfg(test)]
pub(crate) fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

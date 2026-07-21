//! `joey-tools` — the tool system for joey-agent.
//!
//! Port of `tools/` and `toolsets.py`. Provides the [`Tool`] trait, the
//! [`ToolRegistry`], the toolset resolver, schema sanitization, output
//! truncation, the fuzzy matcher behind `patch`, and the self-contained
//! built-in tools (file, terminal, todo, memory, web, skills).

pub mod builtins;
pub mod context;
pub mod fuzzy;
pub mod registry;
pub mod sanitize;
pub mod toolsets;
pub mod tools;
pub mod truncate;

pub use context::ToolContext;
pub use registry::{tool_error, Tool, ToolRegistry, ToolResult};
pub use toolsets::{resolve as resolve_toolset, resolve_multiple as resolve_toolsets};

//! Tool registry and the `Tool` trait (port of `tools/registry.py`).
//!
//! Unlike the Python version — which discovers tools via import side effects —
//! the Rust port registers tools explicitly (no runtime reflection). A tool
//! declares its schema + toolset + an async `execute`; the registry filters by
//! enabled toolset and `check` gating, and produces the OpenAI tool-schema list.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::context::ToolContext;

/// The result of a tool invocation. Handlers return only text or a multimodal
/// envelope; the dispatcher enforces this contract (upstream invariant).
#[derive(Debug, Clone)]
pub enum ToolResult {
    Text(String),
    /// Multimodal content (text + image parts), serialized for the transcript.
    Multimodal(Vec<Value>),
    Error(String),
}

impl ToolResult {
    /// Render the result as the string the model sees in the tool message.
    pub fn to_content_string(&self) -> String {
        match self {
            ToolResult::Text(s) => s.clone(),
            ToolResult::Error(e) => json!({"error": e}).to_string(),
            ToolResult::Multimodal(parts) => {
                // Flatten text parts; image parts are referenced by marker.
                let mut out = String::new();
                for p in parts {
                    match p.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                                out.push_str(t);
                                out.push('\n');
                            }
                        }
                        Some("image_url") => out.push_str("[image]\n"),
                        _ => {}
                    }
                }
                out
            }
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, ToolResult::Error(_))
    }
}

/// Build a JSON error string in the upstream `tool_error` shape.
pub fn tool_error(message: impl Into<String>) -> ToolResult {
    ToolResult::Error(message.into())
}

/// An agent-callable tool.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Wire name exposed to the model (e.g. `read_file`).
    fn name(&self) -> &str;
    /// The toolset this tool belongs to (e.g. `file`).
    fn toolset(&self) -> &str;
    /// One-line description for the tool schema.
    fn description(&self) -> &str;
    /// JSON Schema of the parameters object.
    fn parameters(&self) -> Value;
    /// Optional emoji for progress display.
    fn emoji(&self) -> &str {
        ""
    }
    /// Per-tool max result size in chars before persistence kicks in.
    fn max_result_chars(&self) -> Option<usize> {
        Some(100_000)
    }
    /// Runtime availability gate (env/binaries present). Default: always on.
    fn check(&self, _ctx: &ToolContext) -> bool {
        true
    }
    /// Execute the tool.
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult;
}

/// A registry of tools keyed by name.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool, replacing any prior tool of the same name.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Register every built-in tool.
    pub fn with_builtins() -> Self {
        let mut r = Self::new();
        crate::builtins::register_all(&mut r);
        r
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// The OpenAI tool-schema list for the given set of enabled tool names,
    /// filtered by each tool's `check` gate. Schemas are sanitized.
    pub fn definitions(&self, enabled: &[String], ctx: &ToolContext) -> Vec<Value> {
        let enabled_set: std::collections::HashSet<&str> =
            enabled.iter().map(String::as_str).collect();
        let mut defs = Vec::new();
        for (name, tool) in &self.tools {
            if !enabled_set.contains(name.as_str()) {
                continue;
            }
            if !tool.check(ctx) {
                continue;
            }
            let schema = crate::sanitize::sanitize_parameters(tool.parameters());
            defs.push(json!({
                "type": "function",
                "function": {
                    "name": tool.name(),
                    "description": tool.description(),
                    "parameters": schema,
                }
            }));
        }
        defs
    }

    /// Dispatch a tool call by name, enforcing the result contract.
    pub async fn dispatch(&self, name: &str, args: Value, ctx: &ToolContext) -> ToolResult {
        match self.get(name) {
            Some(tool) => tool.execute(args, ctx).await,
            None => tool_error(format!("unknown tool: {}", name)),
        }
    }
}

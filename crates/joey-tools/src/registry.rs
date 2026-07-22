//! Tool registry and the `Tool` trait (port of `tools/registry.py` +
//! the dispatch-side pieces of `model_tools.py`).
//!
//! Unlike the Python version — which discovers tools via import side effects —
//! the Rust port registers tools explicitly. The registry:
//! * TTL-caches `check` results for ~30s with a 60s last-good grace window
//!   (registry.py:143-206) so flaky availability probes can't strip tools;
//! * returns upstream's dispatch error envelopes (`Unknown tool: X`,
//!   `[TOOL_ERROR] Tool execution failed: ...` with framing-token stripping);
//! * runs layer-2/layer-3 result persistence on oversized outputs.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};

use crate::context::ToolContext;
use crate::pyjson;

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
            ToolResult::Error(e) => pyjson::dumps(&json!({ "error": e })),
            ToolResult::Multimodal(parts) => {
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

/// Build a JSON error result in the upstream `tool_error` shape
/// (`{"error": "..."}` with Python separators).
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
    /// Description for the tool schema.
    fn description(&self) -> &str;
    /// JSON Schema of the parameters object.
    fn parameters(&self) -> Value;
    /// Optional emoji for progress display.
    fn emoji(&self) -> &str {
        ""
    }
    /// Per-tool max result size in chars before persistence kicks in.
    fn max_result_chars(&self) -> Option<usize> {
        Some(crate::storage::DEFAULT_RESULT_SIZE_CHARS)
    }
    /// Runtime availability gate (env/binaries present). Default: always on.
    fn check(&self, _ctx: &ToolContext) -> bool {
        true
    }
    /// Execute the tool.
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult;
}

// ---------------------------------------------------------------------------
// check() TTL cache (registry.py:143-206)
// ---------------------------------------------------------------------------

const CHECK_TTL_SECONDS: f64 = 30.0;
const CHECK_FAILURE_GRACE_SECONDS: f64 = 60.0;

#[derive(Default)]
struct CheckCache {
    /// tool name → (timestamp, value)
    cached: std::collections::HashMap<String, (Instant, bool)>,
    /// tool name → last time check returned true
    last_good: std::collections::HashMap<String, Instant>,
}

static CHECK_CACHE: Lazy<Mutex<CheckCache>> = Lazy::new(|| Mutex::new(CheckCache::default()));

/// Drop all cached check results — call after config changes that affect
/// tool availability.
pub fn invalidate_check_cache() {
    let mut cache = CHECK_CACHE.lock().unwrap();
    cache.cached.clear();
    cache.last_good.clear();
}

fn check_cached(tool: &dyn Tool, ctx: &ToolContext) -> bool {
    let name = tool.name().to_string();
    let now = Instant::now();
    {
        let cache = CHECK_CACHE.lock().unwrap();
        if let Some((ts, value)) = cache.cached.get(&name) {
            if now.duration_since(*ts).as_secs_f64() < CHECK_TTL_SECONDS {
                return *value;
            }
        }
    }
    // Probe outside the lock; panics count as failure.
    let value = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tool.check(ctx)))
        .unwrap_or(false);
    let mut cache = CHECK_CACHE.lock().unwrap();
    if value {
        cache.last_good.insert(name.clone(), now);
        cache.cached.insert(name, (now, true));
        return true;
    }
    if let Some(last_good) = cache.last_good.get(&name) {
        if now.duration_since(*last_good).as_secs_f64() < CHECK_FAILURE_GRACE_SECONDS {
            // Recent success → treat this failure as a flake. Serve last-good
            // True and do NOT cache the failure so the next call re-probes.
            tracing::warn!(
                "check for {} failed within {:.0}s of last success; treating as transient and keeping tool(s) available",
                name,
                CHECK_FAILURE_GRACE_SECONDS
            );
            return true;
        }
    }
    tracing::warn!("check for {} failed; dependent tools will be unavailable this turn", name);
    cache.cached.insert(name, (now, false));
    false
}

// ---------------------------------------------------------------------------
// Tool error sanitization (model_tools._sanitize_tool_error)
// ---------------------------------------------------------------------------

static TOOL_ERROR_ROLE_TAG_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)</?(?:tool_call|function_call|result|response|output|input|system|assistant|user)>",
    )
    .unwrap()
});
static TOOL_ERROR_FENCE_OPEN_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*```(?:json|xml|html|markdown)?\s*").unwrap());
static TOOL_ERROR_FENCE_CLOSE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)\s*```\s*$").unwrap());
static TOOL_ERROR_CDATA_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)<!\[CDATA\[.*?\]\]>").unwrap());
const TOOL_ERROR_MAX_LEN: usize = 2000;

/// Port of `_sanitize_tool_error` — strip structural framing tokens and cap
/// length before an error string reaches the model.
pub fn sanitize_tool_error(error_msg: &str) -> String {
    if error_msg.is_empty() {
        return "[TOOL_ERROR] ".to_string();
    }
    let sanitized = TOOL_ERROR_ROLE_TAG_RE.replace_all(error_msg, "");
    let sanitized = TOOL_ERROR_FENCE_OPEN_RE.replace_all(&sanitized, "");
    let sanitized = TOOL_ERROR_FENCE_CLOSE_RE.replace_all(&sanitized, "");
    let mut sanitized = TOOL_ERROR_CDATA_RE.replace_all(&sanitized, "").into_owned();
    if sanitized.len() > TOOL_ERROR_MAX_LEN {
        let cut = crate::truncate::floor_char_boundary(&sanitized, TOOL_ERROR_MAX_LEN - 3);
        sanitized = format!("{}...", &sanitized[..cut]);
    }
    format!("[TOOL_ERROR] {}", sanitized)
}

/// Tools whose runs must NOT reset the consecutive read/search loop counter.
const READ_SEARCH_TOOLS: &[&str] = &["read_file", "search_files"];

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

    /// The emoji for a tool, defaulting to "⚡" when unset (registry.py:678-681).
    pub fn get_emoji(&self, name: &str) -> String {
        match self.get(name) {
            Some(t) if !t.emoji().is_empty() => t.emoji().to_string(),
            _ => "⚡".to_string(),
        }
    }

    /// Per-tool max result size, resolved through the pinned/registry/default
    /// chain (`budget_config.resolve_threshold`). `None` means unlimited.
    pub fn get_max_result_size(&self, name: &str) -> Option<usize> {
        let registered = self.get(name).and_then(|t| t.max_result_chars());
        crate::storage::resolve_threshold(name, registered)
    }

    /// The OpenAI tool-schema list for the given set of enabled tool names,
    /// filtered by each tool's TTL-cached `check` gate. Schemas are sanitized.
    pub fn definitions(&self, enabled: &[String], ctx: &ToolContext) -> Vec<Value> {
        let enabled_set: std::collections::HashSet<&str> =
            enabled.iter().map(String::as_str).collect();
        let mut defs = Vec::new();
        for (name, tool) in &self.tools {
            if !enabled_set.contains(name.as_str()) {
                continue;
            }
            if !check_cached(tool.as_ref(), ctx) {
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

    /// Dispatch a tool call by name, enforcing the result contract, error
    /// envelope, and output persistence. A random result id is generated for
    /// the persistence filename; use [`ToolRegistry::dispatch_call`] to supply
    /// the provider's tool_call id.
    pub async fn dispatch(&self, name: &str, args: Value, ctx: &ToolContext) -> ToolResult {
        let id = uuid::Uuid::new_v4().to_string();
        self.dispatch_call(name, args, ctx, &id).await
    }

    /// Dispatch with an explicit tool_use id (used to name persisted-output files).
    pub async fn dispatch_call(
        &self,
        name: &str,
        args: Value,
        ctx: &ToolContext,
        tool_use_id: &str,
    ) -> ToolResult {
        let Some(tool) = self.get(name) else {
            // registry.py:625 — {"error": "Unknown tool: X"} (capital U).
            return ToolResult::Error(format!("Unknown tool: {}", name));
        };

        // notify_other_tool_call: any tool other than read_file/search_files
        // breaks the consecutive read/search loop counters.
        if !READ_SEARCH_TOOLS.contains(&name) {
            ctx.state().note_other_tool();
        }

        let fut = tool.execute(args, ctx);
        let result = match futures_util::FutureExt::catch_unwind(
            std::panic::AssertUnwindSafe(fut),
        )
        .await
        {
            Ok(r) => r,
            Err(panic) => {
                let msg = panic_message(&panic);
                let raw = format!("Tool execution failed: Panic: {}", msg);
                return ToolResult::Error(sanitize_tool_error(&raw));
            }
        };

        // Layer 2 + 3 persistence on the rendered content.
        let content = result.to_content_string();
        let threshold = self.get_max_result_size(name);
        let mut persisted = crate::storage::maybe_persist_tool_result(
            &content, name, tool_use_id, threshold,
        );
        // Per-turn aggregate budget: spill anything (persistable) once the
        // turn total exceeds the budget.
        let budget = ctx.turn_budget();
        if threshold.is_some()
            && budget.would_exceed(persisted.len())
            && !persisted.contains(crate::storage::PERSISTED_OUTPUT_TAG)
        {
            persisted = crate::storage::maybe_persist_tool_result(
                &persisted,
                "__budget_enforcement__",
                tool_use_id,
                Some(0),
            );
        }
        budget.add(persisted.len());

        if persisted == content {
            result
        } else {
            ToolResult::Text(persisted)
        }
    }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Boom;
    #[async_trait]
    impl Tool for Boom {
        fn name(&self) -> &str {
            "boom"
        }
        fn toolset(&self) -> &str {
            "test"
        }
        fn description(&self) -> &str {
            "panics"
        }
        fn parameters(&self) -> Value {
            json!({"type": "object", "properties": {}})
        }
        async fn execute(&self, _args: Value, _ctx: &ToolContext) -> ToolResult {
            panic!("kaboom </tool_call>\n```json\nfence");
        }
    }

    fn ctx() -> ToolContext {
        ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "t")
    }

    #[tokio::test]
    async fn unknown_tool_envelope() {
        let r = ToolRegistry::new().dispatch("nope", json!({}), &ctx()).await;
        assert_eq!(r.to_content_string(), r#"{"error": "Unknown tool: nope"}"#);
    }

    #[tokio::test]
    async fn panic_becomes_sanitized_tool_error() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(Boom));
        let r = reg.dispatch("boom", json!({}), &ctx()).await;
        let s = r.to_content_string();
        assert!(s.starts_with(r#"{"error": "[TOOL_ERROR] Tool execution failed: Panic: "#), "{}", s);
        assert!(!s.contains("</tool_call>"));
        assert!(!s.contains("```"));
    }

    #[test]
    fn sanitizer_strips_and_caps() {
        let msg = format!("<system>x</system> {} <![CDATA[secret]]>", "y".repeat(3000));
        let out = sanitize_tool_error(&msg);
        assert!(out.starts_with("[TOOL_ERROR] "));
        assert!(!out.contains("<system>"));
        assert!(!out.contains("CDATA"));
        assert!(out.len() <= TOOL_ERROR_MAX_LEN + "[TOOL_ERROR] ".len());
        assert!(out.ends_with("..."));
    }

    #[test]
    fn default_emoji() {
        let reg = ToolRegistry::new();
        assert_eq!(reg.get_emoji("missing"), "⚡");
    }
}

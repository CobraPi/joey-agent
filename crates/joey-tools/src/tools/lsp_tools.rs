//! LSP-backed tools (diagnostics, definition, references, symbols, rename).
//!
//! These tools require an LSP manager to be registered via the tool context.
//! When no manager is available, the tools' `check()` returns false and they
//! are hidden from the model's tool list.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::lsp::{DiagnosticCounts, LspManager};
use crate::registry::{Tool, ToolResult};
use crate::context::ToolContext;

/// Global LSP manager (registered by the CLI at startup).
static LSP_MANAGER: std::sync::OnceLock<Arc<Mutex<LspManager>>> = std::sync::OnceLock::new();

/// Register the global LSP manager.
pub fn register_lsp_manager(mgr: LspManager) {
    let _ = LSP_MANAGER.set(Arc::new(Mutex::new(mgr)));
}

/// Whether an LSP manager is registered and has configured servers.
pub fn lsp_available() -> bool {
    LSP_MANAGER
        .get()
        .map(|m| m.lock().map(|mgr| mgr.has_servers()).unwrap_or(false))
        .unwrap_or(false)
}

/// Run a blocking LSP manager operation on the blocking thread pool.
///
/// LSP calls do synchronous subprocess I/O (and `diagnostics` includes a
/// 500ms settle sleep) while holding the global manager mutex. Running that
/// directly inside `async fn execute` stalls a tokio worker thread and —
/// under parallel tool dispatch — starves the whole runtime (UI freezes).
/// `spawn_blocking` keeps the async workers free; the mutex still serializes
/// LSP servers themselves, which are single-threaded by protocol design.
async fn with_manager<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&mut LspManager) -> Result<T, crate::lsp::LspError> + Send + 'static,
{
    let mgr = match LSP_MANAGER.get() {
        Some(m) => m.clone(),
        None => return Err("No LSP manager registered".into()),
    };
    tokio::task::spawn_blocking(move || {
        let mut guard = mgr.lock().map_err(|e| format!("LSP lock error: {}", e))?;
        f(&mut guard).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("LSP task failed: {}", e))?
}

/// Helper to format diagnostics for the tool result.
fn format_diagnostics(diags: &[crate::lsp::Diagnostic]) -> Value {
    let counts = diags.iter().fold(
        DiagnosticCounts::default(),
        |mut acc, d| {
            match d.severity.as_str() {
                "error" => acc.errors += 1,
                "warning" => acc.warnings += 1,
                "info" => acc.info += 1,
                "hint" => acc.hints += 1,
                _ => acc.errors += 1,
            }
            acc
        },
    );
    let lines: Vec<String> = diags
        .iter()
        .map(|d| {
            format!(
                "  {}:{} [{}] {}{}",
                d.line + 1,
                d.character + 1,
                d.severity,
                d.message,
                d.source.as_ref().map(|s| format!(" ({})", s)).unwrap_or_default()
            )
        })
        .collect();
    json!({
        "diagnostics": diags.iter().map(|d| json!({
            "line": d.line + 1,
            "character": d.character + 1,
            "severity": d.severity,
            "message": d.message,
            "source": d.source,
        })).collect::<Vec<_>>(),
        "count": {
            "errors": counts.errors,
            "warnings": counts.warnings,
            "info": counts.info,
            "hints": counts.hints,
        },
        "formatted": if lines.is_empty() {
            "No diagnostics found.".to_string()
        } else {
            format!("{}\n{}", counts, lines.join("\n"))
        }
    })
}

// ─── lsp_diagnostics ─────────────────────────────────────────────────

pub struct LspDiagnostics;

#[async_trait]
impl Tool for LspDiagnostics {
    fn name(&self) -> &str { "lsp_diagnostics" }
    fn toolset(&self) -> &str { "lsp" }
    fn emoji(&self) -> &str { "🔬" }
    fn description(&self) -> &str {
        "Get LSP diagnostics (errors, warnings) for a file. Requires an LSP server configured for the file type."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File path to check diagnostics for."
                }
            },
            "required": ["path"]
        })
    }
    fn check(&self, _ctx: &ToolContext) -> bool {
        lsp_available()
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or_default();
        let path = path.to_string();
        match with_manager(move |mgr| mgr.diagnostics(&path)).await {
            Ok(diags) => ToolResult::Text(crate::pyjson::dumps(&format_diagnostics(diags.as_slice()))),
            Err(e) => ToolResult::Text(crate::pyjson::dumps(&json!({
                "error": e
            }))),
        }
    }
}

// ─── lsp_definition ──────────────────────────────────────────────────

pub struct LspDefinition;

#[async_trait]
impl Tool for LspDefinition {
    fn name(&self) -> &str { "lsp_definition" }
    fn toolset(&self) -> &str { "lsp" }
    fn emoji(&self) -> &str { "🎯" }
    fn description(&self) -> &str {
        "Go to the definition of the symbol at the given position. Returns file locations."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path" },
                "line": { "type": "integer", "description": "Line number (0-indexed)" },
                "character": { "type": "integer", "description": "Character offset (0-indexed)" }
            },
            "required": ["path", "line", "character"]
        })
    }
    fn check(&self, _ctx: &ToolContext) -> bool { lsp_available() }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let line = args.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let character = args.get("character").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        match with_manager(move |mgr| mgr.definition(&path, line, character)).await {
            Ok(locs) if locs.is_empty() => {
                ToolResult::Text(crate::pyjson::dumps(&json!({
                    "message": "No definitions found"
                })))
            }
            Ok(locs) => {
                let arr: Vec<Value> = locs.iter().map(|l| json!({
                    "file": l.file,
                    "line": l.line + 1,
                    "character": l.character + 1,
                })).collect();
                ToolResult::Text(crate::pyjson::dumps(&json!({ "definitions": arr })))
            }
            Err(e) => ToolResult::Text(crate::pyjson::dumps(&json!({ "error": e }))),
        }
    }
}

// ─── lsp_references ──────────────────────────────────────────────────

pub struct LspReferences;

#[async_trait]
impl Tool for LspReferences {
    fn name(&self) -> &str { "lsp_references" }
    fn toolset(&self) -> &str { "lsp" }
    fn emoji(&self) -> &str { "🔗" }
    fn description(&self) -> &str {
        "Find all references to the symbol at the given position."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path" },
                "line": { "type": "integer", "description": "Line number (0-indexed)" },
                "character": { "type": "integer", "description": "Character offset (0-indexed)" }
            },
            "required": ["path", "line", "character"]
        })
    }
    fn check(&self, _ctx: &ToolContext) -> bool { lsp_available() }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let line = args.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let character = args.get("character").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        match with_manager(move |mgr| mgr.references(&path, line, character)).await {
            Ok(locs) if locs.is_empty() => {
                ToolResult::Text(crate::pyjson::dumps(&json!({ "message": "No references found" })))
            }
            Ok(locs) => {
                let arr: Vec<Value> = locs.iter().map(|l| json!({
                    "file": l.file,
                    "line": l.line + 1,
                    "character": l.character + 1,
                })).collect();
                ToolResult::Text(crate::pyjson::dumps(&json!({ "references": arr, "count": arr.len() })))
            }
            Err(e) => ToolResult::Text(crate::pyjson::dumps(&json!({ "error": e }))),
        }
    }
}

// ─── lsp_symbols ─────────────────────────────────────────────────────

pub struct LspSymbols;

#[async_trait]
impl Tool for LspSymbols {
    fn name(&self) -> &str { "lsp_symbols" }
    fn toolset(&self) -> &str { "lsp" }
    fn emoji(&self) -> &str { "📋" }
    fn description(&self) -> &str {
        "List document symbols (functions, classes, types) in a file."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path" }
            },
            "required": ["path"]
        })
    }
    fn check(&self, _ctx: &ToolContext) -> bool { lsp_available() }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let path = args.get("path").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        match with_manager(move |mgr| mgr.document_symbols(&path)).await {
            Ok(syms) if syms.is_empty() => {
                ToolResult::Text(crate::pyjson::dumps(&json!({ "message": "No symbols found" })))
            }
            Ok(syms) => {
                let arr: Vec<Value> = syms.iter().map(|s| json!({
                    "name": s.name,
                    "kind": s.kind,
                    "line": s.line + 1,
                    "detail": s.detail,
                })).collect();
                ToolResult::Text(crate::pyjson::dumps(&json!({ "symbols": arr, "count": arr.len() })))
            }
            Err(e) => ToolResult::Text(crate::pyjson::dumps(&json!({ "error": e }))),
        }
    }
}

//! NeuroCode tools — structural indexing/query/status/ingest (T047).
//!
//! These tools expose the NeuroCode engine (graph store, classifier, context
//! assembler, knowledge memory) to the model. `joey-tools` cannot depend on
//! `joey-neurocode` directly (DAG constraint — `joey-neurocode` depends on
//! `joey-tools`), so the concrete engine is abstracted behind the
//! [`NeuroCodeBackend`] trait object. Higher crates (`joey-agent-core`,
//! `joey-cli`) construct an `Arc<dyn NeuroCodeBackend>` from their
//! `NeuroCodeEngine` handle and pass it in via [`register_neurocode_tools`].
//!
//! When no backend is supplied, each tool's `check()` returns `false` and the
//! tools are hidden from the model's tool list.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::context::ToolContext;
use crate::registry::{Tool, ToolResult};

/// Abstract backend the NeuroCode tools delegate to.
///
/// Implemented by higher crates over their concrete engine handle
/// (`joey_neurocode::engine::NeuroCodeEngine`). All methods return a
/// pre-formatted string the model sees verbatim in the tool result.
pub trait NeuroCodeBackend: Send + Sync {
    /// Index (or re-index) the project source tree at `path`.
    ///
    /// When `force` is true, the existing structural index is rebuilt from
    /// scratch; otherwise an incremental/index-if-stale strategy is used.
    fn index(&self, path: &str, force: bool) -> String;

    /// Query the structural dependency graph.
    ///
    /// `query_type` selects the query shape (e.g. `dependencies`,
    /// `dependents`, `definition`, `references`), `symbol` is the seed FQCN or
    /// symbol name, and `limit` caps the number of returned results.
    fn query(&self, query_type: &str, symbol: &str, limit: usize) -> String;

    /// Return a status summary of the NeuroCode engine: whether it is active,
    /// the indexed artifact/edge counts, schema version, and last-index time.
    fn status(&self) -> String;

    /// Ingest domain knowledge from `source_path` into the knowledge memory.
    ///
    /// `category` classifies the knowledge (e.g. `pattern`, `antipattern`,
    /// `rule`, `convention`); `version_tag` optionally pins the framework
    /// version the knowledge applies to; `provenance` records where it came
    /// from (URL, file, or human-authored note).
    fn ingest(
        &self,
        category: &str,
        source_path: &str,
        version_tag: Option<&str>,
        provenance: &str,
    ) -> String;

    /// Whether NeuroCode is active for the current session.
    fn is_active(&self) -> bool;
}

/// Shared constructor logic for the four tools: each holds an optional handle
/// to the backend. `None` disables the tool (check → false).
fn backend_active(backend: &Option<Arc<dyn NeuroCodeBackend>>) -> bool {
    backend.is_some()
}

/// Error returned by `execute` when the backend is unavailable (defensive —
/// `check()` TTL cache may briefly serve a stale value).
fn no_backend_error() -> ToolResult {
    ToolResult::Error(
        "NeuroCode is not available: no engine backend is registered.".to_string(),
    )
}

// ─── neurocode_index ─────────────────────────────────────────────────

/// The `neurocode_index` tool — build/refresh the structural dependency graph.
pub struct NeuroCodeIndex {
    backend: Option<Arc<dyn NeuroCodeBackend>>,
}

#[async_trait]
impl Tool for NeuroCodeIndex {
    fn name(&self) -> &str {
        "neurocode_index"
    }

    fn toolset(&self) -> &str {
        "coding"
    }

    fn emoji(&self) -> &str {
        "🧠"
    }

    fn description(&self) -> &str {
        "Build or refresh the NeuroCode structural dependency graph for a project \
         directory. Parses the source tree with tree-sitter, extracts code \
         artifacts (types, methods, fields) and their dependencies, and persists \
         them to the graph store. Use after cloning or significantly changing a \
         project, or when neurocode_query returns stale results. Returns an \
         ingestion summary (files scanned, artifacts indexed, edges created)."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute or relative path to the project root to index."
                },
                "force": {
                    "type": "boolean",
                    "description": "If true, rebuild the index from scratch instead of incrementally updating. Default: false.",
                    "default": false
                }
            },
            "required": ["path"]
        })
    }

    fn check(&self, _ctx: &ToolContext) -> bool {
        backend_active(&self.backend)
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let Some(backend) = &self.backend else {
            return no_backend_error();
        };
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::Error("path is required".to_string()),
        };
        let force = args
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        ToolResult::Text(backend.index(path, force))
    }
}

// ─── neurocode_query ─────────────────────────────────────────────────

/// The `neurocode_query` tool — query the structural dependency graph.
pub struct NeuroCodeQuery {
    backend: Option<Arc<dyn NeuroCodeBackend>>,
}

#[async_trait]
impl Tool for NeuroCodeQuery {
    fn name(&self) -> &str {
        "neurocode_query"
    }

    fn toolset(&self) -> &str {
        "coding"
    }

    fn emoji(&self) -> &str {
        "🔍"
    }

    fn description(&self) -> &str {
        "Query the NeuroCode structural dependency graph for a symbol or FQCN. \
         Returns code artifacts and their relationships. Query types include \
         `dependencies` (what this symbol depends on), `dependents` (what \
         depends on this symbol), `definition` (where the symbol is declared), \
         and `references` (where it is used). Prefer this over grepping when you \
         need structural/semantic relationships rather than textual matches. \
         Requires the project to be indexed first (neurocode_index)."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query_type": {
                    "type": "string",
                    "description": "The kind of query: `dependencies`, `dependents`, `definition`, or `references`.",
                    "enum": ["dependencies", "dependents", "definition", "references"]
                },
                "symbol": {
                    "type": "string",
                    "description": "The seed symbol to query — an FQCN (e.g. `com.example.Foo.bar`) or a simple symbol name."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return. Default: 20.",
                    "default": 20,
                    "minimum": 1
                }
            },
            "required": ["query_type", "symbol"]
        })
    }

    fn check(&self, _ctx: &ToolContext) -> bool {
        backend_active(&self.backend)
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let Some(backend) = &self.backend else {
            return no_backend_error();
        };
        let query_type = match args.get("query_type").and_then(|v| v.as_str()) {
            Some(q) => q,
            None => return ToolResult::Error("query_type is required".to_string()),
        };
        let symbol = match args.get("symbol").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::Error("symbol is required".to_string()),
        };
        let limit = args
            .get("limit")
            .and_then(|v| v.as_i64())
            .unwrap_or(20)
            .max(1) as usize;
        ToolResult::Text(backend.query(query_type, symbol, limit))
    }
}

// ─── neurocode_status ────────────────────────────────────────────────

/// The `neurocode_status` tool — report engine/index status.
pub struct NeuroCodeStatus {
    backend: Option<Arc<dyn NeuroCodeBackend>>,
}

#[async_trait]
impl Tool for NeuroCodeStatus {
    fn name(&self) -> &str {
        "neurocode_status"
    }

    fn toolset(&self) -> &str {
        "coding"
    }

    fn emoji(&self) -> &str {
        "📊"
    }

    fn description(&self) -> &str {
        "Report the status of the NeuroCode engine: whether it is active, the \
         number of indexed code artifacts and dependency edges, the graph store \
         schema version, and the last-index timestamp. Use to check whether the \
         project is indexed and whether an index refresh is needed before \
         querying."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn check(&self, _ctx: &ToolContext) -> bool {
        backend_active(&self.backend)
    }

    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> ToolResult {
        let Some(backend) = &self.backend else {
            return no_backend_error();
        };
        ToolResult::Text(backend.status())
    }
}

// ─── neurocode_ingest ────────────────────────────────────────────────

/// The `neurocode_ingest` tool — ingest domain knowledge.
pub struct NeuroCodeIngest {
    backend: Option<Arc<dyn NeuroCodeBackend>>,
}

#[async_trait]
impl Tool for NeuroCodeIngest {
    fn name(&self) -> &str {
        "neurocode_ingest"
    }

    fn toolset(&self) -> &str {
        "coding"
    }

    fn emoji(&self) -> &str {
        "📚"
    }

    fn description(&self) -> &str {
        "Ingest domain knowledge from a source path into the NeuroCode knowledge \
         memory. The knowledge is classified by category (patterns, anti-patterns, \
         rules, conventions), optionally pinned to a framework version, and tagged \
         with provenance for traceability. Ingested knowledge is surfaced by the \
         context assembler when relevant to future coding requests. Use to teach \
         NeuroCode project-specific conventions, framework rules, or lessons \
         learned from build/verify cycles."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "category": {
                    "type": "string",
                    "description": "Knowledge category: `pattern`, `antipattern`, `rule`, or `convention`.",
                    "enum": ["pattern", "antipattern", "rule", "convention"]
                },
                "source_path": {
                    "type": "string",
                    "description": "Path to the knowledge source (a file containing the knowledge to ingest)."
                },
                "version_tag": {
                    "type": "string",
                    "description": "Optional framework version the knowledge applies to (e.g. `8.x`, `infinity-24.2`). Omit if version-agnostic."
                },
                "provenance": {
                    "type": "string",
                    "description": "Where the knowledge came from — a URL, doc reference, or a short human-authored note (e.g. `docs.pega.com/casing`, `learned from build failure 2024-01-15`)."
                }
            },
            "required": ["category", "source_path", "provenance"]
        })
    }

    fn check(&self, _ctx: &ToolContext) -> bool {
        backend_active(&self.backend)
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let Some(backend) = &self.backend else {
            return no_backend_error();
        };
        let category = match args.get("category").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return ToolResult::Error("category is required".to_string()),
        };
        let source_path = match args.get("source_path").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolResult::Error("source_path is required".to_string()),
        };
        let version_tag = args.get("version_tag").and_then(|v| v.as_str());
        let provenance = match args.get("provenance").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::Error("provenance is required".to_string()),
        };
        ToolResult::Text(backend.ingest(
            category,
            source_path,
            version_tag,
            provenance,
        ))
    }
}

/// Register the four NeuroCode tools, each wired to `backend`.
///
/// When `backend` is `None` the tools are registered but remain disabled
/// (their `check()` returns `false`), so they are hidden from the model.
pub fn register_neurocode_tools(
    registry: &mut crate::registry::ToolRegistry,
    backend: Option<Arc<dyn NeuroCodeBackend>>,
) {
    registry.register(Arc::new(NeuroCodeIndex {
        backend: backend.clone(),
    }));
    registry.register(Arc::new(NeuroCodeQuery {
        backend: backend.clone(),
    }));
    registry.register(Arc::new(NeuroCodeStatus {
        backend: backend.clone(),
    }));
    registry.register(Arc::new(NeuroCodeIngest { backend }));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A mock backend that echoes its arguments, for deterministic assertions.
    struct MockBackend;

    impl NeuroCodeBackend for MockBackend {
        fn index(&self, path: &str, force: bool) -> String {
            format!("indexed {} force={}", path, force)
        }
        fn query(&self, query_type: &str, symbol: &str, limit: usize) -> String {
            format!("query {} symbol={} limit={}", query_type, symbol, limit)
        }
        fn status(&self) -> String {
            "status-ok".to_string()
        }
        fn ingest(
            &self,
            category: &str,
            source_path: &str,
            version_tag: Option<&str>,
            provenance: &str,
        ) -> String {
            format!(
                "ingested {} from {} version={:?} provenance={}",
                category, source_path, version_tag, provenance
            )
        }
        fn is_active(&self) -> bool {
            true
        }
    }

    fn ctx() -> ToolContext {
        ToolContext::new(std::env::temp_dir(), joey_core::Config::defaults(), "t")
    }

    fn mock_backend() -> Arc<dyn NeuroCodeBackend> {
        Arc::new(MockBackend)
    }

    // ── check() gating ──────────────────────────────────────────────

    #[test]
    fn check_false_without_backend() {
        let c = ctx();
        assert!(!NeuroCodeIndex { backend: None }.check(&c));
        assert!(!NeuroCodeQuery { backend: None }.check(&c));
        assert!(!NeuroCodeStatus { backend: None }.check(&c));
        assert!(!NeuroCodeIngest { backend: None }.check(&c));
    }

    #[test]
    fn check_true_with_backend() {
        let c = ctx();
        let b = mock_backend();
        assert!(NeuroCodeIndex { backend: Some(b.clone()) }.check(&c));
        assert!(NeuroCodeQuery { backend: Some(b.clone()) }.check(&c));
        assert!(NeuroCodeStatus { backend: Some(b.clone()) }.check(&c));
        assert!(NeuroCodeIngest { backend: Some(b) }.check(&c));
    }

    // ── execute() without backend ───────────────────────────────────

    #[tokio::test]
    async fn execute_without_backend_errors() {
        let c = ctx();
        let tools: [&dyn Tool; 4] = [
            &NeuroCodeIndex { backend: None },
            &NeuroCodeQuery { backend: None },
            &NeuroCodeStatus { backend: None },
            &NeuroCodeIngest { backend: None },
        ];
        for tool in tools {
            let r = tool.execute(json!({}), &c).await;
            assert!(r.is_error(), "{} should error without backend", tool.name());
        }
    }

    // ── neurocode_index ─────────────────────────────────────────────

    #[tokio::test]
    async fn index_requires_path() {
        let c = ctx();
        let tool = NeuroCodeIndex {
            backend: Some(mock_backend()),
        };
        let r = tool.execute(json!({}), &c).await;
        assert!(r.is_error());
    }

    #[tokio::test]
    async fn index_calls_backend() {
        let c = ctx();
        let tool = NeuroCodeIndex {
            backend: Some(mock_backend()),
        };
        let r = tool
            .execute(json!({"path": "/proj", "force": true}), &c)
            .await;
        assert_eq!(r.to_content_string(), "indexed /proj force=true");
    }

    #[tokio::test]
    async fn index_defaults_force_false() {
        let c = ctx();
        let tool = NeuroCodeIndex {
            backend: Some(mock_backend()),
        };
        let r = tool.execute(json!({"path": "/proj"}), &c).await;
        assert_eq!(r.to_content_string(), "indexed /proj force=false");
    }

    // ── neurocode_query ─────────────────────────────────────────────

    #[tokio::test]
    async fn query_requires_fields() {
        let c = ctx();
        let tool = NeuroCodeQuery {
            backend: Some(mock_backend()),
        };
        assert!(tool.execute(json!({"query_type": "dependencies"}), &c).await.is_error());
        assert!(tool.execute(json!({"symbol": "Foo"}), &c).await.is_error());
    }

    #[tokio::test]
    async fn query_calls_backend() {
        let c = ctx();
        let tool = NeuroCodeQuery {
            backend: Some(mock_backend()),
        };
        let r = tool
            .execute(
                json!({"query_type": "dependencies", "symbol": "com.x.Foo", "limit": 5}),
                &c,
            )
            .await;
        assert_eq!(
            r.to_content_string(),
            "query dependencies symbol=com.x.Foo limit=5"
        );
    }

    #[tokio::test]
    async fn query_defaults_limit() {
        let c = ctx();
        let tool = NeuroCodeQuery {
            backend: Some(mock_backend()),
        };
        let r = tool
            .execute(json!({"query_type": "definition", "symbol": "Foo"}), &c)
            .await;
        assert_eq!(r.to_content_string(), "query definition symbol=Foo limit=20");
    }

    // ── neurocode_status ────────────────────────────────────────────

    #[tokio::test]
    async fn status_calls_backend() {
        let c = ctx();
        let tool = NeuroCodeStatus {
            backend: Some(mock_backend()),
        };
        let r = tool.execute(json!({}), &c).await;
        assert_eq!(r.to_content_string(), "status-ok");
    }

    // ── neurocode_ingest ────────────────────────────────────────────

    #[tokio::test]
    async fn ingest_requires_fields() {
        let c = ctx();
        let tool = NeuroCodeIngest {
            backend: Some(mock_backend()),
        };
        assert!(tool
            .execute(json!({"category": "pattern", "source_path": "/x"}), &c)
            .await
            .is_error());
    }

    #[tokio::test]
    async fn ingest_calls_backend() {
        let c = ctx();
        let tool = NeuroCodeIngest {
            backend: Some(mock_backend()),
        };
        let r = tool
            .execute(
                json!({
                    "category": "pattern",
                    "source_path": "/docs/p.md",
                    "version_tag": "8.x",
                    "provenance": "docs"
                }),
                &c,
            )
            .await;
        assert_eq!(
            r.to_content_string(),
            "ingested pattern from /docs/p.md version=Some(\"8.x\") provenance=docs"
        );
    }

    #[tokio::test]
    async fn ingest_optional_version() {
        let c = ctx();
        let tool = NeuroCodeIngest {
            backend: Some(mock_backend()),
        };
        let r = tool
            .execute(
                json!({"category": "rule", "source_path": "/r", "provenance": "me"}),
                &c,
            )
            .await;
        assert_eq!(
            r.to_content_string(),
            "ingested rule from /r version=None provenance=me"
        );
    }

    // ── registration ────────────────────────────────────────────────

    #[test]
    fn register_all_four_tools() {
        let mut reg = crate::registry::ToolRegistry::new();
        register_neurocode_tools(&mut reg, Some(mock_backend()));
        let names = reg.names();
        for expected in [
            "neurocode_index",
            "neurocode_query",
            "neurocode_status",
            "neurocode_ingest",
        ] {
            assert!(names.contains(&expected.to_string()), "missing {}", expected);
            assert_eq!(reg.get(expected).unwrap().toolset(), "coding");
        }
    }

    #[test]
    fn register_without_backend_hides_via_check() {
        let mut reg = crate::registry::ToolRegistry::new();
        register_neurocode_tools(&mut reg, None);
        let c = ctx();
        for expected in [
            "neurocode_index",
            "neurocode_query",
            "neurocode_status",
            "neurocode_ingest",
        ] {
            let tool = reg.get(expected).unwrap();
            assert!(!tool.check(&c), "{} should be disabled", expected);
        }
    }
}

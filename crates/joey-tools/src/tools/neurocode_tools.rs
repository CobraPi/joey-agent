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

/// `neurocode_search.expand_lines` contract maximum
/// (contracts/neurocode-rag-tools.md; = `neurocode.rag.context_window_lines`
/// clamp max — T027 pins the plumbing end-to-end).
const EXPAND_LINES_MAX: u64 = 200;

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

    /// Semantic (RAG) search over the indexed project (spec 021 / T014).
    ///
    /// Returns the contract-shaped JSON payload (`results`, `mode`,
    /// `mode_reason`) — never a bare error: backend failures degrade to
    /// `keyword_only` with a `mode_reason` (FR-008).
    fn search(
        &self,
        query: &str,
        file_filter: Option<&str>,
        limit: Option<usize>,
        expand_lines: Option<usize>,
        relation_depth: Option<usize>,
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

// ─── neurocode_search ────────────────────────────────────────────────

/// The `neurocode_search` tool — semantic (RAG) code search (spec 021, T014).
///
/// Registered ONLY when `neurocode.rag.enabled == true` via
/// [`register_neurocode_rag_tools`] — absent from the registry otherwise
/// (FR-009 parity: the model never sees it in disabled state).
pub struct NeuroCodeSearch {
    backend: Option<Arc<dyn NeuroCodeBackend>>,
}

impl NeuroCodeSearch {
    /// The `neurocode_search` JSON schema, byte-exact per
    /// specs/021-please-enhance-neurocode/contracts/neurocode-rag-tools.md.
    pub fn contract_schema() -> Value {
        json!({
            "name": "neurocode_search",
            "description": "Semantic code search over the indexed project: natural-language and exact-symbol queries return ranked code locations with surrounding context and optional related entities.",
            "parameters": {
                "type": "object",
                "properties": {
                    "query":        { "type": "string",  "description": "Natural language and/or exact symbol names" },
                    "file_filter":  { "type": "string",  "description": "Glob restricting results to matching file paths" },
                    "limit":        { "type": "integer", "description": "Max results (default: neurocode.rag.top_k)", "maximum": 50 },
                    "expand_lines": { "type": "integer", "description": "± context lines (default: neurocode.rag.context_window_lines)", "maximum": 200 },
                    "relation_depth": { "type": "integer", "description": "Relationship expansion depth 0-2", "minimum": 0, "maximum": 2 }
                },
                "required": ["query"]
            }
        })
    }
}

#[async_trait]
impl Tool for NeuroCodeSearch {
    fn name(&self) -> &str {
        "neurocode_search"
    }

    fn toolset(&self) -> &str {
        "coding"
    }

    fn emoji(&self) -> &str {
        "🔎"
    }

    fn description(&self) -> &str {
        "Semantic code search over the indexed project: natural-language and \
         exact-symbol queries return ranked code locations with surrounding \
         context and optional related entities."
    }

    fn parameters(&self) -> Value {
        Self::contract_schema()["parameters"].clone()
    }

    fn check(&self, _ctx: &ToolContext) -> bool {
        backend_active(&self.backend)
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let Some(backend) = &self.backend else {
            return no_backend_error();
        };
        // query is required; whitespace-only is a validation error with a
        // clear message and NO backend call (contract: Errors section).
        let query = match args.get("query").and_then(|v| v.as_str()) {
            Some(q) if !q.trim().is_empty() => q,
            Some(_) => {
                return ToolResult::Error(
                    "query must not be empty or whitespace-only: provide a \
                     natural-language phrase and/or exact symbol name to \
                     search for."
                        .to_string(),
                )
            }
            None => {
                return ToolResult::Error(
                    "query is required: provide a natural-language phrase \
                     and/or exact symbol name to search for."
                        .to_string(),
                )
            }
        };
        let file_filter = args.get("file_filter").and_then(|v| v.as_str());
        let limit = args.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize);
        // T027: clamp expand_lines to the contract maximum 200
        // (contracts/neurocode-rag-tools.md `"maximum": 200`). A negative
        // number fails `as_u64` and is treated as absent (the schema pins
        // no minimum — the backend default,
        // `neurocode.rag.context_window_lines`, applies); 0 is a valid
        // in-range value and passes through.
        let expand_lines = args
            .get("expand_lines")
            .and_then(|v| v.as_u64())
            .map(|v| v.min(EXPAND_LINES_MAX) as usize);
        let relation_depth = args
            .get("relation_depth")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        // Backend errors NEVER hard-fail the turn — the backend itself
        // degrades to a keyword_only payload with mode_reason (FR-008).
        ToolResult::Text(backend.search(
            query,
            file_filter,
            limit,
            expand_lines,
            relation_depth,
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

/// Register the RAG `neurocode_search` tool, gated on `rag_enabled`
/// (`neurocode.rag.enabled`, spec 021 / T014).
///
/// When `rag_enabled` is false (the default) NOTHING is registered — the
/// tool is absent from the registry entirely, not merely check()-disabled
/// (FR-009 parity: the model never sees it in disabled state).
pub fn register_neurocode_rag_tools(
    registry: &mut crate::registry::ToolRegistry,
    rag_enabled: bool,
    backend: Option<Arc<dyn NeuroCodeBackend>>,
) {
    if !rag_enabled {
        return;
    }
    registry.register(Arc::new(NeuroCodeSearch { backend }));
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
        fn search(
            &self,
            query: &str,
            file_filter: Option<&str>,
            limit: Option<usize>,
            expand_lines: Option<usize>,
            relation_depth: Option<usize>,
        ) -> String {
            json!({
                "results": [{
                    "file": "src/auth/token.rs",
                    "symbol": query,
                    "context": format!("search: {}", query)
                }],
                "mode": "hybrid",
                "mode_reason": null,
                "_args": {
                    "file_filter": file_filter,
                    "limit": limit,
                    "expand_lines": expand_lines,
                    "relation_depth": relation_depth
                }
            })
            .to_string()
        }
    }

    /// A mock backend whose search always degrades to keyword_only —
    /// simulates backend unhealthiness (FR-008).
    struct DegradedBackend;

    impl NeuroCodeBackend for DegradedBackend {
        fn index(&self, _path: &str, _force: bool) -> String {
            "indexed".to_string()
        }
        fn query(&self, _query_type: &str, _symbol: &str, _limit: usize) -> String {
            "query".to_string()
        }
        fn status(&self) -> String {
            "status-ok".to_string()
        }
        fn ingest(
            &self,
            _category: &str,
            _source_path: &str,
            _version_tag: Option<&str>,
            _provenance: &str,
        ) -> String {
            "ingested".to_string()
        }
        fn is_active(&self) -> bool {
            true
        }
        fn search(
            &self,
            query: &str,
            _file_filter: Option<&str>,
            _limit: Option<usize>,
            _expand_lines: Option<usize>,
            _relation_depth: Option<usize>,
        ) -> String {
            // Contract-shaped degradation: keyword_only + mode_reason, empty
            // results — never a hard error.
            json!({
                "results": [],
                "mode": "keyword_only",
                "mode_reason": format!("vector backend unreachable: query {:?} served by keyword fallback", query)
            })
            .to_string()
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

    // ── neurocode_search (spec 021, T014) ───────────────────────────

    /// Schema pinned byte-for-byte against the contract JSON (contract
    /// "Test obligations" #1): name, description, and parameters asserted
    /// as comparable JSON values with exact equality.
    #[test]
    fn search_schema_pinned_to_contract() {
        let contract = serde_json::from_str::<Value>(
            r#"{
              "name": "neurocode_search",
              "description": "Semantic code search over the indexed project: natural-language and exact-symbol queries return ranked code locations with surrounding context and optional related entities.",
              "parameters": {
                "type": "object",
                "properties": {
                  "query":        { "type": "string",  "description": "Natural language and/or exact symbol names" },
                  "file_filter":  { "type": "string",  "description": "Glob restricting results to matching file paths" },
                  "limit":        { "type": "integer", "description": "Max results (default: neurocode.rag.top_k)", "maximum": 50 },
                  "expand_lines": { "type": "integer", "description": "± context lines (default: neurocode.rag.context_window_lines)", "maximum": 200 },
                  "relation_depth": { "type": "integer", "description": "Relationship expansion depth 0-2", "minimum": 0, "maximum": 2 }
                },
                "required": ["query"]
              }
            }"#,
        )
        .unwrap();
        let tool = NeuroCodeSearch { backend: None };
        let actual = json!({
            "name": tool.name(),
            "description": tool.description(),
            "parameters": tool.parameters(),
        });
        assert_eq!(actual, contract, "neurocode_search schema must match contract byte-for-byte");
        assert_eq!(NeuroCodeSearch::contract_schema(), contract);
    }

    /// FR-009 parity: rag disabled ⇒ registry lacks neurocode_search;
    /// rag enabled ⇒ present (and coding-toolsetted).
    #[test]
    fn rag_registration_gated_on_flag() {
        let mut reg = crate::registry::ToolRegistry::new();
        register_neurocode_rag_tools(&mut reg, false, Some(mock_backend()));
        assert!(
            !reg.names().contains(&"neurocode_search".to_string()),
            "rag disabled must leave neurocode_search ABSENT from the registry"
        );

        let mut reg = crate::registry::ToolRegistry::new();
        register_neurocode_rag_tools(&mut reg, true, Some(mock_backend()));
        assert!(reg.names().contains(&"neurocode_search".to_string()));
        assert_eq!(reg.get("neurocode_search").unwrap().toolset(), "coding");
    }

    /// Whitespace-only/missing query → validation error, no backend call.
    #[tokio::test]
    async fn search_whitespace_query_errors() {
        let c = ctx();
        let tool = NeuroCodeSearch {
            backend: Some(mock_backend()),
        };
        for args in [json!({}), json!({"query": ""}), json!({"query": "   \n\t "})] {
            let r = tool.execute(args, &c).await;
            assert!(r.is_error(), "whitespace/missing query must be a validation error");
            let msg = r.to_content_string();
            assert!(
                msg.contains("query"),
                "error must name the query field: {msg}"
            );
        }
    }

    /// FR-008: backend degradation yields the keyword_only payload shape
    /// with mode_reason — never a hard error.
    #[tokio::test]
    async fn search_degraded_backend_yields_keyword_only_shape() {
        let c = ctx();
        let tool = NeuroCodeSearch {
            backend: Some(Arc::new(DegradedBackend)),
        };
        let r = tool
            .execute(json!({"query": "validate token", "relation_depth": 1}), &c)
            .await;
        assert!(!r.is_error(), "degraded backend must not hard-fail");
        let payload: Value =
            serde_json::from_str(&r.to_content_string()).expect("payload is valid JSON");
        assert!(payload.get("results").is_some(), "results key present");
        assert_eq!(payload["mode"], "keyword_only");
        assert!(
            payload["mode_reason"].is_string() && !payload["mode_reason"].as_str().unwrap().is_empty(),
            "mode_reason must be a non-empty string"
        );
    }

    /// Happy path: args threaded through to the backend, hybrid payload.
    #[tokio::test]
    async fn search_calls_backend_with_args() {
        let c = ctx();
        let tool = NeuroCodeSearch {
            backend: Some(mock_backend()),
        };
        let r = tool
            .execute(
                json!({
                    "query": "token validation",
                    "file_filter": "src/**/*.rs",
                    "limit": 10,
                    "expand_lines": 40,
                    "relation_depth": 2
                }),
                &c,
            )
            .await;
        let payload: Value = serde_json::from_str(&r.to_content_string()).unwrap();
        assert_eq!(payload["mode"], "hybrid");
        assert_eq!(payload["_args"]["file_filter"], "src/**/*.rs");
        assert_eq!(payload["_args"]["limit"], 10);
        assert_eq!(payload["_args"]["expand_lines"], 40);
        assert_eq!(payload["_args"]["relation_depth"], 2);
    }

    /// T027: the expand_lines parameter is plumbed from tool args through
    /// the backend trait search call — value 0 is valid and passes through
    /// verbatim (MockBackend echoes `_args`).
    #[tokio::test]
    async fn search_expand_lines_zero_passes_through() {
        let c = ctx();
        let tool = NeuroCodeSearch {
            backend: Some(mock_backend()),
        };
        let r = tool
            .execute(json!({"query": "q", "expand_lines": 0}), &c)
            .await;
        let payload: Value = serde_json::from_str(&r.to_content_string()).unwrap();
        assert_eq!(payload["_args"]["expand_lines"], 0, "0 is a valid window");
    }

    /// T027: expand_lines > 200 is clamped AT THIS SURFACE per the
    /// contract `"maximum": 200` (contracts/neurocode-rag-tools.md) —
    /// the backend never sees an out-of-contract window.
    #[tokio::test]
    async fn search_expand_lines_above_200_clamps_at_tool_surface() {
        let c = ctx();
        let tool = NeuroCodeSearch {
            backend: Some(mock_backend()),
        };
        for (input, expected) in [(201u64, 200u64), (250, 200), (9999, 200)] {
            let r = tool
                .execute(json!({"query": "q", "expand_lines": input}), &c)
                .await;
            let payload: Value = serde_json::from_str(&r.to_content_string()).unwrap();
            assert_eq!(
                payload["_args"]["expand_lines"], expected,
                "expand_lines {input} must clamp to {expected} at the tool surface"
            );
        }
        // The boundary itself passes through untouched.
        let r = tool
            .execute(json!({"query": "q", "expand_lines": 200}), &c)
            .await;
        let payload: Value = serde_json::from_str(&r.to_content_string()).unwrap();
        assert_eq!(payload["_args"]["expand_lines"], 200);
    }

    /// T027: a negative expand_lines fails `as_u64` and is treated as
    /// absent — `None` reaches the backend so the
    /// `neurocode.rag.context_window_lines` default applies downstream
    /// (the schema declares no minimum; JSON Schema integers are unbounded
    /// below, so the pass-through is the permissive default).
    #[tokio::test]
    async fn search_expand_lines_negative_is_treated_as_absent() {
        let c = ctx();
        let tool = NeuroCodeSearch {
            backend: Some(mock_backend()),
        };
        let r = tool
            .execute(json!({"query": "q", "expand_lines": -5}), &c)
            .await;
        let payload: Value = serde_json::from_str(&r.to_content_string()).unwrap();
        assert!(
            payload["_args"]["expand_lines"].is_null(),
            "negative expand_lines → None (config default applies downstream)"
        );
    }

    /// check() gating mirrors the sibling tools.
    #[test]
    fn search_check_gates_on_backend() {
        let c = ctx();
        assert!(!NeuroCodeSearch { backend: None }.check(&c));
        assert!(NeuroCodeSearch {
            backend: Some(mock_backend())
        }
        .check(&c));
    }
}

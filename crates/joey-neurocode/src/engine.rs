//! `NeuroCodeEngine` — the narrow trait `joey-agent-core` consumes (T013).
//!
//! The structural index, ingestion pipeline, graph store, classifier
//! internals, and feedback loop are all private to this crate (Constitution VI).

use std::path::PathBuf;
use std::sync::Mutex;

use crate::classifier::{ComplexityClassifier, ComplexityRoute, ComplexityTier};
use crate::config::NeuroCodeConfig;
use crate::context::{AssembledContext, ContextAssembler};
use crate::graph::DependencyGraph;
use crate::parse;

/// The input to classification and context assembly (data-model.md Entity 10).
///
/// Constructed by the turn-loop intercept from the user's request + workspace context.
#[derive(Debug, Clone)]
pub struct CodingRequest {
    /// The developer's request text.
    pub text: String,
    /// The file currently open/active (for scope).
    pub active_file: Option<String>,
    /// Symbols in the active selection/cursor position (for graph expansion seeding).
    pub active_symbols: Vec<String>,
    /// The target project root (determines which `graph.db` to use).
    pub project_root: PathBuf,
    /// The available context budget for the resolved tier.
    pub token_budget_hint: u64,
}

/// Narrow interface the agent turn loop consumes to classify a coding
/// request's complexity and assemble a dependency-aware context graph
/// (contracts/neurocode-engine-trait.md).
///
/// Hot-path methods (`classify`, `assemble_context`) are non-async and run off
/// cached/indexed state — no network, no blocking (FR-017).
pub trait NeuroCodeEngine: Send + Sync {
    /// Classify a coding request's complexity and resolve the tier (FR-001).
    fn classify(&self, request: &CodingRequest) -> ComplexityRoute;

    /// Assemble the dependency-aware context graph for a request, formatted
    /// for the resolved tier's context budget (FR-007, FR-008).
    fn assemble_context(
        &self,
        request: &CodingRequest,
        tier: ComplexityTier,
    ) -> AssembledContext;

    /// Streaming variant of [`Self::assemble_context`]: `progress` is invoked
    /// with short human-readable stage descriptions during assembly so UIs
    /// can render a live feed (feature 015 follow-up: realtime context
    /// display). Default impl ignores the callback and delegates — existing
    /// engines stay source-compatible (Constitution V: non-breaking).
    fn assemble_context_with_progress(
        &self,
        request: &CodingRequest,
        tier: ComplexityTier,
        progress: &dyn Fn(&str),
    ) -> AssembledContext {
        let _ = progress;
        self.assemble_context(request, tier)
    }

    /// Whether NeuroCode is enabled for the current session (FR-003).
    fn is_active(&self) -> bool;

    /// Resolve the tier model for the tier the CURRENT request was
    /// classified into (Mode 2 — direct config lookup). Called by the
    /// turn-loop intercept AFTER [`Self::classify`]; the default engine
    /// caches the last classified tier. Returns None when no tier model is
    /// configured (caller falls back to the agent default) or when no
    /// request has been classified yet. Default impl returns None.
    fn resolve_tier_model(&self) -> Option<String> {
        None
    }

    /// Record a source-file edit observed by the agent (feature 015
    /// follow-up: dynamic context). The engine accumulates edits and
    /// decides when they cross the "large edits" threshold. Default impl:
    /// no-op (engines without an index ignore edits).
    fn record_file_edit(&self, path: &str, added: usize, removed: usize) {
        let _ = (path, added, removed);
    }

    /// True when the observed edits warrant an automatic re-index of the
    /// structural graph. The agent asks this at turn end. Default: false.
    fn should_reindex(&self) -> bool {
        false
    }

    /// Rebuild the structural graph index now (automatic re-index after
    /// large edits). Returns the ingestion stats on success. Blocking —
    /// callers should run it off the turn loop's critical path (the agent
    /// runs it via `spawn_blocking`). Default: None (unsupported).
    fn reindex_now(&self) -> Option<crate::parse::IngestionResult> {
        None
    }

    /// Progress toward the auto-reindex thresholds, for status display.
    /// Default: None.
    fn auto_index_progress(&self) -> Option<crate::auto_index::AutoIndexProgress> {
        None
    }
}

/// The default NeuroCode engine implementation.
///
/// Holds the config, classifier, and graph (opened lazily per-project).
pub struct DefaultEngine {
    config: NeuroCodeConfig,
    classifier: ComplexityClassifier,
    /// The graph store, opened for the current project root.
    /// Wrapped in Mutex for thread-safety (FR-017 hot path uses a read lock conceptually,
    /// but rusqlite Connection is Send but not Sync, so we use Mutex).
    graph: Mutex<Option<DependencyGraph>>,
    /// The project root the engine was initialized for.
    project_root: PathBuf,
    /// The active provider id (from the resolved provider profile) — scopes
    /// tier-model resolution to `neurocode.tier.providers.<id>` when present.
    provider: String,
    /// The tier the most recent `classify()` resolved to (resolved —
    /// AmbiguousDefault mapped to the configured default tier). Used by
    /// `resolve_tier_model` so the turn loop's tier routing follows the
    /// actual classification result rather than always the default tier.
    last_tier: Mutex<Option<ComplexityTier>>,
    /// Auto-index state (feature 015 follow-up: dynamic context). Tracks
    /// edits observed since the last index build so the agent can decide
    /// when the structural graph is too stale to trust and re-index.
    /// Mutex-wrapped: the engine is shared (`Arc<dyn NeuroCodeEngine>`)
    /// while the tracker is inherently mutable.
    auto_index: std::sync::Mutex<crate::auto_index::AutoIndexState>,
}

impl DefaultEngine {
    /// Create a new engine from config. The graph is opened lazily on first use.
    pub fn new(config: NeuroCodeConfig, project_root: PathBuf) -> Self {
        let classifier = ComplexityClassifier::from_config(&config);
        let auto_index = crate::auto_index::AutoIndexState::new(&config.auto_index);
        Self {
            config,
            classifier,
            graph: Mutex::new(None),
            project_root,
            provider: String::new(),
            last_tier: Mutex::new(None),
            auto_index: Mutex::new(auto_index),
        }
    }

    /// Scope tier-model resolution to `provider`'s per-provider tier config.
    /// Mirrors how the agent snapshots its resolved provider at construction.
    pub fn set_provider(&mut self, provider: &str) {
        self.provider = provider.trim().to_string();
    }

    /// Open the graph for the project (or reuse the cached one).
    fn ensure_graph(&self) -> bool {
        let mut guard = match self.graph.lock() {
            Ok(g) => g,
            Err(_) => return false,
        };
        if guard.is_none() {
            match DependencyGraph::open_for_project(&self.project_root) {
                Ok(g) => *guard = Some(g),
                Err(e) => {
                    tracing::warn!("neurocode: failed to open graph.db: {}", e);
                    return false;
                }
            }
        }
        true
    }

    /// Trigger ingestion of the project source tree (async, off hot path).
    pub fn index_project(&self) -> parse::IngestionResult {
        let guard = self.graph.lock().ok();
        if guard.is_none() {
            return parse::IngestionResult {
                files_scanned: 0,
                artifacts_seen: 0,
                edges_created: 0,
                errors: vec!["graph lock poisoned".into()],
            };
        }
        let mut guard = guard.unwrap();
        if guard.is_none() {
            match DependencyGraph::open_for_project(&self.project_root) {
                Ok(g) => *guard = Some(g),
                Err(e) => {
                    return parse::IngestionResult {
                        files_scanned: 0,
                        artifacts_seen: 0,
                        edges_created: 0,
                        errors: vec![format!("failed to open graph: {}", e)],
                    };
                }
            }
        }
        if let Some(graph) = guard.as_ref() {
            parse::ingest_project(graph, &self.project_root)
        } else {
            parse::IngestionResult::default()
        }
    }

    /// Borrow the config.
    pub fn config(&self) -> &NeuroCodeConfig {
        &self.config
    }

    /// Pin a tier override (FR-002).
    pub fn pin_tier(&self, tier: ComplexityTier) {
        self.classifier.pin_tier(tier);
    }

    /// Unpin the tier override (FR-002).
    pub fn unpin_tier(&self) {
        self.classifier.unpin_tier();
    }

    /// Borrow the classifier.
    pub fn classifier(&self) -> &ComplexityClassifier {
        &self.classifier
    }

    /// Get a snapshot of the graph for read-only queries (index, status, etc.).
    ///
    /// Opens the persisted per-project `graph.db` on first use (via
    /// [`Self::ensure_graph`]) so command surfaces reflect an index built by
    /// a *previous* engine instance. This matters because the `/neurocode`
    /// command handler constructs a fresh engine per invocation: `/neurocode
    /// index` writes the graph to disk, and the subsequent `/neurocode
    /// status` must read that same graph back rather than reporting
    /// "not initialized".
    pub fn with_graph<F, R>(&self, f: F) -> R
    where
        F: FnOnce(Option<&DependencyGraph>) -> R,
        R: Default,
    {
        if !self.ensure_graph() {
            return R::default();
        }
        let guard = self.graph.lock().ok();
        match guard {
            Some(g) => match g.as_ref() {
                Some(graph) => f(Some(graph)),
                None => f(None),
            },
            None => R::default(),
        }
    }
}

/// Command-level operations for the `/neurocode` slash command (T048,
/// contracts/neurocode-command.md).
///
/// Every method returns plain-text output suitable for direct display
/// (Constitution II — text in/out). This is intentionally a separate trait
/// from [`NeuroCodeEngine`] (which is the narrow hot-path interface the agent
/// turn loop consumes): command operations touch indexing, querying, status,
/// and learned-pattern inspection — none of which belong on the hot path.
pub trait NeuroCodeCommands: Send + Sync {
    /// Status overview (enabled?, index size, tier config, pega version,
    /// pattern counts, domain sources).
    fn status_text(&self) -> String;
    /// Trigger indexing; `force` re-indexes even if the graph already exists.
    fn index_text(&self, force: bool) -> String;
    /// Direct graph query by type + symbol.
    fn query_text(&self, query_type: &str, symbol: &str) -> String;
    /// Show or set the tier: action is "show"|"pin"|"unpin"|"set".
    fn tier_text(&self, action: &str, tier: Option<&str>) -> String;
    /// List learned patterns.
    fn patterns_text(&self) -> String;
    /// List learned anti-patterns.
    fn anti_patterns_text(&self) -> String;
    /// List ingested domain-knowledge sources.
    fn domain_list_text(&self) -> String;
    /// Remove a domain source by id.
    fn domain_remove_text(&self, id: u64) -> String;
    /// Ingest a domain-knowledge source.
    fn ingest_text(
        &self,
        category: &str,
        path: &str,
        version: Option<&str>,
        provenance: &str,
    ) -> String;
}

impl NeuroCodeCommands for DefaultEngine {
    fn status_text(&self) -> String {
        let enabled = if self.is_active() { "enabled" } else { "disabled" };
        // Index size + last-indexed timestamp (derived from the most recent
        // code_artifacts row, if any).
        let (artifact_count, last_indexed) = self.with_graph(|g| {
            let count = g.and_then(|g| g.artifact_count().ok()).unwrap_or(0);
            let last = g
                .and_then(|g| {
                    g.store()
                        .conn()
                        .query_row(
                            "SELECT MAX(indexed_at) FROM code_artifacts",
                            [],
                            |row| row.get::<_, Option<String>>(0),
                        )
                        .ok()
                        .flatten()
                })
                .unwrap_or_default();
            (count, last)
        });
        let count_str = thousands_sep(artifact_count);

        // Pega version (explicit override or auto-detected from Gradle BOM).
        let pega_version = if !self.config.pega.version.is_empty() {
            self.config.pega.version.clone()
        } else {
            "auto-detected".to_string()
        };
        let languages = crate::parse::registry::languages()
            .iter()
            .map(|l| l.id)
            .collect::<Vec<_>>()
            .join(", ");

        // Tier model ids: resolve per-provider for the active provider, showing
        // the scoped values when set (and the flat legacy keys otherwise).
        let tiers = self.config.tier.tiers_for_provider(&self.provider);
        let eco_model = if tiers.economical.is_empty() {
            "(unset)"
        } else {
            tiers.economical.as_str()
        };
        let frontier_model = if tiers.frontier.is_empty() {
            "(unset)"
        } else {
            tiers.frontier.as_str()
        };

        // Pattern + anti-pattern counts.
        let (patterns, anti_patterns, domain_count, conflicts) = self.with_graph(|g| {
            let p = g
                .and_then(|g| g.store().pattern_count().ok())
                .unwrap_or(0);
            let a = g
                .and_then(|g| g.store().anti_pattern_count().ok())
                .unwrap_or(0);
            let d = g
                .and_then(|g| g.store().list_domain_sources().ok())
                .map(|v| v.len())
                .unwrap_or(0);
            // T064: flag conflicting domain sources (same category,
            // overlapping version tags). Newest wins for retrieval.
            let c = g
                .map(|g| crate::memory::domain::resolve_conflicts(g.store()))
                .unwrap_or_default();
            (p, a, d, c)
        });

        let index_line = if artifact_count == 0 {
            "Index: not indexed (run /neurocode index)".to_string()
        } else {
            let ts = if last_indexed.is_empty() {
                String::new()
            } else {
                // Trim the RFC3339 timestamp to a friendlier "YYYY-MM-DD HH:MM".
                let ts = last_indexed.get(..16).unwrap_or(&last_indexed).replace('T', " ");
                format!(" (last indexed {})", ts)
            };
            format!("Index: {} artifacts{}", count_str, ts)
        };

        format!(
            "NeuroCode: {}\n\
             {}\n\
             Languages: {} (grammar) + heuristic fallback\n\
             Pega: {}\n\
             Tiers: economical={}, frontier={}\n\
             Patterns: {} successful, {} anti-patterns active\n\
             Domain: {} sources",
            enabled, index_line, languages, pega_version, eco_model, frontier_model, patterns, anti_patterns, domain_count
        ) + &conflict_line(conflicts.as_slice())
    }

    fn index_text(&self, force: bool) -> String {
        // `--force` currently behaves the same as a normal index (the graph
        // is always re-walked), but we honor the flag for forward-compat and
        // to suppress the "already indexed" early-return hint.
        let _ = force;
        let result = self.index_project();
        if !result.errors.is_empty() && result.files_scanned == 0 {
            return format!(
                "Indexing failed:\n{}",
                result.errors.iter().map(|e| format!("  - {}", e)).collect::<Vec<_>>().join("\n")
            );
        }
        format!(
            "Indexing complete: {} files scanned, {} artifacts, {} edges.\n\
             {} error(s).",
            result.files_scanned, result.artifacts_seen, result.edges_created, result.errors.len()
        )
    }

    fn query_text(&self, query_type: &str, symbol: &str) -> String {
        if symbol.is_empty() {
            return "Usage: /neurocode query <type> <symbol>".to_string();
        }
        self.with_graph(|g| match g {
            None => "NeuroCode: graph not initialized. Run /neurocode index first.".to_string(),
            Some(graph) => match query_type {
                "symbol" | "fts" => {
                    let results = graph.query_fts(symbol, 20).unwrap_or_default();
                    if results.is_empty() {
                        return format!("No artifacts matching '{}'.", symbol);
                    }
                    let mut out = format!("Artifacts matching '{}' ({}):\n", symbol, results.len());
                    for n in results {
                        out.push_str(&format!(
                            "  [{:<10}] {} ({}:{})\n",
                            n.kind.as_str(), n.fqcn, n.source_path, n.id
                        ));
                    }
                    out.trim_end().to_string()
                }
                "definition" => {
                    // Where is the symbol declared? Kind, file, span, signature.
                    let nodes = graph.query_fts(symbol, 10).unwrap_or_default();
                    let matches: Vec<_> = nodes
                        .iter()
                        .filter(|n| {
                            n.simple_name() == symbol
                                || n.fqcn == symbol
                                || n.fqcn.ends_with(&format!(".{}", symbol))
                                || n.fqcn.ends_with(&format!("::{}", symbol))
                        })
                        .collect();
                    if matches.is_empty() {
                        return format!(
                            "No exact definition for '{}' (use `symbol` for fuzzy matches).",
                            symbol
                        );
                    }
                    let mut out = format!("Definition(s) of '{}' ({}):\n", symbol, matches.len());
                    for n in matches {
                        out.push_str(&format!(
                            "  [{}] {}\n    file: {}\n",
                            n.kind.as_str(),
                            n.fqcn,
                            n.source_path
                        ));
                        if let Some((s, e)) = n.source_span {
                            out.push_str(&format!("    span: bytes {}..{}\n", s, e));
                        }
                        if let Some(sig) = &n.signature {
                            out.push_str(&format!("    signature: {}\n", sig));
                        }
                    }
                    out.trim_end().to_string()
                }
                "dependents" | "incoming" | "references" => {
                    // Resolve the node: prefer a type-level exact/qualified
                    // match over member FQCNs that merely contain the symbol.
                    let Some(node) = graph
                        .query_fts(symbol, 20)
                        .ok()
                        .and_then(|results| resolve_query_node(symbol, results))
                    else {
                        return format!("No artifact matching '{}' for dependency lookup.", symbol);
                    };
                    let edges = graph.traverse_to(node.id, None).unwrap_or_default();
                    if edges.is_empty() {
                        return format!("No dependents for '{}'.", node.fqcn);
                    }
                    let mut out = format!("Dependents of {} ({}):\n", node.fqcn, edges.len());
                    for (from_id, kind) in edges {
                        let label = graph
                            .store()
                            .get_node(from_id)
                            .ok()
                            .flatten()
                            .map(|n| n.fqcn)
                            .unwrap_or_else(|| format!("#{}", from_id));
                        out.push_str(&format!("  {} --[{}]--> {}\n", label, kind.as_str(), node.fqcn));
                    }
                    out.trim_end().to_string()
                }
                "dependencies" | "outgoing" => {
                    let Some(node) = graph
                        .query_fts(symbol, 20)
                        .ok()
                        .and_then(|results| resolve_query_node(symbol, results))
                    else {
                        return format!("No artifact matching '{}' for dependency lookup.", symbol);
                    };
                    let edges = graph.traverse_edges(node.id, None).unwrap_or_default();
                    if edges.is_empty() {
                        return format!("No outgoing dependencies from '{}'.", node.fqcn);
                    }
                    let mut out = format!("Dependencies of {} ({}):\n", node.fqcn, edges.len());
                    for (to_id, kind) in edges {
                        let label = graph
                            .store()
                            .get_node(to_id)
                            .ok()
                            .flatten()
                            .map(|n| n.fqcn)
                            .unwrap_or_else(|| format!("#{}", to_id));
                        out.push_str(&format!("  {} --[{}]--> {}\n", node.fqcn, kind.as_str(), label));
                    }
                    out.trim_end().to_string()
                }
                _ => format!(
                    "Unknown query type '{}'. Use: symbol | definition | dependencies | dependents | references",
                    query_type
                ),
            },
        })
    }

    fn tier_text(&self, action: &str, tier: Option<&str>) -> String {
        // Resolve per-provider tier models for display.
        let tiers = self.config.tier.tiers_for_provider(&self.provider);
        let eco_model = if tiers.economical.is_empty() {
            "(unset)"
        } else {
            tiers.economical.as_str()
        };
        let frontier_model = if tiers.frontier.is_empty() {
            "(unset)"
        } else {
            tiers.frontier.as_str()
        };

        match action {
            "show" | "" => {
                let pinned = self.classifier().pinned_tier();
                let mode = match pinned {
                    Some(t) => format!("pinned: {}", t),
                    None => "automatic".to_string(),
                };
                format!(
                    "Tier routing: {}\n\
                     economical={}, frontier={}, ambiguous_default={}",
                    mode, eco_model, frontier_model, self.config.tier.ambiguous_default
                )
            }
            "set" => {
                let Some(t) = tier else {
                    return "Usage: /neurocode tier set <economical|frontier|auto>".to_string();
                };
                match t {
                    "auto" => {
                        self.unpin_tier();
                        "Tier reverted to automatic classification.".to_string()
                    }
                    "economical" => {
                        self.pin_tier(ComplexityTier::Economical);
                        "Tier set to economical for the session.".to_string()
                    }
                    "frontier" => {
                        self.pin_tier(ComplexityTier::Frontier);
                        "Tier set to frontier for the session.".to_string()
                    }
                    _ => format!("Unknown tier '{}'. Use: economical | frontier | auto", t),
                }
            }
            "pin" => {
                let Some(t) = tier else {
                    return "Usage: /neurocode tier pin <economical|frontier>".to_string();
                };
                match t {
                    "economical" => {
                        self.pin_tier(ComplexityTier::Economical);
                        "Pinned tier to economical for this session.".to_string()
                    }
                    "frontier" => {
                        self.pin_tier(ComplexityTier::Frontier);
                        "Pinned tier to frontier for this session.".to_string()
                    }
                    _ => format!("Unknown tier '{}'. Use: economical | frontier", t),
                }
            }
            "unpin" => {
                self.unpin_tier();
                "Tier un-pinned — reverting to automatic classification.".to_string()
            }
            _ => format!(
                "Unknown tier action '{}'. Use: /neurocode tier [pin <tier> | unpin | set <tier>]",
                action
            ),
        }
    }

    fn patterns_text(&self) -> String {
        self.with_graph(|g| match g {
            None => "NeuroCode: graph not initialized. Run /neurocode index first.".to_string(),
            Some(graph) => {
                let rows: Vec<(i64, String, String, String, String)> = graph
                    .store()
                    .conn()
                    .prepare(
                        "SELECT id, prompt_signature, tier, verify_result, created_at \
                         FROM patterns ORDER BY created_at DESC LIMIT 50",
                    )
                    .ok()
                    .and_then(|mut stmt| {
                        stmt.query_map([], |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                                row.get::<_, String>(4)?,
                            ))
                        })
                        .ok()
                        .map(|r| r.filter_map(Result::ok).collect())
                    })
                    .unwrap_or_default();

                let count = graph.store().pattern_count().unwrap_or(0);
                if rows.is_empty() {
                    return format!("Learned patterns: 0 ({} total).", count);
                }
                let mut out = format!("Learned patterns ({} of {}):\n", rows.len(), count);
                for (id, sig, tier, result, created) in &rows {
                    out.push_str(&format!(
                        "  [{}] tier={} result={}\n        signature: {}\n        at: {}\n",
                        id, tier, result, sig, created
                    ));
                }
                out.trim_end().to_string()
            }
        })
    }

    fn anti_patterns_text(&self) -> String {
        self.with_graph(|g| match g {
            None => "NeuroCode: graph not initialized. Run /neurocode index first.".to_string(),
            Some(graph) => {
                let rows: Vec<(i64, String, String, String, String, u32)> = graph
                    .store()
                    .conn()
                    .prepare(
                        "SELECT id, error_signature, error_output, resolution, created_at, hit_count \
                         FROM anti_patterns WHERE status='Active' ORDER BY created_at DESC LIMIT 50",
                    )
                    .ok()
                    .and_then(|mut stmt| {
                        stmt.query_map([], |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, String>(3)?,
                                row.get::<_, String>(4)?,
                                row.get::<_, i64>(5)? as u32,
                            ))
                        })
                        .ok()
                        .map(|r| r.filter_map(Result::ok).collect())
                    })
                    .unwrap_or_default();

                let count = graph.store().anti_pattern_count().unwrap_or(0);
                if rows.is_empty() {
                    return format!("Anti-patterns: 0 active ({} total).", count);
                }
                let mut out = format!("Anti-patterns ({} active):\n", count);
                for (id, sig, err, resolution, created, hits) in &rows {
                    out.push_str(&format!(
                        "  [{}] hits={} signature: {}\n        error: {}\n        resolution: {}\n        at: {}\n",
                        id, hits, sig, err, resolution, created
                    ));
                }
                out.trim_end().to_string()
            }
        })
    }

    fn domain_list_text(&self) -> String {
        self.with_graph(|g| match g {
            None => "NeuroCode: graph not initialized. Run /neurocode index first.".to_string(),
            Some(graph) => {
                let sources = graph.store().list_domain_sources().unwrap_or_default();
                if sources.is_empty() {
                    return "Domain knowledge: no sources ingested.".to_string();
                }
                let mut out = format!("Domain knowledge sources ({}):\n", sources.len());
                for s in &sources {
                    let ver = s.version_tag.as_deref().unwrap_or("-");
                    out.push_str(&format!(
                        "  [{}] {} | version={} | provenance={}\n        path: {}\n        ingested: {}\n",
                        s.id, s.category, ver, s.provenance, s.source_path, s.ingested_at
                    ));
                }
                out.trim_end().to_string()
            }
        })
    }

    fn domain_remove_text(&self, id: u64) -> String {
        self.with_graph(|g| match g {
            None => "NeuroCode: graph not initialized.".to_string(),
            Some(graph) => match graph.store().remove_domain_knowledge(id) {
                Ok(true) => format!("Removed domain source #{}.", id),
                Ok(false) => format!("No domain source with id #{}.", id),
                Err(e) => format!("Failed to remove domain source #{}: {}", id, e),
            },
        })
    }

    fn ingest_text(
        &self,
        category: &str,
        path: &str,
        version: Option<&str>,
        provenance: &str,
    ) -> String {
        let cat = match crate::memory::domain::KnowledgeCategory::parse(category) {
            Ok(c) => c,
            Err(e) => return e,
        };
        let prov = if provenance.is_empty() { path } else { provenance };
        let source = crate::memory::domain::KnowledgeSource {
            category: cat.clone(),
            source_path: path.to_string(),
            version_tag: version.map(str::to_string),
            provenance: prov.to_string(),
        };
        self.with_graph(|g| match g {
            None => "NeuroCode: graph not initialized. Run /neurocode index first.".to_string(),
            Some(graph) => {
                // T063: shared ingestion path — single file or directory,
                // capped + binary-skipping, registry + FTS indexed.
                match crate::memory::domain::ingest_source(graph.store(), &source) {
                    Ok(id) => format!(
                        "Ingested {} source #{} from '{}' [category={}].",
                        cat.as_str(),
                        id,
                        path,
                        cat.as_str()
                    ),
                    Err(e) => format!("Ingestion failed: {}", e),
                }
            }
        })
    }
}

/// Resolve a query symbol to the best node from FTS results: prefer a
/// type-level node whose simple name or FQCN matches the symbol exactly,
/// then a qualified suffix match, then any exact member, then the first
/// type-level hit, then the first hit. Shared by the `dependencies` /
/// `dependents` query paths so member FQCNs that merely CONTAIN the symbol
/// don't hijack the lookup.
fn resolve_query_node(
    symbol: &str,
    results: Vec<crate::graph::node::CodeArtifactNode>,
) -> Option<crate::graph::node::CodeArtifactNode> {
    use crate::graph::node::{ArtifactKind, CodeArtifactNode};
    let is_type_level = |n: &CodeArtifactNode| {
        matches!(
            n.kind,
            ArtifactKind::Class | ArtifactKind::Interface | ArtifactKind::Enum | ArtifactKind::PegaRule
        )
    };
    results
        .iter()
        .find(|n| is_type_level(n) && (n.simple_name() == symbol || n.fqcn == symbol))
        .or_else(|| {
            results
                .iter()
                .find(|n| is_type_level(n) && n.fqcn.ends_with(&format!(".{}", symbol)))
        })
        .or_else(|| results.iter().find(|n| n.simple_name() == symbol))
        .or_else(|| results.iter().find(|n| is_type_level(n)))
        .cloned()
}

/// Format the status line for domain-knowledge conflicts (T064): a summary
/// count plus a compact listing of each conflict (category, version, ids).
/// Returns an empty string when there are no conflicts.
fn conflict_line(conflicts: &[crate::memory::domain::ConflictReport]) -> String {
    if conflicts.is_empty() {
        return String::new();
    }
    let mut out = format!("Domain conflicts: {} (newest source wins)\n", conflicts.len());
    for c in conflicts.iter().take(10) {
        let version = c.version_tag.as_deref().unwrap_or("-");
        let ids: Vec<String> = c
            .sources
            .iter()
            .map(|(id, _, _)| format!("#{}", id))
            .collect();
        out.push_str(&format!(
            "  conflict: {} version={} sources={}\n",
            c.category,
            version,
            ids.join(", ")
        ));
    }
    out
}

/// Format a number with thousands separators (e.g. 1234 -> "1,234").
fn thousands_sep(n: usize) -> String {
    let s = n.to_string();
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*c);
    }
    out
}

impl NeuroCodeEngine for DefaultEngine {
    fn classify(&self, request: &CodingRequest) -> ComplexityRoute {
        let route = self.classifier.classify(request);
        // Cache the RESOLVED tier so resolve_tier_model (called right after
        // by the turn loop) follows the actual classification: a Frontier
        // classification routes to the frontier tier model, not the
        // ambiguous default. Pinned routes cache their pinned tier.
        let resolved = match route.tier {
            ComplexityTier::AmbiguousDefault => {
                // Only resolve ambiguous when the classifier genuinely
                // landed there; overridden (pinned) routes keep their tier.
                if route.overridden {
                    route.tier
                } else {
                    self.config.ambiguous_default_tier()
                }
            }
            other => other,
        };
        if let Ok(mut slot) = self.last_tier.lock() {
            *slot = Some(resolved);
        }
        route
    }

    fn assemble_context(
        &self,
        request: &CodingRequest,
        tier: ComplexityTier,
    ) -> AssembledContext {
        // Non-source fallback (T065, FR-015, generalized): when the target
        // project has no ingestible source artifacts (any language), skip
        // the structural machinery entirely and return an empty context
        // with a clear notice — ordinary retrieval and generation proceed
        // unmodified.
        if !parse::project_has_source(&request.project_root) {
            return AssembledContext {
                cold_mode: false,
                notice: Some(
                    "NeuroCode: project has no supported source artifacts — structural graph disabled; \
                     using ordinary retrieval (FR-015)"
                        .to_string(),
                ),
                ..Default::default()
            };
        }
        // Lazily open the shared per-project graph (FR-021: a subagent engine
        // constructed for the same project root opens the SAME graph.db the
        // parent built — no re-ingestion, no private index).
        self.ensure_graph();
        self.with_graph(|graph_opt| match graph_opt {
            Some(graph) => {
                let assembler = ContextAssembler::new(graph);
                assembler.assemble(request, tier)
            }
            None => AssembledContext {
                cold_mode: true,
                notice: Some("NeuroCode: graph not initialized".into()),
                ..Default::default()
            },
        })
    }

    /// Streaming assembly (feature 015 follow-up): forwards the stage
    /// callback into [`ContextAssembler::assemble_with_progress`] so the
    /// turn loop can emit `NeuroCodeProgress` events for the live UI feed.
    fn assemble_context_with_progress(
        &self,
        request: &CodingRequest,
        tier: ComplexityTier,
        progress: &dyn Fn(&str),
    ) -> AssembledContext {
        // Non-source fallback: no stages to report — behave exactly like the
        // non-streaming path.
        if !parse::project_has_source(&request.project_root) {
            return self.assemble_context(request, tier);
        }
        self.ensure_graph();
        self.with_graph(|graph_opt| match graph_opt {
            Some(graph) => {
                let assembler = ContextAssembler::new(graph);
                assembler.assemble_with_progress(request, tier, progress)
            }
            None => AssembledContext {
                cold_mode: true,
                notice: Some("NeuroCode: graph not initialized".into()),
                ..Default::default()
            },
        })
    }

    fn is_active(&self) -> bool {
        self.config.enabled
    }

    fn resolve_tier_model(&self) -> Option<String> {
        // Follow the last classification (cached by classify). Fall back to
        // the pinned tier / ambiguous default only when no request has been
        // classified yet — e.g. a command-surface engine that never saw a
        // turn. Mode 2: NeuroCode resolves the tier model from its own
        // config, scoped to the active provider.
        let tier = self
            .last_tier
            .lock()
            .ok()
            .and_then(|g| *g)
            .or_else(|| self.classifier.pinned_tier())
            .unwrap_or_else(|| self.config.ambiguous_default_tier());
        let resolver = crate::tier_resolver::TierModelResolver::new(
            self.config.clone(),
            String::new(), // no fallback here — return None if unconfigured
        )
        .with_provider(&self.provider);
        let resolution = resolver.resolve(tier);
        if resolution.fell_back {
            None
        } else {
            Some(resolution.model_id)
        }
    }

    fn record_file_edit(&self, path: &str, added: usize, removed: usize) {
        if let Ok(mut tracker) = self.auto_index.lock() {
            tracker.record_edit(path, added, removed);
        }
    }

    fn should_reindex(&self) -> bool {
        self.auto_index.lock().map(|t| t.should_reindex()).unwrap_or(false)
    }

    fn reindex_now(&self) -> Option<crate::parse::IngestionResult> {
        // Full re-ingestion of the project source tree. This is the same
        // path `/neurocode index` takes (index_project), wrapped with the
        // auto-index bookkeeping: trackers cleared + debounce window
        // restarted on completion.
        let result = self.index_project();
        if result.errors.is_empty() || result.files_scanned > 0 {
            if let Ok(mut tracker) = self.auto_index.lock() {
                tracker.note_reindexed();
            }
        }
        Some(result)
    }

    fn auto_index_progress(&self) -> Option<crate::auto_index::AutoIndexProgress> {
        self.auto_index.lock().ok().map(|t| t.progress())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_is_inactive_by_default() {
        let cfg = NeuroCodeConfig::default();
        let engine = DefaultEngine::new(cfg, PathBuf::from("/tmp/test-project"));
        assert!(!engine.is_active());
    }

    #[test]
    fn engine_classifies() {
        let mut cfg = NeuroCodeConfig::default();
        cfg.enabled = true;
        let engine = DefaultEngine::new(cfg, PathBuf::from("/tmp/test-project"));
        let req = CodingRequest {
            text: "write a test for this".into(),
            active_file: None,
            active_symbols: vec![],
            project_root: PathBuf::from("/tmp/test-project"),
            token_budget_hint: 0,
        };
        let route = engine.classify(&req);
        assert_eq!(route.tier, ComplexityTier::Economical);
    }

    // ── Auto re-index (feature 015 follow-up: dynamic context) ─────────

    /// Edits accumulate toward the threshold via the trait surface, and
    /// the tracker reports progress for UI display.
    #[test]
    fn auto_index_tracks_edits_via_trait() {
        let cfg = NeuroCodeConfig::default();
        let engine = DefaultEngine::new(cfg, PathBuf::from("/tmp/test-project"));
        assert!(!engine.should_reindex(), "fresh engine: nothing edited");
        engine.record_file_edit("src/a.rs", 10, 5);
        engine.record_file_edit("src/b.rs", 20, 5);
        let p = engine.auto_index_progress().unwrap();
        assert_eq!((p.files, p.lines), (2, 40));
        assert!(!engine.should_reindex());
        engine.record_file_edit("src/c.rs", 1, 1);
        assert!(
            engine.should_reindex(),
            "3 distinct files crossed the file threshold"
        );
    }

    /// `reindex_now` on a project with no source is a no-op that still
    /// resets the trackers (additive degradation — nothing to index, but
    /// the edit pressure is consumed so we don't retry every turn).
    #[test]
    fn auto_index_reindex_on_empty_project_resets_tracker() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NeuroCodeConfig::default();
        let engine = DefaultEngine::new(cfg, dir.path().to_path_buf());
        engine.record_file_edit("a.rs", 500, 0);
        assert!(engine.should_reindex(), "line threshold crossed");
        let stats = engine.reindex_now().expect("default engine supports reindex");
        // Empty temp dir: nothing scanned, no errors beyond nothing-to-do.
        assert_eq!(stats.files_scanned, 0);
        assert!(!engine.should_reindex(), "trackers reset after the pass");
    }

    /// `reindex_now` on a real (tiny) source tree re-ingests it: nodes
    /// land in the graph and the trackers clear.
    #[test]
    fn auto_index_reindex_ingests_source() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("svc.rs"),
            "pub struct UserService;\nimpl UserService {\n    pub fn find(&self) -> u32 { 1 }\n}\n",
        )
        .unwrap();
        let cfg = NeuroCodeConfig::default();
        let engine = DefaultEngine::new(cfg, dir.path().to_path_buf());
        engine.record_file_edit("svc.rs", 4, 0);
        // Below both thresholds — but reindex_now is explicit.
        let stats = engine.reindex_now().expect("reindex supported");
        assert_eq!(stats.files_scanned, 1, "the .rs file was ingested");
        assert!(stats.artifacts_seen >= 1, "at least the struct node");
        assert!(!engine.should_reindex());
        // The graph is now populated: assembly is no longer cold.
        let req = CodingRequest {
            text: "refactor UserService".into(),
            active_file: Some("svc.rs".into()),
            active_symbols: vec!["UserService".into()],
            project_root: dir.path().to_path_buf(),
            token_budget_hint: 0,
        };
        let ctx = engine.assemble_context(&req, ComplexityTier::Economical);
        assert!(!ctx.cold_mode, "graph populated by the auto re-index");
        assert!(
            ctx.snapshot.is_some(),
            "assembly carries a graph snapshot after re-index"
        );
    }

    // ── Per-provider tier model display (fix for user-reported bug) ─────────

    /// `/neurocode status` displays the per-provider tier models for the
    /// active provider, not the flat legacy keys.
    #[test]
    fn status_text_shows_per_provider_models() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"
model:
  provider: zai
neurocode:
  enabled: true
  tier:
    frontier:
      model: legacy-frontier
    economical:
      model: legacy-economical
    providers:
      zai:
        frontier: glm-5.2
        economical: glm-4.5-flash
"#,
        )
        .unwrap();
        let cfg = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let nc = NeuroCodeConfig::from_config(&cfg);
        let mut engine = DefaultEngine::new(nc, PathBuf::from("/tmp/test-project"));
        engine.set_provider("zai");

        let status = engine.status_text();

        // Should show zai's per-provider models, not the legacy keys
        assert!(
            status.contains("economical=glm-4.5-flash"),
            "Expected economical=glm-4.5-flash in status, got: {}",
            status
        );
        assert!(
            status.contains("frontier=glm-5.2"),
            "Expected frontier=glm-5.2 in status, got: {}",
            status
        );
        assert!(
            !status.contains("legacy-frontier"),
            "Should NOT contain legacy-frontier when provider-scoped models are set"
        );
        assert!(
            !status.contains("legacy-economical"),
            "Should NOT contain legacy-economical when provider-scoped models are set"
        );
    }

    /// Unlisted providers fall back to the flat legacy keys.
    #[test]
    fn status_text_fallback_to_legacy_keys_for_unlisted_provider() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"
model:
  provider: deepseek
neurocode:
  enabled: true
  tier:
    frontier:
      model: legacy-frontier
    economical:
      model: legacy-economical
    providers:
      zai:
        frontier: glm-5.2
        economical: glm-4.5-flash
"#,
        )
        .unwrap();
        let cfg = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let nc = NeuroCodeConfig::from_config(&cfg);
        let mut engine = DefaultEngine::new(nc, PathBuf::from("/tmp/test-project"));
        engine.set_provider("deepseek");

        let status = engine.status_text();

        // Should fall back to legacy keys for unlisted providers
        assert!(
            status.contains("economical=legacy-economical"),
            "Expected economical=legacy-economical in status for unlisted provider"
        );
        assert!(
            status.contains("frontier=legacy-frontier"),
            "Expected frontier=legacy-frontier in status for unlisted provider"
        );
        assert!(
            !status.contains("glm-5.2"),
            "Should NOT contain zai's models when using deepseek provider"
        );
    }

    /// `/neurocode tier show` displays per-provider tier models.
    #[test]
    fn tier_text_shows_per_provider_models() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"
model:
  provider: zai
neurocode:
  enabled: true
  tier:
    providers:
      zai:
        frontier: glm-5.2
        economical: glm-4.5-flash
"#,
        )
        .unwrap();
        let cfg = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let nc = NeuroCodeConfig::from_config(&cfg);
        let mut engine = DefaultEngine::new(nc, PathBuf::from("/tmp/test-project"));
        engine.set_provider("zai");

        let tier = engine.tier_text("show", None);

        assert!(
            tier.contains("economical=glm-4.5-flash"),
            "Expected economical=glm-4.5-flash in tier show"
        );
        assert!(
            tier.contains("frontier=glm-5.2"),
            "Expected frontier=glm-5.2 in tier show"
        );
    }

    /// Unconfigured tiers display as (unset).
    #[test]
    fn tier_text_shows_unset_when_no_config() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"
model:
  provider: zai
neurocode:
  enabled: true
  tier:
    providers: {}
"#,
        )
        .unwrap();
        let cfg = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let nc = NeuroCodeConfig::from_config(&cfg);
        let mut engine = DefaultEngine::new(nc, PathBuf::from("/tmp/test-project"));
        engine.set_provider("zai");

        let tier = engine.tier_text("show", None);

        assert!(
            tier.contains("economical=(unset)"),
            "Expected economical=(unset) when no config"
        );
        assert!(
            tier.contains("frontier=(unset)"),
            "Expected frontier=(unset) when no config"
        );
    }

    /// Switching providers updates the displayed models.
    #[test]
    fn provider_switch_updates_displayed_models() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"
model:
  provider: zai
neurocode:
  enabled: true
  tier:
    providers:
      zai:
        frontier: glm-5.2
        economical: glm-4.5-flash
      copilot:
        frontier: gpt-5.4
        economical: gpt-4.1-turbo
"#,
        )
        .unwrap();
        let cfg = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let nc = NeuroCodeConfig::from_config(&cfg);
        let mut engine = DefaultEngine::new(nc, PathBuf::from("/tmp/test-project"));

        // Start with zai
        engine.set_provider("zai");
        let status_zai = engine.status_text();
        assert!(status_zai.contains("glm-5.2"));
        assert!(status_zai.contains("glm-4.5-flash"));

        // Switch to copilot
        engine.set_provider("copilot");
        let status_copilot = engine.status_text();
        assert!(status_copilot.contains("gpt-5.4"));
        assert!(status_copilot.contains("gpt-4.1-turbo"));
        assert!(
            !status_copilot.contains("glm-5.2"),
            "Should NOT show zai models after switching to copilot"
        );
    }

    /// Partial provider entries inherit from flat keys.
    #[test]
    fn partial_provider_entry_inherits_from_flat_keys() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            tmp.path(),
            r#"
model:
  provider: zai
neurocode:
  enabled: true
  tier:
    frontier:
      model: flat-frontier
    economical:
      model: flat-economical
    providers:
      zai:
        frontier: glm-5.2
        # economical intentionally left blank to test fallback
"#,
        )
        .unwrap();
        let cfg = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let nc = NeuroCodeConfig::from_config(&cfg);
        let mut engine = DefaultEngine::new(nc, PathBuf::from("/tmp/test-project"));
        engine.set_provider("zai");

        let status = engine.status_text();

        // frontier should be provider-scoped, economical should fall back to flat key
        assert!(
            status.contains("frontier=glm-5.2"),
            "Expected provider-scoped frontier model"
        );
        assert!(
            status.contains("economical=flat-economical"),
            "Expected flat-key fallback for unconfigured economical tier"
        );
    }
}

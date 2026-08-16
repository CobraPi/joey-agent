//! Graph-aware context assembly (FR-007/008, T015-T017).
//!
//! Given a target artifact, perform ranked graph expansion pulling in
//! implemented interfaces, injected dependencies, exchanged types, and Pega
//! rule references. Expansion is best-first (interface/parent > injected dep
//! > dependents), budget-capped per tier, and renders file paths, line
//! numbers, member signatures, and fan-in so the model can act on the
//! context without re-grepping the repo.

pub mod budget;
pub mod discovery;
pub mod snapshot;
pub mod tokens;

use std::collections::{BTreeSet, HashSet};

use crate::classifier::ComplexityTier;
use crate::engine::CodingRequest;
use crate::graph::edge::EdgeKind;
use crate::graph::node::CodeArtifactNode;
use crate::graph::{DependencyGraph, NodeId};
use crate::memory::domain::{self, DomainKnowledge};

pub use budget::TierBudget;
pub use discovery::DiscoveryHints;
pub use snapshot::{ContextGraphSnapshot, EdgeSnapshot, MemberSnapshot, NodeSnapshot};

use crate::parse::pega::node_matches_reference;
use tokens::estimate_tokens;

/// Why an expanded node was pulled into the context (FR-007).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExpansionReason {
    /// The target implements this interface.
    ImplementsInterface,
    /// The target injects/depends on this type.
    InjectedByTarget,
    /// The target exchanges a DTO/type with this type.
    ExchangesTypeWithTarget,
    /// Pega: the target references this rule.
    ReferencesRule,
    /// Pega: the target inherits from this rule.
    InheritsRule,
    /// A member (method/field) of the target type.
    MemberOfTarget,
}

impl ExpansionReason {
    /// Short human label used in the rendered context.
    fn label(&self) -> &'static str {
        match self {
            ExpansionReason::ImplementsInterface => "implements",
            ExpansionReason::InjectedByTarget => "injects",
            ExpansionReason::ExchangesTypeWithTarget => "exchanges type",
            ExpansionReason::ReferencesRule => "references rule",
            ExpansionReason::InheritsRule => "inherits rule",
            ExpansionReason::MemberOfTarget => "member of",
        }
    }

    /// Ranking priority: lower = pulled in first when the budget binds.
    fn rank(&self) -> u8 {
        match self {
            ExpansionReason::InheritsRule => 0,
            ExpansionReason::ImplementsInterface => 1,
            ExpansionReason::MemberOfTarget => 2,
            ExpansionReason::InjectedByTarget => 3,
            ExpansionReason::ExchangesTypeWithTarget => 4,
            ExpansionReason::ReferencesRule => 5,
        }
    }
}

/// An expanded node with its reason for inclusion.
#[derive(Debug, Clone)]
pub struct ExpandedNode {
    pub node: CodeArtifactNode,
    pub reason: ExpansionReason,
    /// Which included node's edges pulled this one in (its id), when known.
    /// Used by the UI snapshot to label "via <name>". Plain `id` (not an
    /// index) so it stays valid across snapshot rebuilds.
    pub via: Option<NodeId>,
    /// Graph distance from the primary target (≥ 1 for expanded nodes).
    pub depth: usize,
}

/// Result of the budget-capped expansion pass: the included nodes plus
/// how many candidates the expanded-node budget turned away.
#[derive(Debug, Clone)]
pub struct ExpansionOutcome {
    pub expanded: Vec<ExpandedNode>,
    pub dropped_for_budget: usize,
}

/// The assembled context for a coding request (data-model.md Entity 5).
#[derive(Debug, Clone)]
pub struct AssembledContext {
    /// The primary target nodes (the request target).
    pub primary_nodes: Vec<CodeArtifactNode>,
    /// Graph-expanded nodes, each tagged with why it was pulled in.
    pub expanded_nodes: Vec<ExpandedNode>,
    /// The final text formatted for the tier (FR-008).
    pub formatted_context: String,
    /// The tier this graph was formatted for.
    pub tier: ComplexityTier,
    /// Estimated tokens in `formatted_context`.
    pub token_estimate: usize,
    /// Whether the project was cold/un-indexed (FR-016 degraded mode).
    pub cold_mode: bool,
    /// Optional notice (e.g., cold-mode or non-Java-project message).
    pub notice: Option<String>,
    /// Interactive-visualization projection (feature 015 follow-up):
    /// structured node/edge view of this assembly, built alongside the
    /// formatted text. `None` for cold-mode / empty assemblies where there
    /// is no meaningful graph to draw.
    pub snapshot: Option<snapshot::ContextGraphSnapshot>,
}

impl Default for AssembledContext {
    fn default() -> Self {
        Self {
            primary_nodes: Vec::new(),
            expanded_nodes: Vec::new(),
            formatted_context: String::new(),
            tier: ComplexityTier::default(),
            token_estimate: 0,
            cold_mode: false,
            notice: None,
            snapshot: None,
        }
    }
}

/// The context assembler: graph expansion + tier-adaptive formatting (FR-007/008).
pub struct ContextAssembler<'a> {
    graph: &'a DependencyGraph,
}

impl<'a> ContextAssembler<'a> {
    pub fn new(graph: &'a DependencyGraph) -> Self {
        Self { graph }
    }

    /// Assemble the dependency-aware context for a request, formatted for the
    /// resolved tier's context budget (FR-007, FR-008).
    ///
    /// Reads from the local SQLite index (no network). Performs ranked graph
    /// expansion pulling in interfaces, deps, exchanged types, and members.
    pub fn assemble(
        &self,
        request: &CodingRequest,
        tier: ComplexityTier,
    ) -> AssembledContext {
        // No-progress delegate (byte-identical behavior to pre-streaming).
        self.assemble_with_progress(request, tier, &|_| {})
    }

    /// Streaming variant of [`Self::assemble`]: `progress` is invoked with a
    /// short human-readable stage description between assembly phases so UIs
    /// can render live progress (feature 015 follow-up: realtime context
    /// feed). The callback is synchronous and must be cheap; the assembled
    /// result is identical to [`Self::assemble`].
    pub fn assemble_with_progress(
        &self,
        request: &CodingRequest,
        tier: ComplexityTier,
        progress: &dyn Fn(&str),
    ) -> AssembledContext {
        let budget = TierBudget::for_tier(tier);

        // Determine the primary target nodes.
        progress("locating target nodes");
        let primary_nodes = self.find_primary_nodes(request);

        // Cold-mode check (FR-016): if the graph is empty, operate in degraded mode.
        let artifact_count = self.graph.artifact_count().unwrap_or(0);
        if artifact_count == 0 {
            progress("cold mode — project not indexed");
            return AssembledContext {
                primary_nodes: Vec::new(),
                expanded_nodes: Vec::new(),
                formatted_context: self.format_cold_mode(request),
                tier,
                token_estimate: 0,
                cold_mode: true,
                notice: Some(
                    "NeuroCode: project not indexed — operating in cold mode \
                     (active file + imports only). Run `/neurocode index` to build \
                     the structural graph."
                        .to_string(),
                ),
                snapshot: None,
            };
        }

        if primary_nodes.is_empty() {
            return AssembledContext {
                formatted_context: String::new(),
                tier,
                ..Default::default()
            };
        }

        // Ranked graph expansion, budget-capped.
        let expansion = self.expand_context(&primary_nodes, budget);
        let dropped_for_budget = expansion.dropped_for_budget;
        let expanded = expansion.expanded;
        progress(&format!(
            "expanded graph: {} node{} pulled in",
            expanded.len(),
            if expanded.len() == 1 { "" } else { "s" }
        ));

        // Format for the tier.
        progress("formatting context for tier");
        let (mut formatted, token_est) =
            self.format_context(&primary_nodes, &expanded, tier, budget);

        // Staleness check: if the newest primary node's indexed_at predates
        // the file's current mtime, the index is stale — say so, and point
        // the model at re-indexing.
        let stale_note = self.staleness_note(&primary_nodes, &request.project_root);
        if stale_note.is_some() {
            progress("index older than source files — noting staleness");
        }

        // Anti-pattern surfacing (T062, FR-011): collect active
        // anti-patterns attached to the primary nodes, bump their hit
        // counts, and append a WARNING section so the model sees prior
        // failures in this area before regenerating.
        let anti_warning = self.anti_pattern_warnings(&primary_nodes);
        if !anti_warning.is_empty() {
            progress("surfacing known anti-patterns");
            formatted.push_str(&anti_warning);
        }
        let mut token_est = token_est + estimate_tokens(&anti_warning);

        // Domain-knowledge surfacing (T063, FR-013/014): FTS-query the
        // ingested domain knowledge with the primary node's identity and
        // annotations, and append the top hits (with provenance) so the
        // model sees version-correct docs, real entity fields, and prior
        // postmortems before generating.
        let domain_section = self.domain_knowledge_section(&primary_nodes);
        if !domain_section.is_empty() {
            progress("surfacing domain knowledge");
            formatted.push_str(&domain_section);
        }
        token_est += estimate_tokens(&domain_section);

        if let Some(note) = stale_note {
            formatted.push_str(&note);
            token_est += estimate_tokens(&note);
        }

        // Interactive-visualization snapshot (feature 015 follow-up): the
        // structured node/edge projection built alongside the text.
        let snapshot = Some(snapshot::ContextGraphSnapshot::build(
            self.graph,
            &primary_nodes,
            &expanded,
            format!("{:?}", tier),
            token_est,
            budget,
            dropped_for_budget,
        ));

        AssembledContext {
            primary_nodes,
            expanded_nodes: expanded,
            formatted_context: formatted,
            tier,
            token_estimate: token_est,
            cold_mode: false,
            notice: None,
            snapshot,
        }
    }

    /// Detect index staleness: any primary node whose source file's mtime is
    /// newer than the node's indexed_at timestamp. `project_root` resolves
    /// the stored project-relative `source_path`. Returns a short warning
    /// section (or None).
    fn staleness_note(
        &self,
        primary: &[CodeArtifactNode],
        project_root: &std::path::Path,
    ) -> Option<String> {
        let mut stale_files: BTreeSet<String> = BTreeSet::new();
        for node in primary.iter().take(3) {
            if node.source_path.is_empty() {
                continue;
            }
            // Stored paths are project-relative; absolute ones (some
            // extractions) pass through unchanged.
            let candidate = project_root.join(&node.source_path);
            let path = if candidate.is_file() {
                candidate
            } else {
                std::path::PathBuf::from(&node.source_path)
            };
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let Ok(modified) = meta.modified() else {
                continue;
            };
            let mtime = modified
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            // indexed_at is RFC3339 UTC; parse the epoch seconds cheaply via
            // chrono's DateTime (already a dependency).
            if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&node.indexed_at) {
                if mtime > ts.timestamp().unsigned_abs() {
                    stale_files.insert(node.source_path.clone());
                }
            }
        }
        if stale_files.is_empty() {
            return None;
        }
        let files: Vec<String> = stale_files
            .into_iter()
            .take(3)
            .map(|f| format!("- {}", f))
            .collect();
        Some(format!(
            "### Index Staleness\n\
             The graph index predates recent edits to:\n{}\n\
             Spans and members below may be outdated — re-run `/neurocode index` \
             after large edits, and prefer reading the file before editing.\n\n",
            files.join("\n")
        ))
    }

    /// Collect and surface known anti-patterns attached to the primary
    /// nodes (T062, FR-011). Bumps each pattern's hit_count and formats a
    /// WARNING-toned section for the model. Returns an empty string when
    /// no anti-pattern applies to this area.
    fn anti_pattern_warnings(&self, primary: &[CodeArtifactNode]) -> String {
        let ids: Vec<NodeId> = primary.iter().map(|n| n.id).collect();
        let Ok(anti_patterns) = self.graph.store().anti_patterns_for_artifacts(&ids) else {
            return String::new();
        };
        if anti_patterns.is_empty() {
            return String::new();
        }
        let mut out =
            String::from("### Known Anti-Patterns (prior failures in this area)\n");
        for (id, error_signature, resolution) in &anti_patterns {
            out.push_str(&format!(
                "- ⚠️ WARNING: {} — known failure in this area; prior fix: {}\n",
                error_signature, resolution
            ));
            // Record that this warning was surfaced (FR-011 hit tracking).
            let _ = self.graph.store().bump_anti_pattern_hit(*id);
        }
        out.push('\n');
        out
    }

    /// Pull relevant ingested domain knowledge into the assembled context
    /// (T063, FR-013/014). FTS-queries the domain knowledge with the primary
    /// node's simple name + annotations (e.g. "UserServiceImpl Transactional"),
    /// then with the simple name alone, keeping the first 3–5 distinct hits.
    ///
    /// Each hit's content is truncated to ~500 chars and surfaced with its
    /// provenance and version tag (FR-014). Postmortems are formatted as
    /// warning-toned notes. Returns an empty string when there are no hits.
    fn domain_knowledge_section(
        &self,
        primary: &[CodeArtifactNode],
    ) -> String {
        const MAX_HITS: usize = 5;
        let mut hits: Vec<DomainKnowledge> = Vec::new();
        let mut seen_provenance: HashSet<String> = HashSet::new();

        let store = self.graph.store();
        for node in primary.iter().take(2) {
            if hits.len() >= MAX_HITS {
                break;
            }
            let simple = node.simple_name();
            // Query 1: simple name + annotations (narrower, ranked first).
            if !node.annotations.is_empty() {
                let q = format!(
                    "{} {}",
                    simple,
                    node.annotations.first().map(String::as_str).unwrap_or("")
                );
                for hit in domain::retrieve(store, &q, None, MAX_HITS) {
                    if hits.len() < MAX_HITS && seen_provenance.insert(hit.provenance.clone()) {
                        hits.push(hit);
                    }
                }
            }
            // Query 2: simple name alone (recall).
            for hit in domain::retrieve(store, simple, None, MAX_HITS) {
                if hits.len() < MAX_HITS && seen_provenance.insert(hit.provenance.clone()) {
                    hits.push(hit);
                }
            }
            // Query 3: implemented interfaces — the natural join for entity
            // catalogs (an `UserServiceImpl` node's catalog entry is keyed by
            // the `UserService` interface it implements).
            for iface in node.implemented_interfaces.iter().take(2) {
                if hits.len() >= MAX_HITS {
                    break;
                }
                for hit in domain::retrieve(store, iface, None, MAX_HITS) {
                    if hits.len() < MAX_HITS && seen_provenance.insert(hit.provenance.clone()) {
                        hits.push(hit);
                    }
                }
            }
        }
        if hits.is_empty() {
            return String::new();
        }

        let mut out = String::from("### Domain Knowledge\n");
        for hit in &hits {
            let snippet: String = hit.content.chars().take(500).collect();
            let trimmed = snippet.trim_end();
            let version = hit.version_tag.as_deref().unwrap_or("-");
            let is_postmortem = hit.category.as_deref() == Some("Postmortem");
            if is_postmortem {
                out.push_str(&format!(
                    "- ⚠️ NOTE (prior postmortem, may recur here): {}\n  provenance: {} | version: {}\n",
                    trimmed, hit.provenance, version
                ));
            } else {
                out.push_str(&format!(
                    "- {}\n  provenance: {} | version: {}\n",
                    trimmed, hit.provenance, version
                ));
            }
        }
        out.push('\n');
        out
    }

    /// Find the primary target nodes from the request.
    ///
    /// Priority order (highest precision first):
    /// 1. Backtick-quoted symbols and CamelCase/dotted identifiers extracted
    ///    from the request text (discovery hints).
    /// 2. Explicit `active_symbols` (IDE/selector-provided).
    /// 3. The active file's type-level nodes (path lookup).
    /// 4. The active file name's stem.
    /// 5. Weak lowercase word seeds from the request text.
    fn find_primary_nodes(&self, request: &CodingRequest) -> Vec<CodeArtifactNode> {
        let mut nodes = Vec::new();

        // 1. Discovery hints from the request text — identifiers the user
        // actually named (backticked, CamelCase, dotted refs).
        let hints = discovery::extract_hints(&request.text);
        for ident in hints.identifiers.iter() {
            // Fetch generously: FTS also matches `declared_dependencies`
            // text, so dependents of the named type can crowd the true node
            // out of a small limit. best_symbol_match re-ranks in Rust.
            if let Ok(results) = self.graph.query_fts(ident, 20) {
                if let Some(best) = best_symbol_match(ident, &results) {
                    nodes.push(best);
                }
            }
            if nodes.len() >= 5 {
                break;
            }
        }

        // 2. Active symbols (IDE-provided) — kept for explicit callers.
        if nodes.is_empty() {
            for symbol in &request.active_symbols {
                if let Ok(results) = self.graph.query_fts(symbol, 3) {
                    nodes.extend(results);
                }
            }
        }

        // 3. Active file → its type-level nodes.
        if nodes.is_empty() {
            if let Some(file) = &request.active_file {
                if let Ok(results) = self.graph.store().nodes_by_source_path(file) {
                    nodes.extend(results);
                }
            }
        }

        // 4. Active file stem as a symbol seed.
        if nodes.is_empty() {
            if let Some(file) = &request.active_file {
                if let Some(stem) = std::path::Path::new(file)
                    .file_stem()
                    .and_then(|s| s.to_str())
                {
                    if let Ok(results) = self.graph.query_fts(stem, 5) {
                        nodes.extend(results);
                    }
                }
            }
        }

        // 5. Weak word seeds from the request text (discovery already ranked
        //    these last; reuse them when nothing better matched).
        if nodes.is_empty() {
            for word in request.text.split_whitespace() {
                let word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '.');
                if word.len() < 4 {
                    continue;
                }
                if let Ok(results) = self.graph.query_fts(word, 3) {
                    nodes.extend(results);
                    if nodes.len() >= 5 {
                        break;
                    }
                }
            }
        }

        // Deduplicate by id.
        let mut seen = HashSet::new();
        nodes.retain(|n| seen.insert(n.id));
        nodes
    }

    /// Expand context from the primary nodes using ranked, budget-capped
    /// best-first graph traversal.
    ///
    /// Ranking (ExpansionReason::rank): inheritance and implemented
    /// interfaces first, then members, injected dependencies, exchanged
    /// types, and Pega rule references — dependents last. When the budget
    /// binds, high-value neighbors displace low-value ones instead of
    /// arbitrary BFS order deciding.
    ///
    /// Pega-aware (T059): when a primary or already-expanded node carries
    /// `pega_metadata`, nodes referenced by `references_rules` (FTS lookup
    /// when no edge exists) and `inherits_from` are pulled in with
    /// `ReferencesRule`/`InheritsRule` reasons.
    fn expand_context(
        &self,
        primary: &[CodeArtifactNode],
        budget: TierBudget,
    ) -> ExpansionOutcome {
        let mut expanded: Vec<ExpandedNode> = Vec::new();
        let mut visited: HashSet<NodeId> = primary.iter().map(|n| n.id).collect();

        // Pega rule-reference expansion from primary nodes' metadata (T059).
        for n in primary {
            if let Some(meta) = &n.pega_metadata {
                for reference in &meta.references_rules {
                    if let Some(node) = self.find_rule_by_reference(reference) {
                        if visited.insert(node.id) {
                            expanded.push(ExpandedNode {
                                node,
                                reason: ExpansionReason::ReferencesRule,
                                via: Some(n.id),
                                depth: 1,
                            });
                        }
                    }
                }
                if let Some(parent) = &meta.inherits_from {
                    if let Some(node) = self.find_rule_by_reference(parent) {
                        if visited.insert(node.id) {
                            expanded.push(ExpandedNode {
                                node,
                                reason: ExpansionReason::InheritsRule,
                                via: Some(n.id),
                                depth: 1,
                            });
                        }
                    }
                }
            }
        }

        // Best-first frontier: (rank, order, node_id, depth, reason, via_id).
        // BinaryHeap is max-first; use Reverse for min-first. `via_id` is the
        // node whose edges pulled this candidate in (for the UI snapshot).
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;
        type FrontierItem = (u8, u64, NodeId, usize, ExpansionReason, NodeId);
        let mut order: u64 = 0;
        let mut frontier: BinaryHeap<Reverse<FrontierItem>> = BinaryHeap::new();

        let seed = |n: &CodeArtifactNode,
                    order: &mut u64,
                    frontier: &mut BinaryHeap<Reverse<FrontierItem>>| {
            // Look at edges from this node (dependencies).
            if let Ok(edges) = self.graph.traverse_edges(n.id, None) {
                for (to_id, kind) in edges {
                    let reason = match kind {
                        EdgeKind::Implements => ExpansionReason::ImplementsInterface,
                        EdgeKind::Injects => ExpansionReason::InjectedByTarget,
                        EdgeKind::ExchangesType => ExpansionReason::ExchangesTypeWithTarget,
                        EdgeKind::MemberOf => ExpansionReason::MemberOfTarget,
                        EdgeKind::ReferencesRule => ExpansionReason::ReferencesRule,
                        EdgeKind::InheritsRule => ExpansionReason::InheritsRule,
                        EdgeKind::IsImplementedBy => ExpansionReason::ImplementsInterface,
                    };
                    *order += 1;
                    frontier.push(Reverse((reason.rank(), *order, to_id, 1, reason, n.id)));
                }
            }
            // And edges TO this node: dependents (injected/implemented) and
            // this type's members (member → MemberOf → type).
            if let Ok(to_edges) = self.graph.traverse_to(n.id, None) {
                for (from_id, kind) in to_edges {
                    let reason = match kind {
                        EdgeKind::IsImplementedBy | EdgeKind::Implements => {
                            ExpansionReason::ImplementsInterface
                        }
                        EdgeKind::MemberOf => ExpansionReason::MemberOfTarget,
                        EdgeKind::Injects => ExpansionReason::InjectedByTarget,
                        EdgeKind::ExchangesType => ExpansionReason::ExchangesTypeWithTarget,
                        EdgeKind::ReferencesRule => ExpansionReason::ReferencesRule,
                        EdgeKind::InheritsRule => ExpansionReason::InheritsRule,
                    };
                    *order += 1;
                    frontier.push(Reverse((reason.rank(), *order, from_id, 1, reason, n.id)));
                }
            }
        };

        for n in primary {
            seed(n, &mut order, &mut frontier);
        }

        let mut dropped_for_budget: usize = 0;
        // Defensive traversal cap: hub nodes (big classes, widely-injected
        // utilities) can seed hundreds of frontier entries; every pop costs
        // a store lookup. The render budget is ≤ 24 nodes, so a cap of 12×
        // that is far beyond anything that can still influence the output.
        let max_pops: usize = budget.max_expanded_nodes.saturating_mul(12).max(60);
        let mut pops: usize = 0;
        while let Some(Reverse((rank, _ord, node_id, depth, reason, via_id))) = frontier.pop() {
            pops += 1;
            if pops > max_pops {
                break;
            }
            if visited.contains(&node_id) {
                continue;
            }
            visited.insert(node_id);

            let Ok(Some(node)) = self.graph.store().get_node(node_id) else {
                continue;
            };

            // Member nodes: skip Method/Field members in the expansion output
            // (they're rendered inside the type roster instead) unless the
            // budget is generous — but still traverse through them.
            let is_member = matches!(node.kind, crate::graph::node::ArtifactKind::Method
                | crate::graph::node::ArtifactKind::Field);

            if is_member {
                // Don't render members as separate related artifacts; they
                // appear in the enclosing type's roster. Still visit their
                // edges to reach the enclosing type (member → type edge).
                if let Ok(edges) = self.graph.traverse_edges(node_id, None) {
                    for (to_id, _kind) in edges {
                        if !visited.contains(&to_id) && depth < budget.max_expansion_depth {
                            order += 1;
                            frontier.push(Reverse((
                                reason.rank().max(ExpansionReason::MemberOfTarget.rank()),
                                order,
                                to_id,
                                depth + 1,
                                reason.clone(),
                                via_id,
                            )));
                        }
                    }
                }
                continue;
            }

            if expanded.len() >= budget.max_expanded_nodes {
                dropped_for_budget += 1;
                continue;
            }
            expanded.push(ExpandedNode {
                node: node.clone(),
                reason: reason.clone(),
                via: Some(via_id),
                depth,
            });

            // Expand one more level when depth allows.
            if depth < budget.max_expansion_depth {
                if let Ok(edges) = self.graph.traverse_edges(node_id, None) {
                    for (to_id, _kind) in edges {
                        if !visited.contains(&to_id) {
                            order += 1;
                            frontier.push(Reverse((rank.max(2), order, to_id, depth + 1, reason.clone(), node_id)));
                        }
                    }
                }
            }

            // Pega rule-reference expansion from an expanded node's metadata
            // (T059): pulls in rules this expanded rule references/inherits,
            // even when no explicit edge was ingested.
            if let Some(meta) = &node.pega_metadata {
                let mut pega_followups: Vec<(CodeArtifactNode, ExpansionReason)> = Vec::new();
                for reference in &meta.references_rules {
                    if let Some(pnode) = self.find_rule_by_reference(reference) {
                        if visited.insert(pnode.id) {
                            pega_followups.push((pnode, ExpansionReason::ReferencesRule));
                        }
                    }
                }
                if let Some(parent) = &meta.inherits_from {
                    if let Some(pnode) = self.find_rule_by_reference(parent) {
                        if visited.insert(pnode.id) {
                            pega_followups.push((pnode, ExpansionReason::InheritsRule));
                        }
                    }
                }
                for (pnode, reason) in pega_followups {
                    expanded.push(ExpandedNode {
                        node: pnode,
                        reason,
                        via: Some(node_id),
                        depth: depth + 1,
                    });
                }
            }
        }

        ExpansionOutcome {
            expanded,
            dropped_for_budget,
        }
    }

    /// FTS-lookup a Pega rule by reference name (T059). Requires an exact
    /// FQCN/simple-name match so free-text FTS hits don't leak in.
    fn find_rule_by_reference(&self, reference: &str) -> Option<CodeArtifactNode> {
        let results = self.graph.query_fts(reference, 10).ok()?;
        results
            .into_iter()
            .find(|n| node_matches_reference(&n.fqcn, reference))
    }

    /// Format the context for the tier's budget (FR-008).
    fn format_context(
        &self,
        primary: &[CodeArtifactNode],
        expanded: &[ExpandedNode],
        tier: ComplexityTier,
        budget: TierBudget,
    ) -> (String, usize) {
        let mut output = String::new();

        output.push_str(&format!(
            "## NeuroCode Context (tier: {})\n\n",
            tier
        ));

        // Primary target — full detail with file path, members, fan-in.
        output.push_str("### Target\n");
        for n in primary.iter().take(budget.max_primary_nodes) {
            output.push_str(&format_node(n));
            // Blast-radius: warn when the target is a hub (many dependents).
            if let Ok(fan_in) = self.graph.store().dependents_count(n.id) {
                if fan_in >= 5 {
                    output.push_str(&format!(
                        "  ⚠ used by {} other artifacts — changes here have wide blast radius\n",
                        fan_in
                    ));
                }
            }
            // Member roster (methods + fields with signatures).
            if matches!(
                n.kind,
                crate::graph::node::ArtifactKind::Class
                    | crate::graph::node::ArtifactKind::Interface
                    | crate::graph::node::ArtifactKind::Enum
                    | crate::graph::node::ArtifactKind::PegaRule
            ) {
                if let Ok(members) = self.graph.store().members_of_enclosing(n.simple_name(), 40) {
                    if !members.is_empty() {
                        output.push_str("  members:\n");
                        for m in members {
                            output.push_str(&format_member(&m, "    "));
                        }
                    }
                }
            }
        }
        output.push('\n');

        // Expanded nodes — budget-limited.
        if !expanded.is_empty() {
            output.push_str("### Related Artifacts\n");
            for exp in expanded.iter().take(budget.max_expanded_nodes) {
                output.push_str(&format!(
                    "**{}** (via {})\n",
                    exp.node.fqcn,
                    exp.reason.label()
                ));
                // Economical tier: summary only; Frontier: full detail.
                if tier == ComplexityTier::Frontier || tier == ComplexityTier::AmbiguousDefault {
                    output.push_str(&format_node(&exp.node));
                } else {
                    // Economical: brief signature.
                    output.push_str(&format!(
                        "  kind={}, annotations={:?}\n",
                        exp.node.kind.as_str(),
                        exp.node.annotations
                    ));
                }
            }
            output.push('\n');
        }

        let token_est = estimate_tokens(&output);
        (output, token_est)
    }

    /// Format a cold-mode (degraded) context (FR-016).
    fn format_cold_mode(&self, request: &CodingRequest) -> String {
        let mut out = String::new();
        out.push_str("## NeuroCode Cold-Mode Context\n\n");
        for file in cold_mode_files(request, &request.project_root) {
            out.push_str(&format!("Active file: {}\n", file.display()));
            if let Ok(content) = std::fs::read_to_string(&file) {
                // Include import-ish lines only (immediate context), across
                // the common language syntaxes.
                for line in content.lines() {
                    let t = line.trim();
                    let is_import = t.starts_with("import ")
                        || t.starts_with("from ")
                        || t.starts_with("use ")
                        || t.starts_with("using ")
                        || t.starts_with("#include")
                        || t.starts_with("require ")
                        || t.starts_with("require_")
                        || t == "package"
                        || t.starts_with("package ");
                    if is_import {
                        out.push_str(t);
                        out.push('\n');
                    }
                }
            }
        }
        out
    }
}

/// Pick the best node from FTS results for a query identifier.
///
/// FTS matches member FQCNs (`UserServiceImpl.findById()`) whenever the
/// type name is queried, so raw FTS rank is not enough. Ranking, best
/// first:
/// 1. Type-level node (Class/Interface/Enum/PegaRule) whose simple name or
///    FQCN exactly equals the identifier.
/// 2. Type-level node whose FQCN ends with the identifier (qualified hit).
/// 3. Any node whose simple name exactly equals the identifier (a member
///    the user explicitly named — `findById`).
/// 4. First type-level FTS result.
/// 5. First FTS result.
fn best_symbol_match<'a>(ident: &str, results: &'a [CodeArtifactNode]) -> Option<CodeArtifactNode> {
    if results.is_empty() {
        return None;
    }
    let is_type_level = |n: &CodeArtifactNode| {
        matches!(
            n.kind,
            crate::graph::node::ArtifactKind::Class
                | crate::graph::node::ArtifactKind::Interface
                | crate::graph::node::ArtifactKind::Enum
                | crate::graph::node::ArtifactKind::PegaRule
        )
    };
    // 1. Type-level exact.
    if let Some(n) = results
        .iter()
        .find(|n| is_type_level(n) && (n.simple_name() == ident || n.fqcn == ident))
    {
        return Some(n.clone());
    }
    // 2. Type-level qualified suffix.
    if let Some(n) = results
        .iter()
        .find(|n| is_type_level(n) && n.fqcn.ends_with(&format!(".{}", ident)))
    {
        return Some(n.clone());
    }
    // 3. Any exact simple-name member.
    if let Some(n) = results.iter().find(|n| n.simple_name() == ident) {
        return Some(n.clone());
    }
    // 4. First type-level.
    if let Some(n) = results.iter().find(|n| is_type_level(n)) {
        return Some(n.clone());
    }
    // 5. First result.
    results.first().cloned()
}

/// The files a cold-mode context should read: the active file plus any
/// path mentions discovered in the request text. Relative paths resolve
/// against the project root; absolute ones pass through.
fn cold_mode_files(request: &CodingRequest, project_root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let push = |raw: &str, files: &mut Vec<std::path::PathBuf>| {
        let path = std::path::Path::new(raw);
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            project_root.join(path)
        };
        if resolved.is_file() && !files.contains(&resolved) {
            files.push(resolved);
        }
    };
    if let Some(file) = &request.active_file {
        push(file, &mut files);
    }
    let hints = discovery::extract_hints(&request.text);
    for p in hints.file_paths {
        push(&p, &mut files);
    }
    files
}

/// Format a single node for context output.
fn format_node(n: &CodeArtifactNode) -> String {
    let mut s = String::new();
    s.push_str(&format!("- **{}** ({})\n", n.fqcn, n.kind.as_str()));
    if !n.source_path.is_empty() {
        // Byte→line conversion would need the file contents; the path alone
        // lets the model read_file the exact declaration. Spans stay in the
        // graph for tools that need them.
        s.push_str(&format!("  file: {}\n", n.source_path));
    }
    if !n.implemented_interfaces.is_empty() {
        s.push_str(&format!("  implements: {}\n", n.implemented_interfaces.join(", ")));
    }
    if !n.annotations.is_empty() {
        s.push_str(&format!("  annotations: @{}\n", n.annotations.join(" @")));
    }
    if !n.declared_dependencies.is_empty() {
        s.push_str(&format!("  dependencies: {}\n", n.declared_dependencies.join(", ")));
    }
    // Fan-in warning for hub types: editing these has wide blast radius.
    // (Looked up by the assembler, which has store access; format_node is
    // kept pure — the caller prepends fan-in lines when relevant.)
    if let Some(meta) = &n.pega_metadata {
        s.push_str(&format!(
            "  pega: rule family={}, name={}\n",
            meta.rule_class_family.as_str(),
            meta.rule_name
        ));
        if !meta.references_rules.is_empty() {
            s.push_str(&format!("  pega references: {}\n", meta.references_rules.join(", ")));
        }
        if let Some(parent) = &meta.inherits_from {
            s.push_str(&format!("  pega inherits from: {}\n", parent));
        }
        if !meta.pega_version.is_empty() {
            s.push_str(&format!("  pega version: {}\n", meta.pega_version));
        }
    }
    s
}

/// Format a member (method/field) line for a type's roster.
fn format_member(m: &CodeArtifactNode, indent: &str) -> String {
    let mut s = String::new();
    let kind_label = match m.kind {
        crate::graph::node::ArtifactKind::Method => "method",
        crate::graph::node::ArtifactKind::Field => "field",
        _ => "member",
    };
    match &m.signature {
        Some(sig) if !sig.is_empty() => {
            s.push_str(&format!("{}{} {}\n", indent, kind_label, sig));
        }
        _ => {
            s.push_str(&format!("{}{} {}\n", indent, kind_label, m.simple_name()));
        }
    }
    s
}

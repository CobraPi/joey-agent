//! Graph-aware context assembly (FR-007/008, T015-T017).
//!
//! Given a target artifact, perform graph expansion (depth ≤ 2 edges) pulling
//! in implemented interfaces, injected dependencies, and exchanged types.

pub mod budget;

use std::collections::HashSet;

use crate::classifier::ComplexityTier;
use crate::engine::CodingRequest;
use crate::graph::edge::EdgeKind;
use crate::graph::node::CodeArtifactNode;
use crate::graph::{DependencyGraph, NodeId};
use crate::memory::domain::{self, DomainKnowledge};

pub use budget::TierBudget;

use crate::parse::pega::node_matches_reference;

/// Why an expanded node was pulled into the context (FR-007).
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

/// An expanded node with its reason for inclusion.
#[derive(Debug, Clone)]
pub struct ExpandedNode {
    pub node: CodeArtifactNode,
    pub reason: ExpansionReason,
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
    /// Reads from the local SQLite index (no network). Performs graph expansion
    /// (depth ≤ 2 edges) pulling in interfaces, deps, and exchanged types.
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
            };
        }

        if primary_nodes.is_empty() {
            return AssembledContext {
                formatted_context: String::new(),
                tier,
                ..Default::default()
            };
        }

        // Graph expansion (depth ≤ 2 edges).
        let expanded = self.expand_context(&primary_nodes, budget.max_expansion_depth);
        progress(&format!(
            "expanded graph: {} node{} pulled in",
            expanded.len(),
            if expanded.len() == 1 { "" } else { "s" }
        ));

        // Format for the tier.
        progress("formatting context for tier");
        let (mut formatted, token_est) =
            self.format_context(&primary_nodes, &expanded, tier, budget);

        // Anti-pattern surfacing (T062, FR-011): collect active
        // anti-patterns attached to the primary nodes, bump their hit
        // counts, and append a WARNING section so the model sees prior
        // failures in this area before regenerating.
        let anti_warning = self.anti_pattern_warnings(&primary_nodes);
        if !anti_warning.is_empty() {
            progress("surfacing known anti-patterns");
            formatted.push_str(&anti_warning);
        }
        let mut token_est = token_est + anti_warning.len() / 4;

        // Domain-knowledge surfacing (T063, FR-013/014): FTS-query the
        // ingested domain knowledge with the primary node's identity and
        // annotations, and append the top hits (with provenance) so the
        // model sees version-correct docs, real entity fields, and prior
        // postmortems before generating.
        let domain_section = self.domain_knowledge_section(&primary_nodes);
        if !domain_section.is_empty() {
            progress("surfacing domain knowledge");
            formatted.push_str(&domain_section);
            token_est += domain_section.len() / 4;
        }

        AssembledContext {
            primary_nodes,
            expanded_nodes: expanded,
            formatted_context: formatted,
            tier,
            token_estimate: token_est,
            cold_mode: false,
            notice: None,
        }
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
    fn domain_knowledge_section(&self, primary: &[CodeArtifactNode]) -> String {
        const MAX_HITS: usize = 5;
        let mut hits: Vec<DomainKnowledge> = Vec::new();
        let mut seen_provenance: HashSet<String> = HashSet::new();

        let store = self.graph.store();
        for node in primary.iter().take(2) {
            if hits.len() >= MAX_HITS {
                break;
            }
            let simple = node.fqcn.rsplit('.').next().unwrap_or(&node.fqcn);
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
    fn find_primary_nodes(&self, request: &CodingRequest) -> Vec<CodeArtifactNode> {
        let mut nodes = Vec::new();

        // Try FTS search with active symbols first.
        for symbol in &request.active_symbols {
            if let Ok(results) = self.graph.query_fts(symbol, 3) {
                nodes.extend(results);
            }
        }

        // If no symbols, try extracting identifiers from the active file name.
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

        // Try keywords from the request text.
        if nodes.is_empty() {
            for word in request.text.split_whitespace() {
                if word.len() < 3 {
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

    /// Expand context from the primary nodes using graph traversal.
    ///
    /// Pega-aware (T059): when a primary or already-expanded node carries
    /// `pega_metadata`, nodes referenced by `references_rules` (FTS lookup
    /// when no edge exists) and `inherits_from` are pulled in with
    /// `ReferencesRule`/`InheritsRule` reasons.
    fn expand_context(
        &self,
        primary: &[CodeArtifactNode],
        max_depth: usize,
    ) -> Vec<ExpandedNode> {
        let mut expanded = Vec::new();
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
                            });
                        }
                    }
                }
            }
        }

        // BFS expansion from each primary node.
        let mut frontier: Vec<(NodeId, usize, ExpansionReason)> = primary
            .iter()
            .flat_map(|n| {
                let mut out = Vec::new();
                // Look at edges from this node.
                if let Ok(edges) = self.graph.traverse_edges(n.id, None) {
                    for (to_id, kind) in edges {
                        let reason = match kind {
                            EdgeKind::Implements => ExpansionReason::ImplementsInterface,
                            EdgeKind::Injects => ExpansionReason::InjectedByTarget,
                            EdgeKind::ExchangesType => ExpansionReason::ExchangesTypeWithTarget,
                            EdgeKind::ReferencesRule => ExpansionReason::ReferencesRule,
                            EdgeKind::InheritsRule => ExpansionReason::InheritsRule,
                            EdgeKind::IsImplementedBy => ExpansionReason::ImplementsInterface,
                        };
                        out.push((to_id, 1, reason));
                    }
                }
                // Also look at edges TO this node (implements/injects from others).
                if let Ok(to_edges) = self.graph.traverse_to(n.id, None) {
                    for (from_id, kind) in to_edges {
                        let reason = match kind {
                            EdgeKind::Implements => ExpansionReason::ImplementsInterface,
                            EdgeKind::Injects => ExpansionReason::InjectedByTarget,
                            EdgeKind::ExchangesType => ExpansionReason::ExchangesTypeWithTarget,
                            EdgeKind::ReferencesRule => ExpansionReason::ReferencesRule,
                            EdgeKind::InheritsRule => ExpansionReason::InheritsRule,
                            EdgeKind::IsImplementedBy => ExpansionReason::ImplementsInterface,
                        };
                        out.push((from_id, 1, reason));
                    }
                }
                out
            })
            .collect();

        while let Some((node_id, depth, reason)) = frontier.pop() {
            if depth > max_depth || visited.contains(&node_id) {
                continue;
            }
            visited.insert(node_id);

            if let Ok(Some(node)) = self.graph.store().get_node(node_id) {
                let next_depth = depth + 1;
                if next_depth <= max_depth {
                    if let Ok(edges) = self.graph.traverse_edges(node_id, None) {
                        for (to_id, _kind) in edges {
                            frontier.push((to_id, next_depth, reason.clone()));
                        }
                    }
                }
                expanded.push(ExpandedNode { node, reason });
            }

            // Pega rule-reference expansion from an expanded node's metadata
            // (T059): pulls in rules this expanded rule references/inherits,
            // even when no explicit edge was ingested.
            if let Some(last) = expanded.last() {
                if let Some(meta) = &last.node.pega_metadata {
                    let mut pega_followups: Vec<(CodeArtifactNode, ExpansionReason)> = Vec::new();
                    for reference in &meta.references_rules {
                        if let Some(node) = self.find_rule_by_reference(reference) {
                            if visited.insert(node.id) {
                                pega_followups.push((node, ExpansionReason::ReferencesRule));
                            }
                        }
                    }
                    if let Some(parent) = &meta.inherits_from {
                        if let Some(node) = self.find_rule_by_reference(parent) {
                            if visited.insert(node.id) {
                                pega_followups.push((node, ExpansionReason::InheritsRule));
                            }
                        }
                    }
                    for (node, reason) in pega_followups {
                        expanded.push(ExpandedNode { node, reason });
                    }
                }
            }
        }

        expanded
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

        output.push_str(&format!("## NeuroCode Context (tier: {})\n\n", tier));

        // Primary target.
        output.push_str("### Target\n");
        for n in primary.iter().take(budget.max_primary_nodes) {
            output.push_str(&format_node(n));
        }
        output.push('\n');

        // Expanded nodes — budget-limited.
        if !expanded.is_empty() {
            output.push_str("### Related Artifacts\n");
            for exp in expanded.iter().take(budget.max_expanded_nodes) {
                output.push_str(&format!("**{}** (via {:?})\n", exp.node.fqcn, exp.reason));
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

        let token_est = output.len() / 4; // rough estimate: ~4 chars/token
        (output, token_est)
    }

    /// Format a cold-mode (degraded) context (FR-016).
    fn format_cold_mode(&self, request: &CodingRequest) -> String {
        let mut out = String::new();
        out.push_str("## NeuroCode Cold-Mode Context\n\n");
        if let Some(file) = &request.active_file {
            out.push_str(&format!("Active file: {}\n", file));
            if let Ok(content) = std::fs::read_to_string(file) {
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

/// Format a single node for context output.
fn format_node(n: &CodeArtifactNode) -> String {
    let mut s = String::new();
    s.push_str(&format!("- **{}** ({})\n", n.fqcn, n.kind.as_str()));
    if !n.implemented_interfaces.is_empty() {
        s.push_str(&format!("  implements: {}\n", n.implemented_interfaces.join(", ")));
    }
    if !n.annotations.is_empty() {
        s.push_str(&format!("  annotations: @{}\n", n.annotations.join(" @")));
    }
    if !n.declared_dependencies.is_empty() {
        s.push_str(&format!("  dependencies: {}\n", n.declared_dependencies.join(", ")));
    }
    // Pega rule-system identity (FR-009/FR-005, T059): expose the rule
    // family, name, references, inheritance, and version to the model.
    if let Some(meta) = &n.pega_metadata {
        s.push_str(&format!("  pega: rule family={}, name={}\n", meta.rule_class_family.as_str(), meta.rule_name));
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

//! UI projection of an assembled context graph (feature 015 follow-up).
//!
//! [`ContextGraphSnapshot`] is a pure-data view of the primary + expanded
//! nodes (with member rosters, fan-in, and the edges among included nodes)
//! that [`super::ContextAssembler`] pulled in for a request. It carries
//! everything an interactive visualization needs and nothing that requires
//! graph-store access, so it can cross the `AgentEvent` boundary into the
//! TUI without dragging the SQLite store along.
//!
//! All strings are pre-rendered (kind tags, reason labels, via names) so
//! consumers never need `joey-neurocode` semantics to draw the graph.

use crate::graph::node::{ArtifactKind, CodeArtifactNode};
use crate::graph::{DependencyGraph, NodeId};

use super::{ExpandedNode, TierBudget};

/// One member (method/field) of an included type-level node.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemberSnapshot {
    /// Simple name (e.g. `findById`).
    pub name: String,
    /// `"method"` | `"field"`.
    pub kind: String,
    /// Declaration signature, when captured at index time.
    pub signature: String,
}

/// A node (artifact) in the snapshot, flattened for rendering.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodeSnapshot {
    pub id: NodeId,
    /// Fully-qualified canonical name.
    pub fqcn: String,
    /// Simple (unqualified) name.
    pub name: String,
    /// Artifact kind tag (`"Class"`, `"Interface"`, `"Enum"`, `"Method"`,
    /// `"Field"`, `"PegaRule"`).
    pub kind: String,
    pub package: String,
    pub source_path: String,
    /// Declaration signature for methods/fields, when captured.
    pub signature: Option<String>,
    pub annotations: Vec<String>,
    pub interfaces: Vec<String>,
    pub dependencies: Vec<String>,
    /// How many other artifacts depend on this one (blast radius).
    pub fan_in: usize,
    /// True for the request's primary target nodes.
    pub primary: bool,
    /// Why this node was included (expansion reason label such as
    /// `"injects"`); `None` for primaries.
    pub reason: Option<String>,
    /// Simple name of the included node whose edges pulled this one in.
    pub via: Option<String>,
    /// Graph distance from the primary target (0 for primaries).
    pub depth: usize,
    /// Method/field roster for type-level nodes.
    pub members: Vec<MemberSnapshot>,
}

impl NodeSnapshot {
    /// True when this is a type-level node (has a member roster).
    pub fn is_type_level(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "Class" | "Interface" | "Enum" | "PegaRule"
        )
    }
}

/// A typed edge between two included nodes, as indices into
/// [`ContextGraphSnapshot::nodes`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EdgeSnapshot {
    pub from: usize,
    pub to: usize,
    /// Edge kind tag (`"Implements"`, `"Injects"`, `"ExchangesType"`,
    /// `"MemberOf"`, `"ReferencesRule"`, `"InheritsRule"`,
    /// `"IsImplementedBy"`).
    pub kind: String,
}

/// Tier budget facts for the assembly (for the stats bar).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BudgetSnapshot {
    pub max_expansion_depth: usize,
    pub max_expanded_nodes: usize,
    /// Nodes found but excluded because the expanded-node budget was full.
    pub dropped_for_budget: usize,
}

/// The full interactive-visualization payload for one assembled context.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContextGraphSnapshot {
    /// Tier that served the request (e.g. `"Frontier"`).
    pub tier: String,
    /// Estimated tokens in the assembled context.
    pub token_estimate: usize,
    /// Whether the project was cold/un-indexed (degraded mode).
    pub cold_mode: bool,
    /// Primaries first, then expanded nodes in expansion (best-first) order.
    pub nodes: Vec<NodeSnapshot>,
    /// Edges among included nodes only.
    pub edges: Vec<EdgeSnapshot>,
    pub budget: BudgetSnapshot,
}

/// Cap on the member roster fetched per type-level node — the detail pane
/// is scrollable-free, so a bounded roster keeps the snapshot cheap.
const MEMBER_LIMIT: usize = 24;

impl ContextGraphSnapshot {
    /// Build the snapshot from an assembly result. Pure reads against the
    /// graph store; per-node lookup failures degrade to defaults (fan-in 0,
    /// empty roster) rather than failing the assembly.
    pub fn build(
        graph: &DependencyGraph,
        primary: &[CodeArtifactNode],
        expanded: &[ExpandedNode],
        tier: String,
        token_estimate: usize,
        budget: TierBudget,
        dropped_for_budget: usize,
    ) -> Self {
        let mut nodes: Vec<NodeSnapshot> = Vec::with_capacity(primary.len() + expanded.len());
        let mut index_of: std::collections::HashMap<NodeId, usize> =
            std::collections::HashMap::new();

        // Primaries: no reason/via, depth 0.
        for n in primary {
            index_of.insert(n.id, nodes.len());
            nodes.push(NodeSnapshot {
                id: n.id,
                fqcn: n.fqcn.clone(),
                name: n.simple_name().to_string(),
                kind: n.kind.as_str().to_string(),
                package: n.package.clone(),
                source_path: n.source_path.clone(),
                signature: n.signature.clone(),
                annotations: n.annotations.clone(),
                interfaces: n.implemented_interfaces.clone(),
                dependencies: n.declared_dependencies.clone(),
                fan_in: 0,
                primary: true,
                reason: None,
                via: None,
                depth: 0,
                members: Vec::new(),
            });
        }
        // Expanded: tagged with reason label + the including node's simple
        // name. `via` always references an already-included node (primary,
        // earlier expanded, or a member id — members resolve to None).
        for e in expanded {
            let via = e
                .via
                .and_then(|id| index_of.get(&id).copied())
                .map(|idx| nodes[idx].name.clone());
            index_of.insert(e.node.id, nodes.len());
            nodes.push(NodeSnapshot {
                id: e.node.id,
                fqcn: e.node.fqcn.clone(),
                name: e.node.simple_name().to_string(),
                kind: e.node.kind.as_str().to_string(),
                package: e.node.package.clone(),
                source_path: e.node.source_path.clone(),
                signature: e.node.signature.clone(),
                annotations: e.node.annotations.clone(),
                interfaces: e.node.implemented_interfaces.clone(),
                dependencies: e.node.declared_dependencies.clone(),
                fan_in: 0,
                primary: false,
                reason: Some(e.reason.label().to_string()),
                via,
                depth: e.depth,
                members: Vec::new(),
            });
        }

        // Enrich: fan-in + member rosters (type-level only). Store misses
        // degrade silently.
        for node in nodes.iter_mut() {
            if let Ok(fan_in) = graph.store().dependents_count(node.id) {
                node.fan_in = fan_in;
            }
            if node.is_type_level() {
                if let Ok(members) =
                    graph.store().members_of_enclosing(&node.name, MEMBER_LIMIT)
                {
                    node.members = members
                        .iter()
                        .map(|m| MemberSnapshot {
                            name: m.simple_name().to_string(),
                            kind: match m.kind {
                                ArtifactKind::Method => "method".to_string(),
                                ArtifactKind::Field => "field".to_string(),
                                _ => "member".to_string(),
                            },
                            signature: m.signature.clone().unwrap_or_default(),
                        })
                        .collect();
                }
            }
        }

        // Edges among included nodes: one outgoing traversal per node.
        let mut edges: Vec<EdgeSnapshot> = Vec::new();
        for (idx, node) in nodes.iter().enumerate() {
            if let Ok(out) = graph.traverse_edges(node.id, None) {
                for (to_id, kind) in out {
                    if let Some(&to_idx) = index_of.get(&to_id) {
                        edges.push(EdgeSnapshot {
                            from: idx,
                            to: to_idx,
                            kind: kind.as_str().to_string(),
                        });
                    }
                }
            }
        }

        ContextGraphSnapshot {
            tier,
            token_estimate,
            cold_mode: false,
            nodes,
            edges,
            budget: BudgetSnapshot {
                max_expansion_depth: budget.max_expansion_depth,
                max_expanded_nodes: budget.max_expanded_nodes,
                dropped_for_budget,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_snapshot_type_level_helper() {
        let mut n = NodeSnapshot {
            kind: "Class".into(),
            ..Default::default()
        };
        assert!(n.is_type_level());
        n.kind = "Method".into();
        assert!(!n.is_type_level());
    }
}

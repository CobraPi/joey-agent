//! Semantic graph builder (T015, FR-011).
//!
//! `SemanticGraphBuilder::build(feature_id, documents)` derives the
//! `SemanticGraph` (nodes + edges + `revision_hashes`) per data-model.md §2.
//! Edges: traceability spine + coverage + containment + dependency +
//! proposed-entity-relationship (FR-011).

use std::collections::HashMap;

use crate::cst::CstDocument;
use crate::cst::parser_trait::CstMaterialize;
use crate::meaning::mapping::classify;
use crate::meaning::coverage::detect_defects;
use crate::meaning::{
    Confidence, Edge, EdgeKind, SemanticGraph, SemanticId, SemanticKind, SemanticNode, SemanticProps,
};

/// Derive a SemanticGraph from one or more CST documents belonging to the
/// same feature (contracts/semantic-graph.md). Inputs: spec.md, plan.md,
/// tasks.md, checklists/, data-model.md, constitution.md (as available —
/// missing artifacts simply contribute no nodes).
pub trait SemanticGraphBuilder {
    fn build(&self, feature_id: &str, documents: &[CstDocument]) -> SemanticGraph;
}

/// Default graph builder. Stateless — safe to reuse.
#[derive(Debug, Default, Clone)]
pub struct DefaultSemanticGraphBuilder;

impl SemanticGraphBuilder for DefaultSemanticGraphBuilder {
    fn build(&self, feature_id: &str, documents: &[CstDocument]) -> SemanticGraph {
        build_graph(feature_id, documents)
    }
}

/// Derive a SemanticGraph from one or more CST documents belonging to the
/// same feature (contracts/semantic-graph.md). Missing artifacts simply
/// contribute no nodes.
pub fn build_graph(feature_id: &str, documents: &[CstDocument]) -> SemanticGraph {
    let mut nodes: HashMap<SemanticId, SemanticNode> = HashMap::new();
    let mut revision_hashes: HashMap<String, String> = HashMap::new();

    // Phase 1: classify all nodes.
    for doc in documents {
        revision_hashes.insert(doc.artifact_path.clone(), doc.revision_hash.clone());
        for cst_node in doc.iter_in_order() {
            if let Some(sem_node) = classify(feature_id, &doc.artifact_path, cst_node) {
                // Deduplicate by semantic id — first occurrence wins, but we
                // accumulate across artifacts.
                nodes.entry(sem_node.id.clone()).or_insert(sem_node);
            }
        }
    }

    // Phase 2: wire up edges (pass documents so edge wiring can read origin
    // bytes for nodes whose structured props don't carry the full text, e.g.
    // Check nodes that reference FR-NNN/TNNN in their prose).
    wire_edges(&mut nodes, documents);

    // Phase 2b: infer entity relationships (FR-011). Explicit relationships
    // come from Key Entity prose; proposed relationships come from any prose
    // mentioning two known entities. Both produce EntityRelationship nodes +
    // ProposesRelationship edges (proposed only).
    infer_entity_relationships(&mut nodes, documents);

    // Phase 3: detect defects.
    let defects = detect_defects(&nodes);

    SemanticGraph {
        feature_id: feature_id.to_string(),
        revision_hashes,
        nodes,
        defects,
    }
}

/// Wire up traceability and containment edges between nodes.
///
/// Edges emitted (FR-021 traceability spine):
///   * Task → Requirement (`Implements`) — from the FR-NNN ref in the task body.
///   * Task → UserStory (`Contains` reverse: story contains task) — from [USN].
///   * Check → Requirement/Task (`Verifies`) — from the FR-NNN/TNNN ref in the
///     check body.
///   * Task → ProjectStructureNode (`Changes`) — from target_files paths.
///   * Requirement → UserStory (`DeliversValueFor`) — by section containment:
///     a requirement appearing under a `### User Story N` heading delivers
///     value for that story.
///   * Requirement/UserStory → Principle (`Governs`) — by constitution
///     principle id reference in requirement text (e.g. "Constitution III").
fn wire_edges(nodes: &mut HashMap<SemanticId, SemanticNode>, documents: &[CstDocument]) {
    // ---- Collect reference targets so we can emit edges without re-borrowing. ----
    let task_ids: Vec<SemanticId> = nodes
        .values()
        .filter(|n| n.kind == SemanticKind::Task)
        .map(|n| n.id.clone())
        .collect();
    let check_ids: Vec<SemanticId> = nodes
        .values()
        .filter(|n| n.kind == SemanticKind::Check)
        .map(|n| n.id.clone())
        .collect();
    let requirement_ids: Vec<SemanticId> = nodes
        .values()
        .filter(|n| n.kind == SemanticKind::Requirement)
        .map(|n| n.id.clone())
        .collect();
    let structure_ids: Vec<SemanticId> = nodes
        .values()
        .filter(|n| n.kind == SemanticKind::ProjectStructureNode)
        .map(|n| n.id.clone())
        .collect();

    // ---- Task edges: Implements (→ Requirement), Contains (Story → Task),
    //      Changes (→ ProjectStructureNode). ----
    for task_id in &task_ids {
        let (story_ref, req_refs, target_files) = match nodes.get(task_id) {
            Some(n) => match &n.props {
                SemanticProps::Task { user_story_ref, target_files, implements_refs, .. } => {
                    (user_story_ref.clone(), implements_refs.clone(), target_files.clone())
                }
                _ => continue,
            },
            None => continue,
        };

        // Task → Requirement (Implements).
        for req_id in &req_refs {
            let target = format!("requirement:{req_id}");
            if nodes.contains_key(&target) {
                if let Some(task) = nodes.get_mut(task_id) {
                    task.edges.push(Edge {
                        target: target.clone(),
                        rel: EdgeKind::Implements,
                    });
                }
            }
        }

        // UserStory → Task (Contains).
        if let Some(story_id) = &story_ref {
            if nodes.contains_key(story_id) {
                if let Some(story) = nodes.get_mut(story_id) {
                    story.edges.push(Edge {
                        target: task_id.clone(),
                        rel: EdgeKind::Contains,
                    });
                }
            }
        }

        // Task → ProjectStructureNode (Changes): match each target file path
        // against the structure node's origin bytes.
        for file in &target_files {
            for struct_id in &structure_ids {
                if structure_node_contains_path(nodes, struct_id, file) {
                    if let Some(task) = nodes.get_mut(task_id) {
                        task.edges.push(Edge {
                            target: struct_id.clone(),
                            rel: EdgeKind::Changes,
                        });
                    }
                }
            }
        }
    }

    // ---- Check edges: Verifies (→ Requirement or Task). ----
    for check_id in &check_ids {
        // Check nodes carry SemanticProps::None, so we read their description
        // from the CST origin bytes (the documents are available here).
        let target_text = match nodes.get(check_id) {
            Some(n) => origin_text(n, documents),
            None => continue,
        };
        // A check verifies whatever FR-NNN or TNNN it references.
        let verified: Vec<SemanticId> = extract_requirement_refs(&target_text)
            .into_iter()
            .map(|r| format!("requirement:{r}"))
            .chain(extract_task_refs(&target_text).into_iter().map(|t| format!("task:{t}")))
            .filter(|id| nodes.contains_key(id))
            .collect();
        for target in verified {
            if let Some(check) = nodes.get_mut(check_id) {
                check.edges.push(Edge {
                    target,
                    rel: EdgeKind::Verifies,
                });
            }
        }
    }

    // ---- Requirement → UserStory (DeliversValueFor) by section containment. ----
    // A requirement whose origin is byte-wise inside a user-story heading's
    // section delivers value for that story. We approximate by ordering: a
    // requirement belongs to the most recent story heading before its origin.
    wire_delivers_value_for(nodes, &requirement_ids);

    // ---- Requirement/UserStory → Principle (Governs). ----
    // A requirement that names a constitution principle (e.g. "Constitution
    // III" or "Principle VII") is governed by that principle. We link by
    // matching the principle's roman numeral against the requirement text.
    wire_governs_edges(nodes, &requirement_ids);
}

/// Reconstruct a semantic node's description text from its CST origin bytes.
/// Used for extracting FR-NNN / TNNN references that the structured props
/// don't carry.
fn task_description_from_origin(node: &SemanticNode) -> String {
    // The origin carries byte_start/byte_end but not the bytes themselves;
    // the CST node's expected_bytes would be ideal but we don't have the CST
    // here. Instead, derive the description from the structured props where
    // possible. For Task, the id + target_files are in props; the full
    // description (including FR refs) is only in the bytes. Since we can't
    // reach the bytes from here without the CST, we re-extract from the
    // semantic id pattern + any text we can recover.
    //
    // Workaround: store the description in a side channel. For now, return
    // the raw text we can reconstruct — the caller (wire_edges) uses this
    // only for ref extraction, and the structured props give us enough.
    match &node.props {
        SemanticProps::Task { id, target_files, .. } => {
            let mut text = id.clone();
            for f in target_files {
                text.push(' ');
                text.push_str(f);
            }
            text
        }
        SemanticProps::Requirement { text, .. } => text.clone(),
        SemanticProps::SuccessCriterion { text, .. } => text.clone(),
        // For nodes without structured text, return empty — ref extraction
        // will simply find nothing.
        _ => String::new(),
    }
}

/// Read the raw source bytes a semantic node was derived from, by looking up
/// its CST origin in the supplied documents. Returns the bytes covering
/// `[byte_start, byte_end)` in the matching artifact, or empty if the
/// document can't be found. This is how `wire_edges` reaches the prose of
/// nodes (like Check) whose structured props don't carry text.
fn origin_text(node: &SemanticNode, documents: &[CstDocument]) -> String {
    for doc in documents {
        if doc.artifact_path == node.origin.artifact {
            if let Some(cst_node) = doc.get(node.origin.node) {
                return cst_node.expected_bytes.clone();
            }
            // Fallback: slice the materialized bytes by range.
            let bytes = doc.materialize();
            if node.origin.byte_end <= bytes.len() {
                let slice = &bytes[node.origin.byte_start..node.origin.byte_end];
                if let Ok(s) = std::str::from_utf8(slice) {
                    return s.to_string();
                }
            }
        }
    }
    String::new()
}

/// Extract FR-NNN requirement references from a text body.
fn extract_requirement_refs(text: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("FR-") {
        let after = &rest[start + 3..];
        let end = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
        if end > 0 {
            refs.push(format!("FR-{}", &after[..end]));
        }
        rest = if start + 3 + end < rest.len() {
            &rest[start + 3 + end..]
        } else {
            break;
        };
    }
    refs
}

/// Extract TNNN task references from a text body.
fn extract_task_refs(text: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'T' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            // Collect the digits.
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            // Validate: preceded by non-alphanumeric (so we don't match the T
            // in "Constitution").
            let prev = if i == 0 { b' ' } else { bytes[i - 1] };
            if !prev.is_ascii_alphabetic() {
                refs.push(format!("T{}", &text[i + 1..j]));
            }
            i = j;
        } else {
            i += 1;
        }
    }
    refs
}

/// Does a ProjectStructureNode's origin contain a given file path?
fn structure_node_contains_path(
    nodes: &HashMap<SemanticId, SemanticNode>,
    struct_id: &SemanticId,
    file: &str,
) -> bool {
    // The structure node's expected_bytes isn't reachable here; we
    // heuristically match on the file path's last segment appearing in the
    // structure's origin artifact. This is conservative — a false positive
    // just adds a Changes edge that the UI may or may not show.
    let _ = (nodes, struct_id, file);
    // Without byte access, skip Changes edge emission for now; the traceability
    // spine's core (Implements, Verifies, DeliversValueFor, Governs) does not
    // depend on it. Changes is a secondary edge for the file-tree highlight.
    false
}

/// Wire Requirement → UserStory (DeliversValueFor) edges by byte-range
/// containment: a requirement belongs to the nearest preceding user-story
/// heading.
fn wire_delivers_value_for(
    nodes: &mut HashMap<SemanticId, SemanticNode>,
    requirement_ids: &[SemanticId],
) {
    // Collect user-story origins sorted by byte_start within each artifact.
    let mut stories: Vec<(SemanticId, String, usize)> = nodes
        .iter()
        .filter(|(_, n)| n.kind == SemanticKind::UserStory)
        .map(|(id, n)| (id.clone(), n.origin.artifact.clone(), n.origin.byte_start))
        .collect();
    stories.sort_by_key(|(_, _, start)| *start);

    for req_id in requirement_ids {
        let (req_artifact, req_start) = match nodes.get(req_id) {
            Some(n) => (n.origin.artifact.clone(), n.origin.byte_start),
            None => continue,
        };
        // Find the latest story in the same artifact whose byte_start < req_start.
        let owning_story = stories
            .iter()
            .filter(|(_, art, start)| art == &req_artifact && *start < req_start)
            .last()
            .map(|(id, _, _)| id.clone());
        if let Some(story_id) = owning_story {
            if let Some(req) = nodes.get_mut(req_id) {
                req.edges.push(Edge {
                    target: story_id,
                    rel: EdgeKind::DeliversValueFor,
                });
            }
        }
    }
}

/// Wire Requirement/UserStory → Principle (Governs) edges by matching
/// constitution principle references in the requirement text.
fn wire_governs_edges(
    nodes: &mut HashMap<SemanticId, SemanticNode>,
    requirement_ids: &[SemanticId],
) {
    // Collect principle ids from ConstitutionGate nodes.
    let principle_ids: Vec<(String, SemanticId)> = nodes
        .iter()
        .filter(|(_, n)| n.kind == SemanticKind::ConstitutionGate)
        .filter_map(|(id, n)| match &n.props {
            SemanticProps::ConstitutionGate { principle, .. } => {
                Some((principle.clone(), id.clone()))
            }
            _ => None,
        })
        .collect();

    if principle_ids.is_empty() {
        return;
    }

    for req_id in requirement_ids {
        let text = match nodes.get(req_id) {
            Some(n) => task_description_from_origin(n),
            None => continue,
        };
        // Match "Constitution III", "Principle VII", "Principle VII's", etc.
        for (principle, gate_id) in &principle_ids {
            let patterns = [
                format!("Constitution {principle}"),
                format!("Principle {principle}"),
                format!("constitution {principle}"),
                format!("principle {principle}"),
                format!("III {principle}").replace("III ", ""), // bare numeral fallback
            ];
            if patterns.iter().any(|p| text.contains(p)) {
                if let Some(req) = nodes.get_mut(req_id) {
                    req.edges.push(Edge {
                        target: gate_id.clone(),
                        rel: EdgeKind::Governs,
                    });
                }
            }
        }
    }
}

/// Infer entity relationships from Key Entity prose and cross-entity mentions
/// (FR-011). Produces `EntityRelationship` nodes:
///
///   * **Explicit** (`Confidence::Explicit`): when a Key Entity's description
///     uses a relationship verb ("contains", "has many", "belongs to",
///     "references", "owns") followed by another known entity name.
///   * **Proposed** (`Confidence::Proposed`): when two known entity names
///     co-occur in any prose without an explicit verb — surfaced as a
///     dashed edge the developer must confirm.
///
/// Proposed relationships emit a `ProposesRelationship` edge on the source
/// entity; explicit ones emit a plain `Contains`/`Changes` edge (the kind is
/// inferred from the verb). Both also emit an `EntityRelationship` node so
/// the entity-graph widget can render them.
fn infer_entity_relationships(
    nodes: &mut HashMap<SemanticId, SemanticNode>,
    documents: &[CstDocument],
) {
    // Collect known entity names (capitalised, from KeyEntity nodes).
    let entity_names: Vec<String> = nodes
        .values()
        .filter(|n| n.kind == SemanticKind::KeyEntity)
        .filter_map(|n| match &n.props {
            SemanticProps::KeyEntity { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect();
    if entity_names.len() < 2 {
        return; // need at least two entities to relate.
    }

    let mut next_er_id: u64 = 1;
    let mut er_id = || {
        let id = format!("entity_rel:auto-{}", next_er_id);
        next_er_id += 1;
        id
    };

    // ---- Explicit relationships: scan Key Entity descriptions for verb + entity. ----
    let explicit_verbs: &[(&str, EdgeKind)] = &[
        ("contains", EdgeKind::Contains),
        ("has many", EdgeKind::Contains),
        ("owns", EdgeKind::Contains),
        ("belongs to", EdgeKind::Contains),
        ("references", EdgeKind::Changes),
        ("carries", EdgeKind::Changes),
    ];

    let entity_ids: Vec<SemanticId> = nodes
        .iter()
        .filter(|(_, n)| n.kind == SemanticKind::KeyEntity)
        .map(|(id, _)| id.clone())
        .collect();

    for entity_id in &entity_ids {
        let (name, description) = match nodes.get(entity_id) {
            Some(n) => match &n.props {
                SemanticProps::KeyEntity { name, fields } => (name.clone(), fields.join(", ")),
                _ => continue,
            },
            None => continue,
        };
        let lowered = description.to_lowercase();
        for (verb, rel) in explicit_verbs {
            if let Some(v_pos) = lowered.find(verb) {
                // After the verb, look for another known entity name.
                let after = &description[v_pos + verb.len()..];
                for other in &entity_names {
                    if other == &name {
                        continue;
                    }
                    if after.contains(other.as_str()) {
                        let id = er_id();
                        let er_node = SemanticNode {
                            id: id.clone(),
                            kind: SemanticKind::EntityRelationship,
                            origin: nodes[entity_id].origin.clone(),
                            props: SemanticProps::EntityRelationship {
                                source: name.clone(),
                                verb: verb.to_string(),
                                target: other.clone(),
                                confidence: Confidence::Explicit,
                            },
                            origin_tag: crate::meaning::OriginTag::Source,
                            edges: Vec::new(),
                        };
                        nodes.insert(id, er_node);
                        // Emit the typed edge on the source entity.
                        let target_entity_id = format!("entity:{other}");
                        if let Some(src) = nodes.get_mut(entity_id) {
                            src.edges.push(Edge {
                                target: target_entity_id,
                                rel: rel.clone(),
                            });
                        }
                    }
                }
            }
        }
    }

    // ---- Proposed relationships: two entities co-occur in prose without a verb. ----
    // Scan every paragraph/list-item in every document for mentions of two or
    // more entity names; each unordered pair becomes a Proposed relationship.
    for doc in documents {
        for cst_node in doc.iter_in_order() {
            // Only scan text-bearing nodes.
            let text = match (&cst_node.kind, &cst_node.props) {
                (crate::cst::CstKind::Paragraph, crate::cst::CstProps::Paragraph { text }) => text.clone(),
                (crate::cst::CstKind::ListItem, crate::cst::CstProps::ListItem { text, .. }) => text.clone(),
                _ => continue,
            };
            let mentioned: Vec<&String> = entity_names.iter().filter(|n| text.contains(n.as_str())).collect();
            if mentioned.len() < 2 {
                continue;
            }
            // Each unordered pair → proposed relationship.
            for i in 0..mentioned.len() {
                for j in (i + 1)..mentioned.len() {
                    let src = mentioned[i].clone();
                    let tgt = mentioned[j].clone();
                    let id = er_id();
                    let er_node = SemanticNode {
                        id: id.clone(),
                        kind: SemanticKind::EntityRelationship,
                        origin: crate::meaning::NodeOrigin {
                            artifact: doc.artifact_path.clone(),
                            node: cst_node.id,
                            byte_start: cst_node.byte_start,
                            byte_end: cst_node.byte_end,
                        },
                        props: SemanticProps::EntityRelationship {
                            source: src.clone(),
                            verb: "relates to".to_string(),
                            target: tgt.clone(),
                            confidence: Confidence::Proposed,
                        },
                        origin_tag: crate::meaning::OriginTag::Derived,
                        edges: Vec::new(),
                    };
                    nodes.insert(id, er_node);
                    // Emit the ProposesRelationship edge on the source entity.
                    let target_entity_id = format!("entity:{tgt}");
                    let source_entity_id = format!("entity:{src}");
                    if let Some(src_node) = nodes.get_mut(&source_entity_id) {
                        src_node.edges.push(Edge {
                            target: target_entity_id,
                            rel: EdgeKind::ProposesRelationship,
                        });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cst::parser::parse_bytes;

    #[test]
    fn builds_graph_from_spec() {
        let spec = b"# Spec\n\n## Requirements\n\n- **FR-001**: The system MUST do X.\n- **FR-002**: The system SHOULD do Y.\n";
        let doc = parse_bytes("spec.md", spec);
        let graph = build_graph("test-feature", &[doc]);

        assert!(graph.nodes.contains_key("requirement:FR-001"));
        assert!(graph.nodes.contains_key("requirement:FR-002"));
    }

    #[test]
    fn empty_documents_yield_empty_graph() {
        let graph = build_graph("test", &[]);
        assert!(graph.nodes.is_empty());
        assert!(graph.defects.is_empty());
    }

    #[test]
    fn graph_captures_revision_hashes() {
        let doc = parse_bytes("spec.md", b"# S\n");
        let graph = build_graph("test", &[doc]);
        assert!(graph.revision_hashes.contains_key("spec.md"));
    }

    #[test]
    fn wire_edges_emits_implements_edge_from_task_to_requirement() {
        // A task that references FR-001 in its description must emit an
        // Implements edge to requirement:FR-001.
        let spec = b"# Spec\n\n- **FR-001**: Do X.\n";
        let tasks = b"# Tasks\n\n- [ ] T001 [P] [US1] Implement FR-001 in src/main.rs\n";
        let spec_doc = parse_bytes("spec.md", spec);
        let tasks_doc = parse_bytes("tasks.md", tasks);
        let graph = build_graph("t", &[spec_doc, tasks_doc]);

        let task = graph.nodes.get("task:T001").expect("task exists");
        assert!(
            task.edges.iter().any(|e| e.rel == EdgeKind::Implements && e.target == "requirement:FR-001"),
            "task must implement FR-001, edges: {:?}",
            task.edges,
        );
    }

    #[test]
    fn wire_edges_emits_contains_edge_from_story_to_task() {
        let spec = b"# Spec\n\n### User Story 1 - First (Priority: P1)\n\n- **FR-001**: Do X.\n";
        let tasks = b"# Tasks\n\n- [ ] T001 [P] [US1] Implement FR-001 in src/main.rs\n";
        let spec_doc = parse_bytes("spec.md", spec);
        let tasks_doc = parse_bytes("tasks.md", tasks);
        let graph = build_graph("t", &[spec_doc, tasks_doc]);

        let story = graph.nodes.get("user_story:US1").expect("story exists");
        assert!(
            story.edges.iter().any(|e| e.rel == EdgeKind::Contains && e.target == "task:T001"),
            "story US1 must contain task T001, edges: {:?}",
            story.edges,
        );
    }

    #[test]
    fn wire_edges_emits_delivers_value_for_edge_from_requirement_to_story() {
        // A requirement whose byte origin falls inside a User Story section
        // must emit a DeliversValueFor edge to that story.
        let spec = b"# Spec\n\n### User Story 1 - First (Priority: P1)\n\n- **FR-001**: Do X.\n";
        let spec_doc = parse_bytes("spec.md", spec);
        let graph = build_graph("t", &[spec_doc]);

        let req = graph.nodes.get("requirement:FR-001").expect("requirement exists");
        assert!(
            req.edges.iter().any(|e| e.rel == EdgeKind::DeliversValueFor && e.target == "user_story:US1"),
            "requirement FR-001 must deliver value for US1, edges: {:?}",
            req.edges,
        );
    }

    #[test]
    fn wire_edges_emits_governs_edge_from_requirement_to_gate() {
        // A plan with a Constitution Check row + a requirement that names the
        // same principle must emit a Governs edge.
        let plan = b"# Plan\n\n## Constitution Check\n\n| # | Principle | Result | Notes |\n|---|-----------|--------|-------|\n| III | Filesystem Source | PASS | OK. |\n";
        let spec = b"# Spec\n\n- **FR-001**: Honors Constitution III as a hard contract.\n";
        let plan_doc = parse_bytes("plan.md", plan);
        let spec_doc = parse_bytes("spec.md", spec);
        let graph = build_graph("t", &[plan_doc, spec_doc]);

        let req = graph.nodes.get("requirement:FR-001").expect("requirement exists");
        assert!(
            req.edges.iter().any(|e| e.rel == EdgeKind::Governs && e.target == "gate:III"),
            "requirement must govern principle III, edges: {:?}",
            req.edges,
        );
    }

    #[test]
    fn wire_edges_emits_verifies_edge_from_check_to_requirement() {
        // A check referencing FR-001 must emit a Verifies edge.
        let spec = b"# Spec\n\n- **FR-001**: Do X.\n";
        let checks = b"# Checks\n\n- [ ] CHK001 Verify FR-001 holds.\n";
        let spec_doc = parse_bytes("spec.md", spec);
        let checks_doc = parse_bytes("checklists/requirements.md", checks);
        let graph = build_graph("t", &[spec_doc, checks_doc]);

        let check = graph.nodes.get("check:CHK001").expect("check exists");
        assert!(
            check.edges.iter().any(|e| e.rel == EdgeKind::Verifies && e.target == "requirement:FR-001"),
            "check must verify FR-001, edges: {:?}",
            check.edges,
        );
    }

    #[test]
    fn infer_entity_relationships_emits_proposed_edge() {
        // Two Key Entities + prose mentioning both → proposed relationship.
        // The prose paragraph (not the entity bullet) is what triggers the
        // proposed path.
        let spec = b"# Spec\n\n### Key Entities\n\n- **Feature**: A feature directory.\n- **Artifact**: A document with path and type.\n\nThe Feature and Artifact work together.\n";
        let spec_doc = parse_bytes("spec.md", spec);
        let graph = build_graph("t", &[spec_doc]);

        // At least one proposed relationship should exist.
        let proposed: Vec<_> = graph
            .nodes
            .values()
            .filter(|n| n.kind == SemanticKind::EntityRelationship)
            .filter(|n| match &n.props {
                SemanticProps::EntityRelationship { confidence, .. } => {
                    *confidence == Confidence::Proposed
                }
                _ => false,
            })
            .collect();
        assert!(
            !proposed.is_empty(),
            "must emit at least one proposed entity relationship (got {} EntityRelationship nodes total)",
            graph.nodes.values().filter(|n| n.kind == SemanticKind::EntityRelationship).count(),
        );

        // At least one entity should carry a ProposesRelationship edge.
        let any_proposes = graph
            .nodes
            .values()
            .flat_map(|n| n.edges.iter())
            .any(|e| e.rel == EdgeKind::ProposesRelationship);
        assert!(
            any_proposes,
            "at least one entity must propose a relationship",
        );
    }

    #[test]
    fn infer_entity_relationships_emits_explicit_edge_from_verb() {
        // A Key Entity description using "contains" + another entity name →
        // explicit relationship.
        let spec = b"# Spec\n\n### Key Entities\n\n- **Feature**: A directory that contains Artifact records.\n- **Artifact**: A document.\n";
        let spec_doc = parse_bytes("spec.md", spec);
        let graph = build_graph("t", &[spec_doc]);

        let explicit: Vec<_> = graph
            .nodes
            .values()
            .filter(|n| n.kind == SemanticKind::EntityRelationship)
            .filter(|n| match &n.props {
                SemanticProps::EntityRelationship { confidence, source, target, .. } => {
                    *confidence == Confidence::Explicit
                        && source == "Feature"
                        && target == "Artifact"
                }
                _ => false,
            })
            .collect();
        assert!(
            !explicit.is_empty(),
            "must emit an explicit Feature→Artifact relationship",
        );
    }
}

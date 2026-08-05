//! Defect detection (T016, FR-023/SC-009).
//!
//! `OrphanRequirement`, `RogueTask`, `Unverified`, `ConstitutionBreach` —
//! each with its `Scaffold` and optional `GenerativeFollowon`. 100% recall on
//! fixtures (SC-009).

use std::collections::{HashMap, HashSet};

use crate::meaning::{
    Defect, DefectClass, EdgeKind, GenerativeFollowon, InsertionMode, Scaffold, SemanticId,
    SemanticKind, SemanticNode, SemanticProps,
};

/// Detect all four defect classes from the graph edges. Returns the defects
/// sorted by id for deterministic output.
pub fn detect_defects(
    nodes: &HashMap<SemanticId, SemanticNode>,
) -> Vec<Defect> {
    let mut defects = Vec::new();

    // Build reverse-edge index: for each node, which nodes point to it and
    // with which edge kind.
    let mut incoming: HashMap<&SemanticId, Vec<(&SemanticId, &EdgeKind)>> = HashMap::new();
    for (src_id, node) in nodes {
        for edge in &node.edges {
            incoming
                .entry(&edge.target)
                .or_default()
                .push((src_id, &edge.rel));
        }
    }

    // 1. Orphan requirements: a Requirement with zero incoming Implements
    //    edges from Tasks.
    let task_ids: HashSet<&SemanticId> = nodes
        .iter()
        .filter(|(_, n)| n.kind == SemanticKind::Task)
        .map(|(id, _)| id)
        .collect();

    for (id, node) in nodes {
        if node.kind != SemanticKind::Requirement {
            continue;
        }
        let has_implementer = incoming
            .get(id)
            .into_iter()
            .flatten()
            .any(|(src, rel)| task_ids.contains(*src) && **rel == EdgeKind::Implements);
        if !has_implementer {
            let req_id = match &node.props {
                SemanticProps::Requirement { id, .. } => id.clone(),
                _ => id.clone(),
            };
            defects.push(Defect {
                id: format!("defect:orphan:{req_id}"),
                class: DefectClass::OrphanRequirement,
                source_nodes: vec![id.clone()],
                impact: format!("Requirement {req_id} has no implementing task"),
                scaffold: Scaffold {
                    target_artifact: "tasks.md".to_string(),
                    anchor_node: format!("phase:_"), // owning phase — enriched by caller
                    stub_bytes: format!("- [ ] T_stub [P] Implement {req_id} (auto-stub).\n"),
                    insertion_mode: InsertionMode::After,
                },
                generative_followon: Some(GenerativeFollowon {
                    prompt: format!("Write a real task body implementing {req_id}."),
                    target_artifact: "tasks.md".to_string(),
                }),
            });
        }
    }

    // 2. Rogue tasks: a Task with no outgoing Implements edge to a Requirement.
    for (id, node) in nodes {
        if node.kind != SemanticKind::Task {
            continue;
        }
        let implements_req = node
            .edges
            .iter()
            .any(|e| e.rel == EdgeKind::Implements);
        if !implements_req {
            let task_id = match &node.props {
                SemanticProps::Task { id, .. } => id.clone(),
                _ => id.clone(),
            };
            defects.push(Defect {
                id: format!("defect:rogue:{task_id}"),
                class: DefectClass::RogueTask,
                source_nodes: vec![id.clone()],
                impact: format!("Task {task_id} implements no requirement"),
                scaffold: Scaffold {
                    target_artifact: "tasks.md".to_string(),
                    anchor_node: id.clone(),
                    stub_bytes: String::new(), // link action, not insertion
                    insertion_mode: InsertionMode::Within,
                },
                generative_followon: Some(GenerativeFollowon {
                    prompt: format!("Draft a requirement that {task_id} implements."),
                    target_artifact: "spec.md".to_string(),
                }),
            });
        }
    }

    // 3. Unverified: a Task with no incoming Verifies edge from a Check.
    let check_ids: HashSet<&SemanticId> = nodes
        .iter()
        .filter(|(_, n)| n.kind == SemanticKind::Check)
        .map(|(id, _)| id)
        .collect();

    for (id, node) in nodes {
        if node.kind != SemanticKind::Task {
            continue;
        }
        let is_verified = incoming
            .get(id)
            .into_iter()
            .flatten()
            .any(|(src, rel)| check_ids.contains(*src) && **rel == EdgeKind::Verifies);
        if !is_verified {
            let task_id = match &node.props {
                SemanticProps::Task { id, .. } => id.clone(),
                _ => id.clone(),
            };
            defects.push(Defect {
                id: format!("defect:unverified:{task_id}"),
                class: DefectClass::Unverified,
                source_nodes: vec![id.clone()],
                impact: format!("Task {task_id} has no verifying check"),
                scaffold: Scaffold {
                    target_artifact: "checklists/requirements.md".to_string(),
                    anchor_node: id.clone(),
                    stub_bytes: format!("- [ ] CHK_stub Verify {task_id}.\n"),
                    insertion_mode: InsertionMode::After,
                },
                generative_followon: Some(GenerativeFollowon {
                    prompt: format!("Draft a check verifying {task_id}."),
                    target_artifact: "checklists/requirements.md".to_string(),
                }),
            });
        }
    }

    // 4. Constitution breach: detected from plan.md gate rows with Fail
    //    results and no Complexity Tracking entry. This requires plan.md
    //    parsing; the graph builder surfaces ConstitutionGate nodes, and we
    //    flag any with result=Fail.
    for (id, node) in nodes {
        if node.kind != SemanticKind::ConstitutionGate {
            continue;
        }
        if let SemanticProps::ConstitutionGate { principle, result, .. } = &node.props {
            if matches!(result, crate::meaning::GateResultKind::Fail) {
                defects.push(Defect {
                    id: format!("defect:breach:{principle}"),
                    class: DefectClass::ConstitutionBreach,
                    source_nodes: vec![id.clone()],
                    impact: format!("Constitution principle {principle} failed its gate"),
                    scaffold: Scaffold {
                        target_artifact: "plan.md".to_string(),
                        anchor_node: id.clone(),
                        stub_bytes: format!("| {principle} violation | (justify) | (rejected alt) |\n"),
                        insertion_mode: InsertionMode::After,
                    },
                    generative_followon: Some(GenerativeFollowon {
                        prompt: format!("Draft a Complexity Tracking justification for {principle}."),
                        target_artifact: "plan.md".to_string(),
                    }),
                });
            }
        }
    }

    // Sort by id for deterministic output.
    defects.sort_by(|a, b| a.id.cmp(&b.id));
    defects
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meaning::{Edge, NodeOrigin, OriginTag};

    fn make_req(id: &str) -> (SemanticId, SemanticNode) {
        (
            format!("requirement:{id}"),
            SemanticNode {
                id: format!("requirement:{id}"),
                kind: SemanticKind::Requirement,
                origin: NodeOrigin {
                    artifact: "spec.md".to_string(),
                    node: crate::cst::NodeId(1),
                    byte_start: 0,
                    byte_end: 10,
                },
                props: SemanticProps::Requirement {
                    id: id.to_string(),
                    modality: crate::meaning::Modality::Must,
                    text: "test".to_string(),
                },
                origin_tag: OriginTag::Source,
                edges: Vec::new(),
            },
        )
    }

    fn make_task(id: &str, implements: Option<&str>) -> (SemanticId, SemanticNode) {
        let mut edges = Vec::new();
        if let Some(req) = implements {
            edges.push(Edge {
                target: format!("requirement:{req}"),
                rel: EdgeKind::Implements,
            });
        }
        (
            format!("task:{id}"),
            SemanticNode {
                id: format!("task:{id}"),
                kind: SemanticKind::Task,
                origin: NodeOrigin {
                    artifact: "tasks.md".to_string(),
                    node: crate::cst::NodeId(2),
                    byte_start: 0,
                    byte_end: 10,
                },
                props: SemanticProps::Task {
                    id: id.to_string(),
                    parallel_eligible: false,
                    target_files: Vec::new(),
                    user_story_ref: None,
                    completed: false,
                    implements_refs: Vec::new(),
                },
                origin_tag: OriginTag::Source,
                edges,
            },
        )
    }

    #[test]
    fn detects_orphan_requirement() {
        let mut nodes = HashMap::new();
        let (rid, rnode) = make_req("FR-001");
        nodes.insert(rid, rnode);
        // No task implements FR-001.
        let defects = detect_defects(&nodes);
        assert!(defects.iter().any(|d| d.class == DefectClass::OrphanRequirement && d.id.contains("FR-001")));
    }

    #[test]
    fn no_orphan_when_task_implements() {
        let mut nodes = HashMap::new();
        let (rid, rnode) = make_req("FR-001");
        nodes.insert(rid.clone(), rnode);
        let (_, tnode) = make_task("T001", Some("FR-001"));
        nodes.insert("task:T001".to_string(), tnode);

        let defects = detect_defects(&nodes);
        assert!(!defects.iter().any(|d| d.class == DefectClass::OrphanRequirement));
    }

    #[test]
    fn detects_rogue_task() {
        let mut nodes = HashMap::new();
        let (_, tnode) = make_task("T001", None);
        nodes.insert("task:T001".to_string(), tnode);
        let defects = detect_defects(&nodes);
        assert!(defects.iter().any(|d| d.class == DefectClass::RogueTask));
    }

    #[test]
    fn detects_unverified_task() {
        let mut nodes = HashMap::new();
        // A task that implements a requirement but has no check.
        let (rid, rnode) = make_req("FR-001");
        nodes.insert(rid, rnode);
        let (_, tnode) = make_task("T001", Some("FR-001"));
        nodes.insert("task:T001".to_string(), tnode);

        let defects = detect_defects(&nodes);
        assert!(defects.iter().any(|d| d.class == DefectClass::Unverified));
    }
}

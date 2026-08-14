//! T059 — Pega context assembly integration test.
//!
//! Seed a graph with two Pega rule nodes connected by a ReferencesRule edge,
//! assemble context, verify the referenced rule appears with the
//! ReferencesRule expansion reason and the formatted context contains the
//! rule family/name/version (FR-009/FR-005).

use std::path::PathBuf;

use joey_neurocode::classifier::ComplexityTier;
use joey_neurocode::context::{ContextAssembler, ExpansionReason};
use joey_neurocode::engine::CodingRequest;
use joey_neurocode::graph::edge::EdgeKind;
use joey_neurocode::graph::node::{ArtifactKind, CodeArtifactNode};
use joey_neurocode::graph::DependencyGraph;
use joey_neurocode::pega::metadata::{PegaMetadata, PegaRuleFamily};

fn pega_rule_node(fqcn: &str, package: &str, source: &str) -> CodeArtifactNode {
    let mut node = CodeArtifactNode::new(
        ArtifactKind::PegaRule,
        fqcn.to_string(),
        package.to_string(),
        source.to_string(),
    );
    node.pega_metadata = Some(PegaMetadata {
        rule_class_family: PegaRuleFamily::RuleObj,
        rule_name: fqcn.rsplit('.').next().unwrap_or(fqcn).to_string(),
        references_rules: Vec::new(),
        inherits_from: None,
        pega_version: "8.8.0".to_string(),
    });
    node
}

/// Seed two Pega rule nodes connected by a ReferencesRule edge:
/// `com.pega.rules.MyActivity` → references → `Rule-Obj-Flow`.
fn seed_graph(graph: &DependencyGraph) {
    let mut activity = pega_rule_node(
        "com.pega.rules.MyActivity",
        "com.pega.rules",
        "src/MyActivity.java",
    );
    activity.pega_metadata.as_mut().unwrap().references_rules =
        vec!["Rule-Obj-Flow".to_string()];

    let flow = pega_rule_node("Rule-Obj-Flow", "Rule-Obj", "src/RuleObjFlow.java");

    let activity_id = graph.upsert_node(&activity).unwrap();
    let flow_id = graph.upsert_node(&flow).unwrap();
    graph
        .upsert_edge(activity_id, flow_id, EdgeKind::ReferencesRule)
        .unwrap();
}

fn make_request(symbol: &str) -> CodingRequest {
    CodingRequest {
        text: format!("refactor {}", symbol),
        active_file: Some("src/MyActivity.java".into()),
        active_symbols: vec![symbol.to_string()],
        project_root: PathBuf::from("."),
        token_budget_hint: 0,
    }
}

#[test]
fn referenced_rule_expanded_with_references_reason() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_graph(&graph);

    let assembler = ContextAssembler::new(&graph);
    let ctx = assembler.assemble(&make_request("MyActivity"), ComplexityTier::Frontier);

    assert!(!ctx.cold_mode);
    assert!(
        !ctx.primary_nodes.is_empty(),
        "primary node should be located via active symbol"
    );

    // The referenced Rule-Obj-Flow node should be expanded with the
    // ReferencesRule reason.
    let flow_exp = ctx
        .expanded_nodes
        .iter()
        .find(|e| e.node.fqcn == "Rule-Obj-Flow")
        .expect("referenced rule should be expanded into context");
    assert_eq!(
        flow_exp.reason,
        ExpansionReason::ReferencesRule,
        "referenced rule should be tagged ReferencesRule"
    );
}

#[test]
fn formatted_context_contains_rule_identity() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_graph(&graph);

    let assembler = ContextAssembler::new(&graph);
    let ctx = assembler.assemble(&make_request("MyActivity"), ComplexityTier::Frontier);

    let text = &ctx.formatted_context;
    assert!(!text.is_empty());
    // Rule family, rule name, references, and version (FR-009/FR-005).
    assert!(text.contains("RuleObj"), "family missing in:\n{}", text);
    assert!(text.contains("MyActivity"), "rule name missing in:\n{}", text);
    assert!(text.contains("Rule-Obj-Flow"), "referenced rule missing in:\n{}", text);
    assert!(text.contains("8.8.0"), "pega version missing in:\n{}", text);
}

#[test]
fn inherits_from_expanded_without_explicit_edge() {
    // T059: metadata-only expansion — no InheritsRule edge exists, the
    // assembler must still pull the parent rule in via FTS lookup.
    let graph = DependencyGraph::open_in_memory().unwrap();

    let mut child = pega_rule_node(
        "com.pega.rules.MyCase",
        "com.pega.rules",
        "src/MyCase.java",
    );
    child.pega_metadata.as_mut().unwrap().inherits_from =
        Some("Rule-Obj-CaseType".to_string());

    let parent = pega_rule_node("Rule-Obj-CaseType", "Rule-Obj", "src/RuleObjCaseType.java");

    graph.upsert_node(&child).unwrap();
    graph.upsert_node(&parent).unwrap();
    // No edge between them.

    let assembler = ContextAssembler::new(&graph);
    let ctx = assembler.assemble(&make_request("MyCase"), ComplexityTier::Frontier);

    let parent_exp = ctx
        .expanded_nodes
        .iter()
        .find(|e| e.node.fqcn == "Rule-Obj-CaseType")
        .expect("inherited rule should be expanded via metadata FTS lookup");
    assert_eq!(parent_exp.reason, ExpansionReason::InheritsRule);
    assert!(
        ctx.formatted_context.contains("inherits from: Rule-Obj-CaseType"),
        "inheritance should appear in formatted context:\n{}",
        ctx.formatted_context
    );
}

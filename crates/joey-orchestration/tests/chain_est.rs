//! T012 — remaining dependency-chain estimation (feature 033, US2).
//! Iterative topological walk: no recursion, 10k-deep graphs must not
//! overflow the stack; cycles estimate as zero.

use joey_orchestration::chain_est::{remaining_chain_edges, remaining_chain_estimate};
use joey_orchestration::evaluator::VerificationPlanView;
use joey_orchestration::task_graph::{
    TaskGraph, TaskId, TaskNode, TaskStatus, WorkerRole, IsolationMode, ModelTier, RiskLevel,
    AcceptanceCriterion,
};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn id(s: &str) -> TaskId {
    TaskId::new(s).expect("valid id")
}

fn node(id_str: &str, deps: &[&str]) -> TaskNode {
    TaskNode {
        id: id(id_str),
        objective: format!("do {}", id_str),
        dependencies: deps.iter().map(|d| id(d)).collect(),
        read_set: vec![],
        write_set: vec![],
        artifact_ids: vec![],
        role: WorkerRole::Implementor,
        model_tier: ModelTier::Economical,
        risk: RiskLevel::Low,
        acceptance: vec![AcceptanceCriterion {
            criterion: "tests pass".to_string(),
            kind: "command".to_string(),
        }],
        verification: VerificationPlanView::default(),
        isolation: IsolationMode::SharedCheckout,
        status: TaskStatus::Pending,
        attempts: 0,
    }
}

fn graph(nodes: Vec<TaskNode>) -> TaskGraph {
    TaskGraph {
        nodes: nodes.into_iter().map(|n| (n.id.clone(), n)).collect(),
        ..TaskGraph::default()
    }
}

#[test]
fn diamond_graph_longest_path_counts_edges() {
    // d depends on b and c; b and c depend on a.
    let g = graph(vec![node("a", &[]), node("b", &["a"]), node("c", &["a"]), node("d", &["b", "c"])]);
    assert_eq!(remaining_chain_edges(&g, &id("a")), 2); // a→b→d (or a→c→d)
    assert_eq!(remaining_chain_edges(&g, &id("b")), 1);
    assert_eq!(remaining_chain_edges(&g, &id("d")), 0);
}

#[test]
fn estimate_multiplies_by_chain_unit() {
    let g = graph(vec![node("a", &[]), node("b", &["a"])]);
    assert_eq!(remaining_chain_estimate(&g, &id("a"), 1000), 1000);
    assert_eq!(remaining_chain_estimate(&g, &id("b"), 1000), 0);
    assert_eq!(remaining_chain_estimate(&g, &id("a"), 250), 250);
}

#[test]
fn deep_chain_10k_does_not_recurse() {
    let mut nodes = Vec::with_capacity(10_000);
    nodes.push(node("t0", &[]));
    for i in 1..10_000 {
        nodes.push(node(&format!("t{}", i), &[&format!("t{}", i - 1)]));
    }
    let g = graph(nodes);
    assert_eq!(remaining_chain_edges(&g, &id("t0")), 9_999);
    assert_eq!(remaining_chain_edges(&g, &id("t9999")), 0);
}

#[test]
fn cyclic_graph_estimates_zero() {
    let g = graph(vec![node("a", &["b"]), node("b", &["a"])]);
    assert_eq!(remaining_chain_edges(&g, &id("a")), 0);
    assert_eq!(remaining_chain_edges(&g, &id("b")), 0);
}

#[test]
fn unknown_id_estimates_zero() {
    let g = graph(vec![node("a", &[])]);
    assert_eq!(remaining_chain_edges(&g, &id("zz")), 0);
    assert_eq!(remaining_chain_estimate(&g, &id("zz"), 1000), 0);
}

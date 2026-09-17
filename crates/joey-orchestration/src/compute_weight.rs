//! Orchestrator-assigned compute-pool weights (feature 033, US2).
//!
//! `weight_for_task` implements the contract formula (contracts/api.md §4):
//! `1.0 + 99.0 * (op_est + remaining_chain) / max_chain`, clamped to
//! [1, 100]. `max_chain` is the largest remaining-chain value across
//! currently known tasks, floored at ONE chain-unit so a single-task graph
//! divides by one full unit. `op_est` defaults to the chain-unit constant
//! — deterministic inputs only; adaptive EWMA feedback was explicitly
//! rejected for determinism (research.md D5).
//!
//! Weight honesty guardrail: weights are computed by the ORCHESTRATOR
//! only and are never accepted from LLM or subagent input.

use crate::chain_est::remaining_chain_estimate;
use crate::task_graph::{TaskGraph, TaskId};

/// Compute weight for task `id`: `1.0 + 99.0 * (op_est_ms +
/// remaining_chain_ms) / max_chain_ms`, clamped to [1.0, 100.0] via
/// `joey_compute::clamp_weight` (NaN-safe).
///
/// `op_est_ms` is the per-op cost estimate; pass the chain-unit constant
/// when no real timing signal exists (the default). `chain_unit_ms` is
/// `orchestration.compute.chain_unit_ms` (default 1000).
pub fn weight_for_task(
    graph: &TaskGraph,
    id: &TaskId,
    op_est_ms: u64,
    chain_unit_ms: u64,
) -> f64 {
    let unit = chain_unit_ms.max(1);
    let remaining = remaining_chain_estimate(graph, id, unit);
    // Largest remaining-chain across known tasks, floored at one unit.
    let max_chain = graph
        .nodes
        .keys()
        .map(|k| remaining_chain_estimate(graph, k, unit))
        .max()
        .unwrap_or(0)
        .max(unit);
    let raw = 1.0 + 99.0 * ((op_est_ms + remaining) as f64 / max_chain as f64);
    joey_compute::clamp_weight(raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::VerificationPlanView;
    use crate::task_graph::{
        AcceptanceCriterion, IsolationMode, ModelTier, RiskLevel, TaskNode, TaskStatus,
        WorkerRole,
    };

    fn tid(s: &str) -> TaskId {
        TaskId::new(s).expect("valid id")
    }

    fn node(id_str: &str, deps: &[&str]) -> TaskNode {
        TaskNode {
            id: tid(id_str),
            objective: format!("do {}", id_str),
            dependencies: deps.iter().map(|d| tid(d)).collect(),
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
    fn single_task_graph_gets_max_weight() {
        // max_chain floored at one unit; op_est = unit → ratio 1 → 100.
        let g = graph(vec![node("a", &[])]);
        assert_eq!(weight_for_task(&g, &tid("a"), 1000, 1000), 100.0);
    }

    #[test]
    fn deep_root_outranks_leaf() {
        let mut nodes = vec![node("t0", &[])];
        for i in 1..5 {
            nodes.push(node(&format!("t{}", i), &[&format!("t{}", i - 1)]));
        }
        let g = graph(nodes);
        let root = weight_for_task(&g, &tid("t0"), 1000, 1000);
        let leaf = weight_for_task(&g, &tid("t4"), 1000, 1000);
        assert!(root > leaf, "root {} should outrank leaf {}", root, leaf);
        assert_eq!(leaf, 25.75); // op only: 1 + 99*(1000/4000); max_chain = t0's 4 edges
    }

    #[test]
    fn weights_always_clamped_into_range() {
        let g = graph(vec![node("a", &[])]);
        assert!(weight_for_task(&g, &tid("a"), 0, 1000) >= 1.0);
        let w = weight_for_task(&g, &tid("a"), 1000, 0);
        assert!(w.is_finite() && w >= 1.0 && w <= 100.0);
    }
}

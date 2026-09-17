//! Remaining dependency-chain estimation for compute-pool weights
//! (feature 033, contracts/api.md §4).
//!
//! `remaining_chain_estimate` counts the LONGEST downstream dependency
//! chain (in edges) from a task — i.e. how much dependent work would be
//! held up if this task stalls — and multiplies by the chain-unit cost
//! (`orchestration.compute.chain_unit_ms`, default 1000 ms).
//!
//! Purely iterative (explicit work stack, memoized): 10k-deep chains
//! cannot overflow the call stack. Cycles estimate as zero (graphs are
//! pre-validated upstream anyway — defensive only).

use crate::task_graph::{TaskGraph, TaskId};
use std::collections::HashMap;

/// Longest downstream dependency-chain length, in EDGES, from `id`.
/// `depth(v) = 0` when nothing depends on `v`, else `1 + max(depth(u))`
/// over every task `u` that depends on `v`. Unknown ids and cycles → 0.
pub fn remaining_chain_edges(graph: &TaskGraph, id: &TaskId) -> usize {
    if !graph.nodes.contains_key(id) {
        return 0;
    }
    // Reverse adjacency: dep → tasks that depend on it.
    let mut dependents: HashMap<&TaskId, Vec<&TaskId>> = HashMap::new();
    // Reverse-graph in-degree = the node's own dependency count.
    let mut indegree: HashMap<&TaskId, usize> =
        graph.nodes.keys().map(|k| (k, 0usize)).collect();
    for (node_id, node) in &graph.nodes {
        for dep in &node.dependencies {
            dependents.entry(dep).or_default().push(node_id);
            *indegree.entry(node_id).or_insert(0) += 1;
        }
    }
    // Kahn's algorithm over the reverse graph: dependencies enter the
    // order before their dependents. Nodes on a cycle never reach
    // in-degree zero and are excluded — that IS the cycles→0 policy, and
    // it guarantees termination (no recursion, no re-push spin).
    let mut queue: Vec<&TaskId> = indegree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(k, _)| *k)
        .collect();
    let mut order: Vec<&TaskId> = Vec::with_capacity(graph.nodes.len());
    while let Some(v) = queue.pop() {
        order.push(v);
        if let Some(children) = dependents.get(v) {
            for u in children {
                let d = indegree.get_mut(u).unwrap();
                *d -= 1;
                if *d == 0 {
                    queue.push(u);
                }
            }
        }
    }
    // Longest dependent-chain depth, computed dependents-first (reverse
    // topological order). Excluded (cycle-member) dependents contribute
    // nothing — work behind a cycle is not schedulable remaining work.
    let mut memo: HashMap<&TaskId, usize> = HashMap::new();
    for v in order.iter().rev() {
        let depth = dependents
            .get(*v)
            .and_then(|cs| cs.iter().filter_map(|u| memo.get(*u).copied()).max())
            .map_or(0, |m| m + 1);
        memo.insert(*v, depth);
    }
    memo.get(id).copied().unwrap_or(0)
}

/// Remaining-chain estimate in milliseconds: longest downstream edge
/// count × `chain_unit_ms` (contracts/api.md §4).
pub fn remaining_chain_estimate(graph: &TaskGraph, id: &TaskId, chain_unit_ms: u64) -> u64 {
    (remaining_chain_edges(graph, id) as u64).saturating_mul(chain_unit_ms)
}

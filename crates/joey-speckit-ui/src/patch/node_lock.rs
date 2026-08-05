//! Per-node locking for concurrent edits (T095, FR-016 concurrency).
//!
//! When a developer edits a node while a run is touching the same file, the
//! edited node locks: the agent's output for that node diverts to the review
//! pane instead of being applied — the developer's intent is never clobbered
//! mid-thought (FR-016, spec Edge Cases). The lock is per-node, not per-file,
//! so unrelated nodes in the same file can still receive agent output into
//! staging.

use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::cst::NodeId;

/// Per-feature, per-node lock set. Tracks which nodes are currently being
/// edited by the developer (and thus must not receive agent output directly).
#[derive(Debug, Default)]
pub struct NodeLockSet {
    /// (feature_id, NodeId) pairs currently locked by developer edits.
    locked: Arc<RwLock<HashSet<(String, u64)>>>,
}

impl NodeLockSet {
    pub fn new() -> Self {
        NodeLockSet::default()
    }

    /// Lock a node for editing (called when the developer starts an edit).
    pub async fn lock(&self, feature_id: &str, node: NodeId) {
        self.locked
            .write()
            .await
            .insert((feature_id.to_string(), node.0 as u64));
    }

    /// Unlock a node (called when the developer's edit is applied or cancelled).
    pub async fn unlock(&self, feature_id: &str, node: NodeId) {
        self.locked
            .write()
            .await
            .remove(&(feature_id.to_string(), node.0 as u64));
    }

    /// Check if a node is currently locked (agent output should divert to staging).
    pub async fn is_locked(&self, feature_id: &str, node: NodeId) -> bool {
        self.locked
            .read()
            .await
            .contains(&(feature_id.to_string(), node.0 as u64))
    }

    /// Check if any node in a set is locked (for batch agent-output checks).
    pub async fn any_locked(&self, feature_id: &str, nodes: &[NodeId]) -> bool {
        let guard = self.locked.read().await;
        nodes.iter().any(|n| guard.contains(&(feature_id.to_string(), n.0 as u64)))
    }

    /// Get all locked nodes for a feature (for the review pane to show which
    /// agent output was diverted).
    pub async fn locked_nodes(&self, feature_id: &str) -> Vec<NodeId> {
        self.locked
            .read()
            .await
            .iter()
            .filter(|(fid, _)| fid == feature_id)
            .map(|(_, nid)| NodeId(*nid as u32))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lock_and_unlock() {
        let locks = NodeLockSet::new();
        assert!(!locks.is_locked("feat", NodeId(1)).await);
        locks.lock("feat", NodeId(1)).await;
        assert!(locks.is_locked("feat", NodeId(1)).await);
        locks.unlock("feat", NodeId(1)).await;
        assert!(!locks.is_locked("feat", NodeId(1)).await);
    }

    #[tokio::test]
    async fn per_node_not_per_file() {
        let locks = NodeLockSet::new();
        locks.lock("feat", NodeId(1)).await;
        // Node 2 in the same file is NOT locked.
        assert!(!locks.is_locked("feat", NodeId(2)).await);
        assert!(locks.any_locked("feat", &[NodeId(1), NodeId(2)]).await);
    }

    #[tokio::test]
    async fn per_feature_isolation() {
        let locks = NodeLockSet::new();
        locks.lock("feat-a", NodeId(1)).await;
        assert!(!locks.is_locked("feat-b", NodeId(1)).await);
    }
}

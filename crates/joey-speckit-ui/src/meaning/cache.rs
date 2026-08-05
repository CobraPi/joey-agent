//! In-memory semantic-graph cache (T017, FR-040, research.md §4).
//!
//! Holds the current `SemanticGraph` per open feature, invalidated by the
//! existing `watcher.rs` events. Lazy recompute on next read (≤400 ms budget).
//! Never persisted (Constitution III).

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::cst::CstDocument;
use crate::meaning::graph::{DefaultSemanticGraphBuilder, SemanticGraphBuilder};
use crate::meaning::SemanticGraph;

/// Per-feature cache state.
#[derive(Debug, Clone, Default)]
pub struct CacheEntry {
    pub graph: Option<Arc<SemanticGraph>>,
    pub stale: bool,
}

/// Thread-safe per-feature semantic-graph cache. Invalidated by watcher
/// events; recomputed lazily on next read.
#[derive(Debug, Default)]
pub struct SemanticCache {
    entries: RwLock<HashMap<String, CacheEntry>>,
    builder: DefaultSemanticGraphBuilder,
}

impl SemanticCache {
    pub fn new() -> Self {
        SemanticCache::default()
    }

    /// Mark a feature's graph as stale (called on watcher events). The next
    /// `get` will recompute.
    pub async fn invalidate(&self, feature_id: &str) {
        let mut entries = self.entries.write().await;
        entries
            .entry(feature_id.to_string())
            .or_default()
            .stale = true;
    }

    /// Drop a feature's entry entirely (on feature close).
    pub async fn remove(&self, feature_id: &str) {
        self.entries.write().await.remove(feature_id);
    }

    /// Get the graph for a feature, recomputing from the documents if stale
    /// or missing. The documents are supplied by the caller (which reads them
    /// from disk).
    pub async fn get_or_recompute(
        &self,
        feature_id: &str,
        documents: &[CstDocument],
    ) -> Arc<SemanticGraph> {
        // Fast path: check if we have a fresh entry.
        {
            let entries = self.entries.read().await;
            if let Some(entry) = entries.get(feature_id) {
                if !entry.stale {
                    if let Some(graph) = &entry.graph {
                        return graph.clone();
                    }
                }
            }
        }

        // Slow path: recompute.
        let graph = Arc::new(self.builder.build(feature_id, documents));
        let mut entries = self.entries.write().await;
        entries.insert(
            feature_id.to_string(),
            CacheEntry {
                graph: Some(graph.clone()),
                stale: false,
            },
        );
        graph
    }

    /// Force-set a graph (used by the WS stream after external recompute).
    pub async fn set(&self, feature_id: &str, graph: Arc<SemanticGraph>) {
        let mut entries = self.entries.write().await;
        entries.insert(
            feature_id.to_string(),
            CacheEntry {
                graph: Some(graph),
                stale: false,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cache_recomputes_after_invalidation() {
        let cache = SemanticCache::new();
        let doc = crate::cst::parser::parse_bytes("spec.md", b"# S\n\n- **FR-001**: x.\n");

        let g1 = cache.get_or_recompute("feat", &[doc.clone()]).await;
        assert!(g1.nodes.contains_key("requirement:FR-001"));

        cache.invalidate("feat").await;

        // Different document — should recompute and reflect new content.
        let doc2 = crate::cst::parser::parse_bytes("spec.md", b"# S\n\n- **FR-002**: y.\n");
        let g2 = cache.get_or_recompute("feat", &[doc2]).await;
        assert!(g2.nodes.contains_key("requirement:FR-002"));
        assert!(!g2.nodes.contains_key("requirement:FR-001"));
    }

    #[tokio::test]
    async fn cache_returns_fresh_entry_without_recompute() {
        let cache = SemanticCache::new();
        let doc = crate::cst::parser::parse_bytes("spec.md", b"# S\n");

        let _ = cache.get_or_recompute("feat", &[doc.clone()]).await;
        // Second call with empty docs should still return the cached graph
        // (fresh, not stale).
        let g = cache.get_or_recompute("feat", &[]).await;
        assert!(g.nodes.is_empty()); // matches first compute
    }
}

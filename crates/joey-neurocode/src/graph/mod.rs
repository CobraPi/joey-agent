//! Graph types: nodes, edges, and the SQLite-backed dependency graph.

pub mod edge;
pub mod node;
pub mod store;

pub use edge::EdgeKind;
pub use node::{ArtifactKind, ArtifactStatus, CodeArtifactNode};
pub use store::{
    project_graph_db_path, DomainFtsRow, DomainSourceRow, GraphStore, SCHEMA_VERSION,
};

use std::path::Path;


/// Internal primary key for a code artifact (SQLite rowid).
pub type NodeId = u64;

/// A typed relationship between two code artifacts (FR-004).
#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub from_id: NodeId,
    pub to_id: NodeId,
    pub edge_kind: EdgeKind,
}

/// The dependency graph: typed-edge graph over CodeArtifactNodes (FR-004).
///
/// Thin wrapper around [`GraphStore`] providing graph-traversal semantics.
pub struct DependencyGraph {
    store: GraphStore,
}

impl DependencyGraph {
    /// Open (or create) the graph at the given path.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Ok(Self {
            store: GraphStore::open(path)?,
        })
    }

    /// Open an in-memory graph (testing).
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Ok(Self {
            store: GraphStore::open_in_memory()?,
        })
    }

    /// Open (or create) the per-project graph at the resolved
    /// `~/.joey/neurocode/projects/<hash>/graph.db` path (T014).
    pub fn open_for_project(project_root: &Path) -> rusqlite::Result<Self> {
        Self::open(&project_graph_db_path(project_root))
    }

    /// Borrow the underlying store.
    pub fn store(&self) -> &GraphStore {
        &self.store
    }

    /// Upsert a node. Returns the rowid.
    pub fn upsert_node(&self, node: &CodeArtifactNode) -> rusqlite::Result<NodeId> {
        self.store.upsert_node(node)
    }

    /// Upsert a typed edge (idempotent).
    pub fn upsert_edge(&self, from: NodeId, to: NodeId, kind: EdgeKind) -> rusqlite::Result<()> {
        self.store.upsert_edge(from, to, kind)
    }

    /// FTS5 search over artifact symbols.
    pub fn query_fts(&self, query: &str, limit: usize) -> rusqlite::Result<Vec<CodeArtifactNode>> {
        self.store.query_fts(query, limit)
    }

    /// Traverse edges from a node, optionally filtered by kind.
    pub fn traverse_edges(
        &self,
        from: NodeId,
        kind_filter: Option<EdgeKind>,
    ) -> rusqlite::Result<Vec<(NodeId, EdgeKind)>> {
        self.store.traverse_from(from, kind_filter)
    }

    /// Count active artifacts.
    pub fn artifact_count(&self) -> rusqlite::Result<usize> {
        self.store.artifact_count()
    }

    /// Traverse edges TO a node, optionally filtered by kind.
    /// Returns `(from_id, edge_kind)` pairs.
    pub fn traverse_to(
        &self,
        to: NodeId,
        kind_filter: Option<EdgeKind>,
    ) -> rusqlite::Result<Vec<(NodeId, EdgeKind)>> {
        self.store.traverse_to(to, kind_filter)
    }
}

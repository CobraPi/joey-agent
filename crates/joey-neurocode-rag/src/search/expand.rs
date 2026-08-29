//! Result expansion: context window + relationship-aware retrieval.
//!
//! Context expansion (T026, FR-006 — contracts/hybrid-search.md stage 7)
//! reads the file from disk at query time, `± expand_context_lines`
//! clamped to file boundaries with a `context_absent` note (never an
//! error) for missing files (R4).
//!
//! Bounded BFS relation expansion (T029, FR-007 — stage 8) follows
//! `rag_chunk_edges` (fully derived at index time, data-model.md §7 —
//! the typed graph stays authoritative) to depth ≤
//! `neurocode.rag.relation_max_depth` (=2), dedups by `chunk_id`
//! (visited set), and marks each expanded item with its `relation_kind`.
//! Expanded items are appended AFTER fused results WITHOUT displacing
//! them: they ride the per-result `relations` field (rendered after the
//! context block by the CLI/TUI), never reordering or rescoring the
//! fused list.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::Connection;

/// The hard BFS-depth cap = `neurocode.rag.relation_max_depth`'s clamp
/// maximum (`crate::config::RELATION_MAX_DEPTH_MAX`, contract-pinned 2 —
/// single-sourced from the config module so the surfaces can't drift).
pub const RELATION_DEPTH_CAP: u8 = crate::config::RELATION_MAX_DEPTH_MAX as u8;

// ─── T026: context expansion (contracts/hybrid-search.md stage 7) ────────────

/// Read the chunk's file from disk at query time and return the
/// `± expand` lines around it, clamped to file start/end (FR-006; R4).
///
/// `None` for a missing/unreadable file — the `context_absent` note,
/// NEVER an error (the result keeps its rank; only the context block is
/// absent). 1-based inclusive `start_line`/`end_line` as stored in
/// `rag_chunks`. Behavior is byte-identical to the T015 inline form this
/// replaced (moved, not changed — the T015 tests stay green as-is).
pub fn expand_context(
    project_root: &Path,
    file: &str,
    start_line: u32,
    end_line: u32,
    expand: u32,
) -> Option<String> {
    let text = std::fs::read_to_string(project_root.join(file)).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len() as u32;
    if total == 0 {
        return Some(String::new());
    }
    let from = start_line.saturating_sub(expand).clamp(1, total);
    let to = end_line.saturating_add(expand).clamp(1, total);
    if from > to {
        return Some(String::new());
    }
    Some(lines[(from - 1) as usize..to as usize].join("\n"))
}

// ─── T029: bounded BFS relation expansion (stage 8, FR-007) ──────────────────

/// One related chunk discovered by [`expand_relations`]: the
/// `rag_chunks` row denormalized (file/symbol — enough to act without a
/// join) plus the `relation_kind` of the discovery edge (the existing
/// typed-graph vocabulary: `Implements`, `Injects`, … — data-model.md
/// §7). The caller maps this onto its per-result relation payload.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpandedRelation {
    pub chunk_id: String,
    pub file: String,
    pub symbol: Option<String>,
    /// The `rag_chunk_edges.edge_kind` of the BFS discovery edge (the
    /// FIRST edge reaching the chunk — deterministic under BFS + the
    /// ordered neighbor query).
    pub relation_kind: String,
}

/// Bounded BFS over `rag_chunk_edges` from `seed_chunk_id` (FR-007:
/// "up to a bounded depth, deduplicated").
///
/// - **Depth bound**: `max_depth` is clamped to [`RELATION_DEPTH_CAP`]
///   (2, = `neurocode.rag.relation_max_depth`'s maximum); depth 1 =
///   direct edges, depth 2 = edges-of-edges. Nothing beyond the bound
///   is ever returned, whatever `max_depth` says.
/// - **Dedup**: a visited `HashSet<chunk_id>` — every chunk is
///   discovered at most once, across ALL paths (a diamond graph's
///   common descendant appears once).
/// - **Non-displacement** (FR-007): `excluded` (the fused results'
///   chunk ids, supplied by the pipeline) seeds the visited set, so
///   already-ranked chunks are never reported as relations — expanded
///   items are additive, appended after the fused list's own entries,
///   and the fused order/scores are untouched by construction (this
///   function never sees them). Traversal does not pass THROUGH
///   excluded chunks either: an excluded chunk surfaces its own
///   neighborhood via its own result's expansion, so nothing is lost.
/// - **Direction**: outgoing edges only (`from_chunk_id` = current).
///   The edge vocabulary carries inverse kinds (`IsImplementedBy`) as
///   their own rows (data-model.md §7), so inverse relations are
///   reachable without walking incoming edges.
/// - **Determinism**: neighbors are visited in `(to_chunk_id,
///   edge_kind)` order; output is BFS discovery order. A dangling edge
///   (target missing from `rag_chunks`) is skipped — never an error.
pub fn expand_relations(
    conn: &Connection,
    seed_chunk_id: &str,
    max_depth: u8,
    excluded: &HashSet<String>,
) -> Vec<ExpandedRelation> {
    let max_depth = max_depth.min(RELATION_DEPTH_CAP);
    if max_depth == 0 {
        return Vec::new();
    }
    let mut visited: HashSet<String> = excluded.clone();
    visited.insert(seed_chunk_id.to_string());

    let mut out: Vec<ExpandedRelation> = Vec::new();
    // (chunk_id, depth it was discovered at) — BFS frontier.
    let mut frontier: Vec<(String, u8)> = vec![(seed_chunk_id.to_string(), 0)];
    while let Some((current, depth)) = frontier.pop_front() {
        if depth >= max_depth {
            continue; // neighbors would exceed the bound.
        }
        for rel in outgoing_relations(conn, &current) {
            if visited.contains(&rel.chunk_id) {
                continue;
            }
            visited.insert(rel.chunk_id.clone());
            frontier.push((rel.chunk_id.clone(), depth + 1));
            out.push(rel);
        }
    }
    out
}

/// Ordered outgoing edges of one chunk, denormalized against
/// `rag_chunks` (inner join drops dangling edges). Deterministic order:
/// `to_chunk_id`, then `edge_kind`.
fn outgoing_relations(conn: &Connection, from_chunk_id: &str) -> Vec<ExpandedRelation> {
    let Ok(mut stmt) = conn.prepare(
        "SELECT e.to_chunk_id, c.source_path, c.symbol_name, e.edge_kind \
         FROM rag_chunk_edges e JOIN rag_chunks c ON c.chunk_id = e.to_chunk_id \
         WHERE e.from_chunk_id = ?1 ORDER BY e.to_chunk_id, e.edge_kind",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(rusqlite::params![from_chunk_id], |row| {
        Ok(ExpandedRelation {
            chunk_id: row.get(0)?,
            file: row.get(1)?,
            symbol: row.get(2)?,
            relation_kind: row.get(3)?,
        })
    }) else {
        return Vec::new();
    };
    rows.filter_map(Result::ok).collect()
}

// Small Vec-dequeue helper (avoids a `VecDeque` import at both use sites).
trait PopFront {
    fn pop_front(&mut self) -> Option<(String, u8)>;
}

impl PopFront for Vec<(String, u8)> {
    fn pop_front(&mut self) -> Option<(String, u8)> {
        if self.is_empty() {
            None
        } else {
            Some(self.remove(0))
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use joey_neurocode::graph::GraphStore;

    // ─── T026: context expansion ─────────────────────────────────────────

    fn write_lines(root: &Path, rel: &str, n: usize) {
        let body: String = (1..=n).map(|i| format!("line{i}\n")).collect();
        let full = root.join(rel);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, body).unwrap();
    }

    /// A chunk at the very start of the file: the window's leading edge
    /// clamps at line 1 (never negative, never a panic).
    #[test]
    fn t026_context_clamps_at_file_start() {
        let tmp = tempfile::tempdir().unwrap();
        write_lines(tmp.path(), "s.py", 10);
        // Chunk lines 1-2, ±5 → from = max(1-5,1) = 1, to = min(2+5,10) = 7.
        let ctx = expand_context(tmp.path(), "s.py", 1, 2, 5).unwrap();
        assert_eq!(ctx, "line1\nline2\nline3\nline4\nline5\nline6\nline7");
    }

    /// A chunk at the very end of the file: the trailing edge clamps at
    /// the last line (never past EOF).
    #[test]
    fn t026_context_clamps_at_file_end() {
        let tmp = tempfile::tempdir().unwrap();
        write_lines(tmp.path(), "s.py", 10);
        // Chunk lines 9-10, ±3 → from = 6, to = min(13,10) = 10.
        let ctx = expand_context(tmp.path(), "s.py", 9, 10, 3).unwrap();
        assert_eq!(ctx, "line6\nline7\nline8\nline9\nline10");
    }

    /// Mid-file chunk with a window that fits: no clamping, exact ±.
    #[test]
    fn t026_context_window_exact_mid_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_lines(tmp.path(), "s.py", 20);
        // Chunk lines 10-11, ±2 → lines 8..13.
        let ctx = expand_context(tmp.path(), "s.py", 10, 11, 2).unwrap();
        assert_eq!(ctx, "line8\nline9\nline10\nline11\nline12\nline13");
    }

    /// A window larger than the file clamps to the WHOLE file (both
    /// edges clamp simultaneously).
    #[test]
    fn t026_window_larger_than_file_clamps_to_whole_file() {
        let tmp = tempfile::tempdir().unwrap();
        write_lines(tmp.path(), "s.py", 3);
        let ctx = expand_context(tmp.path(), "s.py", 2, 2, 200).unwrap();
        assert_eq!(ctx, "line1\nline2\nline3");
    }

    /// Missing file → `context_absent` (`None`), never an error and
    /// never a panic — the result itself is unaffected (FR-006).
    #[test]
    fn t026_missing_file_is_context_absent_never_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(expand_context(tmp.path(), "gone.rs", 1, 5, 3).is_none());
        // A directory where a file should be is also just absent.
        std::fs::create_dir_all(tmp.path().join("dir.rs")).unwrap();
        assert!(expand_context(tmp.path(), "dir.rs", 1, 5, 3).is_none());
    }

    // ─── T029: bounded BFS relation expansion ────────────────────────────

    fn temp_store() -> (tempfile::TempDir, GraphStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = GraphStore::open(&tmp.path().join("graph.db")).unwrap();
        (tmp, store)
    }

    /// Insert a symbol chunk (direct SQL — the chunker's edge derivation
    /// is T028's; here we control the graph precisely).
    fn chunk(store: &GraphStore, id: &str, path: &str, symbol: Option<&str>) {
        store
            .conn()
            .execute(
                "INSERT INTO rag_chunks (chunk_id, chunk_kind, source_path, start_line, \
                 end_line, symbol_name, symbol_kind, content_hash) \
                 VALUES (?1, 'symbol', ?2, 1, 10, ?3, 'class', 'h')",
                rusqlite::params![id, path, symbol],
            )
            .unwrap();
    }

    fn edge(store: &GraphStore, from: &str, to: &str, kind: &str) {
        store
            .conn()
            .execute(
                "INSERT INTO rag_chunk_edges (from_chunk_id, to_chunk_id, edge_kind) \
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![from, to, kind],
            )
            .unwrap();
    }

    fn ids(rels: &[ExpandedRelation]) -> Vec<&str> {
        rels.iter().map(|r| r.chunk_id.as_str()).collect()
    }

    /// Depth bound: a chain a→b→c→d from `a` — depth 1 finds only `b`;
    /// depth 2 finds `b`,`c`; NOTHING beyond the bound (`d` never
    /// appears). An over-large request clamps to the cap (2), and depth
    /// 0 expands nothing.
    #[test]
    fn t029_depth_bound_one_vs_two_differ_nothing_beyond() {
        let (_tmp, store) = temp_store();
        for (id, sym) in [("a", "A"), ("b", "B"), ("c", "C"), ("d", "D")] {
            chunk(&store, id, "src/mod.rs", Some(sym));
        }
        edge(&store, "a", "b", "Injects");
        edge(&store, "b", "c", "Injects");
        edge(&store, "c", "d", "Injects");
        let excluded: HashSet<String> = HashSet::new();

        let d1 = expand_relations(store.conn(), "a", 1, &excluded);
        assert_eq!(ids(&d1), vec!["b"], "depth 1: direct edge only");

        let d2 = expand_relations(store.conn(), "a", 2, &excluded);
        assert_eq!(ids(&d2), vec!["b", "c"], "depth 2 adds the edge-of-edges");

        // Nothing beyond the bound, even when asked (clamped to cap 2).
        let over = expand_relations(store.conn(), "a", 9, &excluded);
        assert_eq!(ids(&over), vec!["b", "c"], "clamped to RELATION_DEPTH_CAP");

        assert!(expand_relations(store.conn(), "a", 0, &excluded).is_empty());
        // Depth differs between 1 and 2 (the contract's observable).
        assert_ne!(ids(&d1), ids(&d2));
    }

    /// Dedup across paths (the diamond graph): a→b, a→c, b→d, c→d — `d`
    /// is reachable via BOTH `b` and `c` but appears EXACTLY once.
    #[test]
    fn t029_dedup_across_paths_diamond_graph() {
        let (_tmp, store) = temp_store();
        for (id, sym) in [("a", "A"), ("b", "B"), ("c", "C"), ("d", "D")] {
            chunk(&store, id, "src/mod.rs", Some(sym));
        }
        edge(&store, "a", "b", "Injects");
        edge(&store, "a", "c", "MemberOf");
        edge(&store, "b", "d", "Implements");
        edge(&store, "c", "d", "Injects");
        let excluded: HashSet<String> = HashSet::new();

        let rels = expand_relations(store.conn(), "a", 2, &excluded);
        assert_eq!(
            ids(&rels),
            vec!["b", "c", "d"],
            "BFS discovery order; d once despite two paths"
        );
        // No chunk_id duplicates, whatever the graph shape.
        let mut seen: Vec<&str> = ids(&rels);
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), ids(&rels).len(), "no duplicates");
        // d carries the FIRST discovery edge's kind (via b: Implements).
        let d = rels.iter().find(|r| r.chunk_id == "d").unwrap();
        assert_eq!(d.relation_kind, "Implements");
    }

    /// Expanded items are denormalized + kind-marked: file, symbol, and
    /// the discovery `relation_kind` ride the entry (FR-007 acceptance).
    #[test]
    fn t029_relations_carry_relation_kind_and_denormalized_target() {
        let (_tmp, store) = temp_store();
        chunk(&store, "iface", "src/iface.rs", Some("SessionStore"));
        chunk(&store, "impl", "src/impl.rs", Some("SessionStoreImpl"));
        edge(&store, "iface", "impl", "IsImplementedBy");
        let rels = expand_relations(store.conn(), "iface", 2, &HashSet::new());
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].file, "src/impl.rs");
        assert_eq!(rels[0].symbol.as_deref(), Some("SessionStoreImpl"));
        assert_eq!(rels[0].relation_kind, "IsImplementedBy");
    }

    /// Non-displacement half (unit level): chunks in `excluded` (the
    /// fused results) are never reported as relations, and traversal
    /// does not pass through them (their neighborhood is surfaced by
    /// their own result's expansion instead).
    #[test]
    fn t029_excluded_fused_chunks_never_reported_nor_traversed() {
        let (_tmp, store) = temp_store();
        for (id, sym) in [("a", "A"), ("b", "B"), ("c", "C")] {
            chunk(&store, id, "src/mod.rs", Some(sym));
        }
        edge(&store, "a", "b", "Injects");
        edge(&store, "b", "c", "Injects");
        // b is a fused result (excluded): not reported for a, and c
        // behind it is NOT reached through b.
        let excluded: HashSet<String> = ["b".to_string()].into_iter().collect();
        let rels = expand_relations(store.conn(), "a", 2, &excluded);
        assert!(rels.is_empty(), "excluded b blocks its subtree: {rels:?}");
        // Not excluded: c reachable as usual.
        let rels = expand_relations(store.conn(), "a", 2, &HashSet::new());
        assert_eq!(ids(&rels), vec!["b", "c"]);
    }

    /// Dangling edges (target chunk absent from `rag_chunks`) are
    /// skipped silently — never an error, never a phantom entry.
    #[test]
    fn t029_dangling_edges_skipped_never_an_error() {
        let (_tmp, store) = temp_store();
        chunk(&store, "a", "src/mod.rs", Some("A"));
        edge(&store, "a", "ghost", "Injects");
        let rels = expand_relations(store.conn(), "a", 2, &HashSet::new());
        assert!(rels.is_empty());
    }

    /// End-to-end non-displacement through the hybrid pipeline: the
    /// fused list's chunk order AND fused scores are IDENTICAL with
    /// relation_depth 0 vs 2; relations are appended per-result with
    /// their kinds, growing with depth (FR-007 acceptance 1–3).
    #[test]
    fn t029_pipeline_non_displacement_fused_order_and_scores_unchanged() {
        use crate::embed::profiles::NOMIC_EMBED_TEXT_V1_5;
        use crate::search::hybrid::{search_cli, SearchRequest};

        let (tmp, store) = temp_store();
        // Only the query symbol matches "Widget" — the related chunks
        // are reachable solely through the edges.
        chunk(&store, "core:1-10:symbol:Widget", "src/core.rs", Some("Widget"));
        chunk(&store, "impl:1-10:symbol:Gadget", "src/impl.rs", Some("Gadget"));
        chunk(&store, "deep:1-10:symbol:DeepHelper", "src/deep.rs", Some("DeepHelper"));
        edge(&store, "core:1-10:symbol:Widget", "impl:1-10:symbol:Gadget", "Injects");
        edge(&store, "impl:1-10:symbol:Gadget", "deep:1-10:symbol:DeepHelper", "MemberOf");

        let req = |depth: u8| SearchRequest {
            query: "Widget".to_string(),
            file_filter: None,
            limit: 10,
            expand_lines: 0,
            relation_depth: depth,
            include_fallback_chunks: true,
        };

        let base = search_cli(&store, tmp.path(), &NOMIC_EMBED_TEXT_V1_5, &req(0)).unwrap();
        let one = search_cli(&store, tmp.path(), &NOMIC_EMBED_TEXT_V1_5, &req(1)).unwrap();
        let two = search_cli(&store, tmp.path(), &NOMIC_EMBED_TEXT_V1_5, &req(2)).unwrap();

        // Fused list undisplaced: same chunks, same order, same scores.
        let key = |o: &crate::search::hybrid::SearchOutcome| {
            o.results
                .iter()
                .map(|r| (r.chunk_id.clone(), r.fused_score.to_bits()))
                .collect::<Vec<_>>()
        };
        assert_eq!(key(&base), key(&one), "depth 1 leaves the fused list untouched");
        assert_eq!(key(&base), key(&two), "depth 2 leaves the fused list untouched");

        // Depth 0: relations populated-but-empty is NOT the contract —
        // they are simply absent (empty) unless requested.
        assert!(base.results[0].relations.is_empty());

        // Depth 1: the direct relation, appended AFTER the fused result
        // (it rides the result; the fused list didn't grow).
        assert_eq!(one.results.len(), base.results.len());
        let rels1 = &one.results[0].relations;
        assert_eq!(rels1.len(), 1);
        assert_eq!(rels1[0].file, "src/impl.rs");
        assert_eq!(rels1[0].symbol.as_deref(), Some("Gadget"));
        assert_eq!(rels1[0].relation_kind, "Injects");

        // Depth 2: strictly more (the bounded frontier grew).
        let rels2 = &two.results[0].relations;
        assert_eq!(rels2.len(), 2);
        assert_eq!(rels2[1].file, "src/deep.rs");
        assert_eq!(rels2[1].relation_kind, "MemberOf");
    }
}

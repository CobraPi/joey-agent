//! T028 integration tests: `rag_chunk_edges` derivation at index time.
//!
//! Pins the contract obligations of task T028 (data-model.md §7 — the table
//! is FULLY DERIVED from the typed graph, which stays authoritative) against
//! a real `GraphStore` on a tempdir project:
//!
//! 1. Edge projection matches artifact-level edges — for every seeded
//!    `graph_edges` row, `rag_chunk_edges` contains exactly the cartesian
//!    pairs of the endpoint artifacts' symbol chunks, with the `edge_kind`
//!    string preserved VERBATIM across the full `EdgeKind` vocabulary.
//!    Edges whose endpoints have no indexed chunks project to nothing.
//! 2. Rebuild idempotence — re-indexing the same project yields the same
//!    edge set with no duplicates, and a NEW typed-graph edge appears after
//!    the next write (rebuilding the index rebuilds edges).
//! 3. Purge removes stale edges — purging a path's chunks inside a
//!    `write_index` drops every derived edge that referenced them (the
//!    no-FK-by-design sweep), while edges among surviving chunks remain,
//!    and no orphan endpoint ever survives.

use std::path::Path;

use joey_neurocode::graph::edge::EdgeKind;
use joey_neurocode::graph::{ArtifactKind, CodeArtifactNode, GraphStore};
use joey_neurocode::parse::registry::parse_any;

use joey_neurocode_rag::embed::profiles::default_profile;
use joey_neurocode_rag::index::chunker::{index_file, ChunkEmbedder, ChunkOptions};
use joey_neurocode_rag::vector::quantize::Quantization;
use joey_neurocode_rag::vector::store::write_index;

/// Deterministic echo embedder (dim checked by the pipeline).
struct EchoEmbedder;

impl ChunkEmbedder for EchoEmbedder {
    fn embed_texts(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        Ok(texts.iter().map(|_| vec![0.5f32; default_profile().dim as usize]).collect())
    }
}

// ── Fixture ────────────────────────────────────────────────────────────

const SHAPES_PY: &str = "\
class Shape:
    def area(self):
        return 0.0
";

const CIRCLE_PY: &str = "\
from shapes import Shape

class Circle(Shape):
    def area(self):
        return 3.14 * self.r * self.r

    def scale(self, factor):
        self.r = self.r * factor
";

/// The tempdir project: two Python files whose classes/methods become
/// symbol chunks wired to seeded artifacts.
fn temp_project() -> (tempfile::TempDir, GraphStore) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("shapes.py"), SHAPES_PY).unwrap();
    std::fs::write(tmp.path().join("circle.py"), CIRCLE_PY).unwrap();
    let store = GraphStore::open(&tmp.path().join("graph.db")).unwrap();
    (tmp, store)
}

fn seed_artifact(store: &GraphStore, kind: ArtifactKind, fqcn: &str, path: &str) -> u64 {
    store
        .upsert_node(&CodeArtifactNode::new(kind, fqcn.to_string(), String::new(), path.to_string()))
        .unwrap()
}

/// Artifacts + typed edges for the fixture. Returns
/// `(ids, seeded_edges)` where ids is keyed by name for assertions.
///
/// Edge plan (artifact-level, seeded via `upsert_edge`):
/// * Circle → Shape for **every kind in the vocabulary** — pins that ALL
///   seven `EdgeKind` strings survive projection verbatim.
/// * Shape.area → Shape (MemberOf) — a shapes-internal edge that must
///   SURVIVE the circle.py purge in test 3.
/// * Ghost → Shape (ReferencesRule) — Ghost has no indexed file, so the
///   edge must project to NOTHING (endpoints without chunks contribute no
///   rows).
fn seed_graph(store: &GraphStore) -> (Vec<(&'static str, u64)>, Vec<(u64, u64, &'static str)>) {
    let shape_ty = seed_artifact(store, ArtifactKind::Interface, "Shape", "shapes.py");
    let shape_area = seed_artifact(store, ArtifactKind::Method, "Shape.area", "shapes.py");
    let circle_ty = seed_artifact(store, ArtifactKind::Class, "Circle", "circle.py");
    let circle_area = seed_artifact(store, ArtifactKind::Method, "Circle.area", "circle.py");
    let circle_scale = seed_artifact(store, ArtifactKind::Method, "Circle.scale", "circle.py");
    let ghost = seed_artifact(store, ArtifactKind::PegaRule, "Ghost", "ghost.py");

    let mut seeded: Vec<(u64, u64, &'static str)> = Vec::new();
    for kind in [
        EdgeKind::Implements,
        EdgeKind::IsImplementedBy,
        EdgeKind::Injects,
        EdgeKind::ExchangesType,
        EdgeKind::MemberOf,
        EdgeKind::ReferencesRule,
        EdgeKind::InheritsRule,
    ] {
        store.upsert_edge(circle_ty, shape_ty, kind).unwrap();
        seeded.push((circle_ty, shape_ty, kind.as_str()));
    }
    store.upsert_edge(shape_area, shape_ty, EdgeKind::MemberOf).unwrap();
    seeded.push((shape_area, shape_ty, EdgeKind::MemberOf.as_str()));
    store.upsert_edge(ghost, shape_ty, EdgeKind::ReferencesRule).unwrap();
    seeded.push((ghost, shape_ty, EdgeKind::ReferencesRule.as_str()));

    (
        vec![
            ("shape_ty", shape_ty),
            ("shape_area", shape_area),
            ("circle_ty", circle_ty),
            ("circle_area", circle_area),
            ("circle_scale", circle_scale),
        ],
        seeded,
    )
}

/// Run the full `index_file` pipeline for one file of the project.
fn index_project_file(store: &GraphStore, root: &Path, rel: &str, purge_self: bool) -> usize {
    let abs = root.join(rel);
    let source = std::fs::read_to_string(&abs).unwrap();
    let mut extraction = parse_any(&abs, &source).unwrap().unwrap();
    extraction.populate_fallback_chunks(&source);
    let purge: Vec<&str> = if purge_self { vec![rel] } else { vec![] };
    let records = index_file(
        store,
        &extraction,
        &source,
        rel,
        &mut EchoEmbedder,
        default_profile(),
        Quantization::F32,
        &ChunkOptions::default(),
        &purge,
    )
    .unwrap();
    records.len()
}

// ── Read-back helpers (independent of the projection code under test) ──

/// The `rag_chunk_edges` TABLE contents, sorted — the assertion source of
/// truth (never the projection function).
fn stored_edges(store: &GraphStore) -> Vec<(String, String, String)> {
    let mut stmt = store
        .conn()
        .prepare("SELECT from_chunk_id, to_chunk_id, edge_kind FROM rag_chunk_edges ORDER BY 1, 2, 3")
        .unwrap();
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

/// Chunk ids wired to one artifact (first-principles projection input).
fn chunks_of_artifact(store: &GraphStore, artifact_id: u64) -> Vec<String> {
    let mut stmt = store
        .conn()
        .prepare("SELECT chunk_id FROM rag_chunks WHERE artifact_id = ?1 ORDER BY chunk_id")
        .unwrap();
    let rows = stmt
        .query_map(rusqlite::params![artifact_id as i64], |r| r.get::<_, String>(0))
        .unwrap();
    rows.collect::<Result<Vec<_>, _>>().unwrap()
}

/// Expected projection computed INDEPENDENTLY of the implementation: for
/// each seeded artifact-level edge, the cartesian product of the endpoint
/// artifacts' chunk sets, kind string preserved.
fn expected_edges(store: &GraphStore, seeded: &[(u64, u64, &str)]) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for (from, to, kind) in seeded {
        for fc in chunks_of_artifact(store, *from) {
            for tc in chunks_of_artifact(store, *to) {
                out.push((fc.clone(), tc, kind.to_string()));
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// No edge endpoint may reference a chunk absent from `rag_chunks`
/// (the no-FK-by-design orphan check).
fn assert_no_orphans(store: &GraphStore) {
    let orphan: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM rag_chunk_edges e
             WHERE e.from_chunk_id NOT IN (SELECT chunk_id FROM rag_chunks)
                OR e.to_chunk_id   NOT IN (SELECT chunk_id FROM rag_chunks)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(orphan, 0, "orphan rag_chunk_edges rows present");
}

// ── 1. Projection matches artifact-level edges ─────────────────────────

#[test]
fn edge_projection_matches_artifact_level_edges() {
    let (tmp, store) = temp_project();
    let (ids, seeded) = seed_graph(&store);
    let ids: std::collections::HashMap<&str, u64> = ids.into_iter().collect();

    index_project_file(&store, tmp.path(), "shapes.py", false);
    index_project_file(&store, tmp.path(), "circle.py", false);

    // Sanity: every seeded artifact with a file owns at least one chunk
    // (symbol chunks resolved their artifact FK at build time).
    for name in ["shape_ty", "shape_area", "circle_ty", "circle_area", "circle_scale"] {
        assert!(
            !chunks_of_artifact(&store, ids[name]).is_empty(),
            "{name} should own symbol chunks"
        );
    }

    let got = stored_edges(&store);
    let want = expected_edges(&store, &seeded);
    assert_eq!(got, want, "rag_chunk_edges must equal the artifact-level projection");

    // The full vocabulary round-trips verbatim (7 kinds on Circle→Shape).
    let kinds: std::collections::BTreeSet<&str> =
        got.iter().map(|(_, _, k)| k.as_str()).collect();
    for kind in [
        "Implements",
        "IsImplementedBy",
        "Injects",
        "ExchangesType",
        "MemberOf",
        "ReferencesRule",
        "InheritsRule",
    ] {
        assert!(kinds.contains(kind), "edge_kind '{kind}' must survive projection verbatim");
    }

    // Ghost (no indexed file) contributes nothing — its ReferencesRule edge
    // appears ONLY between real chunks, never involving ghost-only rows.
    assert!(!got.iter().any(|(f, t, _)| f.starts_with("ghost.py") || t.starts_with("ghost.py")));

    assert_no_orphans(&store);
}

// ── 2. Rebuild idempotence + graph-change pickup ───────────────────────

#[test]
fn rebuild_is_idempotent_and_tracks_graph_changes() {
    let (tmp, store) = temp_project();
    let (ids, seeded) = seed_graph(&store);
    let ids: std::collections::HashMap<&str, u64> = ids.into_iter().collect();

    index_project_file(&store, tmp.path(), "shapes.py", false);
    index_project_file(&store, tmp.path(), "circle.py", false);
    let first = stored_edges(&store);
    assert!(!first.is_empty());

    // Re-index the whole project (each write purges its own path first —
    // the realistic rebuild path). Same edges, no duplicates: the
    // (from, to, kind) PK plus rewrite-from-scratch makes it idempotent.
    index_project_file(&store, tmp.path(), "shapes.py", true);
    index_project_file(&store, tmp.path(), "circle.py", true);
    let second = stored_edges(&store);
    assert_eq!(first, second, "rebuild must reproduce the identical edge set");
    let unique: std::collections::HashSet<_> = second.iter().collect();
    assert_eq!(unique.len(), second.len(), "duplicate edge rows after rebuild");

    // Fully derived FROM THE GRAPH: adding a new typed edge and re-indexing
    // one file projects it (the graph stays authoritative).
    store
        .upsert_edge(ids["circle_scale"], ids["shape_area"], EdgeKind::ExchangesType)
        .unwrap();
    index_project_file(&store, tmp.path(), "circle.py", true);
    let mut with_new = seeded.clone();
    with_new.push((ids["circle_scale"], ids["shape_area"], EdgeKind::ExchangesType.as_str()));
    assert_eq!(stored_edges(&store), expected_edges(&store, &with_new));
    assert_no_orphans(&store);
}

// ── 3. Purge removes stale edges ───────────────────────────────────────

#[test]
fn purge_removes_stale_edges_for_purged_chunks() {
    let (tmp, store) = temp_project();
    let (_ids, seeded) = seed_graph(&store);

    index_project_file(&store, tmp.path(), "shapes.py", false);
    index_project_file(&store, tmp.path(), "circle.py", false);
    assert_eq!(stored_edges(&store), expected_edges(&store, &seeded));

    // Purge circle.py (empty write, purge-only — the write_index purge
    // path). Every edge touching a circle.py chunk must vanish; the
    // shapes-internal MemberOf edge must survive.
    write_index(&store, default_profile(), Quantization::F32, &[], &[], &["circle.py"]).unwrap();

    let after = stored_edges(&store);
    assert!(
        !after.iter().any(|(f, t, _)| f.starts_with("circle.py") || t.starts_with("circle.py")),
        "edges referencing purged chunks must be removed"
    );
    // Exactly the shapes-internal projection remains (Shape.area → Shape).
    let shapes_internal: Vec<(u64, u64, &str)> = seeded
        .iter()
        .filter(|(f, t, _)| is_shapes_artifact(&store, *f) && is_shapes_artifact(&store, *t))
        .copied()
        .collect();
    assert!(!shapes_internal.is_empty(), "fixture must keep a shapes-internal edge");
    assert_eq!(after, expected_edges(&store, &shapes_internal));
    assert_no_orphans(&store);

    // Purging the remaining file empties the table entirely.
    write_index(&store, default_profile(), Quantization::F32, &[], &[], &["shapes.py"]).unwrap();
    assert!(stored_edges(&store).is_empty());
    assert_no_orphans(&store);

    let _ = tmp; // keep the tempdir alive for the test body
}

fn is_shapes_artifact(store: &GraphStore, artifact_id: u64) -> bool {
    let path: Option<String> = store
        .conn()
        .query_row(
            "SELECT source_path FROM code_artifacts WHERE id = ?1",
            rusqlite::params![artifact_id as i64],
            |r| r.get(0),
        )
        .ok();
    path.as_deref() == Some("shapes.py")
}

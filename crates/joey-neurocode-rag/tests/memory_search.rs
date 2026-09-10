//! T009 — memory retrieval-leg integration tests (feature 027).
//!
//! Pins the `memory_search` public surface against real on-disk stores
//! (temp-dir `graph.db`, schema applied by `GraphStore::open`):
//!
//! 1. keyword/dense fusion ranks a clearly-matching episode in the top
//!    hits under keyword-only degradation (no embedder resolves — the
//!    degradation is silent, FR-008), and `memory_dense_scan` ranks the
//!    near-identical vector first;
//! 2. namespace isolation: memory legs never see rag_* rows and vice
//!    versa (zero cross-table reads/writes);
//! 3. two project roots isolate their stores (FR-012 / analyze C1);
//! 4. `index_memory_vector` BLOBs are byte-identical to the canonical
//!    rag quantize codec for both quantizations;
//! 5. empty corpus → empty results, no panic.
//!
//! No network, no ONNX artifacts anywhere: the dense leg of
//! `search_memory` degrades silently when `resolve` yields no backend,
//! and `memory_dense_scan` is driven directly with explicit vectors.

use std::sync::Mutex;

use joey_neurocode::graph::{project_graph_db_path, GraphStore};
use joey_neurocode::memory::{
    EpisodeKind, EpisodeOutcome, EpisodeSource, EpisodeStore, MemoryEpisode, PreferenceOrigin,
    PreferenceStore,
};

use joey_neurocode_rag::config::RagConfig;
use joey_neurocode_rag::memory_search::{
    index_memory_vector, memory_dense_scan, search_memory, MemorySearchRequest,
};
use joey_neurocode_rag::vector::quantize::{encode_f32, encode_int8};

/// Serialize tests that touch JOEY_HOME (cargo test runs threads in one
/// process) — same pattern as joey-neurocode's non_java_fallback.rs.
static HOME_LOCK: Mutex<()> = Mutex::new(());

// ─── fixtures ────────────────────────────────────────────────────────────────

const DIM: usize = 8;

/// Raw query vector handed to `memory_dense_scan` (normalized internally).
fn query_all_ones() -> Vec<f32> {
    vec![1.0f32; DIM]
}

/// Unit all-ones vector: cosine 1.0 (> 0.99) with [`query_all_ones`].
fn all_ones_normalized() -> Vec<f32> {
    vec![1.0f32 / (DIM as f32).sqrt(); DIM]
}

/// Unit +/- alternating vector: exactly orthogonal to all-ones (cosine 0).
fn orthogonal_normalized() -> Vec<f32> {
    (0..DIM)
        .map(|i| if i % 2 == 0 { 1.0f32 } else { -1.0f32 } / (DIM as f32).sqrt())
        .collect()
}

fn episode(id: &str, title: &str, task: &str) -> MemoryEpisode {
    MemoryEpisode {
        id: id.to_string(),
        kind: EpisodeKind::Task,
        title: title.to_string(),
        task: task.to_string(),
        context: String::new(),
        approach: String::new(),
        outcome: EpisodeOutcome::Success,
        lessons: String::new(),
        source: EpisodeSource::Interactive,
        origin_run: String::new(),
        evidence_ids: vec![],
        created_at: String::new(),
        updated_at: String::new(),
    }
}

// ─── 1. keyword/dense fusion ────────────────────────────────────────────────

#[test]
fn keyword_and_dense_fusion_ranks_relevant_higher() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("graph.db");
    let store = GraphStore::open(&db).unwrap(); // applies schema (incl. memory tables)
    let episodes = EpisodeStore::open(&db).unwrap();
    let prefs = PreferenceStore::open(&db).unwrap();

    // One clearly-matching episode + two unrelated ones.
    episodes
        .insert(
            &episode(
                "ep-auth",
                "Fix authentication middleware rejection",
                "The authentication middleware rejected valid tokens after expiry; \
                 adjusted the validation order in the authentication middleware.",
            ),
            None,
            500,
        )
        .unwrap()
        .expect("matching episode must persist");
    episodes
        .insert(
            &episode(
                "ep-db",
                "Database migration scripts",
                "Added incremental migration scripts for the billing schema.",
            ),
            None,
            500,
        )
        .unwrap()
        .expect("unrelated episode 1 must persist");
    episodes
        .insert(
            &episode(
                "ep-ui",
                "UI dashboard polish",
                "Rounded the dashboard cards and fixed the header spacing.",
            ),
            None,
            500,
        )
        .unwrap()
        .expect("unrelated episode 2 must persist");

    // One preference (unrelated to the query, exercises the other table).
    prefs
        .upsert(
            "structure",
            "prefer constructor injection",
            PreferenceOrigin::Explicit,
            &[],
            None,
            None,
            None,
        )
        .unwrap();

    // Dense fixtures: matching episode near-identical to the query vector
    // (cosine 1.0 > 0.99); one unrelated episode exactly orthogonal.
    index_memory_vector(store.conn(), "ep-auth", "episode", &all_ones_normalized(), false)
        .unwrap();
    index_memory_vector(store.conn(), "ep-db", "episode", &orthogonal_normalized(), false)
        .unwrap();

    // search_memory under a config whose backend cannot resolve a real
    // embedder here (backend auto, no model artifacts): the call must
    // SUCCEED (keyword-only degradation is silent) and the matching
    // episode must rank in the top hits.
    let cfg = RagConfig::default();
    let req = MemorySearchRequest {
        query: "authentication middleware".to_string(),
        top_k: 10,
    };
    let hits = search_memory(store.conn(), &cfg, None, &req)
        .expect("keyword-only degradation must be silent, never an error");
    assert!(
        hits.iter().any(|h| h.item_id == "ep-auth"),
        "matching episode must appear in the hits: {hits:?}"
    );
    // Every hit is a memory item, never a rag chunk id.
    for h in &hits {
        assert!(
            h.item_kind == "episode" || h.item_kind == "preference",
            "unexpected item_kind {:?}",
            h.item_kind
        );
    }

    // Direct dense scan: the matching vector's id ranks FIRST; the
    // orthogonal one scores ~0.
    let cands = memory_dense_scan(store.conn(), &query_all_ones(), 3).unwrap();
    assert!(!cands.is_empty(), "dense scan must see the memory vectors");
    assert_eq!(cands[0].chunk_id, "ep-auth", "ranking: {cands:?}");
    let db_cand = cands.iter().find(|c| c.chunk_id == "ep-db");
    if let Some(c) = db_cand {
        assert!(c.score.abs() < 0.01, "orthogonal vector must score ~0: {cands:?}");
    }
}

// ─── 2. namespace isolation memory vs rag ───────────────────────────────────

#[test]
fn namespace_isolation_memory_vs_rag() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("graph.db");
    let store = GraphStore::open(&db).unwrap();
    let episodes = EpisodeStore::open(&db).unwrap();
    let conn = store.conn();

    // Seed one rag_chunks/rag_vectors row via direct SQL (INSERT shape
    // mirroring budget_smoke.rs). The rag vector uses the SAME embedding
    // as the memory vector so a scan that wrongly touched rag_vectors
    // would surface the rag chunk id.
    conn.execute(
        "INSERT INTO rag_chunks \
         (chunk_id, chunk_kind, artifact_id, source_path, start_line, end_line, \
          language, symbol_name, symbol_kind, content_hash, embed_model, embed_dim, updated_at) \
         VALUES ('rag-c1', 'symbol', NULL, 'isolation_probe.py', 1, 10, 'python', \
                 'handler', 'function', 'deadbeef', 'synthetic', 8, '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO rag_vectors (chunk_id, dim, quantization, vector) VALUES ('rag-c1', 8, 'f32', ?1)",
        rusqlite::params![encode_f32(&all_ones_normalized())],
    )
    .unwrap();

    // One memory episode with a vector on the same embedding.
    episodes
        .insert(
            &episode(
                "ep-iso",
                "Namespace isolation probe",
                "Verified the memory namespace isolation probe leaves rag rows untouched.",
            ),
            None,
            500,
        )
        .unwrap()
        .expect("episode must persist");
    index_memory_vector(conn, "ep-iso", "episode", &all_ones_normalized(), false).unwrap();

    // search_memory returns ONLY memory items — the rag chunk id never
    // appears in results.
    let cfg = RagConfig::default();
    let req = MemorySearchRequest {
        query: "namespace isolation probe".to_string(),
        top_k: 10,
    };
    let hits = search_memory(conn, &cfg, None, &req).unwrap();
    assert!(
        hits.iter().any(|h| h.item_id == "ep-iso"),
        "memory episode must be found: {hits:?}"
    );
    assert!(
        hits.iter().all(|h| h.item_id != "rag-c1"),
        "rag chunk id leaked into memory results: {hits:?}"
    );

    // memory_dense_scan ignores rag_vectors: its result set never contains
    // the rag chunk id.
    let cands = memory_dense_scan(conn, &query_all_ones(), 5).unwrap();
    assert!(cands.iter().any(|c| c.chunk_id == "ep-iso"), "{cands:?}");
    assert!(
        cands.iter().all(|c| c.chunk_id != "rag-c1"),
        "rag vector leaked into the memory dense scan: {cands:?}"
    );

    // Deleting the memory episode leaves the rag rows unchanged.
    let rag_chunks_before: i64 =
        conn.query_row("SELECT COUNT(*) FROM rag_chunks", [], |r| r.get(0)).unwrap();
    let rag_vectors_before: i64 =
        conn.query_row("SELECT COUNT(*) FROM rag_vectors", [], |r| r.get(0)).unwrap();
    assert_eq!((rag_chunks_before, rag_vectors_before), (1, 1));

    assert!(episodes.delete("ep-iso").unwrap());
    let rag_chunks_after: i64 =
        conn.query_row("SELECT COUNT(*) FROM rag_chunks", [], |r| r.get(0)).unwrap();
    let rag_vectors_after: i64 =
        conn.query_row("SELECT COUNT(*) FROM rag_vectors", [], |r| r.get(0)).unwrap();
    assert_eq!(
        (rag_chunks_after, rag_vectors_after),
        (1, 1),
        "memory deletion must not touch rag rows"
    );
    let cands = memory_dense_scan(conn, &query_all_ones(), 5).unwrap();
    assert!(
        !cands.iter().any(|c| c.chunk_id == "rag-c1"),
        "post-delete scan must still never surface the rag chunk: {cands:?}"
    );
}

// ─── 3. two project roots isolate stores (FR-012, analyze C1) ───────────────

#[test]
fn two_project_roots_isolate_stores() {
    let root_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();

    // The per-project db paths differ (hash of the canonical root).
    let path_a = project_graph_db_path(root_a.path());
    let path_b = project_graph_db_path(root_b.path());
    assert_ne!(path_a, path_b);

    // Build the stores under an isolated JOEY_HOME so the per-project
    // dbs land in a temp home, never the developer's real ~/.joey (same
    // pattern as joey-neurocode's tests). Env is restored before any
    // assertion so a failure cannot leak the override.
    let _guard = HOME_LOCK.lock().unwrap();
    let home =
        std::env::temp_dir().join(format!("joey-t009-memsearch-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let prev = std::env::var("JOEY_HOME").ok();
    std::env::set_var("JOEY_HOME", &home);

    let store_a = GraphStore::open(&path_a).unwrap();
    let store_b = GraphStore::open(&path_b).unwrap();
    let episodes_a = EpisodeStore::open(&path_a).unwrap();

    match prev {
        Some(v) => std::env::set_var("JOEY_HOME", v),
        None => std::env::remove_var("JOEY_HOME"),
    }

    // Insert an episode ONLY into A, with its embedding.
    episodes_a
        .insert(
            &episode(
                "ep-a",
                "Project A secret work",
                "Work item recorded only in project A's memory store.",
            ),
            None,
            500,
        )
        .unwrap()
        .expect("episode must persist in A");
    let embedding = all_ones_normalized();
    index_memory_vector(store_a.conn(), "ep-a", "episode", &embedding, false).unwrap();

    // B's conn, scanned with A's embedding: no hits (no cross-project reads).
    let hits_b = memory_dense_scan(store_b.conn(), &embedding, 3).unwrap();
    assert!(
        !hits_b.iter().any(|c| c.chunk_id == "ep-a"),
        "cross-project leak into B: {hits_b:?}"
    );

    // A's conn with the same embedding: hit.
    let hits_a = memory_dense_scan(store_a.conn(), &embedding, 3).unwrap();
    assert!(
        hits_a.iter().any(|c| c.chunk_id == "ep-a"),
        "A must see its own episode: {hits_a:?}"
    );
}

// ─── 4. blob codec parity ───────────────────────────────────────────────────

#[test]
fn index_memory_vector_blob_format_matches_rag_codec() {
    let tmp = tempfile::tempdir().unwrap();
    let store = GraphStore::open(&tmp.path().join("graph.db")).unwrap();
    let conn = store.conn();

    // Deterministic 8-dim embedding (not normalized — irrelevant for the
    // byte-parity check, both routes see the same input).
    let emb: Vec<f32> = (0..DIM).map(|i| ((i as f32 * 0.37) % 7.0) / 7.0 - 0.25).collect();

    let read_blob = |item_id: &str| -> (String, i64, Vec<u8>) {
        conn.query_row(
            "SELECT quantization, dim, vector FROM memory_vectors WHERE item_id = ?1",
            rusqlite::params![item_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, Vec<u8>>(2)?)),
        )
        .unwrap()
    };

    // f32 route: memory write vs canonical rag encode path.
    index_memory_vector(conn, "m-f32", "episode", &emb, false).unwrap();
    let (q, dim, blob_mem) = read_blob("m-f32");
    assert_eq!((q.as_str(), dim), ("f32", DIM as i64));
    assert_eq!(blob_mem, encode_f32(&emb), "f32 BLOBs must be byte-identical");

    // int8 route: memory write vs canonical rag encode path.
    index_memory_vector(conn, "m-int8", "episode", &emb, true).unwrap();
    let (q, dim, blob_mem) = read_blob("m-int8");
    assert_eq!((q.as_str(), dim), ("int8", DIM as i64));
    assert_eq!(
        blob_mem, encode_int8(&emb),
        "int8 BLOBs must be byte-identical"
    );
}

// ─── 5. empty corpus ────────────────────────────────────────────────────────

#[test]
fn empty_corpus_returns_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let store = GraphStore::open(&tmp.path().join("graph.db")).unwrap();

    let cfg = RagConfig::default();
    let req = MemorySearchRequest {
        query: "anything at all".to_string(),
        top_k: 5,
    };
    let hits = search_memory(store.conn(), &cfg, None, &req)
        .expect("empty corpus must be Ok(empty), never a panic");
    assert!(hits.is_empty(), "expected no hits, got {hits:?}");

    let cands = memory_dense_scan(store.conn(), &query_all_ones(), 5).unwrap();
    assert!(cands.is_empty(), "expected no dense candidates, got {cands:?}");
}

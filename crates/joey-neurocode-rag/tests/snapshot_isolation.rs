//! T048 — search-during-refresh snapshot isolation (spec edge case:
//! refresh vs query snapshot isolation).
//!
//! Design basis (prior assessment): `GraphStore::open` sets
//! `PRAGMA journal_mode=WAL` (crates/joey-neurocode/src/graph/store.rs
//! open path), and `vector::store::write_index` performs the atomic swap
//! (purge_paths + chunk/vector/edge/meta writes) inside ONE
//! `unchecked_transaction` — COMMIT is the swap point. Under WAL, readers
//! on their own connections keep seeing the pre-commit snapshot while the
//! writer's transaction is open, and no intermediate committed state ever
//! exists. Production shape mirrored here: the refresh (writer) and the
//! search (reader) each open their OWN `GraphStore` connection to the
//! same DB file.
//!
//! Two complementary strategies:
//!
//! 1. `pinned_read_txn_across_refresh_sees_only_pre_commit_snapshot` —
//!    deterministic, no timing dependence: the reader opens and PINS a
//!    read transaction (BEGIN DEFERRED + a first read) BEFORE the refresh
//!    starts, the writer thread then runs a full `write_index` refresh to
//!    COMMIT while that read transaction is still open, and the reader's
//!    in-flight searches (dense leg + the `search_cli_with_embedder`
//!    entrypoint) must observe EXACTLY the pre-refresh snapshot; after
//!    COMMIT (read txn ended), the same searches observe the new state.
//! 2. `concurrent_searches_during_refresh_never_observe_mixed_state` —
//!    true concurrency sampling: while a large refresh transaction is
//!    open on the writer connection, a reader thread loops real search
//!    probes and every observation must be EXACTLY the baseline set or
//!    EXACTLY the final set — never a torn/mixed state (the atomic-swap
//!    guarantee rules out every intermediate value).

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use joey_neurocode::graph::GraphStore;
use joey_neurocode_rag::embed::profiles::default_profile;
use joey_neurocode_rag::index::chunker::{ChunkKind, ChunkRecord};
use joey_neurocode_rag::search::hybrid::{
    dense_leg, search_cli_with_embedder, SearchRequest,
};
use joey_neurocode_rag::vector::quantize::Quantization;
use joey_neurocode_rag::vector::store::{
    chunk_count, load_index_meta, read_vector, write_index,
};

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Deterministic per-seed unit vector: distinct directions per seed,
/// zero-ish mean, L2-normalized (the store's stated storage contract).
/// `vec_for(2)` is used as the query vector, so the chunk stored with
/// seed 2 is the unique cosine-1.0 top hit.
fn vec_for(seed: u64, dim: usize) -> Vec<f32> {
    let v: Vec<f32> = (0..dim)
        .map(|i| {
            ((seed.wrapping_mul(131)
                .wrapping_add((i as u64).wrapping_mul(7)))
                % 97) as f32
                / 97.0
                - 0.5
        })
        .collect();
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    v.into_iter().map(|x| x / norm).collect()
}

/// A fallback chunk record with a hand-picked deterministic id (chunk ids
/// are opaque strings to the store; fallback rows need no artifact).
fn rec(id: &str, path: &str, line: u32) -> ChunkRecord {
    ChunkRecord {
        chunk_id: id.to_string(),
        kind: ChunkKind::Fallback,
        source_path: path.to_string(),
        start_line: line,
        end_line: line + 9,
        language: "python".to_string(),
        content_hash: format!("hash-{id}"),
        embed_text: format!("embed-{id}"),
    }
}

/// All chunk ids in `rag_chunks`, sorted (exact-set probe).
fn probe_ids(conn: &rusqlite::Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT chunk_id FROM rag_chunks ORDER BY chunk_id")
        .unwrap();
    stmt.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn id_set(conn: &rusqlite::Connection) -> BTreeSet<String> {
    probe_ids(conn).into_iter().collect()
}

/// Dense-leg search over the given connection; returns the observed
/// chunk-id set (top_k large enough to cover the whole index).
fn dense_id_set(
    conn: &rusqlite::Connection,
    query: &[f32],
    top_k: usize,
) -> BTreeSet<String> {
    dense_leg(conn, query, top_k, true)
        .unwrap()
        .into_iter()
        .map(|c| c.chunk_id)
        .collect()
}

fn search_req(query: &str) -> SearchRequest {
    SearchRequest {
        query: query.to_string(),
        file_filter: None,
        limit: 5,
        expand_lines: 0,
        relation_depth: 0,
        include_fallback_chunks: true,
    }
}

// ─── 1. deterministic pinned-read-transaction gate ──────────────────────────

/// A search already in flight (read transaction pinned) BEFORE a refresh
/// commits must complete against the pre-refresh snapshot; the same
/// search after the read transaction ends must see the new state.
#[test]
fn pinned_read_txn_across_refresh_sees_only_pre_commit_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("graph.db");
    // Production shape: writer (refresh) and reader (search) each open
    // their OWN connection to the same DB.
    let writer = GraphStore::open(&db).unwrap();
    let reader = GraphStore::open(&db).unwrap();

    // Design-basis pin: the open path enables WAL.
    let mode: String = reader
        .conn()
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");

    let profile = default_profile();
    let dim = profile.dim as usize;

    // Baseline snapshot: keep (untouched), del (purged by the refresh),
    // mod_v1 (replaced by mod_v2 via purge + new id).
    let base_chunks = vec![
        rec("keep_v1", "keep.py", 1),
        rec("del_v1", "del.py", 1),
        rec("mod_v1", "mod.py", 1),
    ];
    let base_vecs: Vec<Option<Vec<f32>>> =
        vec![Some(vec_for(1, dim)), Some(vec_for(4, dim)), Some(vec_for(2, dim))];
    write_index(&writer, profile, Quantization::F32, &base_chunks, &base_vecs, &[])
        .unwrap();

    let baseline_ids: BTreeSet<String> = id_set(reader.conn());
    assert_eq!(
        baseline_ids,
        ["del_v1", "keep_v1", "mod_v1"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    );

    // Refresh batch: purge del.py + mod.py; re-upsert keep_v1; add mod_v2
    // and a large bulk file (big enough that the write transaction stays
    // open for a measurable while).
    const BULK: u64 = 12_000;
    let mut rf_chunks = vec![rec("keep_v1", "keep.py", 1), rec("mod_v2", "mod.py", 1)];
    for i in 0..BULK {
        rf_chunks.push(rec(&format!("bulk_gen:{i}"), "bulk_gen.py", (i % 4_000) as u32 + 1));
    }
    let rf_vecs: Vec<Option<Vec<f32>>> = rf_chunks
        .iter()
        .map(|c| {
            let seed = if c.chunk_id == "keep_v1" {
                1
            } else if c.chunk_id == "mod_v2" {
                2
            } else {
                3 + (c.chunk_id.len() as u64) % 8
            };
            Some(vec_for(seed, dim))
        })
        .collect();
    let purge: Vec<&str> = vec!["del.py", "mod.py"];

    // 1. Pin the reader's snapshot BEFORE the refresh starts.
    reader.conn().execute_batch("BEGIN DEFERRED;").unwrap();
    let pinned = chunk_count(reader.conn()).unwrap();
    assert_eq!(pinned, 3, "pin read happens on the baseline snapshot");

    // 2. Writer thread runs the whole refresh; its COMMIT lands while the
    //    reader's read transaction is still open.
    thread::spawn(move || {
        write_index(&writer, profile, Quantization::F32, &rf_chunks, &rf_vecs, &purge)
            .unwrap();
    })
    .join()
    .unwrap();

    // 3. The STILL-OPEN search observes EXACTLY the pre-refresh snapshot.
    let q = vec_for(2, dim);
    assert_eq!(
        dense_id_set(reader.conn(), &q, 20_000),
        baseline_ids,
        "mid-flight dense search must return the exact baseline set"
    );
    assert_eq!(chunk_count(reader.conn()).unwrap(), 3);
    assert_eq!(load_index_meta(reader.conn()).unwrap().unwrap().chunk_count, 3);
    assert_eq!(id_set(reader.conn()), baseline_ids);

    let out = search_cli_with_embedder(
        &reader,
        tmp.path(),
        profile,
        &search_req("bulk_gen"),
        move |_: &[String]| -> Result<Vec<Vec<f32>>, String> { Ok(vec![q.clone()]) },
    )
    .unwrap();
    assert!(
        !out.results.is_empty(),
        "pinned search still sees the baseline index"
    );
    for r in &out.results {
        assert!(
            baseline_ids.contains(&r.chunk_id),
            "pinned search leaked post-refresh chunk {}",
            r.chunk_id
        );
        assert_ne!(r.file, "bulk_gen.py");
    }
    // The seed-2 chunk is the unique cosine-1.0 hit → deterministic top-1.
    assert_eq!(out.results[0].chunk_id, "mod_v1");
    assert_eq!(out.index_chunk_count, 3);

    // 4. End the read transaction; the SAME searches now see the new state.
    reader.conn().execute_batch("COMMIT;").unwrap();

    let final_count = 2 + BULK; // keep_v1 + mod_v2 + bulk
    assert_eq!(chunk_count(reader.conn()).unwrap(), final_count as u64);
    let final_ids = id_set(reader.conn());
    assert!(!final_ids.contains("del_v1"), "purged chunk must vanish");
    assert!(!final_ids.contains("mod_v1"), "replaced chunk must vanish");
    assert!(final_ids.contains("mod_v2"));
    assert!(final_ids.contains(&format!("bulk_gen:{}", BULK - 1)));
    assert_eq!(final_ids.len() as u64, final_count);
    assert_eq!(
        load_index_meta(reader.conn()).unwrap().unwrap().chunk_count,
        final_count as u64
    );

    let q2 = vec_for(2, dim);
    assert_eq!(
        dense_id_set(reader.conn(), &q2, 20_000),
        final_ids,
        "post-commit dense search must return the exact final set"
    );

    let out2 = search_cli_with_embedder(
        &reader,
        tmp.path(),
        profile,
        &search_req("bulk_gen"),
        move |_: &[String]| -> Result<Vec<Vec<f32>>, String> { Ok(vec![q2.clone()]) },
    )
    .unwrap();
    // RRF fuses dense + keyword legs, so strict global ordering depends on
    // coincidental keyword hits for bulk_gen.py; the isolation-relevant
    // assertions are membership + the dense top hit.
    assert!(
        out2.results.iter().any(|r| r.chunk_id == "mod_v2"),
        "replaced chunk must be searchable after commit"
    );
    let dense_top = dense_leg(reader.conn(), &vec_for(2, dim), 1, true).unwrap();
    assert_eq!(dense_top[0].chunk_id, "mod_v2");
    assert!(
        out2.results.iter().any(|r| r.file == "bulk_gen.py"),
        "newly added chunks must be searchable after commit"
    );
    assert_eq!(out2.index_chunk_count, final_count as u64);

    // Purged vector rows are gone; the replacement stored the new vector.
    assert!(read_vector(reader.conn(), "del_v1").unwrap().is_none());
    let (vdim, vq, decoded) =
        read_vector(reader.conn(), "mod_v2").unwrap().unwrap();
    assert_eq!(vdim, profile.dim);
    assert_eq!(vq, Quantization::F32);
    assert_eq!(decoded, vec_for(2, dim));
}

// ─── 2. true-concurrency sampling ───────────────────────────────────────────

/// Searches executed on a separate connection WHILE a refresh
/// transaction is open must observe either exactly the baseline state or
/// exactly the final state — never a mixed/torn snapshot.
#[test]
fn concurrent_searches_during_refresh_never_observe_mixed_state() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("graph.db");
    let writer = GraphStore::open(&db).unwrap();
    let reader = GraphStore::open(&db).unwrap();

    let profile = default_profile();
    let dim = profile.dim as usize;

    // Baseline: keep2 (untouched), del2 (purged by refresh), bulk_a (kept).
    const BULK_A: u64 = 9_000;
    const BULK_B: u64 = 9_000;
    let mut base_chunks =
        vec![rec("keep2", "keep2.py", 1), rec("del2", "del2.py", 1)];
    for i in 0..BULK_A {
        base_chunks.push(rec(&format!("bulk_a:{i}"), "bulk_a.py", (i % 4_000) as u32 + 1));
    }
    let base_vecs: Vec<Option<Vec<f32>>> = base_chunks
        .iter()
        .map(|c| Some(vec_for(3 + (c.chunk_id.len() as u64) % 8, dim)))
        .collect();
    write_index(&writer, profile, Quantization::F32, &base_chunks, &base_vecs, &[])
        .unwrap();

    let baseline_ids: BTreeSet<String> = id_set(reader.conn());
    let baseline_count = chunk_count(reader.conn()).unwrap();
    assert_eq!(baseline_ids.len() as u64, baseline_count);

    // Refresh: purge del2.py, add bulk_b (new ids), keep everything else.
    let mut rf_chunks = vec![rec("keep2", "keep2.py", 1)];
    for i in 0..BULK_B {
        rf_chunks.push(rec(&format!("bulk_b:{i}"), "bulk_b.py", (i % 4_000) as u32 + 1));
    }
    let rf_vecs: Vec<Option<Vec<f32>>> = rf_chunks
        .iter()
        .map(|c| Some(vec_for(3 + (c.chunk_id.len() as u64) % 8, dim)))
        .collect();
    let purge: Vec<&str> = vec!["del2.py"];

    let expected_final_ids: BTreeSet<String> = baseline_ids
        .iter()
        .filter(|id| *id != &"del2".to_string())
        .cloned()
        .chain((0..BULK_B).map(|i| format!("bulk_b:{i}")))
        .collect();
    let expected_final_count = expected_final_ids.len() as u64;

    let in_flight = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));

    // Writer thread: flag up → one big atomic write_index → flag down.
    let w_in = Arc::clone(&in_flight);
    let w_done = Arc::clone(&done);
    let writer_thread = thread::spawn(move || {
        w_in.store(true, Ordering::SeqCst);
        let r = write_index(
            &writer,
            profile,
            Quantization::F32,
            &rf_chunks,
            &rf_vecs,
            &purge,
        );
        w_in.store(false, Ordering::SeqCst);
        w_done.store(true, Ordering::SeqCst);
        r.unwrap();
    });

    // Reader thread: probe the search surface until the writer finishes.
    let r_in = Arc::clone(&in_flight);
    let r_done = Arc::clone(&done);
    let expected_final_ids_reader = expected_final_ids.clone();
    let reader_thread = thread::spawn(move || {
        let q = vec_for(3, dim);
        let mut mixed: Vec<String> = Vec::new();
        let mut baseline_seen_in_flight = 0u64;
        let mut probes = 0u64;
        let mut dense_probes = 0u64;
        while !r_done.load(Ordering::SeqCst) {
            probes += 1;
            let inflight = r_in.load(Ordering::SeqCst);
            // Probe 1: id-set (one statement — one WAL snapshot).
            let ids = id_set(reader.conn());
            if ids != baseline_ids && ids != expected_final_ids_reader {
                mixed.push(format!(
                    "id set ({}) neither baseline ({}) nor final ({}) — e.g. has del2={} bulk_b0={}",
                    ids.len(),
                    baseline_ids.len(),
                    expected_final_ids_reader.len(),
                    ids.contains("del2"),
                    ids.contains("bulk_b:0")
                ));
            }
            if inflight && ids == baseline_ids {
                baseline_seen_in_flight += 1;
            }
            // Probe 2: chunk_count (single statement).
            let n = chunk_count(reader.conn()).unwrap();
            if n != baseline_count && n != expected_final_count {
                mixed.push(format!("chunk_count {n} is neither {baseline_count} nor {expected_final_count}"));
            }
            // Probe 3 (every 8th): dense-leg search over the whole index.
            if probes % 8 == 0 {
                dense_probes += 1;
                let d = dense_id_set(reader.conn(), &q, 30_000);
                if d != baseline_ids && d != expected_final_ids_reader {
                    mixed.push(format!(
                        "dense id set ({}) neither baseline ({}) nor final ({})",
                        d.len(),
                        baseline_ids.len(),
                        expected_final_ids_reader.len()
                    ));
                }
            }
        }
        // Move the reader connection and the reference sets back out so the
        // post-commit ground truth runs on the SAME reader connection.
        (mixed, baseline_seen_in_flight, probes, dense_probes, reader, baseline_ids)
    });

    writer_thread.join().unwrap();
    let (mixed, baseline_seen_in_flight, probes, dense_probes, reader, _baseline_ids) =
        reader_thread.join().unwrap();

    assert!(mixed.is_empty(), "mixed-state observations: {mixed:?}");
    assert!(probes > 0 && dense_probes > 0, "reader must actually probe");
    assert!(
        baseline_seen_in_flight >= 5,
        "expected several baseline observations while the refresh transaction \
         was open (got {baseline_seen_in_flight} over {probes} probes) — the \
         deterministic pinned-txn test covers the guarantee, but a batch this \
         size should keep the transaction open long enough to sample it"
    );

    // Post-commit ground truth.
    assert_eq!(chunk_count(reader.conn()).unwrap(), expected_final_count);
    assert_eq!(id_set(reader.conn()), expected_final_ids);
    assert!(!expected_final_ids.contains("del2"));
    assert!(expected_final_ids.contains("keep2"));
}

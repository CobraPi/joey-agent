//! T036 — budget/perf smoke tests (SC-003 / SC-004 + memory proxy).
//!
//! These bound the RAG subsystem's steady-state costs at the contract's
//! 250k-chunk ceiling (perf-budgets.md). They are smoke tests, not
//! micro-benchmarks: they assert wall-clock ceilings generous enough to be
//! stable on a dev laptop while still catching order-of-magnitude
//! regressions (e.g. an accidental O(n·top_k) re-sort or an embedding call
//! sneaking into the refresh path).
//!
//! - SC-003: exhaustive `dense_scan` over 100k synthetic 768-dim int8
//!   vectors stays under a 2s p95 across 20 queries.
//! - SC-004: a 12-file Python repo refreshes incrementally (< 5s) after
//!   editing 10 files, with untouched rows' `updated_at` preserved.
//! - Memory proxy (`#[ignore]`): std::mem paper-math accounting of the
//!   in-memory decode footprint at 250k chunks. Documented proxy — real
//!   RSS measurement needs an allocator hook out of scope here.

use std::path::PathBuf;
use std::time::Instant;

use joey_neurocode::graph::GraphStore;
use joey_neurocode_rag::embed::profiles::default_profile;
use joey_neurocode_rag::index::chunker::{ChunkEmbedder, ChunkOptions};
use joey_neurocode_rag::index::incremental::{
    default_indexable_filter, detect_changes, refresh_incremental, snapshot_tree,
    ChangeDelta, DetectionOptions, RefreshBudgets,
};
use joey_neurocode_rag::vector::quantize::{encode_int8, Quantization};
use joey_neurocode_rag::vector::scan::dense_scan;
use rusqlite::params;

// ─── shared helpers ─────────────────────────────────────────────────────────

/// Deterministic unit-vector generator (same shape as the CountingEmbedder
/// in incremental.rs's unit tests, made public-file reusable).
struct NoopEmbedder {
    dim: usize,
    texts_seen: Vec<String>,
}

impl NoopEmbedder {
    fn new(dim: usize) -> Self {
        Self { dim, texts_seen: Vec::new() }
    }
}

impl ChunkEmbedder for NoopEmbedder {
    fn embed_texts(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        self.texts_seen.extend_from_slice(texts);
        Ok(texts
            .iter()
            .map(|t| {
                let v: Vec<f32> = (0..self.dim)
                    .map(|i| ((t.len() as f32 * 0.001 + i as f32) % 7.0) / 7.0)
                    .collect();
                let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                v.into_iter().map(|x| x / norm).collect()
            })
            .collect())
    }
}

fn temp_store() -> (tempfile::TempDir, GraphStore) {
    let tmp = tempfile::tempdir().unwrap();
    let store = GraphStore::open(&tmp.path().join("graph.db")).unwrap();
    (tmp, store)
}

fn write(root: &std::path::Path, rel: &str, contents: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, contents).unwrap();
}

// ─── SC-003: dense_scan latency at scale ────────────────────────────────────

/// GraphStore's v3 schema already creates `rag_chunks`/`rag_vectors` with
/// exactly the contract DDL (store.rs:99-131), so seeding is direct INSERTs
/// into the existing tables — the sanctioned path for scan benchmarks (the
/// write_index path would pay parse+chunk+hash costs this benchmark is
/// explicitly NOT measuring).
#[test]
fn sc003_dense_scan_p95_under_2s_at_100k() {
    const N: usize = 100_000;
    let dim = default_profile().dim as usize;
    assert_eq!(dim, 768, "default profile dim is pinned at 768");

    let (_tmp, store) = temp_store();
    let conn = store.conn();

    let t_insert = Instant::now();
    // int8 blob = dim + 4 bytes (f32 scale prefix + dim int8 codes).
    let template: Vec<f32> = (0..dim).map(|i| ((i as f32 * 0.37) % 7.0) / 7.0).collect();
    let blob = encode_int8(&template);
    assert_eq!(blob.len(), dim + 4, "int8 encoding is dim+4 bytes");

    conn.execute("BEGIN", []).unwrap();
    {
        let mut chunk_stmt = conn
            .prepare(
                "INSERT INTO rag_chunks
                     (chunk_id, chunk_kind, artifact_id, source_path, start_line, end_line,
                      language, symbol_name, symbol_kind, content_hash, embed_model, embed_dim, updated_at)
                 VALUES (?1, 'symbol', NULL, ?2, 1, 10, 'python', ?3, 'function', ?4, 'synthetic', ?5, '2026-01-01T00:00:00Z')",
            )
            .unwrap();
        let mut vec_stmt = conn
            .prepare("INSERT INTO rag_vectors (chunk_id, dim, quantization, vector) VALUES (?1, ?2, 'int8', ?3)")
            .unwrap();
        for i in 0..N {
            let chunk_id = format!("c{i:06}");
            chunk_stmt
                .execute(params![
                    chunk_id,
                    format!("pkg/mod_{:04}.py", i / 25),
                    format!("fn_{i:06}"),
                    format!("hash_{i:06}"),
                    dim as i64,
                ])
                .unwrap();
            vec_stmt.execute(params![format!("c{i:06}"), dim as i64, blob]).unwrap();
        }
    }
    conn.execute("COMMIT", []).unwrap();
    let insert_elapsed = t_insert.elapsed();
    assert!(insert_elapsed.as_secs() < 90, "seeding 100k rows took {insert_elapsed:?}");
    eprintln!("SC-003 seeding: {N} rows in {insert_elapsed:?}");

    // 20 queries, distinct deterministic vectors; p95 of per-query latency.
    let mut queries: Vec<Vec<f32>> = Vec::with_capacity(20);
    for q in 0..20u32 {
        let v: Vec<f32> = (0..dim)
            .map(|i| (((i as f32 * (q as f32 + 1.0)) % 11.0) + 1.0) / 11.0)
            .collect();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        queries.push(v.into_iter().map(|x| x / norm).collect());
    }

    let mut latencies_ms: Vec<f64> = Vec::with_capacity(queries.len());
    for query in &queries {
        let t = Instant::now();
        let hits = dense_scan(conn, query, 10, true).unwrap();
        let dt = t.elapsed();
        assert_eq!(hits.len(), 10, "top-10 must be returned from {N} rows");
        latencies_ms.push(dt.as_secs_f64() * 1000.0);
    }
    latencies_ms.sort_by(|a, b| a.total_cmp(b));
    let p95 = latencies_ms[(latencies_ms.len() as f64 * 0.95).ceil() as usize - 1];
    eprintln!(
        "SC-003 dense_scan over {N}x{dim} int8: p95 = {p95:.1} ms, min = {:.1} ms, max = {:.1} ms",
        latencies_ms[0],
        latencies_ms[latencies_ms.len() - 1]
    );
    assert!(p95 < 2000.0, "SC-003: p95 {p95:.0} ms exceeds the 2s ceiling");
}

// ─── SC-004: incremental refresh budget on a synthetic 12-file repo ─────────

/// Index a synthetic 12-file Python repo, modify 10 files, re-refresh:
/// total refresh work must stay under 5s and the 2 UNTOUCHED files' chunk
/// rows must keep their original `updated_at` (never rewritten).
#[test]
fn sc004_incremental_refresh_under_5s_preserves_updated_at() {
    let (tmp, store) = temp_store();
    let root = tmp.path();
    let profile = default_profile();

    // 12 files, each with a module-level fallback region + one function so
    // every file yields ≥2 chunks.
    let mut files = Vec::new();
    for i in 0..12 {
        let rel = format!("pkg/mod{i:02}.py");
        write(
            root,
            &rel,
            &format!("x_{i} = {i}\n\n\ndef handler_{i}():\n    return {i}\n"),
        );
        files.push(rel);
    }

    // First full index (NOT timed — only the incremental refresh is SC-004's
    // subject; the first pass is bounded by FR-005 worker budgets instead).
    let mut embedder = NoopEmbedder::new(profile.dim as usize);
    let delta = ChangeDelta {
        added: files.iter().map(PathBuf::from).collect(),
        ..ChangeDelta::default()
    };
    let t_first = Instant::now();
    refresh_incremental(
        &store,
        root,
        &delta,
        &mut embedder,
        profile,
        Quantization::Int8,
        &ChunkOptions::default(),
        &RefreshBudgets::default(),
    )
    .unwrap();
    eprintln!("SC-004 first index (12 files): {:?}", t_first.elapsed());

    // Capture the chunk rows for the two UNTOUCHED files before the edit.
    let untouched: Vec<(String, String)> = ["pkg/mod10.py", "pkg/mod11.py"]
        .iter()
        .flat_map(|p| {
            store
                .conn()
                .prepare("SELECT chunk_id, updated_at FROM rag_chunks WHERE source_path = ?1")
                .unwrap()
                .query_map(params![p], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(!untouched.is_empty(), "untouched files must have chunk rows");

    // Modify 10 of the 12 files (append a line — shifts the fallback hash).
    for i in 0..10 {
        let rel = format!("pkg/mod{i:02}.py");
        write(
            root,
            &rel,
            &format!("x_{i} = {i}\n\n\ndef handler_{i}():\n    return {i} + 100\n"),
        );
    }

    // Real change detection drives the refresh (mtime fast-path + hash
    // confirmation), exactly as the worker would.
    let previous = snapshot_tree(root, &default_indexable_filter);
    std::thread::sleep(std::time::Duration::from_millis(1100)); // ensure mtime ticks
    for i in 0..10 {
        let rel = format!("pkg/mod{i:02}.py");
        write(
            root,
            &rel,
            &format!("x_{i} = {i}\n\n\ndef handler_{i}():\n    return {i} + 200\n"),
        );
    }
    let delta = detect_changes(root, &previous, &default_indexable_filter, &DetectionOptions::default());
    assert_eq!(delta.modified.len(), 10, "exactly the 10 edited files: {delta:?}");
    assert!(delta.added.is_empty() && delta.removed.is_empty());

    let mut embedder2 = NoopEmbedder::new(profile.dim as usize);
    let t_refresh = Instant::now();
    let outcome = refresh_incremental(
        &store,
        root,
        &delta,
        &mut embedder2,
        profile,
        Quantization::Int8,
        &ChunkOptions::default(),
        &RefreshBudgets::default(),
    )
    .unwrap();
    let refresh_elapsed = t_refresh.elapsed();
    eprintln!(
        "SC-004 incremental refresh (10/12 files modified): {:?} ({} files reindexed, {} chunks embedded, {} skipped)",
        refresh_elapsed, outcome.files_reindexed, outcome.chunks_embedded, outcome.chunks_skipped
    );
    assert!(
        refresh_elapsed.as_secs_f64() < 5.0,
        "SC-004: refresh took {refresh_elapsed:?}, ceiling is 5s"
    );
    assert_eq!(outcome.files_reindexed, 10);

    // Untouched files' rows were never rewritten: byte-identical updated_at.
    for (chunk_id, updated_at) in &untouched {
        let now: String = store
            .conn()
            .query_row(
                "SELECT updated_at FROM rag_chunks WHERE chunk_id = ?1",
                params![chunk_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            &now, updated_at,
            "untouched chunk {chunk_id} must keep its updated_at"
        );
    }

    // And the modified files' chunks did move (their ids re-derived).
    let total: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM rag_chunks", [], |r| r.get(0))
        .unwrap();
    assert!(total >= 24, "12 files x >=2 chunks, got {total}");
}

// ─── SC-004 (full mix): ≤10-file incremental refresh budget + isolation ─────

/// Full SC-004 scenario: a 36-file repo, one refresh wave touching exactly
/// 7 files (3 modified, 2 added, 2 deleted — deletions exercising the FR-005
/// purge path). Asserts (a) the refresh call alone stays under the 5s
/// wall-clock ceiling, and (b) ONLY changed entries were touched: the
/// untouched files' chunk rows are byte-identical (rowid, content_hash,
/// updated_at) before/after, the embedder never saw their texts, and the
/// ChangeDelta/RefreshOutcome counts are exact.
#[test]
fn sc004_full_mix_refresh_under_5s_touches_only_changed_entries() {
    const TOTAL_FILES: usize = 36;
    const MOD_START: usize = TOTAL_FILES - 9; // files 27..30 modified
    let (tmp, store) = temp_store();
    let root = tmp.path();
    let profile = default_profile();

    let body = |i: usize| format!("x_{i} = {i}\n\ndef handler_{i}():\n    return {i}\n");
    let mut files = Vec::new();
    for i in 0..TOTAL_FILES {
        let rel = format!("pkg/mod{i:02}.py");
        write(root, &rel, &body(i));
        files.push(rel);
    }

    // Baseline full index (NOT timed — SC-004 bounds the incremental pass).
    let mut embedder = NoopEmbedder::new(profile.dim as usize);
    let delta = ChangeDelta {
        added: files.iter().map(PathBuf::from).collect(),
        ..ChangeDelta::default()
    };
    refresh_incremental(
        &store,
        root,
        &delta,
        &mut embedder,
        profile,
        Quantization::Int8,
        &ChunkOptions::default(),
        &RefreshBudgets::default(),
    )
    .unwrap();

    let baseline_rows: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM rag_chunks", [], |r| r.get(0))
        .unwrap();
    assert!(baseline_rows >= (TOTAL_FILES as i64) * 2, "fixture too small: {baseline_rows}");

    // Capture (rowid, chunk_id, content_hash, updated_at) for every row of
    // the files that will stay untouched (7..27 — well clear of both edges).
    let untouched_paths: Vec<String> =
        (7..27).map(|i| format!("pkg/mod{i:02}.py")).collect();
    let snapshot_untouched: Vec<(i64, String, String, String)> = {
        let conn = store.conn();
        let mut out = Vec::new();
        for p in &untouched_paths {
            let mut stmt = conn
                .prepare(
                    "SELECT rowid, chunk_id, content_hash, updated_at
                     FROM rag_chunks WHERE source_path = ?1 ORDER BY rowid",
                )
                .unwrap();
            let rows: Vec<_> = stmt
                .query_map(params![p], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            assert!(!rows.is_empty(), "{p} must have chunk rows at baseline");
            out.extend(rows);
        }
        out
    };

    // ── The edit wave: 3 modified + 2 added + 2 deleted = 7 ≤ 10 files. ──
    let previous = snapshot_tree(root, &default_indexable_filter);
    std::thread::sleep(std::time::Duration::from_millis(1100)); // mtime tick
    for i in MOD_START..MOD_START + 3 {
        write(
            root,
            &format!("pkg/mod{i:02}.py"),
            &format!("x_{i} = {i}\n\ndef handler_{i}():\n    return {i} + 999\n"),
        );
    }
    write(root, "pkg/new_a.py", "a = 1\n\ndef fn_a():\n    return a\n");
    write(root, "pkg/new_b.py", "b = 2\n\ndef fn_b():\n    return b\n");
    std::fs::remove_file(root.join("pkg/mod04.py")).unwrap();
    std::fs::remove_file(root.join("pkg/mod05.py")).unwrap();

    // Real detection: mtime fast-path + SHA-256 confirm (incremental.rs
    // T021) — exactly the delta the refresh worker (refresh_worker.rs) runs.
    let delta = detect_changes(
        root,
        &previous,
        &default_indexable_filter,
        &DetectionOptions::default(),
    );
    assert_eq!(
        delta.total_changed(),
        7,
        "exactly 3 modified + 2 added + 2 removed: {delta:?}"
    );
    assert_eq!(delta.modified.len(), 3, "{delta:?}");
    assert_eq!(delta.added.len(), 2, "{delta:?}");
    assert_eq!(delta.removed.len(), 2, "{delta:?}");
    assert!(delta.renamed.is_empty(), "{delta:?}");

    // Purged paths must currently have rows (they are about to disappear).
    for p in ["pkg/mod04.py", "pkg/mod05.py"] {
        let n: i64 = store
            .conn()
            .query_row("SELECT COUNT(*) FROM rag_chunks WHERE source_path = ?1", params![p], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(n > 0, "{p} should have rows pre-purge");
    }

    // ── SC-004 timing: the refresh call ONLY (fixture setup excluded). ──
    let mut embedder2 = NoopEmbedder::new(profile.dim as usize);
    let t_refresh = Instant::now();
    let outcome = refresh_incremental(
        &store,
        root,
        &delta,
        &mut embedder2,
        profile,
        Quantization::Int8,
        &ChunkOptions::default(),
        &RefreshBudgets::default(),
    )
    .unwrap();
    let refresh_elapsed = t_refresh.elapsed();
    eprintln!(
        "SC-004 full-mix refresh (3 mod + 2 add + 2 del of {TOTAL_FILES} files): {:?} \
         (reindexed {}, indexed {}, purged {}, chunks embedded {}, skipped {})",
        refresh_elapsed, outcome.files_reindexed, outcome.files_indexed,
        outcome.files_purged, outcome.chunks_embedded, outcome.chunks_skipped
    );
    assert!(
        refresh_elapsed.as_secs_f64() < 5.0,
        "SC-004: refresh took {refresh_elapsed:?}, ceiling is 5s"
    );

    // ── Only-changed-entries isolation ─────────────────────────────────────
    // (1) Outcome counts match the delta exactly.
    assert_eq!(outcome.files_reindexed, 3, "{outcome:?}");
    assert_eq!(outcome.files_indexed, 2, "{outcome:?}");
    assert_eq!(outcome.files_purged, 2, "{outcome:?}");
    assert_eq!(outcome.files_deferred, 0, "7 files must fit the default budget");
    assert_eq!(outcome.files_renamed, 0);

    // (2) Untouched files: rows are byte-identical AND at the same rowids —
    // not deleted+reinserted, never rewritten.
    let conn = store.conn();
    for (rowid, chunk_id, content_hash, updated_at) in &snapshot_untouched {
        let now: (String, String, String) = conn
            .query_row(
                "SELECT chunk_id, content_hash, updated_at
                 FROM rag_chunks WHERE rowid = ?1",
                params![rowid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap_or_else(|_| panic!("untouched row {rowid} vanished"));
        assert_eq!(
            now,
            (chunk_id.clone(), content_hash.clone(), updated_at.clone()),
            "untouched chunk row {rowid} was rewritten"
        );
    }

    // (3) The embedder only ever saw texts from the 5 (re)indexed files —
    // proof the hash-skip path (SC-004 core metric, incremental.rs FR-004)
    // spared every untouched chunk.
    let mut touched_texts = embedder2.texts_seen.clone();
    touched_texts.sort();
    assert!(
        !touched_texts.is_empty(),
        "3 modified files' changed chunks must be re-embedded"
    );
    for text in &touched_texts {
        let leaks_into_untouched = untouched_paths.iter().any(|p| {
            // chunk texts carry their file's unique symbol name handler_NN
            text.contains(&format!("handler_{:02}", p.trim_start_matches("pkg/mod").trim_end_matches(".py")))
        });
        assert!(
            !leaks_into_untouched,
            "embedder saw untouched-file text: {text:?}"
        );
    }

    // (4) Purged files have no rows left; added files do (FR-005 end state).
    for p in ["pkg/mod04.py", "pkg/mod05.py"] {
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM rag_chunks WHERE source_path = ?1", params![p], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 0, "{p} must be fully purged");
    }
    for p in ["pkg/new_a.py", "pkg/new_b.py"] {
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM rag_chunks WHERE source_path = ?1", params![p], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(n >= 2, "{p} must be indexed");
    }
}

// ─── Memory proxy (documented paper math, not RSS) ──────────────────────────

/// 250k-chunk steady-state decode footprint, accounted with std::mem over
/// the BLOB + decoded-buffer shapes. PROXY: it pins the per-chunk cost
/// arithmetic (bytes per chunk in each representation), not process RSS —
/// real RSS depends on the allocator and is out of scope for a smoke test.
#[test]
#[ignore = "paper-math memory proxy — run explicitly with --ignored"]
fn memory_proxy_250k_chunks_footprint() {
    const N: usize = 250_000;
    let dim = default_profile().dim as usize;
    assert_eq!(dim, 768);

    // On-disk BLOB (int8): dim + 4 bytes — the SQLite page-cache term.
    let blob_bytes = std::mem::size_of::<u8>() * (dim + 4);

    // Decoded scan buffer: one Vec<f32> per row (what rayon materializes
    // per chunk before the dot product) plus the Vec header (24 bytes on
    // 64-bit: ptr/len/cap).
    let vec_header = std::mem::size_of::<Vec<f32>>();
    let decoded_bytes = vec_header + std::mem::size_of::<f32>() * dim;

    // Full-table decode would be N * (blob + decoded). The scan contract
    // never holds all decoded rows at once (streamed per-row scoring), so
    // the honest ceiling is: N blobs resident in SQLite + K decoded rows
    // in flight (K = rayon pool width). Assert the paper math and the
    // resulting budget at 250k.
    let resident_blobs = N * blob_bytes;
    let full_decode = N * decoded_bytes;

    // Paper-math pins (perf-budgets.md ceiling arithmetic):
    // - 250k int8 blobs ≈ 193 MB of vector BLOB storage;
    // - a hypothetical full materialization ≈ 780 MB — which is exactly
    //   why dense_scan streams per-row instead.
    assert_eq!(blob_bytes, 772);
    assert_eq!(resident_blobs, 193_000_000); // 250_000 * 772
    assert!(full_decode > resident_blobs); // decode inflates ~4x (f32 vs int8)
    assert_eq!(
        full_decode,
        N * (vec_header + 4 * dim),
        "decoded per-chunk cost = Vec header + dim f32s"
    );
    eprintln!(
        "memory proxy @250k chunks: blobs = {:.1} MB, hypothetical full decode = {:.1} MB",
        resident_blobs as f64 / (1024.0 * 1024.0),
        full_decode as f64 / (1024.0 * 1024.0)
    );
}

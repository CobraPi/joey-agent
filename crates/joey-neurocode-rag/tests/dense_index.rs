//! T012 integration tests: dense indexing pipeline (chunker + vector store).
//!
//! Pins the three contract obligations of task T012 against a real
//! `GraphStore` on a tempdir DB:
//!
//! 1. `content_hash` is computed over the RAW UNPREFIXED text — a profile
//!    document-prefix change never changes any chunk hash (data-model.md §1).
//! 2. Single-transaction behavior — a failure mid-batch leaves no partial
//!    rows (FR-004 / clarification Q5; contracts/rag-store-schema.md
//!    § Atomicity).
//! 3. Very-large-file bounding — a ≥10 MB synthetic source file indexes
//!    without failure or stall with bounded processing (edge case 3): it is
//!    chunked (never embedded as one piece) and each embed batch stays small.

use joey_neurocode::graph::GraphStore;
use joey_neurocode::parse::extract::{ExtractedMethod, SourceExtraction};

use joey_neurocode_rag::embed::profiles::{default_profile, CODERANK_EMBED};
use joey_neurocode_rag::index::chunker::{
    build_chunk_records, index_file, ChunkEmbedder, ChunkKind, ChunkOptions,
    EMBED_BATCH_SIZE,
};
use joey_neurocode_rag::vector::quantize::Quantization;
use joey_neurocode_rag::vector::store::{
    chunk_count, load_index_meta, read_vector, vector_count, write_index,
    VectorStoreError,
};

/// Echo embedder: returns a fixed `dim`-dimensional vector per text and
/// records the largest batch it ever saw (bounding assertion).
struct EchoEmbedder {
    dim: usize,
    max_batch_seen: usize,
    batches: usize,
    calls: usize,
}

impl EchoEmbedder {
    fn new(dim: usize) -> Self {
        Self { dim, max_batch_seen: 0, batches: 0, calls: 0 }
    }
}

impl ChunkEmbedder for EchoEmbedder {
    fn embed_texts(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        self.max_batch_seen = self.max_batch_seen.max(texts.len());
        self.batches += 1;
        self.calls += texts.len();
        Ok(texts
            .iter()
            .map(|t| {
                // Deterministic content-derived vector (normalized).
                let v: Vec<f32> = (0..self.dim)
                    .map(|i| {
                        ((t.len() as f32 * 0.001 + i as f32) % 7.0) / 7.0
                    })
                    .collect();
                let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                v.into_iter().map(|x| x / norm).collect()
            })
            .collect())
    }
}

/// Embedder that always fails (transaction test).
struct FailingEmbedder;

impl ChunkEmbedder for FailingEmbedder {
    fn embed_texts(&mut self, _texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        Err("simulated backend failure".to_string())
    }
}

fn temp_store() -> (tempfile::TempDir, GraphStore) {
    let tmp = tempfile::tempdir().unwrap();
    let store = GraphStore::open(&tmp.path().join("graph.db")).unwrap();
    (tmp, store)
}

fn python_extraction(source: &str, with_symbol: bool) -> SourceExtraction {
    let mut ex = SourceExtraction {
        language: "python".to_string(),
        ..Default::default()
    };
    if with_symbol {
        ex.module_functions.push(ExtractedMethod {
            name: "handler".to_string(),
            annotations: vec![],
            signature: None,
            start_byte: 0,
            end_byte: 24.min(source.len() as u32),
        });
    }
    ex.populate_fallback_chunks(source);
    ex
}

// ── 1. Hash-over-unprefixed-text pin ───────────────────────────────────

#[test]
fn content_hash_is_over_raw_unprefixed_text() {
    let (_tmp, store) = temp_store();
    let source = "def handler():\n    return 42\n\nx = 2\n";
    let extraction = python_extraction(source, true);
    let records = build_chunk_records(
        &extraction,
        source,
        "app.py",
        &store,
        &ChunkOptions::default(),
    );

    // Baseline: index under the default (nomic) profile — its document
    // prefix is "search_document: ", applied at EMBED time only.
    let nomic = default_profile();
    assert_eq!(nomic.prefix_document, "search_document: ");
    let mut echo = EchoEmbedder::new(nomic.dim as usize);
    let committed = index_file(
        &store, &extraction, source, "app.py", &mut echo, nomic,
        Quantization::F32, &ChunkOptions::default(), &[],
    )
    .unwrap();
    assert_eq!(committed.len(), records.len());

    // Every stored hash equals SHA-256 of the chunk's UNPREFIXED contextual
    // text — NOT of the prefixed embed input the backend received.
    for r in &committed {
        let prefixed = nomic.document_input(&r.embed_text);
        assert_ne!(r.embed_text, prefixed, "profile prefix must live outside stored text");
        assert_eq!(
            r.content_hash,
            joey_neurocode_rag::index::chunker::content_hash(&r.embed_text),
            "hash input is the raw unprefixed text"
        );
    }

    // THE PIN: switching to a profile with a DIFFERENT (empty) document
    // prefix leaves every chunk hash unchanged — only embeddings rebuild.
    let before: Vec<(String, String)> = committed
        .iter()
        .map(|r| (r.chunk_id.clone(), r.content_hash.clone()))
        .collect();
    let coderank = &CODERANK_EMBED;
    assert_eq!(coderank.prefix_document, "");
    let mut echo2 = EchoEmbedder::new(coderank.dim as usize);
    let reindexed = index_file(
        &store, &extraction, source, "app.py", &mut echo2, coderank,
        Quantization::F32, &ChunkOptions::default(), &["app.py"],
    )
    .unwrap();
    let after: Vec<(String, String)> = reindexed
        .iter()
        .map(|r| (r.chunk_id.clone(), r.content_hash.clone()))
        .collect();
    assert_eq!(before, after, "profile prefix change must not change chunk hashes");
    // And the re-index replaced (not duplicated) the rows.
    assert_eq!(chunk_count(store.conn()).unwrap(), reindexed.len() as u64);
}

// ── 2. Single-transaction behavior ─────────────────────────────────────

#[test]
fn embed_failure_mid_pipeline_leaves_no_partial_rows() {
    let (_tmp, store) = temp_store();
    let source = "a = 1\nb = 2\nc = 3\n";
    let extraction = python_extraction(source, false);
    let profile = default_profile();

    // Seed one pre-existing chunk row so "no partial rows" is meaningful:
    // the failed write must not touch even the seed.
    let seed = build_chunk_records(
        &extraction, "seed.py", "seed.py", &store, &ChunkOptions::default(),
    );
    let seed_vecs = vec![Some(vec![0.5f32; profile.dim as usize]); seed.len()];
    write_index(&store, profile, Quantization::F32, &seed, &seed_vecs, &[]).unwrap();

    let err = index_file(
        &store, &extraction, source, "app.py", &mut FailingEmbedder, profile,
        Quantization::F32, &ChunkOptions::default(), &["seed.py"],
    )
    .unwrap_err();
    assert!(matches!(err, VectorStoreError::Embed(_)));

    // All-or-nothing: the purged seed did NOT disappear (rollback restored
    // it) and none of the new file's chunks/vectors landed.
    let paths: Vec<String> = {
        let mut stmt = store.conn().prepare("SELECT source_path FROM rag_chunks").unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.filter_map(Result::ok).collect()
    };
    assert_eq!(paths, vec!["seed.py".to_string()]);
    assert_eq!(chunk_count(store.conn()).unwrap(), seed.len() as u64);
    assert_eq!(vector_count(store.conn()).unwrap(), seed.len() as u64);
}

#[test]
fn db_failure_mid_batch_leaves_no_partial_rows() {
    let (_tmp, store) = temp_store();
    let source = "a = 1\nb = 2\nc = 3\nd = 4\n";
    let extraction = python_extraction(source, false);
    let profile = default_profile();
    let chunks = build_chunk_records(
        &extraction, source, "app.py", &store,
        &ChunkOptions { max_chunk_lines: 1, ..ChunkOptions::default() },
    );
    assert!(chunks.len() >= 2, "need ≥2 chunks to fail mid-batch");
    // Chunk 0 embeds fine; chunk 1 has the wrong dim → error inside the
    // transaction AFTER chunk 0's rows were staged.
    let mut vectors: Vec<Option<Vec<f32>>> = chunks
        .iter()
        .map(|_| Some(vec![0.5f32; profile.dim as usize]))
        .collect();
    vectors[1] = Some(vec![0.0f32; 3]);
    let err =
        write_index(&store, profile, Quantization::F32, &chunks, &vectors, &[]).unwrap_err();
    assert!(matches!(err, VectorStoreError::DimMismatch { .. }));
    assert_eq!(chunk_count(store.conn()).unwrap(), 0, "no chunk rows may survive");
    assert_eq!(vector_count(store.conn()).unwrap(), 0, "no vector rows may survive");
    assert!(load_index_meta(store.conn()).unwrap().is_none());
}

// ── 3. Very-large-file bounding (≥10 MB) ───────────────────────────────

#[test]
fn ten_megabyte_file_indexes_bounded_without_stall() {
    let (_tmp, store) = temp_store();
    let profile = default_profile();

    // ~10.5 MB of synthetic Python: top-level statements only (fallback
    // chunking path — the parse layer caps fallback chunks at 200 lines,
    // the chunker re-splits under the same cap).
    let line = "value = compute_something(index=42, mode='synthetic', payload='0123456789abcd')\n";
    let lines_per_mb = (1024 * 1024) / line.len() + 1;
    let mut source = String::with_capacity(line.len() * lines_per_mb * 11);
    while source.len() < 10 * 1024 * 1024 {
        source.push_str(line);
    }
    assert!(source.len() >= 10 * 1024 * 1024);

    let extraction = python_extraction(&source, false);
    let mut echo = EchoEmbedder::new(profile.dim as usize);
    let started = std::time::Instant::now();
    let committed = index_file(
        &store, &extraction, &source, "big.py", &mut echo, profile,
        Quantization::F32, &ChunkOptions::default(), &[],
    )
    .unwrap();
    let elapsed = started.elapsed();

    // Bounded processing, not a stall: this synthetic corpus indexes in
    // well under a minute on any dev machine (echo embedder; the real
    // model is the backend's concern, not the pipeline's).
    assert!(elapsed.as_secs() < 120, "indexed 10MB in {elapsed:?} — stalled?");

    // Chunked, never one piece: many chunks, each ≤ the line cap.
    assert!(committed.len() > 50, "expected many chunks, got {}", committed.len());
    for r in &committed {
        let lines = (r.end_line - r.start_line + 1) as usize;
        assert!(lines <= ChunkOptions::default().max_chunk_lines);
        assert!(matches!(r.kind, ChunkKind::Fallback));
    }
    // Every embed batch stayed at the bounded size; the corpus was embedded
    // in batches, never as one piece.
    assert!(echo.max_batch_seen <= EMBED_BATCH_SIZE);
    assert!(echo.batches >= committed.len() / EMBED_BATCH_SIZE);
    assert_eq!(echo.calls, committed.len());

    // Rows landed, vectors readable back, meta consistent.
    assert_eq!(chunk_count(store.conn()).unwrap(), committed.len() as u64);
    assert_eq!(vector_count(store.conn()).unwrap(), committed.len() as u64);
    let meta = load_index_meta(store.conn()).unwrap().unwrap();
    assert_eq!(meta.chunk_count, committed.len() as u64);
    let (_, q, v) = read_vector(store.conn(), &committed[0].chunk_id).unwrap().unwrap();
    assert_eq!((q, v.len()), (Quantization::F32, profile.dim as usize));

    // Determinism spot-check: the same source re-indexed purges + recreates
    // byte-identical hashes (chunk-level skip groundwork, FR-004/SC-004).
    let mut echo2 = EchoEmbedder::new(profile.dim as usize);
    let again = index_file(
        &store, &extraction, &source, "big.py", &mut echo2, profile,
        Quantization::F32, &ChunkOptions::default(), &["big.py"],
    )
    .unwrap();
    let h1: Vec<&str> = committed.iter().map(|r| r.content_hash.as_str()).collect();
    let h2: Vec<&str> = again.iter().map(|r| r.content_hash.as_str()).collect();
    assert_eq!(h1, h2);
    assert_eq!(chunk_count(store.conn()).unwrap(), again.len() as u64);
}

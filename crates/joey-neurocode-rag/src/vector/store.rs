//! BLOB vector table read/write (`rag_chunks` + `rag_vectors` + `rag_index_meta`).
//!
//! Vectors are stored as SQLite BLOBs in `rag_vectors` — f32 `dim×4`
//! little-endian; int8 `dim+4` quantized with an f32 scale prefix (the
//! encoding helpers live in [`super::quantize`] and are re-exported here for
//! T013's scan) — and are written in the SAME transaction as their chunks:
//! one call to [`write_index`] is all-or-nothing; COMMIT is the atomic swap
//! point and any failure mid-batch leaves NO partial rows (FR-004,
//! clarification Q5; contracts/rag-store-schema.md § Atomicity; data-model.md
//! § Storage invariants).
//!
//! Purge semantics: chunk rows for the paths in `purge_paths` are deleted
//! inside the same transaction and cascade to their vectors via the
//! `ON DELETE CASCADE` FK (FR-005 — no orphan vectors). Derived
//! `rag_chunk_edges` carry no FK BY DESIGN (T005) — they are REWRITTEN from
//! the typed graph inside the same transaction (T028's
//! [`crate::index::chunker::rewrite_chunk_edges`], fully derived), so rows
//! referencing purged chunks never survive a write.

use rusqlite::{params, Connection, OptionalExtension};

use joey_neurocode::graph::GraphStore;

use crate::embed::profiles::EmbedProfile;
use crate::index::chunker::{ChunkKind, ChunkRecord};
use crate::vector::quantize::{self, Quantization};

// Re-exported for T013 (`vector::scan`) so the BLOB layout has exactly one
// implementation site (never duplicated).
pub use crate::vector::quantize::{QuantizeError, Quantization as VectorQuantization};

/// Errors from the dense-index write path.
#[derive(Debug)]
pub enum VectorStoreError {
    /// SQLite failure (rolls the whole batch back).
    Sql(rusqlite::Error),
    /// `chunks` and `vectors` are not parallel slices (caller bug — an
    /// error, never a panic: a hard assert would crash the whole
    /// indexing pipeline over a caller mistake).
    MismatchedSlices { chunks: usize, vectors: usize },
    /// A vector whose dimensionality ≠ the profile's `embed_dim` — rejected
    /// at write time per data-model.md §2 validation rules.
    DimMismatch { chunk_id: String, expected: u32, got: usize },
    /// A stored/decoded BLOB of the wrong byte length or encoding.
    CorruptBlob(QuantizeError),
    /// The embedder boundary failed mid-pipeline (backend-agnostic string —
    /// T010's taxonomy maps onto this seam).
    Embed(String),
}

impl std::fmt::Display for VectorStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VectorStoreError::Sql(e) => write!(f, "rag store sql error: {e}"),
            VectorStoreError::MismatchedSlices { chunks, vectors } => write!(
                f,
                "write_index: chunks ({chunks}) and vectors ({vectors}) must be parallel slices"
            ),
            VectorStoreError::DimMismatch { chunk_id, expected, got } => write!(
                f,
                "vector dim mismatch for chunk {chunk_id}: expected {expected}, got {got}"
            ),
            VectorStoreError::CorruptBlob(e) => write!(f, "{e}"),
            VectorStoreError::Embed(e) => write!(f, "embed failure: {e}"),
        }
    }
}

impl std::error::Error for VectorStoreError {}

impl From<rusqlite::Error> for VectorStoreError {
    fn from(e: rusqlite::Error) -> Self {
        VectorStoreError::Sql(e)
    }
}

impl From<QuantizeError> for VectorStoreError {
    fn from(e: QuantizeError) -> Self {
        VectorStoreError::CorruptBlob(e)
    }
}

/// The `rag_index_meta` singleton (id = 1), as read back from the store.
#[derive(Debug, Clone, PartialEq)]
pub struct RagIndexMeta {
    pub schema_version: u32,
    pub embed_profile: String,
    pub embed_model: String,
    pub embed_dim: u32,
    pub pooling: String,
    pub prefix_query: String,
    pub prefix_document: String,
    pub quantization_policy: String,
    pub chunk_count: u64,
    pub last_refresh_at: Option<String>,
    pub refresh_state: String,
    pub created_at: String,
}

/// Write chunks (+ their vectors, + the index-meta singleton, + purges) into
/// the v3 tables in ONE transaction — all-or-nothing.
///
/// * `chunks` — the chunk records for this batch (symbol-aligned and/or
///   fallback), in order.
/// * `vectors` — parallel to `chunks`; `None` means "not yet embedded"
///   (the chunk is written keyword-searchable but excluded from semantic
///   ranking — data-model.md §2).
/// * `purge_paths` — source paths whose previous chunks are deleted first,
///   inside the same transaction (cascade removes their vectors/edges).
///
/// The embedding itself happens BEFORE this call (it is slow and pure); the
/// transaction covers only the DB writes, so COMMIT is a fast atomic swap.
pub fn write_index(
    store: &GraphStore,
    profile: &EmbedProfile,
    quantization: Quantization,
    chunks: &[ChunkRecord],
    vectors: &[Option<Vec<f32>>],
    purge_paths: &[&str],
) -> Result<(), VectorStoreError> {
    if chunks.len() != vectors.len() {
        return Err(VectorStoreError::MismatchedSlices {
            chunks: chunks.len(),
            vectors: vectors.len(),
        });
    }
    let conn = store.conn();
    // unchecked_transaction: GraphStore hands out &Connection (immutable);
    // dropping the Transaction on the error path rolls the batch back.
    let tx = conn.unchecked_transaction()?;

    // 1. Purge previous chunks of the replaced paths (cascade → vectors).
    for path in purge_paths {
        tx.execute("DELETE FROM rag_chunks WHERE source_path = ?1", params![path])?;
    }

    // 2. Chunk rows (upsert on the deterministic chunk_id PK).
    let now = chrono::Utc::now().to_rfc3339();
    {
        let mut chunk_stmt = tx.prepare(
            r#"
            INSERT INTO rag_chunks
                (chunk_id, chunk_kind, artifact_id, source_path, start_line, end_line,
                 language, symbol_name, symbol_kind, content_hash, embed_model, embed_dim,
                 updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ON CONFLICT(chunk_id) DO UPDATE SET
                artifact_id   = excluded.artifact_id,
                start_line    = excluded.start_line,
                end_line      = excluded.end_line,
                language      = excluded.language,
                symbol_name   = excluded.symbol_name,
                symbol_kind   = excluded.symbol_kind,
                content_hash  = excluded.content_hash,
                embed_model   = excluded.embed_model,
                embed_dim     = excluded.embed_dim,
                updated_at    = excluded.updated_at
            "#,
        )?;
        for (chunk, vector) in chunks.iter().zip(vectors.iter()) {
            let (kind_str, artifact_id, symbol_name, symbol_kind) = match &chunk.kind {
                ChunkKind::Symbol { artifact_id, symbol_name, symbol_kind } => (
                    "symbol",
                    *artifact_id,
                    Some(symbol_name.as_str()),
                    Some(symbol_kind.as_str()),
                ),
                ChunkKind::Fallback => ("fallback", None, None, None),
            };
            // Rows with a vector carry the model/dim; without one they stay
            // NULL ("not yet embedded").
            let (model, dim) = match vector {
                Some(_) => (Some(profile.name), Some(profile.dim as i64)),
                None => (None, None),
            };
            chunk_stmt.execute(params![
                chunk.chunk_id,
                kind_str,
                artifact_id,
                chunk.source_path,
                chunk.start_line as i64,
                chunk.end_line as i64,
                chunk.language,
                symbol_name,
                symbol_kind,
                chunk.content_hash,
                model,
                dim,
                now,
            ])?;
        }
    }

    // 3. Vector rows (encode + upsert). The dim check fires BEFORE the
    //    offending write; the whole transaction still rolls back on error.
    //    A chunk whose new vector is None gets any EXISTING rag_vectors
    //    row deleted — otherwise the chunk claims "not yet embedded"
    //    (embed_model/embed_dim NULL) while dense_scan still scores its
    //    stale vector.
    {
        let mut vec_stmt = tx.prepare(
            r#"
            INSERT INTO rag_vectors (chunk_id, dim, quantization, vector)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(chunk_id) DO UPDATE SET
                dim          = excluded.dim,
                quantization = excluded.quantization,
                vector       = excluded.vector
            "#,
        )?;
        for (chunk, vector) in chunks.iter().zip(vectors.iter()) {
            let Some(v) = vector else {
                // "Not yet embedded": drop a stale vector row if one
                // survives from a previous embed of this chunk_id.
                tx.execute(
                    "DELETE FROM rag_vectors WHERE chunk_id = ?1",
                    params![chunk.chunk_id],
                )?;
                continue;
            };
            if v.len() != profile.dim as usize {
                return Err(VectorStoreError::DimMismatch {
                    chunk_id: chunk.chunk_id.clone(),
                    expected: profile.dim,
                    got: v.len(),
                });
            }
            let blob = quantize::encode(v, quantization);
            vec_stmt
                .execute(params![chunk.chunk_id, v.len() as i64, quantization.as_str(), blob])?;
        }
    }

    // 4. Derived chunk-level edges (T028, data-model.md §7): fully derived
    //    from the typed graph + the chunk rows JUST staged above (and the
    //    purges from step 1) — rewrite-from-scratch inside the same
    //    transaction. Rows referencing purged chunks vanish with the
    //    rewrite; the typed graph stays authoritative.
    crate::index::chunker::rewrite_chunk_edges(&tx)?;

    // 5. Index-meta singleton (upsert; created_at preserved on update).
    tx.execute(
        r#"
        INSERT INTO rag_index_meta
            (id, schema_version, embed_profile, embed_model, embed_dim, pooling,
             prefix_query, prefix_document, quantization_policy, chunk_count,
             last_refresh_at, refresh_state, created_at)
        VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                (SELECT COUNT(*) FROM rag_chunks), ?9, 'idle', ?9)
        ON CONFLICT(id) DO UPDATE SET
            embed_profile       = excluded.embed_profile,
            embed_model         = excluded.embed_model,
            embed_dim           = excluded.embed_dim,
            pooling             = excluded.pooling,
            prefix_query        = excluded.prefix_query,
            prefix_document     = excluded.prefix_document,
            quantization_policy = excluded.quantization_policy,
            chunk_count         = (SELECT COUNT(*) FROM rag_chunks),
            last_refresh_at     = excluded.last_refresh_at,
            refresh_state       = 'idle'
        "#,
        params![
            joey_neurocode::NEUROCODE_SCHEMA_VERSION as i64,
            profile.name,
            profile.name,
            profile.dim as i64,
            profile.pooling.as_str(),
            profile.prefix_query,
            profile.prefix_document,
            quantization.as_str(),
            now,
        ],
    )?;

    tx.commit()?;
    Ok(())
}

/// Number of rows currently in `rag_chunks`.
pub fn chunk_count(conn: &Connection) -> rusqlite::Result<u64> {
    conn.query_row("SELECT COUNT(*) FROM rag_chunks", [], |r| r.get::<_, i64>(0))
        .map(|n| n as u64)
}

/// Number of rows currently in `rag_vectors`.
pub fn vector_count(conn: &Connection) -> rusqlite::Result<u64> {
    conn.query_row("SELECT COUNT(*) FROM rag_vectors", [], |r| r.get::<_, i64>(0))
        .map(|n| n as u64)
}

/// Read one stored vector back, decoded `(dim, quantization, elements)`.
/// `None` when the chunk has no vector row ("not yet embedded").
pub fn read_vector(
    conn: &Connection,
    chunk_id: &str,
) -> Result<Option<(u32, Quantization, Vec<f32>)>, VectorStoreError> {
    let row: Option<(i64, String, Vec<u8>)> = conn
        .query_row(
            "SELECT dim, quantization, vector FROM rag_vectors WHERE chunk_id = ?1",
            params![chunk_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((dim, q_str, blob)) = row else { return Ok(None) };
    let q = Quantization::parse(&q_str).ok_or_else(|| QuantizeError {
        encoding: "unknown",
        expected_len: 0,
        got_len: blob.len(),
    })?;
    let v = quantize::decode(&blob, dim as usize, q)?;
    Ok(Some((dim as u32, q, v)))
}

/// Read the `rag_index_meta` singleton, if present.
pub fn load_index_meta(conn: &Connection) -> rusqlite::Result<Option<RagIndexMeta>> {
    conn.query_row(
        r#"SELECT schema_version, embed_profile, embed_model, embed_dim, pooling,
                  prefix_query, prefix_document, quantization_policy, chunk_count,
                  last_refresh_at, refresh_state, created_at
           FROM rag_index_meta WHERE id = 1"#,
        [],
        |r| {
            Ok(RagIndexMeta {
                schema_version: r.get::<_, i64>(0)? as u32,
                embed_profile: r.get(1)?,
                embed_model: r.get(2)?,
                embed_dim: r.get::<_, i64>(3)? as u32,
                pooling: r.get(4)?,
                prefix_query: r.get(5)?,
                prefix_document: r.get(6)?,
                quantization_policy: r.get(7)?,
                chunk_count: r.get::<_, i64>(8)? as u64,
                last_refresh_at: r.get(9)?,
                refresh_state: r.get(10)?,
                created_at: r.get(11)?,
            })
        },
    )
    .optional()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::profiles::default_profile;
    use crate::index::chunker::{build_chunk_records, ChunkOptions};

    fn temp_store() -> (tempfile::TempDir, GraphStore) {
        let tmp = tempfile::tempdir().unwrap();
        let store = GraphStore::open(&tmp.path().join("graph.db")).unwrap();
        (tmp, store)
    }

    /// A fallback-only extraction (populated coarse chunks, no symbols).
    fn fallback_extraction(source: &str) -> joey_neurocode::parse::extract::SourceExtraction {
        let mut ex = joey_neurocode::parse::extract::SourceExtraction {
            language: "python".to_string(),
            ..Default::default()
        };
        ex.populate_fallback_chunks(source);
        ex
    }

    #[test]
    fn write_then_purge_cascades() {
        let (_tmp, store) = temp_store();
        let profile = default_profile();
        let source = "x = 1\ny = 2\nz = x + y\n";
        let extraction = fallback_extraction(source);
        let chunks =
            build_chunk_records(&extraction, source, "m.py", &store, &ChunkOptions::default());
        assert!(!chunks.is_empty());
        let v: Vec<Option<Vec<f32>>> = chunks
            .iter()
            .map(|_| Some(vec![0.25f32; profile.dim as usize]))
            .collect();
        write_index(&store, profile, Quantization::F32, &chunks, &v, &["m.py"]).unwrap();
        assert_eq!(chunk_count(store.conn()).unwrap(), chunks.len() as u64);
        assert_eq!(vector_count(store.conn()).unwrap(), chunks.len() as u64);

        // Purging the path inside a write removes chunks AND cascades vectors.
        write_index(&store, profile, Quantization::F32, &[], &[], &["m.py"]).unwrap();
        assert_eq!(chunk_count(store.conn()).unwrap(), 0);
        assert_eq!(vector_count(store.conn()).unwrap(), 0);
    }

    #[test]
    fn dim_mismatch_mid_batch_leaves_no_partial_rows() {
        let (_tmp, store) = temp_store();
        let profile = default_profile();
        let source = "a = 1\nb = 2\nc = 3\nd = 4\n";
        let extraction = fallback_extraction(source);
        // Force a split into 4 single-line chunks (mid-batch failure needs
        // ≥2 chunks; the 200-line default would keep them as one).
        let opts = ChunkOptions { max_chunk_lines: 1, ..ChunkOptions::default() };
        let chunks = build_chunk_records(&extraction, source, "m.py", &store, &opts);
        // Correct vector for chunk 0, WRONG dim for chunk 1 → error mid-write.
        let mut v: Vec<Option<Vec<f32>>> = Vec::new();
        for (i, _) in chunks.iter().enumerate() {
            if i == 1 {
                v.push(Some(vec![0.0f32; 3]));
            } else {
                v.push(Some(vec![0.5f32; profile.dim as usize]));
            }
        }
        let err = write_index(&store, profile, Quantization::F32, &chunks, &v, &[]).unwrap_err();
        assert!(matches!(err, VectorStoreError::DimMismatch { .. }));
        // ALL-OR-NOTHHING: not even chunk 0's rows landed.
        assert_eq!(chunk_count(store.conn()).unwrap(), 0);
        assert_eq!(vector_count(store.conn()).unwrap(), 0);
        assert!(load_index_meta(store.conn()).unwrap().is_none());
    }

    /// Non-parallel chunks/vectors is a CALLER BUG → MismatchedSlices
    /// error, never a panic (a hard assert would crash the whole
    /// indexing pipeline) — and nothing lands (checked before the tx).
    #[test]
    fn mismatched_slices_is_an_error_never_a_panic() {
        let (_tmp, store) = temp_store();
        let profile = default_profile();
        let source = "x = 1\ny = 2\n";
        let extraction = fallback_extraction(source);
        let chunks =
            build_chunk_records(&extraction, source, "m.py", &store, &ChunkOptions::default());
        assert!(!chunks.is_empty());
        // Vectors shorter than chunks — the caller bug the check guards.
        let short: Vec<Option<Vec<f32>>> = Vec::new();
        let err = write_index(&store, profile, Quantization::F32, &chunks, &short, &[])
            .unwrap_err();
        assert!(
            matches!(err, VectorStoreError::MismatchedSlices { chunks: c, vectors: 0 } if c == chunks.len()),
            "got: {err:?}"
        );
        assert_eq!(chunk_count(store.conn()).unwrap(), 0, "nothing landed");
        assert_eq!(vector_count(store.conn()).unwrap(), 0, "nothing landed");
    }

    /// A chunk re-written with a None vector ("not yet embedded") must
    /// have any STALE rag_vectors row from a previous embed deleted —
    /// otherwise dense_scan keeps scoring the old vector while the chunk
    /// claims embed_model/embed_dim NULL.
    #[test]
    fn none_vector_rewrites_delete_stale_vector_rows() {
        let (_tmp, store) = temp_store();
        let profile = default_profile();
        let source = "x = 1\ny = 2\n";
        let extraction = fallback_extraction(source);
        let chunks =
            build_chunk_records(&extraction, source, "m.py", &store, &ChunkOptions::default());
        let embedded: Vec<Option<Vec<f32>>> = chunks
            .iter()
            .map(|_| Some(vec![0.5f32; profile.dim as usize]))
            .collect();
        write_index(&store, profile, Quantization::F32, &chunks, &embedded, &[]).unwrap();
        assert_eq!(vector_count(store.conn()).unwrap(), chunks.len() as u64);

        // Re-write the SAME chunks with None vectors.
        let unembedded: Vec<Option<Vec<f32>>> = chunks.iter().map(|_| None).collect();
        write_index(&store, profile, Quantization::F32, &chunks, &unembedded, &[]).unwrap();
        assert_eq!(
            chunk_count(store.conn()).unwrap(),
            chunks.len() as u64,
            "chunk rows stay ('not yet embedded')"
        );
        assert_eq!(
            vector_count(store.conn()).unwrap(),
            0,
            "stale vector rows must be deleted, not left behind"
        );
        let (model, dim): (Option<String>, Option<i64>) = store
            .conn()
            .query_row(
                "SELECT embed_model, embed_dim FROM rag_chunks LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(model.is_none(), "embed_model NULL again");
        assert!(dim.is_none(), "embed_dim NULL again");
    }

    #[test]
    fn quantization_switch_updates_blob_layout() {
        let (_tmp, store) = temp_store();
        let profile = default_profile();
        let source = "x = 1\ny = 2\n";
        let extraction = fallback_extraction(source);
        let chunks =
            build_chunk_records(&extraction, source, "s.py", &store, &ChunkOptions::default());
        let v: Vec<Option<Vec<f32>>> = chunks
            .iter()
            .map(|_| Some(vec![1.0f32 / 32.0; profile.dim as usize]))
            .collect();
        write_index(&store, profile, Quantization::F32, &chunks, &v, &[]).unwrap();
        let (dim, q, decoded) =
            read_vector(store.conn(), &chunks[0].chunk_id).unwrap().unwrap();
        assert_eq!((dim, q), (profile.dim, Quantization::F32));
        assert_eq!(decoded.len(), profile.dim as usize);
        assert_eq!(decoded, vec![1.0f32 / 32.0; profile.dim as usize]);

        write_index(&store, profile, Quantization::Int8, &chunks, &v, &[]).unwrap();
        let (dim, q, _) = read_vector(store.conn(), &chunks[0].chunk_id).unwrap().unwrap();
        assert_eq!((dim, q), (profile.dim, Quantization::Int8));
        let raw: Vec<u8> = store
            .conn()
            .query_row(
                "SELECT vector FROM rag_vectors WHERE chunk_id = ?1",
                params![chunks[0].chunk_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(raw.len(), profile.dim as usize + 4);
    }

    #[test]
    fn meta_singleton_written_and_updated() {
        let (_tmp, store) = temp_store();
        let profile = default_profile();
        let source = "x = 1\n";
        let extraction = fallback_extraction(source);
        let chunks =
            build_chunk_records(&extraction, source, "s.py", &store, &ChunkOptions::default());
        let v = vec![Some(vec![0.5f32; profile.dim as usize]); chunks.len()];
        write_index(&store, profile, Quantization::F32, &chunks, &v, &[]).unwrap();
        let meta = load_index_meta(store.conn()).unwrap().unwrap();
        assert_eq!(meta.embed_profile, profile.name);
        assert_eq!(meta.embed_dim, profile.dim);
        assert_eq!(meta.pooling, "mean");
        assert_eq!(meta.prefix_document, profile.prefix_document);
        assert_eq!(meta.chunk_count, chunks.len() as u64);
        assert_eq!(meta.refresh_state, "idle");
        assert_eq!(meta.schema_version, joey_neurocode::NEUROCODE_SCHEMA_VERSION);
        let created = meta.created_at.clone();
        write_index(&store, profile, Quantization::F32, &chunks, &v, &[]).unwrap();
        let meta2 = load_index_meta(store.conn()).unwrap().unwrap();
        assert_eq!(meta2.created_at, created, "created_at is never rewritten");
    }
}

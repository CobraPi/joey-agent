//! In-memory exhaustive vector scan (T013).
//!
//! Exhaustive cosine scan over `rag_vectors` joined to `rag_chunks` with
//! rayon parallelism across rows (contracts/hybrid-search.md stage 3 — the
//! dense leg; research.md R1: brute-force compute is never the bottleneck,
//! BLOB load is; an exhaustive scan keeps the store dependency-free).
//!
//! ## BLOB decode
//!
//! Vectors are stored NORMALIZED, so cosine similarity is a plain dot
//! product on decode (data-model.md §2 / rag-store-schema.md § BLOB
//! encoding):
//!
//! | encoding | layout | byte length |
//! |---|---|---|
//! | `f32`  | `dim` little-endian IEEE-754 words | `dim × 4` |
//! | `int8` | one f32 scale prefix (4 bytes LE) + `dim` int8 codes (value ≈ code × scale) | `dim + 4` |
//!
//! Encode/decode helpers are CANONICAL in [`crate::vector::quantize`]
//! (T012's write-side module) and imported here — the scan never
//! re-implements the layout. Byte length is validated on EVERY decode;
//! anything else is corrupt and rejected with a chunk-naming
//! [`ScanError`].
//!
//! ## Quantization threshold (read side)
//!
//! int8 quantization is an INDEX-TIME storage-format decision applied when
//! the chunk count rises ABOVE `neurocode.rag.quantize_threshold`
//! (default 100000; research.md R1: ~400 MB → ~100 MB resident at 100k
//! chunks). The write side is T012's; this module provides
//! [`encoding_for_chunk_count`] as the single-source helper for WHICH
//! format applies at a given chunk count, while the scan itself decodes
//! BOTH formats per row — a store mid-reindex (or a threshold config
//! change not yet rebuilt) can legitimately contain a mix.

use rayon::prelude::*;
use rusqlite::Connection;
use std::fmt;

pub use crate::vector::quantize::Quantization;
use crate::vector::quantize::{decode, QuantizeError};

// ─── Threshold helper ────────────────────────────────────────────────────────

/// Which storage encoding applies at a given chunk count (R1 threshold
/// rule, shared by index-time write decisions — T012 — and status
/// reporting): int8 strictly ABOVE `quantize_threshold` chunks, f32
/// otherwise (at or below).
///
/// Strictly-above semantics: the default threshold of 100000 means a store
/// with exactly 100000 chunks is still f32; 100001 flips to int8.
pub const fn encoding_for_chunk_count(chunk_count: i64, quantize_threshold: i64) -> Quantization {
    if chunk_count > quantize_threshold {
        Quantization::Int8
    } else {
        Quantization::F32
    }
}

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Errors from the dense scan: corrupt BLOBs, dimension mismatches, and
/// SQL failures — all with chunk-identifying context.
#[derive(Debug)]
pub enum ScanError {
    /// Query embedding is an empty slice.
    EmptyQuery,
    /// Query embedding has zero or non-finite norm (cannot compare).
    DegenerateQuery,
    /// `rag_vectors.quantization` holds a value outside `f32 | int8`.
    UnknownQuantization { chunk_id: String, value: String },
    /// BLOB byte length ≠ `dim × 4` (f32) or `dim + 4` (int8) — corrupt
    /// (details from the canonical [`QuantizeError`]).
    CorruptBlob {
        chunk_id: String,
        source: QuantizeError,
    },
    /// Row `dim` ≠ query embedding length — index/embedder inconsistency.
    DimMismatch {
        chunk_id: String,
        stored_dim: usize,
        query_dim: usize,
    },
    /// Underlying SQLite failure.
    Sql(rusqlite::Error),
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyQuery => write!(f, "dense scan: query embedding is empty"),
            Self::DegenerateQuery => {
                write!(f, "dense scan: query embedding has zero/non-finite norm")
            }
            Self::UnknownQuantization { chunk_id, value } => write!(
                f,
                "corrupt rag_vectors row for chunk {:?}: unknown quantization {:?} \
                 (expected \"f32\" or \"int8\")",
                chunk_id, value
            ),
            Self::CorruptBlob { chunk_id, source } => {
                write!(f, "chunk {:?}: {}", chunk_id, source)
            }
            Self::DimMismatch {
                chunk_id,
                stored_dim,
                query_dim,
            } => write!(
                f,
                "dimension mismatch for chunk {:?}: vector dim {} != query dim {} \
                 (index and query embedding profiles disagree)",
                chunk_id, stored_dim, query_dim
            ),
            Self::Sql(e) => write!(f, "dense scan: sqlite error: {}", e),
        }
    }
}

impl std::error::Error for ScanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CorruptBlob { source, .. } => Some(source),
            Self::Sql(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for ScanError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sql(e)
    }
}

// ─── Scan ────────────────────────────────────────────────────────────────────

/// One dense-leg candidate: a chunk id and its cosine similarity to the
/// query (stored vectors are normalized ⇒ dot product IS the cosine).
#[derive(Debug, Clone, PartialEq)]
pub struct DenseCandidate {
    pub chunk_id: String,
    pub score: f32,
}

/// Raw row materialized from SQLite before the rayon pass (a rusqlite
/// `Connection` is not `Sync`; all SQLite access stays on this thread).
struct RawVectorRow {
    chunk_id: String,
    dim: usize,
    quantization: Quantization,
    blob: Vec<u8>,
}

/// Exhaustive cosine scan over `rag_vectors` joined to `rag_chunks`
/// (contracts/hybrid-search.md stage 3 — the dense leg primitive).
///
/// - Vectors are stored normalized, so the score is a plain dot product;
///   the query is defensively L2-normalized here so scores are true
///   cosines even if a caller passes an unnormalized embedding.
/// - Rows are decoded in parallel with rayon (canonical decoders from
///   [`crate::vector::quantize`]); both `f32` and `int8` rows are handled
///   per-row (see module docs on mid-reindex mixes).
/// - `include_fallback_chunks = false` excludes `chunk_kind = 'fallback'`
///   rows (FR-014 gating) at the SQL level, before any scoring.
/// - Top-`top_k` selection is deterministic: score descending
///   (`total_cmp`), tie-break `chunk_id` lexical ascending.
///
/// Corrupt rows (wrong BLOB size, unknown encoding, dim ≠ query dim)
/// reject the whole scan with a chunk-naming [`ScanError`] — the contract
/// mandates rejection, not silent skips.
pub fn dense_scan(
    conn: &Connection,
    query: &[f32],
    top_k: usize,
    include_fallback_chunks: bool,
) -> Result<Vec<DenseCandidate>, ScanError> {
    if query.is_empty() {
        return Err(ScanError::EmptyQuery);
    }
    let norm = query.iter().map(|v| v * v).sum::<f32>().sqrt();
    if !norm.is_finite() || norm == 0.0 {
        return Err(ScanError::DegenerateQuery);
    }
    let q: Vec<f32> = query.iter().map(|v| v / norm).collect();
    let q_dim = q.len();

    let sql = if include_fallback_chunks {
        "SELECT v.chunk_id, v.dim, v.quantization, v.vector \
         FROM rag_vectors AS v JOIN rag_chunks AS c ON c.chunk_id = v.chunk_id"
    } else {
        "SELECT v.chunk_id, v.dim, v.quantization, v.vector \
         FROM rag_vectors AS v JOIN rag_chunks AS c ON c.chunk_id = v.chunk_id \
         WHERE c.chunk_kind != 'fallback'"
    };

    let mut stmt = conn.prepare(sql)?;
    let rows: Vec<RawVectorRow> = {
        let mut it = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = it.next()? {
            let chunk_id: String = row.get(0)?;
            let dim = row.get::<_, i64>(1)? as usize;
            let quantization: String = row.get(2)?;
            let quantization = Quantization::parse(&quantization).ok_or_else(|| {
                ScanError::UnknownQuantization {
                    chunk_id: chunk_id.clone(),
                    value: quantization,
                }
            })?;
            out.push(RawVectorRow {
                chunk_id,
                dim,
                quantization,
                blob: row.get(3)?,
            });
        }
        out
    };
    drop(stmt);

    // Parallel decode + dot product; collect preserves row order so any
    // error surfaced is deterministic (first corrupt row in scan order).
    // A correct-length BLOB can still decode to non-finite values (NaN
    // scale prefix in int8, NaN f32 words) — such a row is corrupt and is
    // SKIPPED (counted), never surfaced: total_cmp orders NaN greatest, so
    // a NaN candidate would take a top-k slot ahead of every real hit.
    let corrupt_scores = std::sync::atomic::AtomicUsize::new(0);
    let scored: Result<Vec<Option<DenseCandidate>>, ScanError> = rows
        .par_iter()
        .map(|r| {
            if r.dim != q_dim {
                return Err(ScanError::DimMismatch {
                    chunk_id: r.chunk_id.clone(),
                    stored_dim: r.dim,
                    query_dim: q_dim,
                });
            }
            // Canonical decoder enforces the exact byte length per encoding.
            let v = decode(&r.blob, r.dim, r.quantization).map_err(|source| {
                ScanError::CorruptBlob {
                    chunk_id: r.chunk_id.clone(),
                    source,
                }
            })?;
            // Both sides unit-length ⇒ dot == cosine.
            let score: f32 = v.iter().zip(&q).map(|(a, b)| a * b).sum();
            if !score.is_finite() {
                corrupt_scores.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return Ok(None);
            }
            Ok(Some(DenseCandidate {
                chunk_id: r.chunk_id.clone(),
                score,
            }))
        })
        .collect();
    // Fold the Option layer away (None = skipped non-finite row).
    let mut candidates: Vec<DenseCandidate> = scored?.into_iter().flatten().collect();
    let corrupt_scores = corrupt_scores.into_inner();
    if corrupt_scores > 0 {
        eprintln!(
            "[joey neurocode] dense scan skipped {corrupt_scores} non-finite vector row(s)"
        );
    }

    // Deterministic top-k: score desc, then chunk_id lexical asc.
    candidates.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
    });
    candidates.truncate(top_k);
    Ok(candidates)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vector::quantize::{decode_f32, decode_int8, encode_f32, encode_int8};

    // Contract DDL for the two tables this scan reads (rag-store-schema.md).
    const DDL: &str = "
        CREATE TABLE rag_chunks (
            chunk_id     TEXT PRIMARY KEY,
            chunk_kind   TEXT NOT NULL CHECK (chunk_kind IN ('symbol','fallback')),
            artifact_id  INTEGER,
            source_path  TEXT NOT NULL,
            start_line   INTEGER NOT NULL,
            end_line     INTEGER NOT NULL,
            language     TEXT,
            symbol_name  TEXT,
            symbol_kind  TEXT,
            content_hash TEXT NOT NULL,
            embed_model  TEXT,
            embed_dim    INTEGER,
            updated_at   TEXT
        );
        CREATE TABLE rag_vectors (
            chunk_id     TEXT PRIMARY KEY,
            dim          INTEGER NOT NULL,
            quantization TEXT NOT NULL CHECK (quantization IN ('f32','int8')),
            vector       BLOB NOT NULL
        );
    ";

    fn test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(DDL).unwrap();
        conn
    }

    fn insert_chunk(conn: &Connection, id: &str, kind: &str) {
        conn.execute(
            "INSERT INTO rag_chunks (chunk_id, chunk_kind, source_path, start_line, end_line, \
             content_hash) VALUES (?1, ?2, 'src/x.rs', 1, 10, 'h')",
            rusqlite::params![id, kind],
        )
        .unwrap();
    }

    fn insert_vector(conn: &Connection, id: &str, dim: usize, quant: &str, blob: &[u8]) {
        conn.execute(
            "INSERT INTO rag_vectors (chunk_id, dim, quantization, vector) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![id, dim as i64, quant, blob],
        )
        .unwrap();
    }

    // ── BLOB decode validation through the canonical decoders ───────────

    #[test]
    fn decode_f32_roundtrip() {
        let v = [0.6f32, -0.8, 0.0, 1.0];
        let decoded = decode_f32(&encode_f32(&v), 4).unwrap();
        assert_eq!(decoded, v.to_vec());
        // Empty dim edge: zero bytes decode to zero elements.
        assert!(decode_f32(&[], 0).unwrap().is_empty());
    }

    #[test]
    fn decode_f32_rejects_wrong_sizes() {
        // dim 4 → 16 bytes expected; 15 and 17 both rejected, as is a dim
        // disagreeing with the blob.
        for blob_len in [15usize, 17] {
            let err = decode_f32(&vec![0u8; blob_len], 4).unwrap_err();
            assert_eq!(err.encoding, "f32");
            assert_eq!((err.expected_len, err.got_len), (16, blob_len));
            assert_eq!(
                err.to_string(),
                "corrupt f32 vector BLOB: expected 16 bytes, got 15 or 17"
                    .replace("15 or 17", &blob_len.to_string())
            );
        }
        assert!(decode_f32(&vec![0u8; 16], 5).is_err());
    }

    #[test]
    fn decode_int8_roundtrip() {
        // Hand-built blob: scale 0.01, codes [60, -80, 0, 127] →
        // [0.6, -0.8, 0.0, 1.27]; dim+4 = 8 bytes total.
        let mut blob = 0.01f32.to_le_bytes().to_vec();
        blob.extend([60i8, -80, 0, 127].iter().map(|&c| c as u8));
        assert_eq!(blob.len(), 8);
        let d = decode_int8(&blob, 4).unwrap();
        let expect = [0.6f32, -0.8, 0.0, 1.27];
        for (got, want) in d.iter().zip(expect) {
            assert!((got - want).abs() < 1e-6, "dequantized {} != {}", got, want);
        }
        // And the canonical encoder round-trips through the same layout.
        let v = [0.5f32, -0.25, 0.125, 1.0];
        let rt = decode_int8(&encode_int8(&v), 4).unwrap();
        let scale = f32::from_le_bytes(encode_int8(&v)[0..4].try_into().unwrap());
        for (orig, got) in v.iter().zip(rt.iter()) {
            assert!((orig - got).abs() <= scale / 2.0 + 1e-6);
        }
    }

    #[test]
    fn decode_int8_rejects_wrong_sizes() {
        // dim+4 = 8 expected; 7 and 9 both rejected.
        for len in [7usize, 9] {
            let err = decode_int8(&vec![0u8; len], 4).unwrap_err();
            assert_eq!(err.encoding, "int8");
            assert_eq!((err.expected_len, err.got_len), (8, len));
        }
        // Truncated scale prefix (< 4 bytes total).
        assert!(decode_int8(&[0u8, 0, 0], 0).is_err());
    }

    // ── Quantization threshold switching ─────────────────────────────────

    #[test]
    fn encoding_for_chunk_count_switches_at_threshold() {
        use crate::config::DEFAULT_QUANTIZE_THRESHOLD as T;
        // Default threshold 100000: below and AT the threshold -> f32.
        assert_eq!(encoding_for_chunk_count(0, T), Quantization::F32);
        assert_eq!(encoding_for_chunk_count(99_999, T), Quantization::F32);
        assert_eq!(
            encoding_for_chunk_count(100_000, T),
            Quantization::F32,
            "strictly ABOVE the threshold flips — at-threshold stays f32"
        );
        // Strictly above -> int8.
        assert_eq!(encoding_for_chunk_count(100_001, T), Quantization::Int8);
        assert_eq!(encoding_for_chunk_count(500_000, T), Quantization::Int8);
        // Threshold 0: any positive count quantizes; zero chunks stays f32.
        assert_eq!(encoding_for_chunk_count(0, 0), Quantization::F32);
        assert_eq!(encoding_for_chunk_count(1, 0), Quantization::Int8);
    }

    #[test]
    fn quantization_blob_lengths_at_768() {
        assert_eq!(Quantization::F32.blob_len(768), 3072);
        assert_eq!(Quantization::Int8.blob_len(768), 772);
        assert_eq!(Quantization::parse("f16"), None);
    }

    // ── Cosine ranking on hand-made vectors ──────────────────────────────

    #[test]
    fn cosine_ranking_pinned_on_handmade_vectors() {
        let conn = test_db();
        // Unit query q = [1,0,0,0]; hand-made unit documents with known cosines.
        let (a, c, b, d) = (
            [1.0f32, 0.0, 0.0, 0.0], // cos 1.0
            [0.6, 0.8, 0.0, 0.0],    // cos 0.6 (unit: 0.36+0.64=1)
            [0.0, 1.0, 0.0, 0.0],    // cos 0.0
            [-1.0, 0.0, 0.0, 0.0],   // cos -1.0
        );
        for (id, kind, v) in
            [("a", "symbol", a), ("c", "symbol", c), ("b", "symbol", b), ("d", "fallback", d)]
        {
            insert_chunk(&conn, id, kind);
            insert_vector(&conn, id, 4, "f32", &encode_f32(&v));
        }
        let q = [1.0f32, 0.0, 0.0, 0.0];
        let ranked = dense_scan(&conn, &q, 4, true).unwrap();
        let ids: Vec<&str> = ranked.iter().map(|r| r.chunk_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "c", "b", "d"], "rank order must follow cosine");
        assert!((ranked[0].score - 1.0).abs() < 1e-6);
        assert!((ranked[1].score - 0.6).abs() < 1e-6);
        assert!((ranked[2].score - 0.0).abs() < 1e-6);
        assert!((ranked[3].score - (-1.0)).abs() < 1e-6);

        // top_k truncation:
        let top2 = dense_scan(&conn, &q, 2, true).unwrap();
        assert_eq!(
            top2.iter().map(|r| r.chunk_id.as_str()).collect::<Vec<_>>(),
            vec!["a", "c"]
        );
    }

    #[test]
    fn unnormalized_query_yields_true_cosines() {
        let conn = test_db();
        insert_chunk(&conn, "v", "symbol");
        insert_vector(&conn, "v", 2, "f32", &encode_f32(&[0.6, 0.8]));
        // 2 * unit query — defensive normalization must give cos 0.6, not 1.2.
        let ranked = dense_scan(&conn, &[2.0f32, 0.0], 1, true).unwrap();
        assert_eq!(ranked.len(), 1);
        assert!((ranked[0].score - 0.6).abs() < 1e-6);
    }

    #[test]
    fn tie_break_is_chunk_id_lexical_ascending() {
        let conn = test_db();
        for id in ["x2", "x1", "x3"] {
            insert_chunk(&conn, id, "symbol");
            insert_vector(&conn, id, 2, "f32", &encode_f32(&[0.0, 1.0]));
        }
        let ranked = dense_scan(&conn, &[0.0f32, 1.0], 3, true).unwrap();
        assert_eq!(
            ranked.iter().map(|r| r.chunk_id.as_str()).collect::<Vec<_>>(),
            vec!["x1", "x2", "x3"]
        );
        assert!((ranked[0].score - ranked[2].score).abs() < f32::EPSILON);
    }

    #[test]
    fn scan_decodes_int8_rows_and_mixed_format_stores() {
        let conn = test_db();
        // int8 row: [0.6, 0.8] as codes [60, 80] scale 0.01.
        let mut blob = 0.01f32.to_le_bytes().to_vec();
        blob.extend([60i8, 80].iter().map(|&c| c as u8));
        insert_chunk(&conn, "q8", "symbol");
        insert_vector(&conn, "q8", 2, "int8", &blob);
        // f32 row in the same store: exactly opposite direction.
        insert_chunk(&conn, "qf", "symbol");
        insert_vector(&conn, "qf", 2, "f32", &encode_f32(&[-1.0, 0.0]));
        let ranked = dense_scan(&conn, &[1.0f32, 0.0], 2, true).unwrap();
        assert_eq!(ranked[0].chunk_id, "q8");
        assert!(
            (ranked[0].score - 0.6).abs() < 1e-5,
            "int8 dequantized cosine ~0.6, got {}",
            ranked[0].score
        );
        assert!((ranked[1].score - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn fallback_chunks_excluded_when_requested() {
        let conn = test_db();
        insert_chunk(&conn, "sym", "symbol");
        insert_vector(&conn, "sym", 2, "f32", &encode_f32(&[1.0, 0.0]));
        insert_chunk(&conn, "coarse", "fallback");
        insert_vector(&conn, "coarse", 2, "f32", &encode_f32(&[1.0, 0.0]));
        assert_eq!(dense_scan(&conn, &[1.0, 0.0], 10, true).unwrap().len(), 2);
        let filtered = dense_scan(&conn, &[1.0, 0.0], 10, false).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].chunk_id, "sym");
    }

    /// Non-finite DECODED values (NaN f32 words, NaN int8 scale) are
    /// corrupt rows: SKIPPED (counted), never surfaced and never an error
    /// — total_cmp orders NaN greatest, so a NaN candidate would steal a
    /// top-k slot ahead of every real hit.
    #[test]
    fn non_finite_rows_are_skipped_not_surfaced_nor_errors() {
        let conn = test_db();
        // Healthy row: unit vector along the query axis.
        insert_chunk(&conn, "good", "symbol");
        insert_vector(&conn, "good", 2, "f32", &encode_f32(&[1.0, 0.0]));
        // NaN f32 words — a correct-length blob that decodes to NaN.
        insert_chunk(&conn, "nan_f32", "symbol");
        let nan_words: Vec<u8> = [f32::NAN, f32::NAN]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        insert_vector(&conn, "nan_f32", 2, "f32", &nan_words);
        // NaN int8 scale prefix + benign codes.
        insert_chunk(&conn, "nan_q8", "symbol");
        let mut nan_q8 = f32::NAN.to_le_bytes().to_vec();
        nan_q8.extend([0i8, 0].iter().map(|&c| c as u8));
        insert_vector(&conn, "nan_q8", 2, "int8", &nan_q8);

        let ranked = dense_scan(&conn, &[1.0, 0.0], 5, true).unwrap();
        assert_eq!(ranked.len(), 1, "only the healthy row survives: {ranked:?}");
        assert_eq!(ranked[0].chunk_id, "good");
        assert!((ranked[0].score - 1.0).abs() < 1e-6);
    }

    #[test]
    fn empty_table_returns_empty_and_zero_top_k() {
        let conn = test_db();
        assert!(dense_scan(&conn, &[1.0, 0.0], 10, true).unwrap().is_empty());
        insert_chunk(&conn, "s", "symbol");
        insert_vector(&conn, "s", 2, "f32", &encode_f32(&[1.0, 0.0]));
        assert!(dense_scan(&conn, &[1.0, 0.0], 0, true).unwrap().is_empty());
    }

    // ── Corrupt-row rejection through the scan ───────────────────────────

    #[test]
    fn scan_rejects_wrong_size_blob_with_clear_error() {
        let conn = test_db();
        insert_chunk(&conn, "corrupt", "symbol");
        insert_vector(&conn, "corrupt", 4, "f32", &vec![0u8; 15]); // 15 ≠ 16
        let err = dense_scan(&conn, &[1.0, 0.0, 0.0, 0.0], 1, true).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("corrupt"), "message: {}", msg);
        assert!(msg.contains("\"corrupt\""), "names the chunk: {}", msg);
        assert!(msg.contains("16 bytes"), "message: {}", msg);
    }

    #[test]
    fn scan_rejects_dim_mismatch_and_empty_and_degenerate_query() {
        let conn = test_db();
        insert_chunk(&conn, "d768", "symbol");
        insert_vector(&conn, "d768", 4, "f32", &encode_f32(&[1.0, 0.0, 0.0, 0.0]));
        let err = dense_scan(&conn, &[1.0, 0.0], 1, true).unwrap_err();
        assert!(err.to_string().contains("dimension mismatch"));
        assert!(matches!(
            err,
            ScanError::DimMismatch {
                stored_dim: 4,
                query_dim: 2,
                ..
            }
        ));
        assert!(matches!(
            dense_scan(&conn, &[], 1, true),
            Err(ScanError::EmptyQuery)
        ));
        assert!(matches!(
            dense_scan(&conn, &[0.0, 0.0], 1, true),
            Err(ScanError::DegenerateQuery)
        ));
    }
}

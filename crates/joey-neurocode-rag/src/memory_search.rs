//! Memory retrieval leg (feature 027, spec 027): hybrid search over the
//! per-project graph.db memory tables (`memory_episodes`/`memory_preferences`/
//! `memory_vectors`, schema v4). Reuses the RAG machinery as functions per
//! FR-005: embedding via `EmbeddingBackend` resolve, the vector quantize
//! codec (byte-identical BLOBs to rag_vectors), pure `rrf_fuse` fusion. The
//! dense scan is table-local (`memory_dense_scan`) mirroring vector/scan.rs's
//! decode+cosine loop — `dense_scan` itself is hardwired to rag_* tables and
//! is NOT parameterizable, so the same math runs against memory_vectors;
//! zero SQL touches rag_* tables (namespace isolation).

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use rusqlite::Connection;

use crate::embed::{resolve, EmbedError, EmbeddingBackend};
use crate::search::rrf::{rrf_fuse, RrfEntry};
use crate::vector::quantize::{decode, encode, Quantization};
use crate::vector::scan::DenseCandidate;

/// Snippet cap (~300 chars) for [`MemoryHit::snippet`].
const SNIPPET_CAP: usize = 300;

/// Per-leg candidate breadth, mirroring the hybrid pipeline's leg width
/// (`limit.saturating_mul(2).max(20)` — search/hybrid.rs).
fn leg_width(top_k: usize) -> usize {
    top_k.saturating_mul(2).max(20)
}

// ─── Public types ────────────────────────────────────────────────────────────

/// One memory search request: raw query text + result budget.
#[derive(Debug, Clone)]
pub struct MemorySearchRequest {
    pub query: String,
    pub top_k: usize,
}

/// One fused memory hit. `item_kind` is `"episode" | "preference"`.
///
/// - `title` = episode.title, or `category: statement` for a preference.
/// - `snippet` = task/approach/lessons joined (episode) or the statement
///   (preference), capped at ~300 chars.
#[derive(Debug, Clone)]
pub struct MemoryHit {
    pub item_id: String,
    /// `"episode"` or `"preference"`.
    pub item_kind: String,
    /// Fused RRF score (`rrf_fuse`).
    pub score: f64,
    pub title: String,
    pub snippet: String,
    pub created_at: String,
}

/// Failures of the memory retrieval leg.
///
/// `DegradedKeywordOnly` is a caller-facing marker only —
/// [`search_memory`] NEVER returns it: dense-leg degradation (no backend,
/// embed failure) silently skips the dense leg and returns the keyword-only
/// hits, mirroring FR-008. `Err` is only for hard failures (sqlite on the
/// keyword/fetch legs, embed-config failures on the [`embed_texts`] path).
#[derive(Debug)]
pub enum MemorySearchError {
    Sqlite(rusqlite::Error),
    Embed(crate::embed::EmbedError),
    DegradedKeywordOnly,
}

impl fmt::Display for MemorySearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(e) => write!(f, "memory search sqlite error: {e}"),
            Self::Embed(e) => write!(f, "memory search embedding error: {e}"),
            Self::DegradedKeywordOnly => write!(
                f,
                "memory search degraded (keyword-only): no embedding backend served the dense leg"
            ),
        }
    }
}

impl std::error::Error for MemorySearchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(e) => Some(e),
            Self::Embed(e) => Some(e),
            Self::DegradedKeywordOnly => None,
        }
    }
}

impl From<rusqlite::Error> for MemorySearchError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

impl From<EmbedError> for MemorySearchError {
    fn from(e: EmbedError) -> Self {
        Self::Embed(e)
    }
}

// ─── Key helpers ─────────────────────────────────────────────────────────────

/// Opaque RRF key for an episode (kind tag + raw item id — rrf keys are
/// opaque ids; the tag keeps a same-text id across the two tables distinct).
fn episode_key(id: &str) -> String {
    format!("memory://episode/{id}")
}

/// Opaque RRF key for a preference.
fn preference_key(id: &str) -> String {
    format!("memory://preference/{id}")
}

/// Escape SQL LIKE wildcards (`%`, `_`, `\`) so a token matches literally
/// (`ESCAPE '\'` in the SQL) — same rule as search/hybrid.rs's `like_escape`.
fn like_escape(token: &str) -> String {
    token
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Cap a string at ~`n` chars (char-boundary safe).
fn cap_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Episode snippet: task/approach/lessons (non-empty parts) joined, capped.
fn episode_snippet(task: &str, approach: &str, lessons: &str) -> String {
    let parts: Vec<&str> = [task, approach, lessons]
        .into_iter()
        .filter(|s| !s.trim().is_empty())
        .collect();
    cap_chars(&parts.join(" | "), SNIPPET_CAP)
}

// ─── Keyword legs ────────────────────────────────────────────────────────────

/// Keyword leg over `memory_episodes`: every whitespace token must appear
/// (case-insensitively, substring — SQLite LIKE is ASCII case-insensitive)
/// in `title` OR `task` OR `approach` OR `lessons`. Escaping/style mirrors
/// search/hybrid.rs's `like_search_chunks`. Deterministic order: rowid
/// ascending (occurrence order).
fn keyword_episodes(
    conn: &Connection,
    tokens: &[&str],
    limit: usize,
) -> Result<Vec<String>, rusqlite::Error> {
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    let mut conditions: Vec<String> = Vec::with_capacity(tokens.len());
    let mut params: Vec<String> = Vec::with_capacity(tokens.len() * 4);
    for tok in tokens {
        let pat = format!("%{}%", like_escape(tok));
        conditions.push(
            "(title LIKE ? ESCAPE '\\' OR task LIKE ? ESCAPE '\\' \
             OR approach LIKE ? ESCAPE '\\' OR lessons LIKE ? ESCAPE '\\')"
                .to_string(),
        );
        for _ in 0..4 {
            params.push(pat.clone());
        }
    }
    let sql = format!(
        "SELECT id FROM memory_episodes WHERE {} ORDER BY rowid LIMIT {}",
        conditions.join(" AND "),
        limit
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| r.get::<_, String>(0))?;
    Ok(rows.flatten().collect())
}

/// Keyword leg over `memory_preferences`: every token must appear in
/// `statement` OR `category`. Same escaping/style as [`keyword_episodes`];
/// rowid ascending.
fn keyword_preferences(
    conn: &Connection,
    tokens: &[&str],
    limit: usize,
) -> Result<Vec<String>, rusqlite::Error> {
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    let mut conditions: Vec<String> = Vec::with_capacity(tokens.len());
    let mut params: Vec<String> = Vec::with_capacity(tokens.len() * 2);
    for tok in tokens {
        let pat = format!("%{}%", like_escape(tok));
        conditions
            .push("(statement LIKE ? ESCAPE '\\' OR category LIKE ? ESCAPE '\\')".to_string());
        params.push(pat.clone());
        params.push(pat);
    }
    let sql = format!(
        "SELECT id FROM memory_preferences WHERE {} ORDER BY rowid LIMIT {}",
        conditions.join(" AND "),
        limit
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |r| r.get::<_, String>(0))?;
    Ok(rows.flatten().collect())
}

// ─── Dense leg ───────────────────────────────────────────────────────────────

/// Blocking bridge over the async `EmbeddingBackend::embed` — a dedicated
/// thread with its own current-thread runtime so it is safe to call from
/// plain threads and inside an async runtime alike (the same pattern as the
/// CLI RAG wiring's `block_on_embed`).
fn block_on_embed(
    backend: &Arc<dyn EmbeddingBackend>,
    batch: &[String],
) -> Result<Vec<Vec<f32>>, EmbedError> {
    let backend = Arc::clone(backend);
    let batch = batch.to_vec();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| EmbedError::Other(format!("embed runtime: {e}")))?;
        rt.block_on(async move { backend.embed(&batch).await })
    })
    .join()
    .map_err(|_| EmbedError::Other("embedding thread panicked".to_string()))?
}

// ─── Hit materialization ─────────────────────────────────────────────────────

/// Fetch the text row for one fused key and build the [`MemoryHit`].
/// `Ok(None)` = the row vanished between the scan and this fetch (skipped,
/// never an error); real SQL failures propagate.
fn fetch_hit(conn: &Connection, key: &str, score: f64) -> Result<Option<MemoryHit>, rusqlite::Error> {
    if let Some(id) = key.strip_prefix("memory://episode/") {
        let row = conn.query_row(
            "SELECT title, task, approach, lessons, created_at \
             FROM memory_episodes WHERE id = ?1",
            rusqlite::params![id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            },
        );
        match row {
            Ok((title, task, approach, lessons, created_at)) => Ok(Some(MemoryHit {
                item_id: id.to_string(),
                item_kind: "episode".to_string(),
                score,
                title,
                snippet: episode_snippet(&task, &approach, &lessons),
                created_at,
            })),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    } else if let Some(id) = key.strip_prefix("memory://preference/") {
        let row = conn.query_row(
            "SELECT category, statement, created_at FROM memory_preferences WHERE id = ?1",
            rusqlite::params![id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        );
        match row {
            Ok((category, statement, created_at)) => Ok(Some(MemoryHit {
                item_id: id.to_string(),
                item_kind: "preference".to_string(),
                score,
                title: format!("{}: {}", category, statement),
                snippet: cap_chars(&statement, SNIPPET_CAP),
                created_at,
            })),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    } else {
        Ok(None)
    }
}

// ─── search_memory ───────────────────────────────────────────────────────────

/// Hybrid memory search over the per-project memory tables: keyword (LIKE)
/// leg + dense (cosine over `memory_vectors`) leg fused via `rrf_fuse`,
/// truncated to `top_k`, text rows fetched by id for the snippets.
///
/// Dense-leg degradation (no backend resolves, or the embed call fails)
/// NEVER errors — the dense leg is simply skipped and the keyword-only hits
/// are returned (FR-008 semantics; [`MemorySearchError::DegradedKeywordOnly`]
/// exists as a caller-facing marker only). `Err` is only for hard sqlite
/// failures. Zero SQL here touches rag_* tables (namespace isolation).
pub fn search_memory(
    conn: &Connection,
    cfg: &crate::config::RagConfig,
    store: Option<&joey_neurocode::graph::store::GraphStore>,
    req: &MemorySearchRequest,
) -> Result<Vec<MemoryHit>, MemorySearchError> {
    if req.query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let top_k = req.top_k.max(1);
    let width = leg_width(top_k);
    let tokens: Vec<&str> = req.query.split_whitespace().collect();

    // ── Keyword leg (hard-fail channel: sqlite only) ───────────────────
    // Occurrence order: episodes by rowid first, then preferences by rowid;
    // ranks are the ascending 1-based ordinals across that order.
    let mut keyword_ranks: HashMap<String, u32> = HashMap::new();
    {
        let mut seen: HashSet<String> = HashSet::new();
        let mut ordered: Vec<String> = Vec::new();
        ordered.extend(keyword_episodes(conn, &tokens, width)?.iter().map(|id| episode_key(id)));
        ordered.extend(
            keyword_preferences(conn, &tokens, width)?.iter().map(|id| preference_key(id)),
        );
        for (idx, key) in ordered.into_iter().enumerate() {
            if seen.insert(key.clone()) {
                keyword_ranks.insert(key, idx as u32 + 1);
            }
        }
    }

    // ── Dense leg (degradable: any backend/embed problem skips it) ─────
    // The call shape mirrors the production search path exactly: the
    // resolved backend from `embed::resolve` is a documents-kind adapter,
    // and the CLI wiring hands RAW query text to it (search/hybrid.rs
    // pre-applies the profile QUERY prefix, the wiring strips it and lets
    // the backend apply its own — net input is the raw query).
    //
    // Panic-proofing (FR-008 silent degradation): `resolve` loads the
    // ONNX Runtime dylib, which PANICS (ort dlopen failure) on machines
    // without it — the resolve+embed portion runs under catch_unwind so a
    // panic degrades to keyword-only instead of unwinding the caller. An
    // empty memory_vectors corpus short-circuits the whole leg before any
    // backend resolution (nothing to score, and no embedder load paid).
    let mut dense_ranks: HashMap<String, u32> = HashMap::new();
    let memory_vector_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM memory_vectors", [], |r| r.get(0))?;
    if memory_vector_count > 0 {
        let embedded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            resolve(cfg, store)
                .ok()
                .and_then(|(_, backend)| backend)
                .map(|backend| block_on_embed(&backend, &[req.query.clone()]))
        }));
        match embedded {
            Ok(Some(Ok(vectors))) if vectors.len() == 1 => {
                let candidates = memory_dense_scan(conn, &vectors[0], width)?;
                for (position, c) in candidates.iter().enumerate() {
                    // The scan returns raw item ids; the kind tag lives in
                    // memory_vectors.item_kind (never in rag_* tables).
                    let kind: Option<String> = conn
                        .query_row(
                            "SELECT item_kind FROM memory_vectors WHERE item_id = ?1",
                            rusqlite::params![c.chunk_id],
                            |r| r.get(0),
                        )
                        .ok();
                    let key = match kind.as_deref() {
                        Some("episode") => episode_key(&c.chunk_id),
                        Some("preference") => preference_key(&c.chunk_id),
                        _ => continue,
                    };
                    dense_ranks.entry(key).or_insert(position as u32 + 1);
                }
            }
            Err(_panic) => {
                tracing::warn!(
                    target: "neurocode",
                    "degraded to keyword-only memory search after embedder panic (dense leg skipped)"
                );
            }
            // Ok(None) (no backend), Ok(Some(Err(_))) (embed failure), or a
            // wrong-size vector batch: dense leg skipped, silently.
            _ => {}
        }
    }

    // ── Fuse (pure rrf_fuse) → top_k → fetch text rows ─────────────────
    let mut all_keys: Vec<String> = keyword_ranks.keys().cloned().collect();
    all_keys.extend(dense_ranks.keys().cloned());
    let mut seen: HashSet<String> = HashSet::new();
    let mut entries: Vec<RrfEntry> = Vec::with_capacity(all_keys.len());
    for key in all_keys {
        if seen.insert(key.clone()) {
            entries.push(RrfEntry {
                keyword_rank: keyword_ranks.get(&key).copied(),
                dense_rank: dense_ranks.get(&key).copied(),
                chunk_id: key,
            });
        }
    }
    let mut hits: Vec<MemoryHit> = Vec::with_capacity(top_k);
    for scored in rrf_fuse(entries).into_iter().take(top_k) {
        if let Some(hit) = fetch_hit(conn, &scored.chunk_id, scored.score)? {
            hits.push(hit);
        }
    }
    Ok(hits)
}

// ─── memory_dense_scan ───────────────────────────────────────────────────────

/// Raw row materialized from SQLite before scoring (a rusqlite `Connection`
/// is not `Sync`; all SQLite access stays on this thread) — the same shape
/// as vector/scan.rs's `RawVectorRow`.
struct RawMemoryVectorRow {
    chunk_id: String,
    dim: usize,
    quantization: String,
    blob: Vec<u8>,
}

/// Exhaustive cosine scan over `memory_vectors`, mirroring vector/scan.rs's
/// decode+cosine loop (`dense_scan` itself is hardwired to rag_* tables and
/// is not parameterizable — the same math runs here against memory_vectors):
///
/// - SELECT `item_id AS chunk_id, dim, quantization, vector`; decode each
///   BLOB with the SAME canonical quantize decoders (`f32` vs `int8` by the
///   `quantization` column), byte-length-validated on every decode;
/// - the query is defensively L2-normalized so scores are true cosines
///   (stored vectors are normalized ⇒ dot product IS the cosine);
/// - rows that cannot serve (unknown quantization, dim ≠ query dim, corrupt
///   BLOB, non-finite score) are SKIPPED and counted — the `rusqlite::Error`
///   return channel cannot carry scan-class errors, and a memory-table row
///   must never fail the turn;
/// - deterministic top-k: score descending (`total_cmp`), tie-break
///   `chunk_id` lexical ascending.
pub fn memory_dense_scan(
    conn: &Connection,
    query: &[f32],
    top_k: usize,
) -> Result<Vec<DenseCandidate>, rusqlite::Error> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let norm = query.iter().map(|v| v * v).sum::<f32>().sqrt();
    if !norm.is_finite() || norm == 0.0 {
        return Ok(Vec::new());
    }
    let q: Vec<f32> = query.iter().map(|v| v / norm).collect();
    let q_dim = q.len();

    let mut stmt =
        conn.prepare("SELECT item_id AS chunk_id, dim, quantization, vector FROM memory_vectors")?;
    let rows: Vec<RawMemoryVectorRow> = {
        let mut it = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = it.next()? {
            out.push(RawMemoryVectorRow {
                chunk_id: row.get(0)?,
                dim: row.get::<_, i64>(1)? as usize,
                quantization: row.get(2)?,
                blob: row.get(3)?,
            });
        }
        out
    };
    drop(stmt);

    let mut skipped = 0usize;
    let mut candidates: Vec<DenseCandidate> = Vec::with_capacity(rows.len());
    for r in rows {
        let scored = (|| -> Option<DenseCandidate> {
            let quantization = Quantization::parse(&r.quantization)?;
            if r.dim != q_dim {
                return None;
            }
            // Canonical decoder enforces the exact byte length per encoding.
            let v = decode(&r.blob, r.dim, quantization).ok()?;
            // Both sides unit-length ⇒ dot == cosine.
            let score: f32 = v.iter().zip(&q).map(|(a, b)| a * b).sum();
            if !score.is_finite() {
                return None;
            }
            Some(DenseCandidate { chunk_id: r.chunk_id.clone(), score })
        })();
        match scored {
            Some(c) => candidates.push(c),
            None => skipped += 1,
        }
    }
    if skipped > 0 {
        eprintln!(
            "[joey neurocode] memory dense scan skipped {skipped} unusable vector row(s)"
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

// ─── index_memory_vector ─────────────────────────────────────────────────────

/// Encode and upsert one memory vector — the single codec source for
/// `memory_vectors` (byte-identical BLOBs to `rag_vectors`): the SAME
/// canonical quantize encoders the chunker/write_index path uses
/// (`encode_f32` LE words, or `encode_int8` scale-prefixed codes when
/// `quantize_int8`), `dim = embedding.len()`, `INSERT OR REPLACE` keyed by
/// `item_id`.
pub fn index_memory_vector(
    conn: &Connection,
    item_id: &str,
    item_kind: &str,
    embedding: &[f32],
    quantize_int8: bool,
) -> Result<(), rusqlite::Error> {
    let quantization = if quantize_int8 { Quantization::Int8 } else { Quantization::F32 };
    let blob = encode(embedding, quantization);
    conn.execute(
        "INSERT OR REPLACE INTO memory_vectors \
         (item_id, item_kind, dim, quantization, vector) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            item_id,
            item_kind,
            embedding.len() as i64,
            quantization.as_str(),
            blob
        ],
    )?;
    Ok(())
}

// ─── embed_texts ─────────────────────────────────────────────────────────────

/// Embed raw texts through the resolved backend — the write-side helper for
/// memory capture (episode input = title + task + approach + lessons;
/// preference input = statement (+ category); callers build the RAW text,
/// this function embeds it).
///
/// Prefix semantics match index/chunker.rs exactly: the profile DOCUMENT
/// prefix is applied at EMBED TIME ONLY, by the embedder — callers pass RAW
/// text, the resolved documents-kind backend prepends the prefix, and it is
/// never stored or hashed. One batch call, order-preserving.
pub fn embed_texts(
    cfg: &crate::config::RagConfig,
    store: Option<&joey_neurocode::graph::store::GraphStore>,
    texts: &[String],
) -> Result<Vec<Vec<f32>>, MemorySearchError> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    // Panic-proof resolve (FR-008): `resolve` loads the ONNX Runtime dylib,
    // which PANICS (ort dlopen failure) on machines without it — the panic
    // is caught and surfaced as a structured Err instead of unwinding.
    let resolved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| resolve(cfg, store)));
    let (decision, backend) = match resolved {
        Ok(r) => r.map_err(MemorySearchError::Embed)?,
        Err(_panic) => {
            return Err(MemorySearchError::Embed(EmbedError::Other(
                "embedding backend resolution panicked (likely ONNX Runtime dylib load failure)"
                    .to_string(),
            )));
        }
    };
    let backend = backend.ok_or_else(|| {
        MemorySearchError::Embed(EmbedError::MissingArtifacts(format!(
            "no embedding backend resolved for memory embedding: {}",
            decision.degradation_reason
        )))
    })?;
    let out = block_on_embed(&backend, texts).map_err(MemorySearchError::Embed)?;
    if out.len() != texts.len() {
        return Err(MemorySearchError::Embed(EmbedError::EmptyResult(format!(
            "backend returned {} vectors for {} texts (order/size must be preserved)",
            out.len(),
            texts.len()
        ))));
    }
    Ok(out)
}

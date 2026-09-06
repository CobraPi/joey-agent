//! Hybrid dense + keyword retrieval (T013 — dense leg).
//!
//! Dense leg (query embedded via the profile QUERY prefix) fused with the
//! existing FTS5 `query_fts` BM25 leg (negated rank → ascending ordinals,
//! research.md R1 gotcha) via RRF; exact-symbol-first guarantee; file-scope
//! filter applied in BOTH legs before fusion; degradation path mapping every
//! `EmbedError` class to keyword-only with `mode_reason` (contracts/
//! hybrid-search.md).
//!
//! ## What lives here now (T013)
//!
//! The dense-leg primitive only — contracts/hybrid-search.md stage 3:
//!
//! - [`dense_leg`] — scan an ALREADY-EMBEDDED query against `rag_vectors`
//!   (joined to `rag_chunks`) via [`crate::vector::scan::dense_scan`]:
//!   exhaustive cosine (dot on normalized vectors) with rayon, BLOB decode
//!   validation for both `f32` (`dim×4`) and `int8` (`dim+4` with f32
//!   scale prefix) rows, deterministic top-k.
//! - [`dense_leg_with_embedder`] — the documented wrapper that applies the
//!   model profile's QUERY prefix to the raw query BEFORE tokenization
//!   (data-model.md §2 "queries are embedded with the profile's query
//!   prefix") and embeds it through an injected closure. The embedding
//!   backend trait lands in T010; until it is wired into the hybrid caller
//!   (T016+), callers inject embedding as a batch-oriented closure
//!   (`Fn(&[String]) -> Result<Vec<Vec<f32>>, E>` — the same shape as the
//!   future `EmbeddingBackend::embed`). This module NEVER touches `embed/`
//!   internals.
//!
//! ## What lands later (same file, sibling tasks)
//!
//! - T020 — degradation: every `EmbedError` class → KeywordOnly +
//!   `mode_reason`, never a turn hard-fail. [`DenseLegError::Embed`] is the
//!   degradation signal those callers will intercept.
//!
//! RRF fusion itself lives in `search/rrf.rs` (T017).

use std::fmt;

use rusqlite::Connection;

use crate::embed::profiles::EmbedProfile;
use crate::vector::scan::{dense_scan, DenseCandidate, ScanError};

/// The RRF `k` constant shared with [`crate::search::rrf`] (k = 60,
/// contracts/hybrid-search.md stage 4).
pub use crate::search::rrf::RRF_K;

// ─── dense_leg (primitive — query already embedded) ─────────────────────────

/// Dense leg over an already-embedded query: exhaustive cosine scan with
/// deterministic top-`top_k` selection (contracts/hybrid-search.md stage 3).
///
/// The caller (hybrid pipeline, T016+) supplies the query embedding via
/// the embedding backend; this function only scores. See
/// [`dense_scan`] for decode-validation, rayon, and tie-break semantics —
/// this is a thin pass-through that exists so the pipeline has ONE named
/// dense-leg entry point (contract stage 3) independent of how the query
/// was embedded.
pub fn dense_leg(
    conn: &Connection,
    query_embedding: &[f32],
    top_k: usize,
    include_fallback_chunks: bool,
) -> Result<Vec<DenseCandidate>, ScanError> {
    dense_scan(conn, query_embedding, top_k, include_fallback_chunks)
}

// ─── Wrapper: profile QUERY prefix + injected embedder ──────────────────────

/// Failures of the prefix + embed + scan path. [`Self::Embed`] carries the
/// injected backend's failure reason verbatim — the T020 degradation path
/// maps it (and, once T010 lands, every `EmbedError` taxonomy class) to
/// KeywordOnly mode with `mode_reason`; the turn never hard-fails (FR-008).
#[derive(Debug)]
pub enum DenseLegError {
    /// The injected embedder failed — reason preserved for degradation
    /// reporting (`SearchDiagnostics.mode_reason`, T020).
    Embed(String),
    /// The embedder returned no vector for the single-element query batch
    /// (order-preserving backends must return exactly one).
    EmptyEmbeddingResult,
    /// Scan/decode failure (corrupt BLOB, dim mismatch, SQL error).
    Scan(ScanError),
}

impl fmt::Display for DenseLegError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Embed(reason) => write!(f, "dense leg embedder failure: {}", reason),
            Self::EmptyEmbeddingResult => write!(
                f,
                "dense leg embedder returned no vector for the query batch"
            ),
            Self::Scan(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for DenseLegError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Scan(e) => Some(e),
            _ => None,
        }
    }
}

impl From<ScanError> for DenseLegError {
    fn from(e: ScanError) -> Self {
        Self::Scan(e)
    }
}

/// Documented wrapper applying the profile QUERY prefix before embedding:
/// the dense leg for a RAW query string.
///
/// Contract rule (profiles rule 1 / data-model.md §2): the embedder MUST
/// consume the profile's query prefix prepended to the raw text BEFORE
/// tokenization — `profile.query_input(query)` produces exactly that
/// string (`prefix ++ raw`, nothing else; raw passed verbatim, no
/// trimming). The DOCUMENT prefix is deliberately NOT applied to queries.
///
/// Embedding is injected as a batch closure (`Fn(&[String]) ->
/// Result<Vec<Vec<f32>>, E>`) matching the future
/// `EmbeddingBackend::embed` shape (T010) — when the backend trait lands,
/// the hybrid caller (T016+) passes `|texts| backend.embed(texts)` here;
/// the signature already fits, so no rewrite is needed.
///
/// The single embedded vector is scanned via [`dense_leg`]. Errors:
/// embedder failure → [`DenseLegError::Embed`] (degradation signal);
/// scan/decode failures → [`DenseLegError::Scan`].
pub fn dense_leg_with_embedder<E: fmt::Display, F>(
    conn: &Connection,
    profile: &EmbedProfile,
    query: &str,
    top_k: usize,
    include_fallback_chunks: bool,
    embed: F,
) -> Result<Vec<DenseCandidate>, DenseLegError>
where
    F: FnOnce(&[String]) -> Result<Vec<Vec<f32>>, E>,
{
    // Pre-tokenization input: QUERY prefix ++ raw query, verbatim.
    let model_input = profile.query_input(query);
    let batch = [model_input];
    let mut vectors = embed(&batch).map_err(|e| DenseLegError::Embed(e.to_string()))?;
    if vectors.len() != 1 {
        return Err(DenseLegError::EmptyEmbeddingResult);
    }
    let query_embedding = vectors.pop().expect("len checked == 1");
    dense_leg(conn, &query_embedding, top_k, include_fallback_chunks).map_err(Into::into)
}

// ─── T015: CLI/TUI search orchestration (`search_cli`) ──────────────────────
//
// The thin orchestration the `/neurocode search` CLI path (and, through it,
// the TUI result view) calls. Shared value types live HERE so the CLI and
// the TUI render the SAME payload (contracts/neurocode-rag-command.md §
// Grammar additions; Principle II parity).
//
// What exists at T015 (deliberate scope — full hybrid fusion is T016–T020):
//
// - keyword leg: the existing FTS5 `query_fts` (symbol hits mapped to their
//   `rag_chunks` rows) MERGED with a chunk-table LIKE scan over
//   `symbol_name`/`source_path` (the only way fallback chunks — which have
//   no `code_artifacts` row — are reachable; FR-014 coverage);
// - dense leg: [`dense_leg_with_embedder`] with an INJECTED embedder; the
//   CLI passes the always-failing no-backend embedder, which the FR-008
//   degradation machinery converts into `mode = keyword_only` +
//   `mode_reason` (the degradation story until T009/T010 land a backend);
// - merge: dense hits (cosine order) first, keyword-only hits after
//   (FTS order, then chunk_id) — the RRF fusion of T017 replaces this;
// - file-scope glob filter applied to BOTH legs BEFORE the merge (T019's
//   rule, honored early);
// - context expansion: stage-7 clamped ±`expand_lines` read — lives in
//   `search/expand.rs` (T026 formalized the T015 inline form there);
// - relation expansion: stage-8 bounded BFS over `rag_chunk_edges`
//   (`search/expand.rs`, T029) — relations appended per-result AFTER
//   the fused list, never displacing it (FR-007).
//
// T016–T020 land IN THIS FILE on top of these types (ordinal conversion,
// RRF, exact-symbol-first, formalized pre-fusion filter, full degradation
// taxonomy) — `search_cli*` is structured so those replace internals, not
// the surface.

use std::path::Path;

use joey_neurocode::graph::{ArtifactKind, CodeArtifactNode, GraphStore};

use crate::search::expand;

/// `/neurocode search` request — data-model.md §4 `SearchRequest`, field
/// names following the tool payload (`file_filter`, `expand_lines`).
///
/// Built by the CLI grammar parser (`--path`→`file_filter`, `--limit`→
/// `limit`, `--expand-lines`→`expand_lines`, `--relations`→`relation_depth`
/// validated 0–2 — contracts/neurocode-rag-command.md).
#[derive(Debug, Clone)]
pub struct SearchRequest {
    /// Natural language and/or exact symbol names. MUST be non-empty after
    /// trim (validated in [`search_cli_with_embedder`] — edge case 1).
    pub query: String,
    /// Glob restricting results to matching file paths (FR-003).
    pub file_filter: Option<String>,
    /// Max results (default `neurocode.rag.top_k`).
    pub limit: usize,
    /// ± context lines around the hit (FR-006; clamped 0–200 upstream).
    pub expand_lines: u32,
    /// Relationship expansion depth 0–2 (FR-007; expansion itself is T029).
    pub relation_depth: u8,
    /// Whether fallback coarse chunks participate (FR-014).
    pub include_fallback_chunks: bool,
}

/// The FR-014 badge: how a result's chunk kind is rendered everywhere
/// (`symbol-aligned | fallback` — visible distinction in CLI and TUI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkBadge {
    SymbolAligned,
    Fallback,
}

impl ChunkBadge {
    /// The contract's exact wire string (tools payload `chunk_kind`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SymbolAligned => "symbol-aligned",
            Self::Fallback => "fallback",
        }
    }
}

impl fmt::Display for ChunkBadge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One related entity appended by relation expansion (T029 populates;
/// carried now so the payload shape is final).
#[derive(Debug, Clone)]
pub struct RelatedEntity {
    pub file: String,
    pub symbol: Option<String>,
    pub relation_kind: String,
}

/// One entry in the blended response list (data-model.md §5 `RankedResult`).
#[derive(Debug, Clone)]
pub struct RankedResult {
    pub chunk_id: String,
    /// Denormalized so consumers act without a join (FR-001 acceptance 1).
    pub file: String,
    pub symbol: Option<String>,
    pub symbol_kind: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    /// FR-014 badge.
    pub chunk_kind: ChunkBadge,
    /// Indicative until RRF (T017): cosine for dense hits, single-leg RRF
    /// `1/(60 + keyword_rank)` for keyword-only hits.
    pub fused_score: f64,
    /// Rank in the dense list; `None` in keyword-only degradation.
    pub semantic_rank: Option<u32>,
    /// Rank in the keyword list; `None` if the chunk didn't match keywords.
    pub keyword_rank: Option<u32>,
    /// Present **iff** results are keyword-only (FR-008 explicit indication).
    pub degradation_note: Option<String>,
    /// Clamped ±`expand_lines` context read at query time; `None` when the
    /// file is missing (`context_absent`, never an error — stage 7).
    pub context: Option<String>,
    /// Populated (possibly empty) only when `relation_depth > 0` (T029).
    pub relations: Vec<RelatedEntity>,
}

/// FR-008 mode indication (`mode` in the tool-shaped payload).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    Hybrid,
    KeywordOnly,
}

impl SearchMode {
    /// The contract's exact wire string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hybrid => "hybrid",
            Self::KeywordOnly => "keyword_only",
        }
    }
}

impl fmt::Display for SearchMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The search response shared by the CLI plain-text/JSON renderers and the
/// TUI result view: results + FR-008 diagnostics + the empty-index hint
/// input.
#[derive(Debug, Clone)]
pub struct SearchOutcome {
    pub results: Vec<RankedResult>,
    pub mode: SearchMode,
    /// `Some(reason)` iff `mode == KeywordOnly` (never a turn hard-fail).
    pub mode_reason: Option<String>,
    /// Pre-limit candidate count (contracts/hybrid-search.md).
    pub total_candidates: usize,
    /// Rows in `rag_chunks` at query time (drives the "index is empty —
    /// run /neurocode index" hint; distinct from a genuine no-match).
    pub index_chunk_count: u64,
}

/// Failures of the search entry points. `Validation` covers edge case 1
/// (empty/whitespace query → clear message, NOT a search, NOT a crash).
#[derive(Debug)]
pub enum SearchError {
    Validation(String),
    Store(String),
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(msg) => write!(f, "search validation: {msg}"),
            Self::Store(msg) => write!(f, "search store error: {msg}"),
        }
    }
}

impl std::error::Error for SearchError {}

/// The degradation reason reported when no embedding backend is wired
/// (the CLI's posture until T009/T010 land a resolvable backend).
pub const NO_EMBEDDER_REASON: &str =
    "no embedding backend available (backend unconfigured or model artifacts \
     absent) — keyword-only fallback";

/// SC-002's exact-symbol-first bar: ≥ 95% of exact-symbol queries must see
/// the exact `symbol_name` match rank first. The guarantee itself is
/// deterministic (a pin, not a probability — see [`pin_exact_symbol_first`]);
/// this named constant encodes the threshold the contract states so the
/// multi-query benchmark asserts against the contract number, not a local
/// magic value (in practice the pin yields 100%).
pub const EXACT_SYMBOL_FIRST_MIN_RATE: f64 = 0.95;

// ─── T020: embedding-failure taxonomy → degradation mapping ─────────────────
//
// The FORMAL `EmbedError` taxonomy lands with T010's backend trait; until
// then this module classifies the EXISTING error types (LocalOnnxError,
// ArtifactError via LocalOnnxError::Artifacts) into the taxonomy CLASSES
// the contract names, parameterized over any embed-closure error `E:
// Display`. The pipeline consumes the class (not the raw string) so the
// T010 wave only needs to grow `classify_embed_failure` — the degradation
// path below it stays as-is.

/// Degradation reason classes (contracts/hybrid-search.md § Degradation
/// semantics; contracts/embedding-backend.md § Error taxonomy, pre-T010
/// form). Every class maps to KeywordOnly — none is a turn hard-fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedFailure {
    /// Model artifacts absent (`model_dir` unset / files missing) — the
    /// clean "no model present" state (T010: `EmbedError::ModelFilesMissing`).
    ModelFilesMissing,
    /// SHA-256 mismatch against the pinned `rag_model_artifacts` row —
    /// refuse to load (T010: `EmbedError::ModelFilesCorrupt`).
    ModelFilesCorrupt,
    /// ONNX Runtime dylib could not be loaded (T010: the load-failure
    /// class of `EmbedError::Runtime`).
    DylibLoad,
    /// Session/tokenizer load failure (T010: the session-failure class of
    /// `EmbedError::Runtime`).
    SessionLoad,
    /// Batch inference failed at run time (T010: `EmbedError::Inference`).
    Inference,
    /// Backend returned no vector for the single-element query batch
    /// (order-preservation violation; T010: `EmbedError::EmptyResult`).
    EmptyResult,
    /// Anything else a backend can report (SQL, IO, transport, dimension
    /// mismatch, …) — still degrades safely, just less specifically.
    Other,
}

impl EmbedFailure {
    /// The stable machine-readable key ( surfaced as the prefix of
    /// `SearchDiagnostics.mode_reason`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelFilesMissing => "model_files_missing",
            Self::ModelFilesCorrupt => "model_files_corrupt",
            Self::DylibLoad => "dylib_load",
            Self::SessionLoad => "session_load",
            Self::Inference => "inference",
            Self::EmptyResult => "empty_result",
            Self::Other => "other",
        }
    }

    /// The human-facing degradation note (FR-008 explicit indication).
    pub fn degradation_note(self) -> String {
        format!(
            "semantic backend degraded ({}): {} — keyword-only results",
            self.as_str(),
            match self {
                Self::ModelFilesMissing => {
                    "embedding model artifacts are absent or unconfigured"
                }
                Self::ModelFilesCorrupt => {
                    "embedding model artifacts failed integrity verification"
                }
                Self::DylibLoad => "the ONNX Runtime dylib could not be loaded",
                Self::SessionLoad => "the embedding session failed to load",
                Self::Inference => "embedding inference failed",
                Self::EmptyResult => "the backend returned no embedding",
                Self::Other => "the embedding backend failed",
            }
        )
    }

    /// Classify a raw backend error message (the `Display` of whatever the
    /// embed closure returned) into a taxonomy class.
    ///
    /// T010 structure: an exact **class-key** match (the
    /// `EmbedError::class_key` vocabulary — see
    /// [`crate::embed::EmbedError`]) resolves structurally first, no
    /// heuristics; Display strings from the existing error types still
    /// classify through the marker heuristics below (`local-onnx
    /// artifacts: model files missing: …`, `local-onnx dylib load: …`).
    /// Remote backend classes land with T030.
    pub fn classify(msg: &str) -> Self {
        // Structural fast path: exact class-key vocabulary match.
        if let Some(class) = Self::from_class_key(msg.trim()) {
            return class;
        }
        let m = msg.to_lowercase();
        if m.contains("model files missing")
            || m.contains("artifacts absent")
            || m.contains("no embedding backend")
            || m.contains("unconfigured")
        {
            Self::ModelFilesMissing
        } else if m.contains("model files corrupt")
            || m.contains("sha-256") && m.contains("mismatch")
        {
            Self::ModelFilesCorrupt
        } else if m.contains("dylib") {
            Self::DylibLoad
        } else if m.contains("session") || m.contains("tokenizer load") {
            Self::SessionLoad
        } else if m.contains("inference") {
            Self::Inference
        } else if m.contains("no vector") || m.contains("empty") || m.contains("no embedding") {
            Self::EmptyResult
        } else {
            Self::Other
        }
    }

    /// Map an exact taxonomy class key (the `EmbedError::class_key`
    /// vocabulary, identical to [`Self::as_str`]) to its class — the
    /// structural entry point T010's `EmbedError` round-trips through.
    pub fn from_class_key(key: &str) -> Option<Self> {
        match key {
            k if k == Self::ModelFilesMissing.as_str() => Some(Self::ModelFilesMissing),
            k if k == Self::ModelFilesCorrupt.as_str() => Some(Self::ModelFilesCorrupt),
            k if k == Self::DylibLoad.as_str() => Some(Self::DylibLoad),
            k if k == Self::SessionLoad.as_str() => Some(Self::SessionLoad),
            k if k == Self::Inference.as_str() => Some(Self::Inference),
            k if k == Self::EmptyResult.as_str() => Some(Self::EmptyResult),
            k if k == Self::Other.as_str() => Some(Self::Other),
            _ => None,
        }
    }

    /// Build the `mode_reason` string the diagnostics carry.
    pub fn mode_reason(self) -> String {
        format!("{} — {}", self.as_str(), self.degradation_note())
    }
}

impl fmt::Display for EmbedFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Classify a [`DenseLegError`] (the pipeline's internal dense-leg
/// failure) into the taxonomy — `Embed` messages are classified, shape
/// errors map structurally (T020).
impl From<&DenseLegError> for EmbedFailure {
    fn from(e: &DenseLegError) -> Self {
        match e {
            DenseLegError::Embed(msg) => Self::classify(msg),
            DenseLegError::EmptyEmbeddingResult => Self::EmptyResult,
            DenseLegError::Scan(_) => Self::Other,
        }
    }
}

/// Structural classification of the T010 [`EmbedError`] taxonomy — no
/// string matching: variant dispatch only. This is the mapping the
/// pipeline uses once callers pass `EmbeddingBackend::embed` errors
/// through; the class keys align with [`EmbedFailure::as_str`] so the
/// `Display`/class-key paths agree with this one. Remote variants
/// (`Unreachable`/`AuthRejected`/`RateLimited`/`MalformedResponse`/
/// `Consent`) map to [`EmbedFailure::Other`] until T030/T032 grow their
/// dedicated classes.
impl From<&crate::embed::EmbedError> for EmbedFailure {
    fn from(e: &crate::embed::EmbedError) -> Self {
        use crate::embed::EmbedError;
        match e {
            EmbedError::MissingArtifacts(_) => Self::ModelFilesMissing,
            EmbedError::CorruptArtifacts(_) => Self::ModelFilesCorrupt,
            EmbedError::DylibLoad(_) => Self::DylibLoad,
            EmbedError::SessionLoad(_) => Self::SessionLoad,
            EmbedError::Inference(_) => Self::Inference,
            EmbedError::EmptyResult(_) => Self::EmptyResult,
            EmbedError::DimensionMismatch(_)
            | EmbedError::Unreachable(_)
            | EmbedError::AuthRejected(_)
            | EmbedError::RateLimited(_)
            | EmbedError::MalformedResponse(_)
            | EmbedError::Consent(_)
            | EmbedError::RemoteBackendsLandLater(_)
            | EmbedError::UnknownProfile(_)
            | EmbedError::Other(_) => Self::Other,
        }
    }
}

/// Normalize a query/symbol for the exact-symbol comparison (FR-002:
/// trimmed + casefolded — `SearchStore`, ` searchstore `, and `SEARCHSTORE`
/// all compare equal; implemented as Unicode `to_lowercase`, the standard
/// library's case-fold mapping).
fn normalize_symbol_cmp(s: &str) -> String {
    s.trim().to_lowercase()
}

/// The exact-symbol-first pin (FR-002 / SC-002; contracts/hybrid-search.md
/// § Exact-symbol guarantee): chunks whose normalized `symbol_name` equals
/// the normalized query move to the FRONT of the fused list, ahead of all
/// fused-only competitors, in `chunk_id` lexical order (deterministic).
/// Non-matching results keep their fused order behind them. Returns the
/// reordered list plus the pinned winner's `chunk_id` (`None` when no
/// result's symbol matches — the common case, list unchanged).
fn pin_exact_symbol_first(
    query: &str,
    results: Vec<RankedResult>,
) -> (Vec<RankedResult>, Option<String>) {
    let q = normalize_symbol_cmp(query);
    let is_exact = |r: &RankedResult| {
        r.symbol
            .as_deref()
            .is_some_and(|s| normalize_symbol_cmp(s) == q)
    };
    if !results.iter().any(|r| is_exact(r)) {
        return (results, None);
    }
    let mut exact: Vec<RankedResult> = Vec::new();
    let mut rest: Vec<RankedResult> = Vec::new();
    for r in results {
        if is_exact(&r) {
            exact.push(r);
        } else {
            rest.push(r);
        }
    }
    // Deterministic order among exact matches: chunk_id lexical ascending.
    exact.sort_by(|a, b| a.chunk_id.cmp(&b.chunk_id));
    let pinned_id = exact.first().map(|r| r.chunk_id.clone());
    exact.extend(rest);
    (exact, pinned_id)
}

/// Resolve the exact-symbol chunk straight from `rag_chunks` (T018's
/// unconditional half): the legs USUALLY surface the exact chunk (a
/// single-token symbol query always substring-matches `symbol_name` in the
/// LIKE leg), but the contract's MUST holds even when leg limits cut it —
/// so the pin consults the table directly, within the active file scope.
///
/// Matching: byte-exact or ASCII-lower-equal via SQL, then verified with
/// the full casefold normalization in Rust (SQL `lower()` is ASCII-only);
/// queries containing non-ASCII (where casefolding diverges from
/// lowercasing, e.g. `ß`/`SS`) fall back to a full table sweep. Deterministic
/// winner: `chunk_id` lexical ascending.
fn find_exact_symbol_chunk(
    store: &GraphStore,
    query: &str,
    file_filter: Option<&str>,
) -> Option<ChunkRow> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return None;
    }
    let target = normalize_symbol_cmp(trimmed);
    let matches = |row: &ChunkRow| {
        row.symbol_name
            .as_deref()
            .is_some_and(|s| normalize_symbol_cmp(s) == target)
            && file_filter
                .map(|p| glob_match(p, &row.source_path))
                .unwrap_or(true)
    };
    let in_scope = |row: &ChunkRow| {
        file_filter
            .map(|p| glob_match(p, &row.source_path))
            .unwrap_or(true)
    };

    // Fast path: byte-exact / ASCII-lower prefilter (covers the common
    // case entirely inside SQLite).
    let sql = format!(
        "SELECT {CHUNK_COLUMNS} FROM rag_chunks \
         WHERE symbol_name = ?1 OR lower(symbol_name) = lower(?1) \
         ORDER BY chunk_id"
    );
    if let Ok(mut stmt) = store.conn().prepare(&sql) {
        if let Ok(rows) = stmt.query_map(rusqlite::params![trimmed], row_to_chunk) {
            if let Some(row) = rows.flatten().find(|row| matches(row)) {
                return Some(row);
            }
        }
    }

    // Non-ASCII fallback: full sweep over symbol chunks (casefolding can
    // equate what SQL lower() cannot). Scoped by the glob, verified by the
    // same normalization.
    if trimmed.is_ascii() {
        return None;
    }
    let sql = format!(
        "SELECT {CHUNK_COLUMNS} FROM rag_chunks \
         WHERE symbol_name IS NOT NULL ORDER BY chunk_id"
    );
    let mut stmt = store.conn().prepare(&sql).ok()?;
    let found = stmt
        .query_map([], row_to_chunk)
        .ok()?
        .flatten()
        .find(|row| in_scope(row) && matches(row));
    found
}

// ─── T019: leg candidate constructors (file-scope filter INSIDE, pre-fusion) ─

/// Keyword-leg candidates with the file-scope filter applied INSIDE the
/// leg — BEFORE fusion (T019, FR-003; contracts/hybrid-search.md stage 5:
/// "the glob filter is applied in BOTH legs BEFORE fusion, never
/// post-fusion"). Returns `(row, ascending 1-based ordinal)` pairs; a
/// filtered-out path NEVER appears here.
fn keyword_leg_candidates(
    store: &GraphStore,
    project_root: &Path,
    trimmed_query: &str,
    limit: usize,
    file_filter: Option<&str>,
    include_fallback_chunks: bool,
) -> Result<Vec<(ChunkRow, u32)>, SearchError> {
    let mut leg: Vec<(ChunkRow, u32)> = Vec::new();
    let fts_hits = fts_symbol_hits(store, trimmed_query, limit.saturating_mul(2).max(20));
    let bm25_values: Vec<f64> = fts_hits.iter().map(|(_, rank)| *rank).collect();
    let ordinals = bm25_ranks_to_ordinals(&bm25_values);
    for ((row, _bm25), ordinal) in fts_hits.into_iter().zip(ordinals) {
        if !leg.iter().any(|(r, _)| r.chunk_id == row.chunk_id) {
            leg.push((row, ordinal));
        }
    }
    let mut next_ordinal = leg.last().map_or(1, |(_, o)| o.saturating_add(1));
    let tokens: Vec<&str> = trimmed_query.split_whitespace().collect();
    for row in like_search_chunks(store, &tokens, limit.saturating_mul(2).max(20))? {
        if !leg.iter().any(|(r, _)| r.chunk_id == row.chunk_id) {
            leg.push((row, next_ordinal));
            next_ordinal = next_ordinal.saturating_add(1);
        }
    }
    // Content scan (the fallback-chunk coverage half, FR-014): fallback
    // chunks have no FTS row and no symbol — their ONLY keyword surface is
    // the chunk body, read from disk at query time.
    for row in content_search_chunks(store, project_root, &tokens) {
        if !leg.iter().any(|(r, _)| r.chunk_id == row.chunk_id) {
            leg.push((row, next_ordinal));
            next_ordinal = next_ordinal.saturating_add(1);
        }
    }
    // File-scope filter (pre-fusion, inside the leg). Filtered entries
    // drop out; survivors keep the ordinals earned in the FULL leg
    // (rank-based fusion stays monotone and deterministic either way).
    if let Some(pattern) = file_filter {
        leg.retain(|(r, _)| glob_match(pattern, &r.source_path));
    }
    if !include_fallback_chunks {
        leg.retain(|(r, _)| r.chunk_kind != "fallback");
    }
    Ok(leg)
}

/// Dense-leg candidates with the file-scope filter applied INSIDE the leg
/// — BEFORE fusion (T019). Dense ranks are positions in the cosine-ordered
/// candidate list (assigned pre-filter, like the keyword ordinals); a
/// filtered-out path NEVER appears here. Each survivor carries the rank it
/// earned in the FULL candidate list — filtering drops entries, it never
/// renumbers the survivors (the keyword leg's documented contract, ~line
/// 686). Embedder failure propagates as [`DenseLegError`] (the T020
/// degradation signal).
fn dense_leg_candidates<E: fmt::Display, F>(
    store: &GraphStore,
    profile: &EmbedProfile,
    raw_query: &str,
    limit: usize,
    file_filter: Option<&str>,
    include_fallback_chunks: bool,
    embed: F,
) -> Result<Vec<(ChunkRow, f32, u32)>, DenseLegError>
where
    F: FnOnce(&[String]) -> Result<Vec<Vec<f32>>, E>,
{
    let candidates = dense_leg_with_embedder(
        store.conn(),
        profile,
        raw_query,
        limit.saturating_mul(2).max(20),
        include_fallback_chunks,
        embed,
    )?;
    // Rank (1-based position in the cosine-ordered candidate list) is
    // assigned from the candidate position BEFORE the retain below — a
    // filtered-out rank-1 hit must leave the rank-2 survivor at 2, not
    // compact it to 1.
    let mut triples: Vec<(ChunkRow, f32, u32)> = Vec::with_capacity(candidates.len());
    for (position, c) in candidates.iter().enumerate() {
        match fetch_chunk_row(store, &c.chunk_id) {
            Ok(row) => triples.push((row, c.score, position as u32 + 1)),
            // The candidate's row vanished between the scan and this
            // fetch (a refresh committed a purge in between — WAL gives
            // per-statement snapshots, not cross-statement ones).
            // Dropping just this candidate is correct: it no longer
            // exists in committed truth.
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            // Real SQL failures (locked DB, IO) propagate through the
            // leg's existing error channel — the FR-008 machinery turns
            // them into keyword_only + mode_reason instead of the dense
            // leg silently shrinking.
            Err(e) => return Err(DenseLegError::Scan(ScanError::Sql(e))),
        }
    }
    if let Some(pattern) = file_filter {
        triples.retain(|(row, _, _)| glob_match(pattern, &row.source_path));
    }
    Ok(triples)
}

/// `/neurocode search` orchestration, keyword-only posture: the injected
/// embedder always fails with [`NO_EMBEDDER_REASON`], exercising the exact
/// FR-008 degradation machinery the CLI uses (mode/mode_reason appear in
/// the payload). Full hybrid fusion lands in T016–T020.
pub fn search_cli(
    store: &GraphStore,
    project_root: &Path,
    profile: &EmbedProfile,
    req: &SearchRequest,
) -> Result<SearchOutcome, SearchError> {
    search_cli_with_embedder(
        store,
        project_root,
        profile,
        req,
        |_texts: &[String]| Err::<Vec<Vec<f32>>, &str>(NO_EMBEDDER_REASON),
    )
}

/// Full orchestration with an injected embedder (the future
/// `EmbeddingBackend::embed` closure, T010). Pipeline order per
/// contracts/hybrid-search.md: validate → keyword leg → dense leg (any
/// embedder failure ⇒ KeywordOnly + `mode_reason`, never an error —
/// FR-008) → merge (dense order first; RRF lands T017) → file-scope
/// filter (both legs, pre-merge) → limit → context expansion.
pub fn search_cli_with_embedder<E, F>(
    store: &GraphStore,
    project_root: &Path,
    profile: &EmbedProfile,
    req: &SearchRequest,
    embed: F,
) -> Result<SearchOutcome, SearchError>
where
    E: fmt::Display,
    F: FnOnce(&[String]) -> Result<Vec<Vec<f32>>, E>,
{
    // Stage 1 — validate (edge case 1: whitespace-only is NOT a search).
    let trimmed = req.query.trim();
    if trimmed.is_empty() {
        return Err(SearchError::Validation(
            "query is empty or whitespace-only — provide at least one search \
             term"
                .to_string(),
        ));
    }
    let limit = req.limit.max(1);

    let index_chunk_count =
        crate::vector::store::chunk_count(store.conn()).unwrap_or(0);

    // Stage 2 — keyword leg (T016 ordinals + T019 pre-fusion file-scope
    // filter, applied INSIDE the leg).
    let keyword_leg = keyword_leg_candidates(
        store,
        project_root,
        trimmed,
        limit,
        req.file_filter.as_deref(),
        req.include_fallback_chunks,
    )?;

    // Stage 3 — dense leg (T019 pre-fusion file-scope filter inside) with
    // degradation (FR-008, T020): ANY dense-leg failure class — embedder
    // error, empty batch result, scan failure — classifies into the
    // [`EmbedFailure`] taxonomy and degrades to KeywordOnly with a
    // structured `mode_reason` + per-result `degradation_note`; the turn
    // NEVER hard-fails on a backend problem.
    let dense_attempt = dense_leg_candidates(
        store,
        profile,
        &req.query,
        limit,
        req.file_filter.as_deref(),
        req.include_fallback_chunks,
        embed,
    );
    let (mode, mode_reason, dense_pairs) = match dense_attempt {
        Ok(pairs) => (SearchMode::Hybrid, None, pairs),
        Err(e) => {
            let failure = EmbedFailure::from(&e);
            let reason = failure.mode_reason();
            (
                SearchMode::KeywordOnly,
                Some(format!("{} [{}]", reason, e)),
                Vec::new(),
            )
        }
    };

    // Stage 4 — RRF fusion k=60 (T017): union both legs by chunk_id, fuse
    // `score(d) = Σ 1/(60 + rank_leg(d))`, order by score desc with the
    // deterministic tie-break (keyword rank first, then chunk_id lexical
    // ascending — [`crate::search::rrf::rrf_fuse`]). Dense ranks are the
    // positions in the (pre-fusion-filtered) cosine-ordered candidate
    // list; keyword ranks are the T016 ordinals.
    let keyword_only = mode == SearchMode::KeywordOnly;
    let mut by_id: std::collections::HashMap<String, crate::search::rrf::RrfEntry> =
        std::collections::HashMap::new();
    for (row, keyword_rank) in keyword_leg.iter() {
        by_id
            .entry(row.chunk_id.clone())
            .or_insert_with(|| crate::search::rrf::RrfEntry {
                chunk_id: row.chunk_id.clone(),
                keyword_rank: None,
                dense_rank: None,
            })
            .keyword_rank = Some(*keyword_rank);
    }
    for (row, _score, dense_rank) in dense_pairs.iter() {
        by_id
            .entry(row.chunk_id.clone())
            .or_insert_with(|| crate::search::rrf::RrfEntry {
                chunk_id: row.chunk_id.clone(),
                keyword_rank: None,
                dense_rank: None,
            })
            .dense_rank = Some(*dense_rank);
    }
    let rows_by_id: std::collections::HashMap<&String, &ChunkRow> = keyword_leg
        .iter()
        .map(|(row, _)| (&row.chunk_id, row))
        .chain(dense_pairs.iter().map(|(row, _, _)| (&row.chunk_id, row)))
        .collect();
    let fused = crate::search::rrf::rrf_fuse(by_id.into_values().collect());
    let mut merged: Vec<RankedResult> = fused
        .into_iter()
        .filter_map(|s| {
            let row = *rows_by_id.get(&s.chunk_id)?;
            Some(row.to_result(
                s.dense_rank,
                s.keyword_rank,
                s.score,
                keyword_only.then(|| mode_reason.clone().unwrap_or_default()),
                req.relation_depth,
            ))
        })
        .collect();

    // Stage 4.5 — exact-symbol-first pin (T018, FR-002/SC-002): the
    // normalized query exactly equal to a `symbol_name` in scope ranks
    // that chunk FIRST, deterministically ahead of fused-only
    // competitors. The pin also rescues the exact chunk when BOTH legs
    // missed it (e.g. tight leg limits) by consulting `rag_chunks`
    // directly — the guarantee is unconditional within the file scope.
    if let Some(row) = find_exact_symbol_chunk(store, &req.query, req.file_filter.as_deref()) {
        if !merged.iter().any(|r| r.chunk_id == row.chunk_id) {
            // The legs never surfaced it; prepend with no leg ranks (a
            // rescued exact match outranks everything by contract, not by
            // leg evidence). Keep the FR-014 badge + degradation note.
            merged.insert(
                0,
                row.to_result(
                    None,
                    None,
                    0.0,
                    keyword_only.then(|| mode_reason.clone().unwrap_or_default()),
                    req.relation_depth,
                ),
            );
        }
    }
    let (mut merged, _pinned) = pin_exact_symbol_first(&req.query, merged);

    // Stages 5–8 — total (pre-limit), truncate, context expansion,
    // relation expansion.
    let total_candidates = merged.len();
    merged.truncate(limit);
    // T027: clamp the window at consumption too — `SearchRequest` is a
    // public type, so the pipeline never trusts upstream surfaces
    // (config default 20; request override wins; always 0–200 —
    // `RagConfig::effective_context_window` is the shared rule).
    let expand_lines = req
        .expand_lines
        .min(crate::config::CONTEXT_WINDOW_LINES_MAX as u32);
    for result in &mut merged {
        result.context = expand::expand_context(
            project_root,
            &result.file,
            result.start_line,
            result.end_line,
            expand_lines,
        );
    }

    // Stage 8 — relation expansion (T029, FR-007): bounded BFS over
    // `rag_chunk_edges` per fused result. Expanded items are marked with
    // their `relation_kind` and appended per-result WITHOUT displacing
    // fused results — the fused chunk ids seed the BFS visited set, so
    // relations are strictly additive (an already-ranked chunk's
    // neighborhood surfaces via its OWN result's expansion). Depth is
    // clamped to the `relation_max_depth` cap inside `expand_relations`.
    if req.relation_depth > 0 {
        let fused_ids: std::collections::HashSet<String> =
            merged.iter().map(|r| r.chunk_id.clone()).collect();
        let budget_per_result = limit;
        for result in &mut merged {
            let mut relations = expand::expand_relations(
                store.conn(),
                &result.chunk_id,
                req.relation_depth,
                &fused_ids,
            );
            relations.truncate(budget_per_result);
            result.relations = relations
                .into_iter()
                .map(|rel| RelatedEntity {
                    file: rel.file,
                    symbol: rel.symbol,
                    relation_kind: rel.relation_kind,
                })
                .collect();
        }
    }

    Ok(SearchOutcome {
        results: merged,
        mode,
        mode_reason,
        total_candidates,
        index_chunk_count,
    })
}

// ─── T016: keyword-leg bm25 → ascending-ordinal conversion ──────────────────

/// Convert NEGATED FTS5 bm25 ranks to ascending 1-based ordinals (T016,
/// contracts/hybrid-search.md stage 2; research.md R1 gotcha).
///
/// `query_fts` orders by the FTS5 `rank` column, which for bm25 is the
/// **negated** relevance (lower = better; `ORDER BY rank ASC` returns
/// best-first). Fusion needs ascending 1-based ordinals where **1 = best**,
/// so the conversion sorts by bm25 value ascending: the most negative
/// (highest-relevance) row gets ordinal 1. Naively reading the sign the
/// other way — treating `-0.4` as the best of `{-3.2, -1.1, -0.4}` — would
/// invert the leg and is exactly what this function's unit test pins
/// against. Ties keep stable input order.
///
/// Returns ordinals positionally aligned with the input: `out[i]` is the
/// 1-based rank of `bm25_ranks[i]`.
pub fn bm25_ranks_to_ordinals(bm25_ranks: &[f64]) -> Vec<u32> {
    // Index order under ascending bm25 (lower = better), stable on ties.
    let mut order: Vec<usize> = (0..bm25_ranks.len()).collect();
    order.sort_by(|&a, &b| bm25_ranks[a].total_cmp(&bm25_ranks[b]));
    let mut ordinals = vec![0u32; bm25_ranks.len()];
    for (pos, idx) in order.into_iter().enumerate() {
        ordinals[idx] = (pos + 1) as u32;
    }
    ordinals
}

// ─── keyword-leg helpers ─────────────────────────────────────────────────────

/// One `rag_chunks` row in memory (the denormalized result source).
#[derive(Debug, Clone)]
struct ChunkRow {
    chunk_id: String,
    chunk_kind: String,
    source_path: String,
    start_line: u32,
    end_line: u32,
    symbol_name: Option<String>,
    symbol_kind: Option<String>,
}

impl ChunkRow {
    /// Map to a [`RankedResult`] (badge from `chunk_kind`, FR-014).
    fn to_result(
        &self,
        semantic_rank: Option<u32>,
        keyword_rank: Option<u32>,
        fused_score: f64,
        degradation_note: Option<String>,
        relation_depth: u8,
    ) -> RankedResult {
        RankedResult {
            chunk_id: self.chunk_id.clone(),
            file: self.source_path.clone(),
            symbol: self.symbol_name.clone(),
            symbol_kind: self.symbol_kind.clone(),
            start_line: self.start_line,
            end_line: self.end_line,
            chunk_kind: if self.chunk_kind == "fallback" {
                ChunkBadge::Fallback
            } else {
                ChunkBadge::SymbolAligned
            },
            fused_score,
            semantic_rank,
            keyword_rank,
            degradation_note,
            context: None,
            relations: if relation_depth > 0 {
                Vec::new() // T029 lands bounded BFS expansion.
            } else {
                Vec::new()
            },
        }
    }
}

const CHUNK_COLUMNS: &str =
    "chunk_id, chunk_kind, source_path, start_line, end_line, symbol_name, \
     symbol_kind";

/// Fetch one `rag_chunks` row by id. Fallible (not `.ok()`) so transient
/// SQL failures (locked DB, IO) propagate — callers distinguish "row
/// genuinely gone" (`QueryReturnedNoRows`) from real errors; silently
/// mapping everything to None dropped dense candidates on transient
/// failures.
fn fetch_chunk_row(store: &GraphStore, chunk_id: &str) -> rusqlite::Result<ChunkRow> {
    let sql = format!(
        "SELECT {CHUNK_COLUMNS} FROM rag_chunks WHERE chunk_id = ?1"
    );
    store
        .conn()
        .query_row(&sql, rusqlite::params![chunk_id], row_to_chunk)
}

fn row_to_chunk(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChunkRow> {
    Ok(ChunkRow {
        chunk_id: row.get(0)?,
        chunk_kind: row.get(1)?,
        source_path: row.get(2)?,
        start_line: row.get::<_, i64>(3)? as u32,
        end_line: row.get::<_, i64>(4)? as u32,
        symbol_name: row.get(5)?,
        symbol_kind: row.get(6)?,
    })
}

/// Map an FTS symbol hit to its symbol-aligned chunk row (same file, same
/// simple name). `None` when the artifact has no indexed chunk yet.
/// `ORDER BY chunk_id` keeps the LIMIT 1 deterministic — without it SQLite
/// picks an unspecified row when several chunks share the symbol (e.g. a
/// symbol split into multiple line-range pieces), making leg composition
/// nondeterministic across runs.
fn find_symbol_chunk(store: &GraphStore, node: &CodeArtifactNode) -> Option<ChunkRow> {
    let sql = format!(
        "SELECT {CHUNK_COLUMNS} FROM rag_chunks \
         WHERE source_path = ?1 AND symbol_name = ?2 ORDER BY chunk_id LIMIT 1"
    );
    store
        .conn()
        .query_row(
            &sql,
            rusqlite::params![node.source_path, node.simple_name()],
            row_to_chunk,
        )
        .ok()
}

/// FTS5 symbol hits mapped to their chunk rows, each paired with its RAW
/// bm25 `rank` value (T016).
///
/// This runs the same external-content FTS5 join `GraphStore::query_fts`
/// runs (token quoting mirrored), but SELECTs the `rank` column as well:
/// the store API drops the rank value and preserves only the order it
/// induces, while the keyword leg needs the real NEGATED bm25 numbers for
/// the ascending-ordinal conversion (contracts/hybrid-search.md stage 2).
/// Unavailable FTS yields an empty leg (the pre-T016 graceful posture).
fn fts_symbol_hits(store: &GraphStore, query: &str, limit: usize) -> Vec<(ChunkRow, f64)> {
    let fts_query: String = query
        .split_whitespace()
        .map(|w| format!("\"{}\"", w.replace('"', "")))
        .collect::<Vec<_>>()
        .join(" ");
    if fts_query.is_empty() {
        return Vec::new();
    }
    let sql = format!(
        "SELECT fts.rank, ca.kind, ca.fqcn, ca.source_path \
         FROM code_artifacts_fts fts JOIN code_artifacts ca ON ca.id = fts.rowid \
         WHERE code_artifacts_fts MATCH ?1 \
         ORDER BY rank LIMIT {limit}"
    );
    let Ok(mut stmt) = store.conn().prepare(&sql) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(rusqlite::params![&fts_query], |row| {
        Ok((
            row.get::<_, f64>(0)?,      // bm25 rank (NEGATED: lower = better)
            row.get::<_, String>(1)?,   // kind
            row.get::<_, String>(2)?,   // fqcn
            row.get::<_, String>(3)?,   // source_path
        ))
    }) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (rank, kind, fqcn, source_path) in rows.flatten() {
        let Ok(kind) = ArtifactKind::parse(&kind) else {
            continue;
        };
        // Reuse the node's own simple-name derivation (no local copy).
        let node = CodeArtifactNode::new(kind, fqcn, String::new(), source_path);
        if let Some(row) = find_symbol_chunk(store, &node) {
            out.push((row, rank));
        }
    }
    out
}

/// Escape SQL LIKE wildcards (`%`, `_`, `\`) so query tokens match
/// literally (`ESCAPE '\'` in the SQL).
fn like_escape(token: &str) -> String {
    token
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Chunk-table keyword scan: every token must appear (case-insensitively,
/// substring) in `symbol_name` OR `source_path` — this is the leg that
/// reaches fallback chunks (no FTS row exists for them). Deterministic
/// order: `chunk_id` ascending.
fn like_search_chunks(
    store: &GraphStore,
    tokens: &[&str],
    limit: usize,
) -> Result<Vec<ChunkRow>, SearchError> {
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    let mut conditions: Vec<String> = Vec::new();
    let mut params: Vec<String> = Vec::new();
    for tok in tokens {
        let pat = format!("%{}%", like_escape(tok));
        conditions.push(
            "(symbol_name LIKE ? ESCAPE '\\' OR source_path LIKE ? ESCAPE '\\')"
                .to_string(),
        );
        params.push(pat.clone());
        params.push(pat);
    }
    let sql = format!(
        "SELECT {CHUNK_COLUMNS} FROM rag_chunks WHERE {} ORDER BY chunk_id \
         LIMIT {}",
        conditions.join(" AND "),
        limit
    );
    let mut stmt = store
        .conn()
        .prepare(&sql)
        .map_err(|e| SearchError::Store(e.to_string()))?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), row_to_chunk)
        .map_err(|e| SearchError::Store(e.to_string()))?;
    Ok(rows.filter_map(Result::ok).collect())
}

/// Minimal glob for `file_filter` (`*` = any run incl. `/`, `?` = one
/// char; case-sensitive path match). Kept local until glob matching needs
/// a shared home (T019 formalizes the filter contract).
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Content keyword scan over chunk BODIES read from disk at query time.
///
/// `rag_chunks` stores no body text (only the hash), and fallback chunks
/// have neither an FTS row nor a symbol — reading the indexed line range
/// from the file under `project_root` is the only keyword surface they
/// have (FR-014 coverage). A chunk whose file is missing or whose line
/// range falls outside the file simply doesn't match (never an error).
/// Deterministic order: `chunk_id` ascending.
///
/// The scan is restricted at the SQL level to the rows this leg exists
/// for: fallback chunks (`chunk_kind = 'fallback'`, i.e. no symbol and no
/// FTS coverage — the `code_artifacts_fts` surface only indexes symbol
/// artifacts). Symbol chunks are reachable via the FTS and LIKE legs;
/// rescanning their bodies here materialized the entire `rag_chunks`
/// table and read every indexed file per query for no additional hits.
fn content_search_chunks(
    store: &GraphStore,
    project_root: &Path,
    tokens: &[&str],
) -> Vec<ChunkRow> {
    if tokens.is_empty() {
        return Vec::new();
    }
    // Group chunk rows by path so each file is read at most once.
    let mut rows: Vec<ChunkRow> = match store.conn().prepare(&format!(
        "SELECT {CHUNK_COLUMNS} FROM rag_chunks \
         WHERE chunk_kind = 'fallback' ORDER BY chunk_id"
    )) {
        Ok(mut stmt) => match stmt.query_map([], row_to_chunk) {
            Ok(iter) => iter.filter_map(Result::ok).collect(),
            Err(_) => return Vec::new(),
        },
        Err(_) => return Vec::new(),
    };
    let mut by_path: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for (idx, row) in rows.iter().enumerate() {
        by_path.entry(row.source_path.clone()).or_default().push(idx);
    }
    let mut keep: Vec<bool> = vec![false; rows.len()];
    for (path, idxs) in by_path {
        let Ok(text) = std::fs::read_to_string(project_root.join(&path)) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        for idx in idxs {
            let row = &rows[idx];
            let start = (row.start_line as usize).saturating_sub(1).min(lines.len());
            let end = (row.end_line as usize).min(lines.len());
            if start >= end {
                continue;
            }
            let body = lines[start..end].join("\n").to_lowercase();
            if tokens
                .iter()
                .all(|t| body.contains(&t.to_lowercase()))
            {
                keep[idx] = true;
            }
        }
    }
    let mut out: Vec<ChunkRow> = Vec::new();
    for (idx, k) in keep.into_iter().enumerate() {
        if k {
            out.push(rows[idx].clone());
        }
    }
    rows.clear();
    out
}

/// Stage-7 context expansion lives in [`crate::search::expand::
/// expand_context`] (T026 moved the T015 inline form there unchanged —
/// same clamping, same `context_absent` posture). Stage-8 relation
/// expansion is [`crate::search::expand::expand_relations`] (T029).

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::profiles::{CODERANK_EMBED, NOMIC_EMBED_TEXT_V1_5};
    use crate::vector::quantize::Quantization;

    // Same contract DDL as scan.rs tests (rag-store-schema.md).
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

    fn insert_f32_vector(conn: &Connection, id: &str, v: &[f32]) {
        let blob: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
        conn.execute(
            "INSERT INTO rag_vectors (chunk_id, dim, quantization, vector) VALUES (?1, ?2, 'f32', ?3)",
            rusqlite::params![id, v.len() as i64, blob],
        )
        .unwrap();
    }

    // ── Query-prefix application (wrapper's constructed input) ──────────

    #[test]
    fn wrapper_prepends_nomic_query_prefix_pre_tokenization() {
        let seen = std::sync::Mutex::new(Vec::<String>::new());
        let conn = test_db();
        insert_chunk(&conn, "a", "symbol");
        insert_f32_vector(&conn, "a", &[1.0, 0.0]);
        let ranked = dense_leg_with_embedder(
            &conn,
            &NOMIC_EMBED_TEXT_V1_5,
            "where is token validation handled",
            1,
            true,
            |texts: &[String]| {
                *seen.lock().unwrap() = texts.to_vec();
                Ok::<Vec<Vec<f32>>, std::convert::Infallible>(vec![vec![1.0, 0.0]])
            },
        )
        .unwrap();
        // The embedder consumed EXACTLY prefix ++ raw — verbatim, no trim.
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["search_query: where is token validation handled".to_string()].as_slice()
        );
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].chunk_id, "a");
    }

    #[test]
    fn wrapper_prepends_coderank_query_prefix_and_never_document_prefix() {
        let seen = std::sync::Mutex::new(Vec::new());
        let conn = test_db();
        insert_chunk(&conn, "a", "symbol");
        insert_f32_vector(&conn, "a", &[0.0, 1.0]);
        dense_leg_with_embedder(
            &conn,
            &CODERANK_EMBED,
            "parse entry point",
            1,
            true,
            |texts: &[String]| {
                *seen.lock().unwrap() = texts.to_vec();
                Ok::<Vec<Vec<f32>>, std::convert::Infallible>(vec![vec![0.0, 1.0]])
            },
        )
        .unwrap();
        let got = seen.lock().unwrap();
        assert_eq!(
            got.as_slice(),
            ["Represent this query for searching relevant code: parse entry point".to_string()]
                .as_slice()
        );
        // Document prefix must NOT leak into queries (nomic's would be
        // "search_document: " — check via the same wrapper on nomic).
        drop(got);
        let seen2 = std::sync::Mutex::new(Vec::new());
        dense_leg_with_embedder(
            &conn,
            &NOMIC_EMBED_TEXT_V1_5,
            "q",
            1,
            true,
            |texts: &[String]| {
                *seen2.lock().unwrap() = texts.to_vec();
                Ok::<Vec<Vec<f32>>, std::convert::Infallible>(vec![vec![0.0, 1.0]])
            },
        )
        .unwrap();
        let got2 = seen2.lock().unwrap();
        assert_eq!(got2.as_slice(), ["search_query: q".to_string()].as_slice());
        assert!(!got2[0].contains("search_document"));
    }

    #[test]
    fn wrapper_passes_raw_query_verbatim_into_the_prefix() {
        // No trimming/case-folding of the raw text (profiles rule 1).
        let seen = std::sync::Mutex::new(Vec::new());
        let conn = test_db();
        insert_chunk(&conn, "a", "symbol");
        insert_f32_vector(&conn, "a", &[1.0, 0.0]);
        dense_leg_with_embedder(&conn, &NOMIC_EMBED_TEXT_V1_5, "  Mixed CASE  ", 1, true, |texts: &[String]| {
            *seen.lock().unwrap() = texts.to_vec();
            Ok::<Vec<Vec<f32>>, std::convert::Infallible>(vec![vec![1.0, 0.0]])
        })
        .unwrap();
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["search_query:   Mixed CASE  ".to_string()].as_slice()
        );
    }

    // ── Embedder failure + batch-shape errors (degradation signals) ─────

    #[test]
    fn wrapper_maps_embedder_failure_to_dense_leg_error_embed() {
        let conn = test_db();
        let err = dense_leg_with_embedder(
            &conn,
            &NOMIC_EMBED_TEXT_V1_5,
            "q",
            1,
            true,
            |_texts: &[String]| Err::<Vec<Vec<f32>>, _>("backend offline"),
        )
        .unwrap_err();
        match &err {
            DenseLegError::Embed(reason) => assert_eq!(reason, "backend offline"),
            other => panic!("expected Embed, got {:?}", other),
        }
        assert_eq!(err.to_string(), "dense leg embedder failure: backend offline");
    }

    #[test]
    fn wrapper_rejects_empty_batch_result() {
        let conn = test_db();
        let err = dense_leg_with_embedder(
            &conn,
            &NOMIC_EMBED_TEXT_V1_5,
            "q",
            1,
            true,
            |_texts: &[String]| Ok::<_, std::convert::Infallible>(vec![]),
        )
        .unwrap_err();
        assert!(matches!(err, DenseLegError::EmptyEmbeddingResult));
    }

    #[test]
    fn wrapper_propagates_scan_errors() {
        let conn = test_db();
        insert_chunk(&conn, "bad", "symbol");
        // dim 4 declared, 2-element blob -> corrupt; embedder succeeds.
        conn.execute(
            "INSERT INTO rag_vectors (chunk_id, dim, quantization, vector) VALUES ('bad', 4, 'f32', X'0000')",
            [],
        )
        .unwrap();
        let err = dense_leg_with_embedder(
            &conn,
            &NOMIC_EMBED_TEXT_V1_5,
            "q",
            1,
            true,
            |_texts: &[String]| Ok::<_, std::convert::Infallible>(vec![vec![1.0, 0.0, 0.0, 0.0]]),
        )
        .unwrap_err();
        assert!(matches!(err, DenseLegError::Scan(ScanError::CorruptBlob { .. })));
    }

    // ── dense_leg primitive passes through to the scan ───────────────────

    #[test]
    fn dense_leg_primitive_matches_dense_scan() {
        let conn = test_db();
        insert_chunk(&conn, "a", "symbol");
        insert_f32_vector(&conn, "a", &[0.6, 0.8]);
        insert_chunk(&conn, "b", "symbol");
        insert_f32_vector(&conn, "b", &[1.0, 0.0]);
        let via_primitive = dense_leg(&conn, &[1.0, 0.0], 2, true).unwrap();
        assert_eq!(via_primitive.len(), 2);
        assert_eq!(via_primitive[0].chunk_id, "b");
        assert!((via_primitive[0].score - 1.0).abs() < 1e-6);
    }

    // ─── T016: bm25 → ascending-ordinal conversion ──────────────────────

    /// The R1 gotcha pin: NEGATED bm25 (lower = better) converts so the
    /// MOST-negative value is ordinal 1 (T016's task-named example:
    /// -3.2, -1.1, -0.4 → 1, 2, 3).
    #[test]
    fn t016_bm25_sign_conversion_negated_values_become_ascending_ordinals() {
        let ordinals = bm25_ranks_to_ordinals(&[-3.2, -1.1, -0.4]);
        assert_eq!(ordinals, vec![1, 2, 3]);
    }

    /// Ordinals are positional (out[i] = rank of input i), proven by
    /// shuffling the same three values.
    #[test]
    fn t016_ordinals_are_positional_under_input_permutation() {
        let ordinals = bm25_ranks_to_ordinals(&[-0.4, -3.2, -1.1]);
        assert_eq!(ordinals, vec![3, 1, 2]);
    }

    /// Reading the sign backwards (treating bm25 as higher=better, i.e.
    /// -0.4 first) is precisely the inversion this conversion must NOT do.
    #[test]
    fn t016_wrong_sign_reading_is_the_inverted_ordering() {
        let wrong: Vec<u32> = {
            // Sort descending (as if higher = better) and assign 1-based.
            let vals = [-3.2f64, -1.1, -0.4];
            let mut order: Vec<usize> = (0..vals.len()).collect();
            order.sort_by(|&a, &b| vals[b].total_cmp(&vals[a]));
            let mut out = vec![0u32; vals.len()];
            for (pos, idx) in order.into_iter().enumerate() {
                out[idx] = (pos + 1) as u32;
            }
            out
        };
        assert_eq!(wrong, vec![3, 2, 1], "sanity: the wrong reading inverts");
        assert_ne!(
            bm25_ranks_to_ordinals(&[-3.2, -1.1, -0.4]),
            wrong,
            "the conversion must not be the wrong-sign ordering"
        );
    }

    /// Ties keep stable input order; a single value is ordinal 1; empty
    /// input yields empty output.
    #[test]
    fn t016_ties_stable_and_edges() {
        assert_eq!(bm25_ranks_to_ordinals(&[-1.5, -1.5, -2.0]), vec![2, 3, 1]);
        assert_eq!(bm25_ranks_to_ordinals(&[-7.0]), vec![1]);
        assert!(bm25_ranks_to_ordinals(&[]).is_empty());
        // Every permutation of the input is a valid 1..=n ordinal assignment.
        let ordinals = bm25_ranks_to_ordinals(&[-0.4, -1.1, -3.2, -0.4]);
        assert_eq!(ordinals, vec![3, 2, 1, 4]);
    }

    /// End-to-end through the real FTS5 engine: rows deliberately indexed
    /// so the bm25 order differs from row insertion order — the resulting
    /// `keyword_rank`s in the outcome MUST be the ascending ordinals of the
    /// bm25 order (the most-negative rank first), never inverted.
    #[test]
    fn t016_keyword_leg_ranks_follow_bm25_order_not_insertion_order() {
        use joey_neurocode::graph::{ArtifactKind, CodeArtifactNode, GraphStore};

        let tmp = tempfile::tempdir().unwrap();
        let store = GraphStore::open(&tmp.path().join("graph.db")).unwrap();

        // Three artifacts whose FTS relevance for "zebra" is engineered to
        // differ: fqcn "zzz" matches the token best (only fqcn text), then
        // "aaa.zzz.yyy", then one wrapped in extra filler tokens. The point
        // is not the exact bm25 numbers but that a REAL rank order exists
        // and the leg's ordinals follow it ascending.
        let mut expected_order: Vec<String> = Vec::new();
        for (fqcn, path) in [
            ("AlphaZebra", "src/one.rs"),
            ("BetaZebra", "src/two.rs"),
            ("GammaZebra", "src/three.rs"),
        ] {
            let node = CodeArtifactNode::new(
                ArtifactKind::Class,
                fqcn.to_string(),
                String::new(),
                path.to_string(),
            );
            let id = store.upsert_node(&node).unwrap();
            expected_order.push(fqcn.to_string());
            // Chunk row with the same symbol so the FTS hit maps to it.
            store.conn().execute(
                "INSERT INTO rag_chunks (chunk_id, chunk_kind, source_path, start_line, \
                 end_line, symbol_name, symbol_kind, content_hash) \
                 VALUES (?1, 'symbol', ?2, 1, 5, ?3, 'class', 'h')",
                rusqlite::params![format!("{path}:1-5:symbol:{fqcn}"), path, fqcn],
            ).unwrap();
            let _ = id;
        }

        let request = SearchRequest {
            query: "zebra".to_string(),
            file_filter: None,
            limit: 10,
            expand_lines: 0,
            relation_depth: 0,
            include_fallback_chunks: true,
        };
        let out = search_cli(&store, tmp.path(), &NOMIC_EMBED_TEXT_V1_5, &request).unwrap();

        // Keyword-only (no embedder): results carry keyword_rank = the
        // ascending ordinals of bm25 order, i.e. 1, 2, 3 ... in result
        // order, monotone increasing, each exactly once.
        assert_eq!(out.mode, SearchMode::KeywordOnly);
        let ranks: Vec<u32> = out
            .results
            .iter()
            .filter_map(|r| r.keyword_rank)
            .collect();
        assert_eq!(ranks, vec![1, 2, 3], "ordinals ascend in result order");
        let syms: Vec<&str> = out
            .results
            .iter()
            .filter_map(|r| r.symbol.as_deref())
            .collect();
        // All three symbols found; order is SOME permutation of the set —
        // pinned further by T018's exact-symbol guarantee.
        assert_eq!(syms.len(), 3);
        for fq in &expected_order {
            assert!(syms.contains(&fq.as_str()), "missing {fq} in {syms:?}");
        }
    }

    // ─── T018: exact-symbol-first guarantee (FR-002 / SC-002) ───────────

    mod exact_symbol_tests {
        use super::super::*;
        use crate::embed::profiles::NOMIC_EMBED_TEXT_V1_5;
        use joey_neurocode::graph::GraphStore;

        fn temp_store() -> (tempfile::TempDir, GraphStore) {
            let tmp = tempfile::tempdir().unwrap();
            let store = GraphStore::open(&tmp.path().join("graph.db")).unwrap();
            (tmp, store)
        }

        /// Insert a symbol chunk directly (the pipeline's own write path is
        /// exercised elsewhere; here we control bm25/dense evidence
        /// precisely).
        fn insert_symbol_chunk(
            store: &GraphStore,
            path: &str,
            symbol: &str,
            start: u32,
            end: u32,
        ) -> String {
            let chunk_id = format!("{path}:{start}-{end}:symbol:{symbol}");
            store
                .conn()
                .execute(
                    "INSERT INTO rag_chunks (chunk_id, chunk_kind, source_path, start_line, \
                     end_line, symbol_name, symbol_kind, content_hash) \
                     VALUES (?1, 'symbol', ?2, ?3, ?4, ?5, 'class', 'h')",
                    rusqlite::params![chunk_id, path, start as i64, end as i64, symbol],
                )
                .unwrap();
            chunk_id
        }

        fn req(query: &str) -> SearchRequest {
            SearchRequest {
                query: query.to_string(),
                file_filter: None,
                limit: 10,
                expand_lines: 0,
                relation_depth: 0,
                include_fallback_chunks: true,
            }
        }

        /// The pin: normalized exact `symbol_name` match ranks first,
        /// ahead of fused-only competitors with better RRF scores.
        #[test]
        fn t018_exact_match_ranks_first_ahead_of_fused_competitors() {
            let (tmp, store) = temp_store();
            // A decoy whose symbol CONTAINS the query (substring, not
            // exact) and one exact match. With no embedder, the keyword
            // leg's LIKE scan hits both; the exact symbol must lead.
            insert_symbol_chunk(&store, "src/a.rs", "SessionStore", 1, 30);
            insert_symbol_chunk(&store, "src/b.rs", "SessionStoreFactory", 1, 20);
            insert_symbol_chunk(&store, "src/c.rs", "CachedSessionStore", 1, 20);

            let out = search_cli(&store, tmp.path(), &NOMIC_EMBED_TEXT_V1_5, &req("SessionStore"))
                .unwrap();
            assert!(!out.results.is_empty());
            assert_eq!(
                out.results[0].symbol.as_deref(),
                Some("SessionStore"),
                "exact symbol first: {:?}",
                out.results.iter().map(|r| r.symbol.clone()).collect::<Vec<_>>()
            );
            assert_eq!(out.results[0].file, "src/a.rs");
        }

        /// Normalization: trimmed + casefolded query matches (and only
        /// the) exact symbol.
        #[test]
        fn t018_normalization_trim_and_casefold() {
            let (tmp, store) = temp_store();
            insert_symbol_chunk(&store, "src/a.rs", "Grüssen", 1, 10);
            insert_symbol_chunk(&store, "src/b.rs", "Grussen", 1, 10);

            // Trim + casefold: "  GRÜSSEN  " casefolds equal to "grüssen".
            let out = search_cli(
                &store,
                tmp.path(),
                &NOMIC_EMBED_TEXT_V1_5,
                &req("  GRÜSSEN  "),
            )
            .unwrap();
            assert_eq!(out.results[0].symbol.as_deref(), Some("Grüssen"));

            // ASCII case fold: "sessionstore" ≡ "SessionStore".
            insert_symbol_chunk(&store, "src/c.rs", "SessionStore", 1, 10);
            let out = search_cli(&store, tmp.path(), &NOMIC_EMBED_TEXT_V1_5, &req("sessionstore"))
                .unwrap();
            assert_eq!(
                out.results[0].symbol.as_deref(),
                Some("SessionStore"),
                "case-insensitive exact match leads"
            );
        }

        /// The rescue path: leg limits cut the exact chunk from BOTH legs
        /// (limit 1, decoy alphabetically first in LIKE order) — the pin
        /// still surfaces it first (the guarantee is unconditional).
        #[test]
        fn t018_rescue_when_both_legs_miss_the_exact_chunk() {
            let (tmp, store) = temp_store();
            insert_symbol_chunk(&store, "src/aaa.rs", "AaaZzz", 1, 10);
            let target = insert_symbol_chunk(&store, "src/zzz.rs", "ZzzTarget", 1, 10);

            let mut tight = req("ZzzTarget");
            tight.limit = 1;
            let out = search_cli(&store, tmp.path(), &NOMIC_EMBED_TEXT_V1_5, &tight).unwrap();
            assert_eq!(
                out.results[0].chunk_id, target,
                "rescued exact chunk leads despite leg limits: {:?}",
                out.results
            );
        }

        /// Deterministic winner among same-symbol chunks: smallest
        /// `chunk_id` lexical.
        #[test]
        fn t018_deterministic_tie_break_among_exact_matches() {
            let (tmp, store) = temp_store();
            insert_symbol_chunk(&store, "src/b.rs", "Dup", 1, 10);
            let a = insert_symbol_chunk(&store, "src/a.rs", "Dup", 5, 10);
            // b.rs chunk id "src/b.rs:..." vs a.rs — lexical min leads.
            let out = search_cli(&store, tmp.path(), &NOMIC_EMBED_TEXT_V1_5, &req("Dup")).unwrap();
            assert_eq!(out.results[0].chunk_id, a);
        }

        /// SC-002 multi-query benchmark: ≥ 20 exact-symbol queries, exact
        /// match ranking first ≥ EXACT_SYMBOL_FIRST_MIN_RATE (95%) of the
        /// time. On this deterministic corpus the pin yields 100% — the
        /// assertion uses the contract's encoded threshold.
        #[test]
        fn t018_multi_query_exact_symbol_benchmark_sc002() {
            let (tmp, store) = temp_store();
            // 24 symbols across files; decoys are substring-containing
            // symbols (the hardest fused-only competitors).
            let mut queries: Vec<String> = Vec::new();
            for i in 0..24 {
                let sym = format!("Symbol{i:02}");
                let path = format!("src/mod{}.rs", i % 4);
                insert_symbol_chunk(&store, &path, &sym, 1 + i, 10 + i);
                insert_symbol_chunk(&store, &path, &format!("{sym}Ext"), 1, 5);
                insert_symbol_chunk(&store, &path, &format!("Pre{sym}"), 1, 5);
                queries.push(sym);
            }
            assert!(queries.len() >= 20, "SC-002 needs ≥20 exact queries");

            let mut first_hits = 0usize;
            for q in &queries {
                let out = search_cli(&store, tmp.path(), &NOMIC_EMBED_TEXT_V1_5, &req(q)).unwrap();
                let led = out
                    .results
                    .first()
                    .is_some_and(|r| r.symbol.as_deref() == Some(q.as_str()));
                if led {
                    first_hits += 1;
                }
            }
            let rate = first_hits as f64 / queries.len() as f64;
            assert!(
                rate >= EXACT_SYMBOL_FIRST_MIN_RATE,
                "SC-002: exact-first rate {rate} < {} ({} of {} led)",
                EXACT_SYMBOL_FIRST_MIN_RATE,
                first_hits,
                queries.len()
            );
            // On this corpus the deterministic pin yields 100%.
            assert_eq!(first_hits, queries.len(), "deterministic pin: all lead");
        }

        /// The pin never fires on non-symbol queries (fallback-only
        /// results keep fused order; multi-token queries don't match a
        /// single symbol name).
        #[test]
        fn t018_no_pin_for_non_exact_queries() {
            let (tmp, store) = temp_store();
            insert_symbol_chunk(&store, "src/a.rs", "SessionStore", 1, 10);
            // Multi-token query normalizes to "session store" ≠ any symbol.
            let out = search_cli(
                &store,
                tmp.path(),
                &NOMIC_EMBED_TEXT_V1_5,
                &req("session store"),
            )
            .unwrap();
            // LIKE leg finds SessionStore (both tokens? "session" AND "store"...
            // "session store".split_whitespace → ["session","store"]; the LIKE
            // leg requires EVERY token to appear; "SessionStore" contains
            // "session" but not "store" as substring... case-insensitively
            // "sessionstore" contains both "session" and "store". ✓)
            assert!(out.results.iter().all(|r| r.keyword_rank.is_some() || r.semantic_rank.is_some()));
            // And exact-symbol queries against symbols absent from the
            // index never fabricate a result.
            let none = search_cli(&store, tmp.path(), &NOMIC_EMBED_TEXT_V1_5, &req("NoSuchSymbol"))
                .unwrap();
            assert!(none.results.is_empty());
        }
    }

    // ─── T019: file-scope filter in BOTH legs BEFORE fusion ─────────────

    mod file_scope_tests {
        use super::super::*;
        use crate::embed::profiles::NOMIC_EMBED_TEXT_V1_5;
        use crate::index::chunker::{build_chunk_records, ChunkOptions};
        use crate::vector::quantize::Quantization;
        use crate::vector::store::write_index;
        use joey_neurocode::graph::GraphStore;
        use joey_neurocode::parse::extract::SourceExtraction;

        fn temp_store() -> (tempfile::TempDir, GraphStore) {
            let tmp = tempfile::tempdir().unwrap();
            let store = GraphStore::open(&tmp.path().join("graph.db")).unwrap();
            (tmp, store)
        }

        /// Two files with a shared token; the glob keeps only `src/`.
        fn index_two_files(tmp: &tempfile::TempDir, store: &GraphStore) {
            for (path, body) in [
                ("src/a.py", "value_alpha = 1\n"),
                ("lib/b.py", "value_beta = 2\n"),
            ] {
                let full = tmp.path().join(path);
                std::fs::create_dir_all(full.parent().unwrap()).unwrap();
                std::fs::write(full, body).unwrap();
                let mut ex = SourceExtraction {
                    language: "python".to_string(),
                    ..Default::default()
                };
                ex.populate_fallback_chunks(body);
                let records =
                    build_chunk_records(&ex, body, path, store, &ChunkOptions::default());
                let dim = NOMIC_EMBED_TEXT_V1_5.dim as usize;
                // Give every chunk a real vector so the dense leg sees both.
                let vectors: Vec<Option<Vec<f32>>> = records
                    .iter()
                    .map(|r| {
                        // src/ chunk points at [1,0,...]; lib/ at [0,1,...].
                        let mut v = vec![0.0f32; dim];
                        let i = if r.source_path.starts_with("src/") { 0 } else { 1 };
                        v[i] = 1.0;
                        Some(v)
                    })
                    .collect();
                write_index(store, &NOMIC_EMBED_TEXT_V1_5, Quantization::F32, &records, &vectors, &[])
                    .unwrap();
            }
        }

        /// KEYWORD leg, candidate level: filtered-out paths appear in
        /// NEITHER leg's candidate list (T019, FR-003). Asserted directly
        /// against `keyword_leg_candidates`' output.
        #[test]
        fn t019_keyword_leg_candidates_exclude_filtered_paths() {
            let (tmp, store) = temp_store();
            index_two_files(&tmp, &store);
            let leg = keyword_leg_candidates(
                &store,
                tmp.path(),
                "value",
                10,
                Some("src/*"),
                true,
            )
            .unwrap();
            assert!(!leg.is_empty(), "in-scope hits present");
            assert!(
                leg.iter().all(|(r, _)| r.source_path.starts_with("src/")),
                "no lib/ path in the keyword candidate list: {:?}",
                leg.iter().map(|(r, _)| r.source_path.clone()).collect::<Vec<_>>()
            );
            // Unfiltered: both files present (the filter is what removes).
            let unfiltered = keyword_leg_candidates(&store, tmp.path(), "value", 10, None, true)
                .unwrap();
            assert!(unfiltered.len() >= 2);
        }

        /// DENSE leg, candidate level: same filter, same exclusion —
        /// asserted directly against `dense_leg_candidates`' output with a
        /// working embedder.
        #[test]
        fn t019_dense_leg_candidates_exclude_filtered_paths() {
            let (tmp, store) = temp_store();
            index_two_files(&tmp, &store);
            let dim = NOMIC_EMBED_TEXT_V1_5.dim as usize;
            let leg = dense_leg_candidates(
                &store,
                &NOMIC_EMBED_TEXT_V1_5,
                "value",
                10,
                Some("src/*"),
                true,
                |_texts: &[String]| Ok::<_, std::convert::Infallible>(vec![vec![1.0f32; dim]]),
            )
            .unwrap();
            assert!(!leg.is_empty(), "in-scope dense hits present");
            assert!(
                leg.iter().all(|(r, _, _)| r.source_path.starts_with("src/")),
                "no lib/ path in the dense candidate list: {:?}",
                leg.iter().map(|(r, _, _)| r.source_path.clone()).collect::<Vec<_>>()
            );
            // Unfiltered: both files reachable (query aligned with src/ but
            // exhaustive scan returns lib/ too).
            let unfiltered = dense_leg_candidates(
                &store,
                &NOMIC_EMBED_TEXT_V1_5,
                "value",
                10,
                None,
                true,
                |_texts: &[String]| Ok::<_, std::convert::Infallible>(vec![vec![1.0f32; dim]]),
            )
            .unwrap();
            assert!(unfiltered.len() >= 2);
        }

        /// The rank-carry pin: ranks are positions in the FULL cosine-
        /// ordered candidate list, assigned BEFORE the file filter —
        /// filtering out the rank-1 hit must leave the rank-2 survivor at
        /// dense rank 2, never compacted to 1 (the documented leg contract
        /// mirrored from the keyword leg).
        #[test]
        fn t019_dense_leg_filter_keeps_full_list_ranks() {
            let (tmp, store) = temp_store();
            index_two_files(&tmp, &store);
            let dim = NOMIC_EMBED_TEXT_V1_5.dim as usize;
            // Query aligned with the lib/ unit vector (index 1): lib/ is
            // the cosine rank-1 hit, src/ ranks 2 in the FULL list.
            let lib_query = || {
                let mut v = vec![0.0f32; dim];
                v[1] = 1.0;
                move |_texts: &[String]| Ok::<_, std::convert::Infallible>(vec![v.clone()])
            };

            // Fixture sanity: lib/ leads the FULL (unfiltered) list.
            let full = dense_leg_candidates(
                &store,
                &NOMIC_EMBED_TEXT_V1_5,
                "value",
                10,
                None,
                true,
                lib_query(),
            )
            .unwrap();
            assert!(
                full.first().is_some_and(|(r, _, _)| r.source_path.starts_with("lib/")),
                "fixture sanity: lib/ must lead the full list: {:?}",
                full.iter().map(|(r, _, rk)| (r.source_path.clone(), rk)).collect::<Vec<_>>()
            );

            // The glob `src/*` filters out the lib/ rank-1 hit; the src/
            // survivor keeps the rank it earned in the FULL list (2), not
            // the compacted position 1.
            let scoped = dense_leg_candidates(
                &store,
                &NOMIC_EMBED_TEXT_V1_5,
                "value",
                10,
                Some("src/*"),
                true,
                lib_query(),
            )
            .unwrap();
            let survivor_rank = scoped
                .iter()
                .find(|(r, _, _)| r.source_path.starts_with("src/"))
                .map(|(_, _, rank)| *rank);
            assert_eq!(
                survivor_rank,
                Some(2),
                "filtered dense survivors keep full-list ordinals (2), not compacted ranks: {:?}",
                scoped.iter().map(|(r, _, rk)| (r.source_path.clone(), rk)).collect::<Vec<_>>()
            );
        }

        /// End-to-end through the fused outcome with a working embedder:
        /// the glob scopes the final list AND the fused list never
        /// contains a filtered path at any rank (never post-fusion-only
        /// filtering).
        #[test]
        fn t019_fused_results_scoped_by_file_filter_end_to_end() {
            let (tmp, store) = temp_store();
            index_two_files(&tmp, &store);
            let dim = NOMIC_EMBED_TEXT_V1_5.dim as usize;
            let mut request = SearchRequest {
                query: "value".to_string(),
                file_filter: Some("src/*".to_string()),
                limit: 10,
                expand_lines: 0,
                relation_depth: 0,
                include_fallback_chunks: true,
            };
            let _ = &mut request;
            let out = search_cli_with_embedder(
                &store,
                tmp.path(),
                &NOMIC_EMBED_TEXT_V1_5,
                &request,
                |_texts: &[String]| Ok::<_, std::convert::Infallible>(vec![vec![1.0f32; dim]]),
            )
            .unwrap();
            assert_eq!(out.mode, SearchMode::Hybrid);
            assert!(!out.results.is_empty());
            assert!(
                out.results.iter().all(|r| r.file.starts_with("src/")),
                "filtered-out paths absent from the fused list: {:?}",
                out.results.iter().map(|r| r.file.clone()).collect::<Vec<_>>()
            );
        }
    }

    // ─── T020: degradation taxonomy — every failure class degrades safely ──

    mod degradation_tests {
        use super::super::*;
        use crate::embed::artifacts::ArtifactError;
        use crate::embed::local_onnx::LocalOnnxError;
        use crate::embed::profiles::NOMIC_EMBED_TEXT_V1_5;
        use joey_neurocode::graph::GraphStore;

        fn temp_store() -> (tempfile::TempDir, GraphStore) {
            let tmp = tempfile::tempdir().unwrap();
            let store = GraphStore::open(&tmp.path().join("graph.db")).unwrap();
            (tmp, store)
        }

        fn req(query: &str) -> SearchRequest {
            SearchRequest {
                query: query.to_string(),
                file_filter: None,
                limit: 10,
                expand_lines: 0,
                relation_depth: 0,
                include_fallback_chunks: true,
            }
        }

        /// One searchable chunk so degraded (keyword-only) results exist.
        fn seed_chunk(store: &GraphStore) {
            store
                .conn()
                .execute(
                    "INSERT INTO rag_chunks (chunk_id, chunk_kind, source_path, start_line, \
                     end_line, symbol_name, symbol_kind, content_hash) \
                     VALUES ('src/a.rs:1-10:symbol:Handler', 'symbol', 'src/a.rs', 1, 10, \
                     'Handler', 'class', 'h')",
                    [],
                )
                .unwrap();
        }

        /// The parameterized degradation matrix (T020): every taxonomy
        /// class reachable from the EXISTING error types — and the two
        /// shape errors — yields KeywordOnly, `degradation_note` on every
        /// result, `semantic_rank = None`, a class-keyed `mode_reason`,
        /// and NEVER a turn hard-fail (Ok, not Err). T010 grows this list.
        #[test]
        fn t020_every_failure_class_degrades_to_keyword_only() {
            let cases: Vec<(&str, LocalOnnxError, EmbedFailure)> = vec![
                (
                    "missing artifacts",
                    LocalOnnxError::Artifacts(ArtifactError::ModelFilesMissing(
                        "model_dir has no model.onnx".into(),
                    )),
                    EmbedFailure::ModelFilesMissing,
                ),
                (
                    "corrupt artifacts",
                    LocalOnnxError::Artifacts(ArtifactError::ModelFilesCorrupt(
                        "sha-256 mismatch for model.onnx".into(),
                    )),
                    EmbedFailure::ModelFilesCorrupt,
                ),
                (
                    "dylib load failure",
                    LocalOnnxError::DylibLoad("libonnxruntime.dylib not found".into()),
                    EmbedFailure::DylibLoad,
                ),
                (
                    "session failure",
                    LocalOnnxError::SessionLoad("graph input surface rejected".into()),
                    EmbedFailure::SessionLoad,
                ),
                (
                    "inference failure",
                    LocalOnnxError::Inference("batch run failed".into()),
                    EmbedFailure::Inference,
                ),
            ];
            assert!(cases.len() >= 5, "all five pre-T010 classes covered");

            for (name, err, want_class) in cases {
                let (tmp, store) = temp_store();
                seed_chunk(&store);
                let msg = err.to_string();

                let out = search_cli_with_embedder(
                    &store,
                    tmp.path(),
                    &NOMIC_EMBED_TEXT_V1_5,
                    &req("Handler"),
                    move |_texts: &[String]| Err::<Vec<Vec<f32>>, _>(msg.clone()),
                )
                .expect("NEVER a turn hard-fail (FR-008)");

                assert_eq!(out.mode, SearchMode::KeywordOnly, "{name}: mode");
                let reason = out.mode_reason.as_deref().unwrap_or_else(|| {
                    panic!("{name}: mode_reason present in keyword-only mode")
                });
                assert!(
                    reason.starts_with(want_class.as_str()),
                    "{name}: mode_reason class-prefixed, got: {reason}"
                );
                assert!(!out.results.is_empty(), "{name}: results still served");
                assert!(
                    out.results.iter().all(|r| r.semantic_rank.is_none()),
                    "{name}: semantic_rank = None on every result"
                );
                assert!(
                    out.results.iter().all(|r| r.degradation_note.is_some()),
                    "{name}: degradation_note on every result"
                );
                // The class machinery is deterministic on the message.
                assert_eq!(EmbedFailure::classify(&err.to_string()), want_class, "{name}");
            }
        }

        /// The empty-result shape error (embedder returns zero vectors)
        /// maps structurally to `EmptyResult` and degrades identically.
        #[test]
        fn t020_empty_embedding_result_degrades_structurally() {
            let (tmp, store) = temp_store();
            seed_chunk(&store);
            let out = search_cli_with_embedder(
                &store,
                tmp.path(),
                &NOMIC_EMBED_TEXT_V1_5,
                &req("Handler"),
                |_texts: &[String]| Ok::<_, std::convert::Infallible>(vec![]),
            )
            .expect("empty batch degrades, never fails the turn");
            assert_eq!(out.mode, SearchMode::KeywordOnly);
            assert!(
                out.mode_reason
                    .as_deref()
                    .unwrap_or_default()
                    .starts_with(EmbedFailure::EmptyResult.as_str()),
                "empty batch → empty_result class: {:?}",
                out.mode_reason
            );
            assert!(out.results.iter().all(|r| r.semantic_rank.is_none()));
            assert!(out.results.iter().all(|r| r.degradation_note.is_some()));
        }

        /// The taxonomy itself: class keys, notes, and classification of
        /// every `LocalOnnxError` variant (the T010 adaptation surface).
        #[test]
        fn t020_taxonomy_classifies_every_local_onnx_error_variant() {
            use LocalOnnxError as E;
            let cases: Vec<(LocalOnnxError, EmbedFailure)> = vec![
                (
                    E::Artifacts(ArtifactError::ModelFilesMissing("m".into())),
                    EmbedFailure::ModelFilesMissing,
                ),
                (
                    E::Artifacts(ArtifactError::ModelFilesCorrupt("m".into())),
                    EmbedFailure::ModelFilesCorrupt,
                ),
                (E::DylibLoad("m".into()), EmbedFailure::DylibLoad),
                (E::TokenizerLoad("tokenizer load failed".into()), EmbedFailure::SessionLoad),
                (E::SessionLoad("m".into()), EmbedFailure::SessionLoad),
                (E::Inference("m".into()), EmbedFailure::Inference),
                // Dimension mismatch is not a taxonomy class pre-T010 → Other.
                (E::DimensionMismatch("m".into()), EmbedFailure::Other),
                // Artifact IO/DB errors → Other (not artifact-integrity).
                (
                    E::Artifacts(ArtifactError::Io("m".into())),
                    EmbedFailure::Other,
                ),
                (
                    E::Artifacts(ArtifactError::Db("m".into())),
                    EmbedFailure::Other,
                ),
            ];
            for (err, want) in cases {
                assert_eq!(
                    EmbedFailure::from(&DenseLegError::Embed(err.to_string())),
                    want,
                    "classify {err}"
                );
            }
            // Every class renders a non-empty note + mode_reason.
            for class in [
                EmbedFailure::ModelFilesMissing,
                EmbedFailure::ModelFilesCorrupt,
                EmbedFailure::DylibLoad,
                EmbedFailure::SessionLoad,
                EmbedFailure::Inference,
                EmbedFailure::EmptyResult,
                EmbedFailure::Other,
            ] {
                assert!(!class.degradation_note().is_empty());
                assert!(class.mode_reason().starts_with(class.as_str()));
            }
        }

        /// A healthy backend keeps Hybrid mode with no degradation notes —
        /// the positive control for the degradation matrix.
        #[test]
        fn t020_healthy_backend_stays_hybrid() {
            let (tmp, store) = temp_store();
            seed_chunk(&store);
            let dim = NOMIC_EMBED_TEXT_V1_5.dim as usize;
            let out = search_cli_with_embedder(
                &store,
                tmp.path(),
                &NOMIC_EMBED_TEXT_V1_5,
                &req("Handler"),
                |_texts: &[String]| {
                    let mut v = vec![0.0f32; dim];
                    v[0] = 1.0; // unit vector — a healthy, non-degenerate query
                    Ok::<_, std::convert::Infallible>(vec![v])
                },
            )
            .unwrap();
            assert_eq!(out.mode, SearchMode::Hybrid);
            assert!(out.mode_reason.is_none());
            assert!(out.results.iter().all(|r| r.degradation_note.is_none()));
        }
    }

    // Threshold helper surfaced from the scan module for pipeline callers.
    #[test]
    fn threshold_helper_is_reachable_and_strictly_above() {
        use crate::config::DEFAULT_QUANTIZE_THRESHOLD as T;
        use crate::vector::scan::encoding_for_chunk_count;
        assert_eq!(encoding_for_chunk_count(100_000, T), Quantization::F32);
        assert_eq!(encoding_for_chunk_count(100_001, T), Quantization::Int8);
    }

    // ─── T015 `search_cli` orchestration ─────────────────────────────────

    mod search_cli_tests {
        use super::super::*;
        use crate::embed::profiles::NOMIC_EMBED_TEXT_V1_5;
        use crate::index::chunker::{
            build_chunk_records, ChunkOptions,
        };
        use crate::vector::store::write_index;
        use crate::vector::quantize::Quantization;
        use joey_neurocode::graph::GraphStore;
        use joey_neurocode::parse::extract::SourceExtraction;

        fn temp_store() -> (tempfile::TempDir, GraphStore) {
            let tmp = tempfile::tempdir().unwrap();
            let store = GraphStore::open(&tmp.path().join("graph.db")).unwrap();
            (tmp, store)
        }

        /// Python top-level sample: imports + top-level statements + one
        /// function → fallback chunks for the top-level region PLUS a
        /// symbol chunk for `main` (the T015 e2e fixture shape).
        const PY_TOPLEVEL: &str = "import os\nimport sys\n\nAPI_KEY = \"secret-literal\"\n\ndef main():\n    print(os.name)\n    return 0\n";

        fn index_python_toplevel(store: &GraphStore) -> Vec<crate::index::chunker::ChunkRecord> {
            let mut ex = SourceExtraction {
                language: "python".to_string(),
                ..Default::default()
            };
            ex.populate_fallback_chunks(PY_TOPLEVEL);
            build_chunk_records(&ex, PY_TOPLEVEL, "scripts/top.py", store, &ChunkOptions::default())
        }

        fn req(query: &str) -> SearchRequest {
            SearchRequest {
                query: query.to_string(),
                file_filter: None,
                limit: 10,
                expand_lines: 0,
                relation_depth: 0,
                include_fallback_chunks: true,
            }
        }

        /// Whitespace-only query → Validation, no search executed (edge 1).
        #[test]
        fn whitespace_only_query_is_rejected_before_any_search() {
            let (_tmp, store) = temp_store();
            for q in ["", "   ", "\t\n "] {
                let err = search_cli(&store, Path::new("."), &NOMIC_EMBED_TEXT_V1_5, &req(q))
                    .unwrap_err();
                match err {
                    SearchError::Validation(msg) => assert!(
                        msg.contains("whitespace-only") || msg.contains("empty"),
                        "clear message, got: {msg}"
                    ),
                    other => panic!("expected Validation, got {other:?}"),
                }
            }
        }

        /// Write the fixture source under `root` so the content scan can
        /// read chunk bodies at query time.
        fn write_fixture(root: &Path, rel: &str, body: &str) {
            let full = root.join(rel);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, body).unwrap();
        }

        /// Keyword-only degradation: the CLI's no-embedder posture yields
        /// mode=keyword_only + mode_reason on every result, never an error.
        #[test]
        fn no_backend_degrades_to_keyword_only_with_reason() {
            let (_tmp, store) = temp_store();
            write_fixture(_tmp.path(), "scripts/top.py", PY_TOPLEVEL);
            let records = index_python_toplevel(&store);
            let vectors = vec![None; records.len()];
            write_index(&store, &NOMIC_EMBED_TEXT_V1_5, Quantization::F32, &records, &vectors, &[])
                .unwrap();

            let out = search_cli(
                &store,
                _tmp.path(),
                &NOMIC_EMBED_TEXT_V1_5,
                &req("top"),
            )
            .unwrap();
            assert_eq!(out.mode, SearchMode::KeywordOnly);
            let reason = out.mode_reason.as_deref().expect("mode_reason present");
            assert!(reason.contains("keyword-only fallback"), "{reason}");
            assert!(!out.results.is_empty(), "fallback chunk reachable");
            assert!(out.results.iter().all(|r| r.degradation_note.is_some()));
            assert!(out.results.iter().all(|r| r.semantic_rank.is_none()));
        }

        /// FR-014: the Python top-level sample's fallback chunk is found by
        /// keyword search and carries the FALLBACK badge.
        #[test]
        fn python_toplevel_fallback_chunk_searchable_and_badged() {
            let (_tmp, store) = temp_store();
            write_fixture(_tmp.path(), "scripts/top.py", PY_TOPLEVEL);
            let records = index_python_toplevel(&store);
            // Sanity: the fixture really produced a fallback chunk.
            assert!(
                records.iter().any(|r| r.kind == crate::index::chunker::ChunkKind::Fallback),
                "fixture must contain a fallback chunk"
            );
            let vectors = vec![None; records.len()];
            write_index(&store, &NOMIC_EMBED_TEXT_V1_5, Quantization::F32, &records, &vectors, &[])
                .unwrap();

            let out = search_cli(
                &store,
                _tmp.path(),
                &NOMIC_EMBED_TEXT_V1_5,
                &req("secret-literal"),
            )
            .unwrap();
            assert!(!out.results.is_empty(), "top-level code searchable");
            let fb = out
                .results
                .iter()
                .find(|r| r.chunk_kind == ChunkBadge::Fallback)
                .expect("fallback-chunk result present and badged");
            assert_eq!(fb.file, "scripts/top.py");
            assert_eq!(fb.symbol, None, "fallback chunks carry no symbol");
        }

        /// No-match: empty results + zero candidates is a CLEAR response,
        /// distinct from failure.
        #[test]
        fn no_match_returns_clear_empty_not_error() {
            let (_tmp, store) = temp_store();
            write_fixture(_tmp.path(), "scripts/top.py", PY_TOPLEVEL);
            let records = index_python_toplevel(&store);
            let vectors = vec![None; records.len()];
            write_index(&store, &NOMIC_EMBED_TEXT_V1_5, Quantization::F32, &records, &vectors, &[])
                .unwrap();

            let out = search_cli(
                &store,
                _tmp.path(),
                &NOMIC_EMBED_TEXT_V1_5,
                &req("zzzznotpresentzzzz"),
            )
            .unwrap();
            assert!(out.results.is_empty());
            assert_eq!(out.total_candidates, 0);
            // Indexed count > 0 → genuine no-match (not an empty index).
            assert!(out.index_chunk_count > 0);
        }

        /// File-scope glob filters BOTH legs pre-merge (T019's rule honored
        /// at T015).
        #[test]
        fn file_filter_scopes_keyword_leg() {
            let (_tmp, store) = temp_store();
            // Two files: one matching the glob, one not.
            for (path, body) in [
                ("src/a.py", "value_alpha = 1\n"),
                ("lib/b.py", "value_beta = 2\n"),
            ] {
                write_fixture(_tmp.path(), path, body);
                let mut ex = SourceExtraction {
                    language: "python".to_string(),
                    ..Default::default()
                };
                ex.populate_fallback_chunks(body);
                let records =
                    build_chunk_records(&ex, body, path, &store, &ChunkOptions::default());
                let vectors = vec![None; records.len()];
                write_index(&store, &NOMIC_EMBED_TEXT_V1_5, Quantization::F32, &records, &vectors, &[])
                    .unwrap();
            }

            let mut request = req("value");
            request.file_filter = Some("src/*".to_string());
            let out = search_cli(&store, _tmp.path(), &NOMIC_EMBED_TEXT_V1_5, &request).unwrap();
            assert!(!out.results.is_empty());
            assert!(
                out.results.iter().all(|r| r.file.starts_with("src/")),
                "filtered-out paths appear in neither leg: {:?}",
                out.results.iter().map(|r| r.file.clone()).collect::<Vec<_>>()
            );
        }

        /// Context expansion: clamped ±`expand_lines` read at query time;
        /// missing file → context_absent (None), never an error.
        #[test]
        fn context_expansion_clamps_and_tolerates_missing_files() {
            let (tmp, store) = temp_store();
            std::fs::write(tmp.path().join("s.py"), "l1\nl2\nl3\nl4\nl5\n").unwrap();
            let body = "l1\nl2\nl3\nl4\nl5\n";
            let mut ex = SourceExtraction {
                language: "python".to_string(),
                ..Default::default()
            };
            ex.populate_fallback_chunks(body);
            let records =
                build_chunk_records(&ex, body, "s.py", &store, &ChunkOptions::default());
            let vectors = vec![None; records.len()];
            write_index(&store, &NOMIC_EMBED_TEXT_V1_5, Quantization::F32, &records, &vectors, &[])
                .unwrap();

            // Window 2 around a 5-line chunk clamps to the whole file.
            let mut wide = req("l1");
            wide.expand_lines = 2;
            let out = search_cli(&store, tmp.path(), &NOMIC_EMBED_TEXT_V1_5, &wide).unwrap();
            let ctx = out.results[0].context.as_deref().expect("context present");
            assert!(ctx.contains("l1") && ctx.contains("l5"), "clamped window: {ctx}");

            // Missing file on disk (indexed but never written) → the chunk
            // is still reachable via its PATH (LIKE leg) but context is
            // absent (None), never an error.
            let ghost = tempfile::tempdir().unwrap();
            let mut g = req("s.py");
            g.expand_lines = 0;
            let out = search_cli(&store, ghost.path(), &NOMIC_EMBED_TEXT_V1_5, &g).unwrap();
            assert!(
                !out.results.is_empty(),
                "path leg reaches the chunk without the file: {:?}",
                out.results
            );
            assert!(out.results[0].context.is_none(), "context_absent, not an error");
        }

        /// T027: the pipeline clamps the window at CONSUMPTION too —
        /// `SearchRequest` is a public type, so an out-of-contract
        /// `expand_lines` (here 5000, far past the 200 max) still yields
        /// the 200-bounded read. With the chunk at line 5000 of a
        /// 10000-line file, ±200 = lines 4800..5200 (401 lines); an
        /// UNclamped window would return the whole file.
        #[test]
        fn t027_pipeline_clamps_expand_lines_at_consumption() {
            let (tmp, store) = temp_store();
            let body: String = (1..=10000)
                .map(|n| format!("line{n}\n"))
                .collect::<String>();
            std::fs::create_dir_all(tmp.path().join("src")).unwrap();
            std::fs::write(tmp.path().join("src/big.rs"), &body).unwrap();
            // Insert the symbol chunk directly (same shape as the
            // exact_symbol_tests helper — controls the chunk span precisely).
            let chunk_id = "src/big.rs:5000-5000:symbol:BigSym".to_string();
            store
                .conn()
                .execute(
                    "INSERT INTO rag_chunks (chunk_id, chunk_kind, source_path, start_line, \
                     end_line, symbol_name, symbol_kind, content_hash) \
                     VALUES (?1, 'symbol', 'src/big.rs', 5000, 5000, 'BigSym', 'class', 'h')",
                    rusqlite::params![chunk_id],
                )
                .unwrap();

            let mut request = req("BigSym");
            request.expand_lines = 5000; // way past the 200 contract max
            let out = search_cli(&store, tmp.path(), &NOMIC_EMBED_TEXT_V1_5, &request).unwrap();
            assert_eq!(out.results[0].symbol.as_deref(), Some("BigSym"));
            let ctx = out.results[0].context.as_deref().expect("context present");
            let lines: Vec<&str> = ctx.lines().collect();
            assert_eq!(
                lines.first(),
                Some(&"line4800"),
                "±200 from line 5000 — clamped at consumption"
            );
            assert_eq!(lines.last(), Some(&"line5200"));
            assert_eq!(lines.len(), 401, "5000±200 = 401 lines, not 10000");
        }

        /// include_fallback_chunks=false excludes fallback chunks from the
        /// keyword leg (FR-014 participation flag).
        #[test]
        fn fallback_exclusion_flag_is_honored() {
            let (_tmp, store) = temp_store();
            write_fixture(_tmp.path(), "scripts/top.py", PY_TOPLEVEL);
            let records = index_python_toplevel(&store);
            let vectors = vec![None; records.len()];
            write_index(&store, &NOMIC_EMBED_TEXT_V1_5, Quantization::F32, &records, &vectors, &[])
                .unwrap();

            let mut request = req("main");
            request.include_fallback_chunks = false;
            let out = search_cli(&store, _tmp.path(), &NOMIC_EMBED_TEXT_V1_5, &request).unwrap();
            assert!(
                out.results.iter().all(|r| r.chunk_kind != ChunkBadge::Fallback),
                "fallback chunks excluded: {:?}",
                out.results
            );
        }

        /// A working embedder yields Hybrid mode with dense ranks (the
        /// T016+ posture already flows through the same surface).
        #[test]
        fn working_embedder_yields_hybrid_mode() {
            let (_tmp, store) = temp_store();
            write_fixture(_tmp.path(), "scripts/top.py", PY_TOPLEVEL);
            let records = index_python_toplevel(&store);
            // Deterministic dummy vectors (dim must match the profile).
            let dim = NOMIC_EMBED_TEXT_V1_5.dim as usize;
            let vectors: Vec<Option<Vec<f32>>> =
                records.iter().map(|_| Some(vec![0.5f32; dim])).collect();
            write_index(&store, &NOMIC_EMBED_TEXT_V1_5, Quantization::F32, &records, &vectors, &[])
                .unwrap();

            let out = search_cli_with_embedder(
                &store,
                _tmp.path(),
                &NOMIC_EMBED_TEXT_V1_5,
                &req("main"),
                |_texts: &[String]| Ok::<_, std::convert::Infallible>(vec![vec![0.5f32; dim]]),
            )
            .unwrap();
            assert_eq!(out.mode, SearchMode::Hybrid);
            assert!(out.mode_reason.is_none());
            assert!(
                out.results.iter().any(|r| r.semantic_rank.is_some()),
                "dense ranks present: {:?}",
                out.results
            );
        }

        /// Empty index: zero chunks → empty results + count 0 (the CLI's
        /// "run /neurocode index" hint input).
        #[test]
        fn empty_index_reports_zero_chunks() {
            let (_tmp, store) = temp_store();
            let out = search_cli(&store, _tmp.path(), &NOMIC_EMBED_TEXT_V1_5, &req("anything"))
                .unwrap();
            assert!(out.results.is_empty());
            assert_eq!(out.index_chunk_count, 0);
        }

        /// Glob matcher pin (`*` crosses `/`, `?` single char, literal
        /// mismatch fails).
        #[test]
        fn glob_matcher_semantics() {
            assert!(glob_match("src/*", "src/a/b.py"));
            assert!(glob_match("*.py", "scripts/top.py"));
            assert!(glob_match("src/?.py", "src/a.py"));
            assert!(!glob_match("src/?.py", "src/ab.py"));
            assert!(!glob_match("lib/*", "src/a.py"));
            assert!(glob_match("**", "anything/at/all"));
        }
    }
}

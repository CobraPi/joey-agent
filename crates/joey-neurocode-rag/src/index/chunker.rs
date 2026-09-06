//! Chunk records from parse spans + fallback (T012).
//!
//! Symbol-aligned chunks from the parse layer's extracted artifacts plus
//! fallback coarse chunks for regions with no named artifacts; contextual
//! prefix (file path + imports) prepended to the chunk body; `content_hash`
//! computed over the RAW UNPREFIXED text (SHA-256, hex) — "unprefixed"
//! means WITHOUT the profile's document prefix, which is applied at EMBED
//! time only (data-model.md §1 `content_hash` + §1 prefixing note;
//! research.md R4). Pinned so a profile change never invalidates chunk
//! hashes; only embeddings rebuild.
//!
//! The embedding call boundary is the minimal [`ChunkEmbedder`] trait so
//! T010's backend trait can adapt to it without this module depending on
//! `embed::local_onnx` or any backend implementation.
//!
//! **T028 — derived chunk-level edges** (data-model.md §7): the typed
//! graph's artifact-level `graph_edges` are projected down to their chunks
//! (`rag_chunk_edges`) at index time. Fully derived and rebuildable — the
//! typed graph stays authoritative; [`rewrite_chunk_edges`] is invoked by
//! the write path inside the same transaction as the chunk rows.

use std::path::Path;

use rusqlite::Connection;
use sha2::{Digest, Sha256};

use joey_neurocode::graph::GraphStore;
use joey_neurocode::parse::extract::SourceExtraction;
use joey_neurocode::parse::spans::LineIndex;

/// Discriminated chunk kind (data-model.md §1: `SymbolAligned | FallbackCoarse`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChunkKind {
    /// A named artifact's chunk. `artifact_id` FK → `code_artifacts.id`;
    /// resolved from the typed graph when the artifact exists there, else
    /// `None` (parse-only chunk — the DDL leaves the column nullable).
    Symbol {
        artifact_id: Option<u64>,
        symbol_name: String,
        /// `class | interface | enum | method | field`
        symbol_kind: String,
    },
    /// No symbol identity — file + line-range identity only (FR-014).
    Fallback,
}

/// One chunk record — the in-memory form of one `rag_chunks` row
/// (data-model.md §1 common fields).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkRecord {
    /// Deterministic identity: `source_path + start_line + end_line + kind
    /// discriminator (+ symbol name, keeping same-line type/method chunks
    /// distinct)` — a moved/shifted chunk gets a new id; ids are never
    /// mutated in place (data-model.md §1).
    pub chunk_id: String,
    pub kind: ChunkKind,
    /// Repository-relative path of the source file.
    pub source_path: String,
    /// First line of the chunk's code body (1-based, inclusive).
    pub start_line: u32,
    /// Last line of the chunk's code body (1-based, inclusive).
    pub end_line: u32,
    /// Language tag (`"python"`, `"java"`, …).
    pub language: String,
    /// SHA-256 hex over the chunk's RAW UNPREFIXED text — the constructed
    /// contextual text (path + imports prefix + body) EXCLUDING the profile's
    /// document prefix, which is applied at embed time and never hashed.
    pub content_hash: String,
    /// The contextual text handed to the embedder AFTER the profile's
    /// document prefix is applied by the pipeline — i.e. WITHOUT the profile
    /// prefix here (profile prefix at embed time, not stored, not hashed).
    pub embed_text: String,
}

/// Knobs for [`build_chunk_records`].
#[derive(Debug, Clone)]
pub struct ChunkOptions {
    /// Split chunks longer than this many LINES into consecutive pieces
    /// (bounded processing — a ≥10 MB file must never become one monolithic
    /// chunk). Default [`DEFAULT_MAX_CHUNK_LINES`].
    pub max_chunk_lines: usize,
    /// Cap on the number of imports rendered into the contextual prefix
    /// (bounded prefix; default [`DEFAULT_MAX_IMPORTS`]).
    pub max_imports: usize,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self {
            max_chunk_lines: DEFAULT_MAX_CHUNK_LINES,
            max_imports: DEFAULT_MAX_IMPORTS,
        }
    }
}

/// Default per-chunk line cap. The R4 chunk band is 256–1024 tokens; 200
/// lines of source sits in that band (matching
/// `parse::extract::DEFAULT_FALLBACK_CHUNK_MAX_LINES`).
pub const DEFAULT_MAX_CHUNK_LINES: usize = 200;

/// Default import cap in the contextual prefix.
pub const DEFAULT_MAX_IMPORTS: usize = 30;

/// Embedding batch size — bounded processing for very large files (the
/// ≥10 MB bounding case embeds many small batches, never one piece; also
/// the backend contract's batch-64 default, research.md R2).
pub const EMBED_BATCH_SIZE: usize = 64;

/// Minimal embedding call boundary (T012's decoupling seam).
///
/// T010's `EmbeddingBackend` trait adapts to this — the chunker itself never
/// touches `embed::local_onnx` or any backend. Batches in, vectors out,
/// order-preserving. Inputs arrive with the profile document prefix ALREADY
/// applied by the pipeline.
pub trait ChunkEmbedder {
    /// Embed `texts` into `dim`-dimensional vectors, preserving order
    /// (`result.len() == texts.len()`).
    fn embed_texts(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String>;
}

/// The full indexing pipeline for one file (T012): chunk → hash → embed in
/// bounded batches → single-transaction write (chunks + vectors + meta +
/// purges; all-or-nothing via [`crate::vector::store::write_index`]).
///
/// * `extraction` — the parse result for `source`; fallback chunks must be
///   populated first ([`SourceExtraction::populate_fallback_chunks`]) for
///   symbol-free regions to be covered.
/// * `embedder` — the [`ChunkEmbedder`] boundary; the profile document
///   prefix is applied HERE (embed time), never hashed into `content_hash`.
/// * `purge_paths` — paths purged inside the same transaction (refresh).
///
/// Returns the chunk records exactly as committed.
pub fn index_file(
    store: &GraphStore,
    extraction: &SourceExtraction,
    source: &str,
    source_path: &str,
    embedder: &mut dyn ChunkEmbedder,
    profile: &crate::embed::profiles::EmbedProfile,
    quantization: crate::vector::quantize::Quantization,
    options: &ChunkOptions,
    purge_paths: &[&str],
) -> Result<Vec<ChunkRecord>, crate::vector::store::VectorStoreError> {
    let records = build_chunk_records(extraction, source, source_path, store, options);

    // Embed in bounded batches: profile prefix (embed time, not hashed)
    // → vector. A huge file becomes many small batches, never one piece.
    let mut vectors: Vec<Option<Vec<f32>>> = Vec::with_capacity(records.len());
    let mut batch: Vec<String> = Vec::with_capacity(EMBED_BATCH_SIZE);
    let mut flush = |batch: &mut Vec<String>,
                     vectors: &mut Vec<Option<Vec<f32>>>|
     -> Result<(), crate::vector::store::VectorStoreError> {
        if batch.is_empty() {
            return Ok(());
        }
        let embedded = embedder
            .embed_texts(batch)
            .map_err(crate::vector::store::VectorStoreError::Embed)?;
        if embedded.len() != batch.len() {
            return Err(crate::vector::store::VectorStoreError::Embed(format!(
                "embedder returned {} vectors for {} texts (order/size must be preserved)",
                embedded.len(),
                batch.len()
            )));
        }
        vectors.extend(embedded.into_iter().map(Some));
        batch.clear();
        Ok(())
    };
    for chunk in &records {
        batch.push(profile.document_input(&chunk.embed_text));
        if batch.len() == EMBED_BATCH_SIZE {
            flush(&mut batch, &mut vectors)?;
        }
    }
    flush(&mut batch, &mut vectors)?;
    debug_assert_eq!(vectors.len(), records.len());

    crate::vector::store::write_index(
        store,
        profile,
        quantization,
        &records,
        &vectors,
        purge_paths,
    )?;
    Ok(records)
}

/// Build the chunk records for one parsed file: symbol-aligned chunks from
/// the parse spans (split to `max_chunk_lines`), plus fallback coarse chunks
/// (from `extraction.fallback_chunks` — populate them first; this function
/// does not mutate the extraction).
///
/// Each record carries the contextual prefix inside `embed_text` and the
/// SHA-256 `content_hash` over that same RAW UNPREFIXED text (the profile's
/// document prefix is nowhere in either — it is applied at embed time).
pub fn build_chunk_records(
    extraction: &SourceExtraction,
    source: &str,
    source_path: &str,
    store: &GraphStore,
    options: &ChunkOptions,
) -> Vec<ChunkRecord> {
    let index = LineIndex::new(source);
    let spans = extraction.line_spans(source);
    let mut out: Vec<ChunkRecord> = Vec::new();

    let prefix = contextual_prefix(source_path, extraction, options.max_imports);

    // ── Symbol-aligned chunks ─────────────────────────────────────────
    // Type declarations …
    for ty in &spans.types {
        let artifact_id = find_artifact_id(store, source_path, &ty.name);
        push_symbol_chunks(
            &mut out,
            &index,
            source,
            &prefix,
            source_path,
            &extraction.language,
            ty.start_line,
            ty.end_line,
            &ty.name,
            &ty.kind,
            artifact_id,
            options.max_chunk_lines,
        );
        // … and each method inside (finer-grained retrieval targets).
        for m in &ty.methods {
            let artifact_id = find_artifact_id(store, source_path, &m.name);
            push_symbol_chunks(
                &mut out,
                &index,
                source,
                &prefix,
                source_path,
                &extraction.language,
                m.start_line,
                m.end_line,
                &m.name,
                &m.kind,
                artifact_id,
                options.max_chunk_lines,
            );
        }
    }
    // … and module-level functions.
    for f in &spans.module_functions {
        let artifact_id = find_artifact_id(store, source_path, &f.name);
        push_symbol_chunks(
            &mut out,
            &index,
            source,
            &prefix,
            source_path,
            &extraction.language,
            f.start_line,
            f.end_line,
            &f.name,
            &f.kind,
            artifact_id,
            options.max_chunk_lines,
        );
    }

    // ── Fallback coarse chunks ────────────────────────────────────────
    for fb in &spans.fallback_chunks {
        push_fallback_chunks(
            &mut out,
            &index,
            source,
            &prefix,
            source_path,
            &extraction.language,
            fb.start_line,
            fb.end_line,
            options.max_chunk_lines,
        );
    }

    out
}

/// The contextual prefix shared by every chunk of one file (R4: "code body
/// with file path and import/dependency context prepended"):
/// `path: <path>` header plus a bounded `imports:` line.
fn contextual_prefix(source_path: &str, extraction: &SourceExtraction, max_imports: usize) -> String {
    let mut prefix = format!("path: {source_path}\n");
    if !extraction.imports.is_empty() {
        let shown: Vec<&str> =
            extraction.imports.iter().take(max_imports).map(|s| s.as_str()).collect();
        prefix.push_str(&format!("imports: {}\n", shown.join(", ")));
        if extraction.imports.len() > max_imports {
            prefix.push_str(&format!(
                "imports: … (+{} more)\n",
                extraction.imports.len() - max_imports
            ));
        }
    }
    prefix
}

/// Resolve a symbol's `code_artifacts.id` for the chunk's FK. The typed
/// graph is keyed `(fqcn, kind, source_path)`; match on exact fqcn or an
/// ends-with-`.name` qualified form within the same file. `None` when not
/// found (parse-only chunk) — acceptable: the DDL leaves the FK nullable.
/// `ORDER BY id` keeps the LIMIT 1 deterministic (an unspecified row
/// choice would make chunk FKs nondeterministic across runs when multiple
/// rows match the LIKE form). The LIKE pattern escapes `\`, `%` and `_`
/// (with `ESCAPE '\'`) so symbol names containing them match LITERALLY —
/// an unescaped `_` acted as a single-char wildcard and bound wrong
/// artifact ids (e.g. symbol `foo_bar` matching `fooXbar`).
fn find_artifact_id(store: &GraphStore, source_path: &str, symbol: &str) -> Option<u64> {
    store
        .conn()
        .query_row(
            "SELECT id FROM code_artifacts
             WHERE source_path = ?1 AND (fqcn = ?2 OR fqcn LIKE ?3 ESCAPE '\\')
             ORDER BY id LIMIT 1",
            rusqlite::params![
                source_path,
                symbol,
                format!("%.{}", like_escape_symbol(symbol))
            ],
            |row| row.get::<_, i64>(0),
        )
        .ok()
        .map(|id| id as u64)
}

/// Escape SQL LIKE wildcards (`%`, `_`, `\`) so a symbol name matches
/// literally in [`find_artifact_id`]'s `ESCAPE '\'` LIKE form (same rule
/// as the keyword leg's `like_escape` in `search/hybrid.rs`).
fn like_escape_symbol(token: &str) -> String {
    token
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Push one symbol-aligned chunk, split into ≤ `max_chunk_lines` pieces.
#[allow(clippy::too_many_arguments)]
fn push_symbol_chunks(
    out: &mut Vec<ChunkRecord>,
    index: &LineIndex,
    source: &str,
    file_prefix: &str,
    source_path: &str,
    language: &str,
    start_line: u32,
    end_line: u32,
    symbol: &str,
    symbol_kind: &str,
    artifact_id: Option<u64>,
    max_chunk_lines: usize,
) {
    for (first, last) in line_pieces(start_line, end_line, max_chunk_lines) {
        let body = lines_text(index, source, first, last);
        // content_hash over the RAW UNPREFIXED text = contextual text
        // WITHOUT the profile prefix (which never reaches this function).
        let embed_text = format!("{}{}", file_prefix, body);
        out.push(ChunkRecord {
            chunk_id: make_chunk_id(source_path, first, last, "symbol", Some(symbol)),
            kind: ChunkKind::Symbol {
                artifact_id,
                symbol_name: symbol.to_string(),
                symbol_kind: symbol_kind.to_string(),
            },
            source_path: source_path.to_string(),
            start_line: first,
            end_line: last,
            language: language.to_string(),
            content_hash: content_hash(&embed_text),
            embed_text,
        });
    }
}

/// Push one fallback chunk (parse-layer fallbacks are already ≤
/// [`joey_neurocode::parse::extract::DEFAULT_FALLBACK_CHUNK_MAX_LINES`]
/// lines; re-split under a tighter caller cap).
#[allow(clippy::too_many_arguments)]
fn push_fallback_chunks(
    out: &mut Vec<ChunkRecord>,
    index: &LineIndex,
    source: &str,
    file_prefix: &str,
    source_path: &str,
    language: &str,
    start_line: u32,
    end_line: u32,
    max_chunk_lines: usize,
) {
    for (first, last) in line_pieces(start_line, end_line, max_chunk_lines) {
        let body = lines_text(index, source, first, last);
        let embed_text = format!("{}{}", file_prefix, body);
        out.push(ChunkRecord {
            chunk_id: make_chunk_id(source_path, first, last, "fallback", None),
            kind: ChunkKind::Fallback,
            source_path: source_path.to_string(),
            start_line: first,
            end_line: last,
            language: language.to_string(),
            content_hash: content_hash(&embed_text),
            embed_text,
        });
    }
}

/// Split `[start_line, end_line]` into consecutive pieces of at most
/// `max_lines` lines (one piece when the span already fits).
fn line_pieces(start_line: u32, end_line: u32, max_lines: usize) -> Vec<(u32, u32)> {
    let max_lines = max_lines.max(1) as u32;
    let mut pieces = Vec::new();
    let mut first = start_line;
    while first <= end_line {
        let last = (first + max_lines - 1).min(end_line);
        pieces.push((first, last));
        first = last + 1;
    }
    pieces
}

/// The raw body text of lines `[first, last]` joined with `\n` (no trailing
/// newline).
fn lines_text(index: &LineIndex, source: &str, first: u32, last: u32) -> String {
    let mut buf = String::new();
    for line in first..=last {
        if line > first {
            buf.push('\n');
        }
        buf.push_str(index.line_text(source, line));
    }
    buf
}

/// Deterministic chunk identity from `source_path + line range + kind
/// discriminator (+ symbol name — keeps a same-line type/method pair from
/// colliding on the PK)` (data-model.md §1).
fn make_chunk_id(
    source_path: &str,
    start: u32,
    end: u32,
    kind: &str,
    symbol: Option<&str>,
) -> String {
    match symbol {
        None => format!("{}:{}-{}:{}", source_path, start, end, kind),
        Some(s) => format!("{}:{}-{}:{}:{}", source_path, start, end, kind, s),
    }
}

/// SHA-256 hex (64 lowercase chars) over the RAW UNPREFIXED chunk text.
///
/// THE pinned invariant (data-model.md §1): the input is the constructed
/// contextual text (path + imports prefix + body) and EXCLUDES the profile's
/// document prefix — a profile/prefix change never changes a chunk hash;
/// only embeddings rebuild.
pub fn content_hash(raw_unprefixed_text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw_unprefixed_text.as_bytes());
    hasher.finalize().iter().fold(String::with_capacity(64), |s, b| s + &format!("{:02x}", b))
}

/// Normalize a path to the repo-relative `/`-separated form used in chunk
/// identity (Windows `\` normalized away).
pub fn normalize_source_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

// ===========================================================================
// T028 — rag_chunk_edges derivation (typed-graph edge → chunk projection)
// ===========================================================================

/// One derived `rag_chunk_edges` row: a typed-graph edge projected down to
/// the symbol chunks of its endpoint artifacts (data-model.md §7).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChunkEdgeRow {
    pub from_chunk_id: String,
    pub to_chunk_id: String,
    /// The `graph_edges.edge_kind` string VERBATIM (`EdgeKind::as_str`
    /// vocabulary: Implements, IsImplementedBy, Injects, ExchangesType,
    /// MemberOf, ReferencesRule, InheritsRule). Never round-tripped through
    /// `EdgeKind::parse` — projection is string-preserving by design.
    pub edge_kind: String,
}

/// Project every artifact-level typed-graph edge (`graph_edges`) down to the
/// symbol chunks of its endpoint artifacts (data-model.md §7: "derived at
/// index time from the existing typed-graph edges, artifact-to-artifact
/// projected down to their chunks").
///
/// For each `graph_edges` row, the symbol chunks whose `rag_chunks.artifact_id`
/// equals `from_id` pair with those equal to `to_id` (cartesian per endpoint —
/// a symbol split into multiple line-range pieces contributes each piece),
/// with the `edge_kind` string preserved verbatim. Edges whose endpoints have
/// no indexed chunks (fallback-only coverage, artifacts not yet indexed)
/// produce nothing; fallback chunks never carry `artifact_id` and never
/// participate. Deterministic output order (sorted) so re-indexing an
/// unchanged project is byte-stable.
pub fn project_chunk_edges(conn: &Connection) -> rusqlite::Result<Vec<ChunkEdgeRow>> {
    let mut stmt = conn.prepare(
        r#"
        SELECT fc.chunk_id, tc.chunk_id, ge.edge_kind
        FROM graph_edges ge
        JOIN rag_chunks fc ON fc.artifact_id = ge.from_id
        JOIN rag_chunks tc ON tc.artifact_id = ge.to_id
        ORDER BY fc.chunk_id, tc.chunk_id, ge.edge_kind
        "#,
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ChunkEdgeRow {
            from_chunk_id: r.get(0)?,
            to_chunk_id: r.get(1)?,
            edge_kind: r.get(2)?,
        })
    })?;
    let mut out: Vec<ChunkEdgeRow> = rows.collect::<Result<_, _>>()?;
    out.sort();
    out.dedup();
    Ok(out)
}

/// Rewrite `rag_chunk_edges` from the current typed graph + chunk registry.
///
/// FULLY DERIVED (data-model.md §7): the table is emptied and re-projected,
/// so a rebuild rebuilds edges and any rows referencing purged chunks
/// disappear (the `no FK by design` sweep — consistent with the incremental
/// refresh path's explicit sweeps in `index::incremental`). Must be called
/// INSIDE the caller's write transaction so the swap stays atomic; returns
/// the number of projected rows now in the table.
pub fn rewrite_chunk_edges(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute("DELETE FROM rag_chunk_edges", [])?;
    let edges = project_chunk_edges(conn)?;
    {
        let mut stmt =
            conn.prepare("INSERT INTO rag_chunk_edges (from_chunk_id, to_chunk_id, edge_kind) VALUES (?1, ?2, ?3)")?;
        for e in &edges {
            stmt.execute(rusqlite::params![
                e.from_chunk_id,
                e.to_chunk_id,
                e.edge_kind
            ])?;
        }
    }
    Ok(edges.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_over_raw_unprefixed_text() {
        // Known-vector sanity: sha256("abc").
        assert_eq!(
            content_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(content_hash("x").len(), 64);
        // The PIN (full-pipeline form in tests/dense_index.rs): the profile
        // document prefix is applied at EMBED time only — content_hash never
        // sees it, so changing profiles cannot change any chunk hash.
        let text = "path: a.py\nimports: os\nx = 1\n";
        let nomic_input = format!("search_document: {}", text);   // nomic prefix
        let coderank_input = text.to_string();                    // empty prefix
        // Hash the UNPREFIXED form (what the chunker hashes)…
        let h = content_hash(text);
        // …and observe the prefixed embed inputs differ from both each
        // other and the hashed text: prefix application is embed-time only.
        assert_ne!(nomic_input, coderank_input);
        assert_ne!(h, content_hash(&nomic_input));
    }

    #[test]
    fn chunk_id_is_deterministic_and_position_sensitive() {
        let a = make_chunk_id("src/a.py", 1, 10, "symbol", Some("Foo"));
        assert_eq!(a, "src/a.py:1-10:symbol:Foo");
        // Shifted chunk → new identity (old purged, new created — never
        // in-place id mutation).
        assert_ne!(a, make_chunk_id("src/a.py", 2, 11, "symbol", Some("Foo")));
        // Same-line type/method pair stays distinct (PK-safety).
        assert_ne!(
            make_chunk_id("s.py", 1, 1, "symbol", Some("A")),
            make_chunk_id("s.py", 1, 1, "symbol", Some("f"))
        );
        // Fallback chunks carry no symbol component.
        assert_eq!(make_chunk_id("s.py", 1, 5, "fallback", None), "s.py:1-5:fallback");
    }

    #[test]
    fn line_pieces_split_bounded() {
        assert_eq!(line_pieces(1, 10, 200), vec![(1, 10)]);
        assert_eq!(line_pieces(1, 500, 200), vec![(1, 200), (201, 400), (401, 500)]);
        assert_eq!(line_pieces(5, 5, 200), vec![(5, 5)]);
        assert_eq!(line_pieces(1, 400, 200), vec![(1, 200), (201, 400)]);
    }

    #[test]
    fn contextual_prefix_contains_path_and_bounded_imports() {
        let mut ex = SourceExtraction {
            language: "java".into(),
            package: "com.acme".into(),
            ..Default::default()
        };
        for i in 0..40 {
            ex.imports.push(format!("com.example.mod{}", i));
        }
        let p = contextual_prefix("src/A.java", &ex, 30);
        assert!(p.starts_with("path: src/A.java\n"));
        assert!(p.contains("com.example.mod0"));
        assert!(p.contains("com.example.mod29"));
        assert!(!p.contains("com.example.mod30,"));
        assert!(p.contains("+10 more"));
        // No imports → no imports line.
        let bare = SourceExtraction { language: "python".into(), ..Default::default() };
        assert_eq!(contextual_prefix("m.py", &bare, 30), "path: m.py\n");
    }

    #[test]
    fn record_hash_equals_hash_of_embed_text() {
        // The record's stored hash is exactly SHA-256(embed_text) — the
        // contextual text minus any profile prefix.
        let store = joey_neurocode::graph::GraphStore::open_in_memory().unwrap();
        let source = "def a():\n    return 1\n\nx = 2\n";
        let mut ex = SourceExtraction { language: "python".into(), ..Default::default() };
        ex.module_functions.push(joey_neurocode::parse::extract::ExtractedMethod {
            name: "a".into(),
            annotations: vec![],
            signature: None,
            start_byte: 0,
            end_byte: 21,
        });
        ex.populate_fallback_chunks(source);
        let records = build_chunk_records(&ex, source, "m.py", &store, &ChunkOptions::default());
        assert!(records.len() >= 2, "one symbol + one fallback at least");
        for r in &records {
            assert_eq!(r.content_hash, content_hash(&r.embed_text));
            assert!(r.embed_text.starts_with("path: m.py\n"));
            assert!(r.start_line >= 1 && r.start_line <= r.end_line);
        }
        let sym = records.iter().find(|r| r.kind != ChunkKind::Fallback).unwrap();
        assert!(matches!(sym.kind, ChunkKind::Symbol { ref symbol_name, .. } if symbol_name == "a"));
        let fb = records.iter().find(|r| r.kind == ChunkKind::Fallback).unwrap();
        assert!(fb.embed_text.contains("x = 2"));
    }

    /// LIKE wildcard escape pin: `find_artifact_id`'s qualified form must
    /// match symbol names LITERALLY — an unescaped `_` acted as a
    /// single-char wildcard, so symbol `foo_bar` wrongly matched an
    /// artifact `mod.fooXbar` (and bound its id). With `ESCAPE '\'`
    /// escaping, `foo_bar` matches only the exact `mod.foo_bar` form.
    #[test]
    fn find_artifact_id_like_wildcards_match_literally() {
        use joey_neurocode::graph::{ArtifactKind, CodeArtifactNode};

        let store = joey_neurocode::graph::GraphStore::open_in_memory().unwrap();
        let path = "src/m.py";
        let decoy = CodeArtifactNode::new(
            ArtifactKind::Class,
            "mod.fooXbar".to_string(),
            String::new(),
            path.to_string(),
        );
        let target = CodeArtifactNode::new(
            ArtifactKind::Class,
            "mod.foo_bar".to_string(),
            String::new(),
            path.to_string(),
        );
        let decoy_id = store.upsert_node(&decoy).unwrap();
        let target_id = store.upsert_node(&target).unwrap();
        assert_ne!(decoy_id, target_id);

        // The wildcard bug: LIKE '%.foo_bar' (unescaped `_`) matched
        // `mod.fooXbar` too, and with ORDER BY id LIMIT 1 could bind the
        // decoy. Escaped, only the literal `mod.foo_bar` matches.
        assert_eq!(find_artifact_id(&store, path, "foo_bar"), Some(target_id));
        // The decoy is reachable by its own literal name only.
        assert_eq!(find_artifact_id(&store, path, "fooXbar"), Some(decoy_id));
        // No `%` wildcard either: a symbol containing `%` matches nothing
        // except its literal form.
        let pct = CodeArtifactNode::new(
            ArtifactKind::Class,
            "mod.foo%bar".to_string(),
            String::new(),
            path.to_string(),
        );
        let pct_id = store.upsert_node(&pct).unwrap();
        assert_eq!(find_artifact_id(&store, path, "foo%bar"), Some(pct_id));
        // And `fooXbar` must NOT match the `%.foo_bar` form any more than
        // `foo_bar` matches `%.fooXbar` — cross-check via a name that would
        // be produced by wildcard expansion of the OTHER symbol.
        assert_eq!(find_artifact_id(&store, path, "foo_bar"), Some(target_id));
    }
}

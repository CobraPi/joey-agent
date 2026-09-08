//! Production wiring for the spec-021 RAG pipeline (T041b).
//!
//! `joey-agent-core` deliberately does NOT depend on `joey-neurocode-rag`
//! (its `RagRefreshWorker` / `RagPrefetchSource` are injection traits); this
//! module is the joey-cli-side adapter layer where both crates meet, and the
//! single home of the production paths:
//!
//! - [`execute_rag_search`] — the shared hybrid-search execution used by
//!   BOTH `/neurocode search` (`commands::neurocode`) and the agent's
//!   `neurocode_search` tool (`neurocode_wiring::EngineBackend::search`):
//!   resolve the embedding backend (`auto` → LocalOnnx when the artifacts
//!   verify, else keyword-only degradation — never a network call), embed
//!   the dense leg when a real backend resolves, and otherwise serve the
//!   keyword-only FTS/LIKE leg with the pinned FR-008 degradation shape.
//! - [`rag_index_text`] — the `/neurocode index` RAG half: run the rag
//!   crate's atomic refresh worker (`index::refresh_worker::run_refresh` —
//!   one transaction writing chunks + vectors + edges + meta) when a
//!   backend resolves; in the degraded state write vector-less chunk rows
//!   via `vector::store::write_index` (the store's supported "not yet
//!   embedded" shape) so the keyword leg has real rows to search, with the
//!   degradation reason surfaced in the output text — the index command
//!   never hard-fails the turn on a missing embedder.
//! - [`production_rag_refresh_worker`] / [`production_rag_prefetch_source`]
//!   — the joey-agent-core injection traits implemented over the rag crate,
//!   installed by `repl.rs` / `oneshot.rs` only when `neurocode.rag.enabled`
//!   (default off ⇒ zero behavior change).
//!
//! Prefix contract note (important): the rag crate's pipeline seams
//! (`ChunkEmbedder` for indexing, the `search_cli_with_embedder` embed
//! closure for search) receive text that the PIPELINE has already prefixed
//! with the profile query/document prefix, while the `EmbeddingBackend`
//! trait applies prefixes itself (raw text in). The adapters here strip the
//! one pipeline-applied prefix constant before handing text to the backend,
//! so the prefix is applied exactly once regardless of which side applies
//! it — `strip_prefix` on the exact profile constant removes precisely the
//! instance the pipeline prepended, never more.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use joey_agent_core::agent::{RagPrefetchSource, RagRefreshSummary, RagRefreshWorker};
use joey_neurocode::graph::DependencyGraph;
use joey_neurocode_rag::config::{RagBackend, RagConfig};
use joey_neurocode_rag::embed::profiles;
use joey_neurocode_rag::embed::{
    self, BackendKind, EmbedError, EmbeddingBackend, GatedRemoteBackend, LocalOnnxBackend,
    ResolvedBackend,
};
use joey_neurocode_rag::embed::local_onnx::LocalOnnxSettings;
use joey_neurocode_rag::index::chunker::{ChunkEmbedder, ChunkOptions};
use joey_neurocode_rag::index::incremental::{
    default_indexable_filter, detect_changes, DetectionOptions, RefreshBudgets,
};
use joey_neurocode_rag::index::refresh_worker::{
    reconstruct_previous, run_refresh, RefreshWorkerOptions,
};
use joey_neurocode_rag::search::hybrid::{
    search_cli, search_cli_with_embedder, SearchError, SearchOutcome, SearchRequest,
};
use joey_neurocode_rag::vector::quantize::Quantization;
use joey_neurocode_rag::vector::store as vector_store;

// ---------------------------------------------------------------------------
// Embedding-backend resolution (query + documents adapters, LocalOnnx cached)
// ---------------------------------------------------------------------------

/// Process-wide cache of loaded `LocalOnnx` sessions, keyed by
/// (model_dir, profile) — loading a ~130 MB ONNX session per `/neurocode
/// search` invocation would be prohibitive; the session is `Send + Sync`
/// (internal mutex) and its artifact set is verified at first load, so
/// reuse across queries/index runs is safe. Failures are never cached.
fn local_onnx_cache(
) -> &'static Mutex<HashMap<(PathBuf, &'static str), Arc<joey_neurocode_rag::embed::local_onnx::LocalOnnx>>> {
    static CACHE: OnceLock<
        Mutex<HashMap<(PathBuf, &'static str), Arc<joey_neurocode_rag::embed::local_onnx::LocalOnnx>>>,
    > = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Load (or fetch from cache) the verified LocalOnnx session.
fn load_local_onnx_cached(
    profile: &'static profiles::EmbedProfile,
    settings: &LocalOnnxSettings,
    store: Option<&joey_neurocode::graph::GraphStore>,
) -> Result<Arc<joey_neurocode_rag::embed::local_onnx::LocalOnnx>, EmbedError> {
    let key = (settings.model_dir.clone(), profile.name);
    if let Ok(guard) = local_onnx_cache().lock() {
        if let Some(hit) = guard.get(&key) {
            return Ok(Arc::clone(hit));
        }
    }
    let loaded = Arc::new(joey_neurocode_rag::embed::local_onnx::LocalOnnx::load(
        profile, settings, store,
    )?);
    if let Ok(mut guard) = local_onnx_cache().lock() {
        guard.insert(key, Arc::clone(&loaded));
    }
    Ok(loaded)
}

/// The consent directory for the current project (parent of the per-project
/// `graph.db` — where `consent.json` lives). Mirrors the rag crate's private
/// `consent_dir_from_cwd` via its public `consent_file_path` helper.
fn consent_dir_for_cwd() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    joey_neurocode_rag::consent::consent_file_path(&cwd)
        .parent()
        .map(|p| p.to_path_buf())
}

/// Resolve the QUERY-side embedding backend for production hybrid search —
/// the query-kind mirror of the rag crate's `embed::resolve` (which
/// constructs document-kind adapters for indexing): `auto` resolves
/// LocalOnnx when the artifacts verify and degrades to keyword-only (with
/// the structured `degradation_reason`) when they do not; remote kinds are
/// constructed (no network) and wrapped in the T032 consent gate.
/// Resolution itself never touches the network.
pub(crate) fn resolve_query_backend(
    rag: &RagConfig,
    store: Option<&joey_neurocode::graph::GraphStore>,
) -> Result<(ResolvedBackend, Option<Arc<dyn EmbeddingBackend>>), EmbedError> {
    let profile = effective_profile(rag, rag.backend);
    let profile_name = profile.name.to_string();
    let decision = embed::resolve_kind(rag.backend, &profile_name, &rag.model_dir)?;
    let consent_dir = consent_dir_for_cwd();
    match decision.kind {
        BackendKind::KeywordOnly => Ok((decision, None)),
        BackendKind::LocalOnnx => {
            let settings = LocalOnnxSettings::from_rag_config(rag);
            let loaded = load_local_onnx_cached(profile, &settings, store)?;
            let backend: Arc<dyn EmbeddingBackend> =
                Arc::new(LocalOnnxBackend::query(loaded));
            Ok((decision, Some(backend)))
        }
        BackendKind::Copilot => {
            let inner = joey_neurocode_rag::embed::copilot::CopilotEmbeddings::query(
                rag.api_key.clone(),
                rag.copilot_model.clone(),
                rag.timeout_secs,
            )?;
            let backend: Arc<dyn EmbeddingBackend> =
                Arc::new(GatedRemoteBackend::new(inner, rag.enabled, consent_dir));
            Ok((decision, Some(backend)))
        }
        BackendKind::OpenAiCompat => {
            let inner = joey_neurocode_rag::embed::openai_compat::OpenAiCompat::query(
                rag.base_url.clone(),
                rag.model.clone(),
                rag.api_key.clone(),
                rag.timeout_secs,
            )?;
            let backend: Arc<dyn EmbeddingBackend> =
                Arc::new(GatedRemoteBackend::new(inner, rag.enabled, consent_dir));
            Ok((decision, Some(backend)))
        }
        BackendKind::OllamaNative => {
            let inner = joey_neurocode_rag::embed::ollama::OllamaNative::query(
                rag.base_url.clone(),
                rag.model.clone(),
                rag.api_key.clone(),
                String::new(), // keep_alive: Ollama's own default on the wire
                rag.timeout_secs,
            )?;
            let backend: Arc<dyn EmbeddingBackend> =
                Arc::new(GatedRemoteBackend::new(inner, rag.enabled, consent_dir));
            Ok((decision, Some(backend)))
        }
    }
}

/// Resolve the DOCUMENT-side embedding backend for indexing — the same
/// decision ladder as [`resolve_query_backend`] but with document-kind
/// adapters (`embed::resolve`'s constructor choices, LocalOnnx cached).
pub(crate) fn resolve_documents_backend(
    rag: &RagConfig,
    store: Option<&joey_neurocode::graph::GraphStore>,
) -> Result<(ResolvedBackend, Option<Arc<dyn EmbeddingBackend>>), EmbedError> {
    let profile = effective_profile(rag, rag.backend);
    let profile_name = profile.name.to_string();
    let decision = embed::resolve_kind(rag.backend, &profile_name, &rag.model_dir)?;
    let consent_dir = consent_dir_for_cwd();
    match decision.kind {
        BackendKind::KeywordOnly => Ok((decision, None)),
        BackendKind::LocalOnnx => {
            let settings = LocalOnnxSettings::from_rag_config(rag);
            let loaded = load_local_onnx_cached(profile, &settings, store)?;
            let backend: Arc<dyn EmbeddingBackend> =
                Arc::new(LocalOnnxBackend::documents(loaded));
            Ok((decision, Some(backend)))
        }
        BackendKind::Copilot => {
            let inner = joey_neurocode_rag::embed::copilot::CopilotEmbeddings::documents(
                rag.api_key.clone(),
                rag.copilot_model.clone(),
                rag.timeout_secs,
            )?;
            let backend: Arc<dyn EmbeddingBackend> =
                Arc::new(GatedRemoteBackend::new(inner, rag.enabled, consent_dir));
            Ok((decision, Some(backend)))
        }
        BackendKind::OpenAiCompat => {
            let inner = joey_neurocode_rag::embed::openai_compat::OpenAiCompat::documents(
                rag.base_url.clone(),
                rag.model.clone(),
                rag.api_key.clone(),
                rag.timeout_secs,
            )?;
            let backend: Arc<dyn EmbeddingBackend> =
                Arc::new(GatedRemoteBackend::new(inner, rag.enabled, consent_dir));
            Ok((decision, Some(backend)))
        }
        BackendKind::OllamaNative => {
            let inner = joey_neurocode_rag::embed::ollama::OllamaNative::documents(
                rag.base_url.clone(),
                rag.model.clone(),
                rag.api_key.clone(),
                String::new(),
                rag.timeout_secs,
            )?;
            let backend: Arc<dyn EmbeddingBackend> =
                Arc::new(GatedRemoteBackend::new(inner, rag.enabled, consent_dir));
            Ok((decision, Some(backend)))
        }
    }
}

/// Run one blocking `EmbeddingBackend::embed` on a dedicated thread with its
/// own current-thread runtime — safe from sync AND async contexts (never
/// `Runtime::new` on the caller's thread; the same pattern the model-fetch
/// downloader uses). In-process LocalOnnx work internally lands on the
/// blocking pool; remote backends perform their HTTP call here.
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

// ---------------------------------------------------------------------------
// Shared hybrid-search execution (CLI `/neurocode search` + agent tool)
// ---------------------------------------------------------------------------

/// Execute one RAG search against the per-project store, wiring a real
/// embedding backend into the dense leg when one resolves. Degradation
/// mirrors the search-side semantics exactly: when resolution fails or
/// degrades (no artifacts, explicit-but-missing local_onnx, …) the call
/// falls back to `search_cli`'s keyword-only posture with its pinned
/// `mode_reason` — the turn NEVER hard-fails on a backend problem (FR-008).
pub(crate) fn execute_rag_search(
    rag: &RagConfig,
    project_root: &Path,
    request: &SearchRequest,
) -> Result<SearchOutcome, SearchError> {
    let graph = DependencyGraph::open_for_project(project_root)
        .map_err(|e| SearchError::Store(format!("cannot open the project graph: {e}")))?;
    let resolved = resolve_query_backend(rag, Some(graph.store()));
    let profile = effective_profile(rag, rag.backend);
    match resolved {
        // Real backend: embed the query leg (the pipeline pre-applies the
        // profile QUERY prefix; strip it and let the backend re-apply it).
        Ok((_, Some(backend))) => {
            let prefix = profile.prefix_query.to_string();
            search_cli_with_embedder(
                graph.store(),
                project_root,
                profile,
                request,
                move |texts: &[String]| {
                    let raw: Vec<String> = texts
                        .iter()
                        .map(|t| {
                            t.strip_prefix(prefix.as_str())
                                .map(str::to_string)
                                .unwrap_or_else(|| t.clone())
                        })
                        .collect();
                    block_on_embed(&backend, &raw).map_err(|e| e.to_string())
                },
            )
        }
        // Degraded (KeywordOnly) or resolution error: keyword-only leg with
        // the pinned degradation reason — identical to the pre-wiring
        // behavior (V9 semantics preserved).
        _ => search_cli(graph.store(), project_root, profile, request),
    }
}

// ---------------------------------------------------------------------------
// Index path: /neurocode index RAG half + the shared refresh core
// ---------------------------------------------------------------------------

/// What one production RAG index/refresh pass did + how it ran.
pub(crate) struct RagIndexReport {
    pub summary: RagRefreshSummary,
    /// "local_onnx" / "openai_compat" / "ollama" when a real backend served,
    /// or the degradation note when the keyword-only path ran.
    pub backend_line: String,
    /// Files deferred by the per-turn budgets (reported, never dropped).
    pub files_deferred: usize,
}

/// `ChunkEmbedder` adapter over an `EmbeddingBackend` (document kind): the
/// pipeline pre-applies the profile document prefix; strip it and let the
/// backend re-apply it (see the module-level prefix note).
struct BackendChunkEmbedder {
    backend: Arc<dyn EmbeddingBackend>,
    document_prefix: String,
}

impl ChunkEmbedder for BackendChunkEmbedder {
    fn embed_texts(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
        let raw: Vec<String> = texts
            .iter()
            .map(|t| {
                t.strip_prefix(self.document_prefix.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| t.clone())
            })
            .collect();
        block_on_embed(&self.backend, &raw).map_err(|e| e.to_string())
    }
}

/// The sidecar path for a project root (sibling of the per-project
/// `graph.db`) — matches `refresh_worker::fingerprint_sidecar_path`'s
/// derivation from the store's main-db path.
fn fingerprint_sidecar_for(project_root: &Path) -> PathBuf {
    joey_neurocode::graph::project_graph_db_path(project_root)
        .parent()
        .map(|p| p.join("refresh_fingerprints.json"))
        .unwrap_or_else(|| PathBuf::from("refresh_fingerprints.json"))
}

/// Quantization policy for new vectors from `neurocode.rag.quantize_threshold`
/// (approximated on the CURRENT chunk count — the post-refresh count is not
/// knowable before embedding; a cold index starts f32 and crosses to int8 on
/// the next refresh once the threshold is exceeded).
fn quantization_for(rag: &RagConfig, conn: &rusqlite::Connection) -> Quantization {
    let count = vector_store::chunk_count(conn).unwrap_or(0);
    if count >= rag.quantize_threshold.max(0) as u64 {
        Quantization::Int8
    } else {
        Quantization::F32
    }
}

/// Degraded (no embedder) index path: write vector-less chunk rows via the
/// store's supported "not yet embedded" shape so the keyword leg has real
/// rows (V2's no-model path). One `write_index` transaction: existing paths
/// purged + all records upserted + edges rewritten + meta updated. Only
/// runs when there is actually a change to write (callers gate on the
/// detected delta / force).
fn degraded_vectorless_rebuild(
    store: &joey_neurocode::graph::GraphStore,
    root: &Path,
    profile: &profiles::EmbedProfile,
) -> Result<(usize, usize), String> {
    // Purge every currently indexed path no longer on disk (stale rows must
    // not survive a rebuild); rows for live paths are replaced by their own
    // write_index batch below.
    let mut stmt = store
        .conn()
        .prepare("SELECT DISTINCT source_path FROM rag_chunks")
        .map_err(|e| e.to_string())?;
    let indexed: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default();
    drop(stmt);
    let gone: Vec<String> = indexed
        .into_iter()
        .filter(|p| !root.join(p).is_file())
        .collect();
    for path in &gone {
        store
            .conn()
            .execute("DELETE FROM rag_chunks WHERE source_path = ?1", rusqlite::params![path])
            .map_err(|e| e.to_string())?;
    }
    if !gone.is_empty() {
        store
            .conn()
            .execute(
                "DELETE FROM rag_chunk_edges WHERE from_chunk_id NOT IN \
                 (SELECT chunk_id FROM rag_chunks) OR to_chunk_id NOT IN \
                 (SELECT chunk_id FROM rag_chunks)",
                [],
            )
            .map_err(|e| e.to_string())?;
    }

    // Walk + parse + chunk every indexable file, writing each file's rows
    // in its own `write_index` transaction (one transaction per file bounds
    // memory on huge trees; each batch purges its own path).
    let mut files_written = 0usize;
    for fp in joey_neurocode_rag::index::incremental::snapshot_tree(
        root,
        &default_indexable_filter,
    ) {
        let rel = Path::new(&fp.source_path);
        let abs = root.join(rel);
        let Ok(source) = std::fs::read_to_string(&abs) else {
            continue;
        };
        let mut extraction = match joey_neurocode::parse::registry::parse_any(&abs, &source) {
            Some(Ok(ex)) => ex,
            _ => continue, // unsupported/unparseable — out of RAG scope
        };
        extraction.populate_fallback_chunks(&source);
        let file_records = joey_neurocode_rag::index::chunker::build_chunk_records(
            &extraction,
            &source,
            &fp.source_path,
            store,
            &ChunkOptions::default(),
        );
        let vectors = vec![None; file_records.len()];
        vector_store::write_index(
            store,
            profile,
            Quantization::F32,
            &file_records,
            &vectors,
            &[fp.source_path.as_str()],
        )
        .map_err(|e| e.to_string())?;
        files_written += 1;
    }
    let chunk_total =
        vector_store::chunk_count(store.conn()).map_err(|e| e.to_string())? as usize;
    Ok((files_written, chunk_total))
}

/// The effective embedding profile for `rag` under backend resolution:
/// Copilot embeddings serve `profile_for(copilot_model)` (1536d for
/// text-embedding-3-small) REGARDLESS of `neurocode.rag.model`, which only
/// names the local/openai/ollama models — using the raw `rag.model` profile
/// for a copilot-backed index compares 768d-vs-768d, skips the rebuild, and
/// dies mid-refresh with "vector dim mismatch" on the first 1536d vector.
/// Behind a pinned custom endpoint the effective model (and dim) follows the
/// proxy-aware resolution, matching what the backend actually embeds with.
pub(crate) fn effective_profile(
    rag: &RagConfig,
    backend: RagBackend,
) -> &'static profiles::EmbedProfile {
    if backend == RagBackend::Copilot {
        let model = joey_neurocode_rag::embed::copilot::effective_model(&rag.copilot_model);
        joey_neurocode_rag::embed::copilot::profile_for(&model)
    } else {
        profiles::lookup(&rag.model).unwrap_or_else(profiles::default_profile)
    }
}

/// A backend/profile switch (e.g. local_onnx 768d -> copilot 1536d) leaves
/// the on-disk index embedded under the OLD profile. Refreshing under the
/// new profile would either skip unchanged files (stale-dim vectors) or
/// fail the per-chunk dim check mid-transaction ("vector dim mismatch").
/// The index is a derived artifact — a profile change requires a full
/// re-embed, exactly like `--force`.
fn needs_backend_rebuild(
    meta: Option<&vector_store::RagIndexMeta>,
    profile: &profiles::EmbedProfile,
) -> bool {
    match meta {
        Some(m) => m.embed_profile != profile.name || m.embed_dim != profile.dim,
        // Missing meta with chunk rows present (pre-profile-pinning index):
        // dimensions unknowable ⇒ rebuild rather than risk stale-dim rows.
        None => true,
    }
}

/// One production RAG refresh over the project at `root`:
///
/// - real backend → the rag crate's atomic `run_refresh` (detect +
///   refresh_incremental, single transaction, chunks/vectors/edges/meta);
/// - degraded backend → vector-less chunk-row rebuild (only when the
///   detected delta is non-empty or `force`), mirroring the CLI's index
///   -time degradation semantics.
///
/// `force` resets the RAG index first (delete the fingerprint sidecar +
/// purge `rag_chunks`) so everything re-embeds. The structural graph is
/// untouched (force there is handled by the engine's own index path).
pub(crate) fn run_production_refresh(
    rag: &RagConfig,
    root: &Path,
    force: bool,
    progress: &dyn Fn(usize, usize),
) -> Result<RagIndexReport, String> {
    let graph = DependencyGraph::open_for_project(root)
        .map_err(|e| format!("cannot open the project graph: {e}"))?;
    let store = graph.store();
    // Effective profile for the configured backend (copilot ⇒ 1024d
    // metis-1024-I16-Binary via copilot_model, not the 768d rag.model
    // default) — the same resolution the embedder below will use.
    let profile = effective_profile(rag, rag.backend);

    let resolved = resolve_documents_backend(rag, Some(store));
    match resolved {
        Ok((decision, Some(backend))) => {
            let rebuild_for_switch = !force
                && needs_backend_rebuild(
                    vector_store::load_index_meta(store.conn()).ok().flatten().as_ref(),
                    profile,
                );
            if force || rebuild_for_switch {
                // Reset for a full re-embed: sidecar first (so detection
                // sees "no previous"), then the chunk rows (cascade removes
                // vectors; edges are fully derived and rewritten by the
                // refresh). Brief empty window before the refresh commits —
                // a user-invoked --force rebuild, not the hot path.
                // `rebuild_for_switch` reaches here when the embedding
                // profile changed since the index was written (e.g. a
                // local_onnx <-> copilot backend switch): stale-dim vectors
                // would fail the per-chunk dim check mid-refresh ("vector
                // dim mismatch"), so the whole derived index is rebuilt.
                let _ = std::fs::remove_file(fingerprint_sidecar_for(root));
                store
                    .conn()
                    .execute_batch("DELETE FROM rag_chunks; DELETE FROM rag_chunk_edges;")
                    .map_err(|e| format!("forcing RAG reset: {e}"))?;
            }
            let mut embedder = BackendChunkEmbedder {
                backend,
                document_prefix: profile.prefix_document.to_string(),
            };
            let options = RefreshWorkerOptions {
                detection: DetectionOptions::default(),
                budgets: RefreshBudgets::from_config(rag),
                chunk_options: ChunkOptions::default(),
                quantization: quantization_for(rag, store.conn()),
                rename_assist: true,
            };
            let report = run_refresh(store, root, &mut embedder, profile, &options)
                .map_err(|e| e.to_string())?;
            progress(1, 1);
            Ok(RagIndexReport {
                summary: RagRefreshSummary {
                    files_indexed: report.outcome.files_indexed,
                    files_reindexed: report.outcome.files_reindexed,
                    files_purged: report.outcome.files_purged,
                    files_renamed: report.outcome.files_renamed,
                    chunks_embedded: report.outcome.chunks_embedded,
                    chunks_skipped: report.outcome.chunks_skipped,
                },
                backend_line: if rebuild_for_switch {
                    format!(
                        "backend {} (index re-embedded: embedding profile changed, {}d)",
                        decision.kind.as_str(),
                        profile.dim
                    )
                } else {
                    format!("backend {}", decision.kind.as_str())
                },
                files_deferred: report.outcome.files_deferred,
            })
        }
        Ok((decision, None)) => {
            // Degraded: no embedder ⇒ the incremental pipeline cannot run
            // (it embeds changed chunks in-transaction). Write vector-less
            // chunk rows instead — only when something actually changed
            // (or force), so background triggers on untouched trees no-op.
            if !force {
                let previous = reconstruct_previous(store, root);
                let delta = detect_changes(
                    root,
                    &previous,
                    &default_indexable_filter,
                    &DetectionOptions::default(),
                );
                if delta.is_empty() {
                    return Ok(RagIndexReport {
                        summary: RagRefreshSummary::default(),
                        backend_line: format!(
                            "keyword-only (degraded: {})",
                            decision.degradation_reason
                        ),
                        files_deferred: 0,
                    });
                }
            } else {
                let _ = std::fs::remove_file(fingerprint_sidecar_for(root));
            }
            let (files, chunks) = degraded_vectorless_rebuild(store, root, profile)?;
            progress(1, 1);
            Ok(RagIndexReport {
                summary: RagRefreshSummary {
                    files_indexed: files,
                    chunks_embedded: 0, // vectors deliberately absent
                    ..RagRefreshSummary::default()
                },
                backend_line: format!(
                    "keyword-only (degraded: {}) — {} chunk rows written WITHOUT vectors",
                    decision.degradation_reason, chunks
                ),
                files_deferred: 0,
            })
        }
        Err(e) => Err(format!(
            "embedding backend resolution failed: {e} (no index was written)"
        )),
    }
}

/// The `/neurocode index` RAG text section: empty when RAG is disabled
/// (byte-identical output parity), otherwise the refresh outcome or the
/// honest failure/degradation note — never a turn failure.
pub(crate) fn rag_index_text(config: &joey_core::Config, project_root: &Path, force: bool) -> String {
    let rag = RagConfig::load(config);
    if !rag.enabled {
        return String::new();
    }
    match run_production_refresh(&rag, project_root, force, &|_, _| {}) {
        Ok(report) => {
            let s = &report.summary;
            let mut out = format!(
                "\nRAG: {} — {} file(s) indexed, {} re-indexed, {} purged, {} chunks embedded, {} skipped",
                report.backend_line,
                s.files_indexed,
                s.files_reindexed,
                s.files_purged,
                s.chunks_embedded,
                s.chunks_skipped,
            );
            if report.files_deferred > 0 {
                out.push_str(&format!(
                    " ({} file(s) deferred by refresh budgets — next refresh re-detects them)",
                    report.files_deferred
                ));
            }
            if report.backend_line.starts_with("keyword-only") {
                out.push_str(
                    "\n     keyword search is available via /neurocode search; place model \
                     artifacts (or /neurocode model fetch) and re-run /neurocode index --force \
                     to enable hybrid semantic search",
                );
            }
            out
        }
        Err(e) => format!("\nRAG: index failed: {e}"),
    }
}

// ---------------------------------------------------------------------------
// joey-agent-core injection traits over the rag crate (T025/T033 production)
// ---------------------------------------------------------------------------

/// Production `RagRefreshWorker`: one fire-and-forget background refresh of
/// the CWD project's RAG index through [`run_production_refresh`] (real
/// backend ⇒ the rag crate's atomic incremental refresh; degraded ⇒ the
/// vector-less chunk-row rebuild, gated on actual changes). Installed by
/// repl.rs/oneshot.rs when `neurocode.rag.enabled`.
struct ProductionRagRefreshWorker;

impl RagRefreshWorker for ProductionRagRefreshWorker {
    fn run_refresh(&self, progress: &dyn Fn(usize, usize)) -> Result<RagRefreshSummary, String> {
        let config =
            joey_core::Config::load().unwrap_or_else(|_| joey_core::Config::defaults());
        let rag = RagConfig::load(&config);
        if !rag.enabled {
            return Ok(RagRefreshSummary::default());
        }
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        run_production_refresh(&rag, &root, false, progress).map(|r| r.summary)
    }
}

/// Build the production refresh worker handle (installed on the agent).
pub(crate) fn production_rag_refresh_worker() -> Arc<dyn RagRefreshWorker> {
    Arc::new(ProductionRagRefreshWorker)
}

/// Production `RagPrefetchSource` (T033, FR-015): a small keyword-match
/// block for the current prompt. Deliberately LOCAL-ONLY by construction —
/// it never loads the embedder and never embeds (pure FTS/LIKE over the
/// local store), so no code egress is possible from this path regardless
/// of the configured backend; the agent-side gate additionally arms only
/// for hard-verified-local backends. `None` when nothing matches.
struct ProductionRagPrefetchSource;

impl RagPrefetchSource for ProductionRagPrefetchSource {
    fn prefetch_block(&self, user_prompt: &str) -> Option<String> {
        let config =
            joey_core::Config::load().unwrap_or_else(|_| joey_core::Config::defaults());
        let rag = RagConfig::load(&config);
        if !rag.enabled {
            return None;
        }
        // Prompt terms → query tokens: drop noise/short tokens so the AND
        // semantics of the keyword leg still surface symbol matches.
        let tokens: Vec<&str> = user_prompt
            .split_whitespace()
            .filter(|t| t.len() >= 4 && t.chars().all(|c| c.is_alphanumeric() || c == '_'))
            .take(4)
            .collect();
        if tokens.is_empty() {
            return None;
        }
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let graph = DependencyGraph::open_for_project(&root).ok()?;
        let profile = effective_profile(&rag, rag.backend);
        let request = SearchRequest {
            query: tokens.join(" "),
            file_filter: None,
            limit: 5,
            expand_lines: 0,
            relation_depth: 0,
            include_fallback_chunks: rag.include_fallback_chunks,
        };
        let outcome = search_cli(graph.store(), &root, profile, &request).ok()?;
        if outcome.results.is_empty() {
            return None;
        }
        let mut block = String::from("## NeuroCode RAG pre-fetch (keyword matches for the prompt)\n");
        for (i, r) in outcome.results.iter().take(5).enumerate() {
            let symbol = r.symbol.as_deref().unwrap_or("(top-level code)");
            let kind = r.symbol_kind.as_deref().unwrap_or("region");
            let badge = if r.chunk_kind.as_str() == "fallback" {
                "fallback-chunk"
            } else {
                "symbol-aligned"
            };
            let lines = if r.start_line == r.end_line {
                format!("L{}", r.start_line)
            } else {
                format!("L{}-{}", r.start_line, r.end_line)
            };
            block.push_str(&format!(
                "{}. {} [{}] {} ({}) {}\n",
                i + 1,
                r.file,
                badge,
                symbol,
                kind,
                lines
            ));
        }
        block.push_str("Call neurocode_search for ranked hybrid results and context expansion.");
        Some(block)
    }
}

/// Build the production pre-fetch source handle (installed on the agent).
pub(crate) fn production_rag_prefetch_source() -> Arc<dyn RagPrefetchSource> {
    Arc::new(ProductionRagPrefetchSource)
}

/// Install the production RAG worker + pre-fetch source on an agent when
/// `neurocode.rag.enabled` (the shared call for the repl/oneshot wiring
/// sites). Disabled (the default) ⇒ installs nothing, zero behavior change.
pub(crate) fn install_rag_injections(agent: &mut joey_agent_core::Agent, config: &joey_core::Config) {
    if !config.get_bool("neurocode.rag.enabled", false) {
        return;
    }
    agent.set_rag_refresh_worker(Some(production_rag_refresh_worker()));
    agent.set_rag_prefetch_source(Some(production_rag_prefetch_source()));
}

#[cfg(test)]
mod backend_selection_tests {
    use super::*;

    /// Serializes tests in this module that mutate the endpoint env vars
    /// (COPILOT_API_BASE_URL / AI_USAGE_HUD_BASE_URL) — same convention as
    /// llm_selector.rs / engine.rs ENV_LOCK and joey-providers'
    /// `copilot::TEST_ENV_LOCK`: rust runs `#[test]` fns as parallel
    /// threads in one process, so a sibling's set_var would otherwise land
    /// inside another test's scrubbed-env window.
    ///
    /// Cross-module serialization: distinct modules' ENV_LOCKs do NOT
    /// exclude each other, so this module's endpoint-env mutations are
    /// additionally serialized against every OTHER joey-cli test module
    /// that mutates the same vars by acquiring each mutating module's
    /// ENV_LOCK in a FIXED alphabetical-by-file-path order:
    /// `crate::engine::actor_tests::ENV_LOCK` (engine.rs), then
    /// `crate::llm_selector::tests::ENV_LOCK` (llm_selector.rs).
    /// (neurocode_wiring.rs's HudEnvGuard tests already serialize on
    /// joey-core's single cross-crate TEST_HOME_OVERRIDE_LOCK, which our
    /// tests never take, so no additional lock is needed for them.)
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn explicit_copilot_backend_resolves() {
        // Hermetic: this machine may export AI_USAGE_HUD_BASE_URL (proxy),
        // which flips the default Copilot model — detach from the ambient
        // env for the duration (save/remove/restore, same pattern as the
        // resolve_constructs_gated_copilot_backend test in embed/mod.rs).
        // ENV_LOCK is taken BEFORE the save so no sibling's concurrent
        // set_var can be captured or land inside the scrubbed window.
        // The sibling module locks follow in the fixed alphabetical order
        // documented on ENV_LOCK (engine.rs, then llm_selector.rs).
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard1 = crate::engine::actor_tests::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard2 = crate::llm_selector::tests::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = (
            std::env::var("COPILOT_API_BASE_URL").ok(),
            std::env::var("AI_USAGE_HUD_BASE_URL").ok(),
        );
        std::env::remove_var("COPILOT_API_BASE_URL");
        std::env::remove_var("AI_USAGE_HUD_BASE_URL");
        let mut rag = RagConfig::default();
        rag.backend = RagBackend::Copilot;
        let (decision, backend) = resolve_query_backend(&rag, None).unwrap();
        assert_eq!(decision.kind, BackendKind::Copilot);
        let info = backend.expect("backend constructed").describe_embedder();
        assert_eq!(info.backend_kind, BackendKind::Copilot);
        assert_eq!(info.model, "metis-1024-I16-Binary");
        assert_eq!(info.dim, 1024);
        if let Some(v) = saved.0 {
            std::env::set_var("COPILOT_API_BASE_URL", v);
        }
        if let Some(v) = saved.1 {
            std::env::set_var("AI_USAGE_HUD_BASE_URL", v);
        }
    }

    #[test]
    fn explicit_copilot_backend_ignores_llm_provider() {
        // The embedding backend is a pure function of `neurocode.rag.*`
        // keys — RagConfig no longer carries any provider field at all, so
        // an explicit `backend=copilot` works with ANY LLM provider
        // (e.g. z.ai) by construction. Both the query and the documents
        // adapters must resolve Copilot.
        let mut rag = RagConfig::default();
        rag.backend = RagBackend::Copilot;
        let (decision, _backend) = resolve_query_backend(&rag, None).unwrap();
        assert_eq!(decision.kind, BackendKind::Copilot);
        let (decision, _backend) = resolve_documents_backend(&rag, None).unwrap();
        assert_eq!(decision.kind, BackendKind::Copilot);
    }

    #[test]
    fn auto_never_implicitly_selects_copilot() {
        // `auto` resolves strictly through the local ladder — it can never
        // implicitly become Copilot. Point model_dir at a nonexistent temp
        // path so resolve_kind degrades to KeywordOnly deterministically on
        // any machine (mirrors the #[ignore]d default_stays_local_ladder,
        // without its machine-state dependence).
        let mut rag = RagConfig::default();
        rag.model_dir = std::env::temp_dir()
            .join(format!("joey-rag-test-no-artifacts-{}", std::process::id()));
        let (decision, backend) = resolve_query_backend(&rag, None).unwrap();
        assert_eq!(decision.kind, BackendKind::KeywordOnly);
        assert_ne!(decision.kind, BackendKind::Copilot);
        assert!(backend.is_none());
    }

    #[test]
    fn effective_profile_follows_copilot_model_not_rag_model() {
        // Hermetic: effective_profile is proxy-aware; pin the NO-proxy
        // endpoint mode for the duration (same save/remove/restore pattern
        // as resolve_constructs_gated_copilot_backend in embed/mod.rs).
        // ENV_LOCK taken BEFORE the save (see ENV_LOCK docs); the sibling
        // module locks follow in the fixed alphabetical order documented on
        // ENV_LOCK (engine.rs, then llm_selector.rs).
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard1 = crate::engine::actor_tests::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard2 = crate::llm_selector::tests::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = (
            std::env::var("COPILOT_API_BASE_URL").ok(),
            std::env::var("AI_USAGE_HUD_BASE_URL").ok(),
        );
        std::env::remove_var("COPILOT_API_BASE_URL");
        std::env::remove_var("AI_USAGE_HUD_BASE_URL");
        let rag = RagConfig::default();
        let profile = effective_profile(&rag, RagBackend::Copilot);
        assert_eq!(profile.name, "metis-1024-I16-Binary");
        assert_eq!(profile.dim, 1024);
        if let Some(v) = saved.0 {
            std::env::set_var("COPILOT_API_BASE_URL", v);
        }
        if let Some(v) = saved.1 {
            std::env::set_var("AI_USAGE_HUD_BASE_URL", v);
        }
    }

    #[test]
    fn needs_backend_rebuild_true_when_meta_missing() {
        assert!(needs_backend_rebuild(None, profiles::default_profile()));
    }

    #[test]
    fn needs_backend_rebuild_false_on_matching_profile() {
        let p = profiles::default_profile();
        let meta = rag_index_meta_for(p.name, p.dim);
        assert!(!needs_backend_rebuild(Some(&meta), p));
    }

    #[test]
    fn needs_backend_rebuild_true_on_dim_change() {
        // nomic (768d) index meta vs a 1536d profile (explicit
        // text-embedding-3-small copilot model, not the metis default)
        let meta = rag_index_meta_for(profiles::default_profile().name, 1536);
        let copilot = joey_neurocode_rag::embed::copilot::profile_for("text-embedding-3-small");
        assert_ne!(profiles::default_profile().dim, copilot.dim);
        assert!(needs_backend_rebuild(Some(&meta), copilot));
    }

    #[test]
    fn needs_backend_rebuild_true_on_profile_name_change() {
        let meta = rag_index_meta_for("text-embedding-3-small", profiles::default_profile().dim);
        assert!(needs_backend_rebuild(
            Some(&meta),
            profiles::default_profile()
        ));
    }

    /// Minimal `RagIndexMeta` with the identity fields that matter here.
    fn rag_index_meta_for(embed_profile: &str, embed_dim: u32) -> vector_store::RagIndexMeta {
        vector_store::RagIndexMeta {
            schema_version: 3,
            embed_profile: embed_profile.to_string(),
            embed_model: String::new(),
            embed_dim,
            pooling: String::new(),
            prefix_query: String::new(),
            prefix_document: String::new(),
            quantization_policy: String::new(),
            chunk_count: 0,
            last_refresh_at: None,
            refresh_state: String::new(),
            created_at: String::new(),
        }
    }

    #[test]
    #[ignore = "machine-state-dependent: resolves LocalOnnx (not KeywordOnly) when real model artifacts exist under the active JOEY_HOME; run explicitly with a clean JOEY_HOME"]
    fn default_stays_local_ladder() {
        let rag = RagConfig::default();
        let (decision, backend) = resolve_query_backend(&rag, None).unwrap();
        assert_eq!(decision.kind, BackendKind::KeywordOnly);
        assert!(backend.is_none());
    }
}

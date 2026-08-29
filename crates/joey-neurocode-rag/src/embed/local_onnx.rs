//! LocalOnnx embedder — the PRIMARY, fully-local in-process backend.
//!
//! contracts/embedding-backend.md §1 "LocalOnnx" (research.md R2/R6):
//!
//! - Embedding model runs **in-process** via `ort` `=2.0.0-rc.13`
//!   (`load-dynamic`): the ONNX Runtime dylib is located at RUNTIME through
//!   the ladder `neurocode.rag.local.ort_dylib_path` → `ORT_DYLIB_PATH` env
//!   → system lookup, and is **never embedded in the binary** (R6).
//! - Tokenization is fully **OFFLINE** (`Tokenizer::from_file`, padded
//!   `encode_batch(texts, true)`); this module NEVER contacts the network
//!   and NEVER downloads anything — no `hf-hub`, no auto-fetch, ever (R8).
//! - Artifacts in `neurocode.rag.local.model_dir` pass the
//!   [`artifacts`](super::artifacts) integrity gate BEFORE any ort work
//!   (self-registration on manual placement; refuse on SHA-256 mismatch).
//! - Pooling/normalization/prefixes come from the model PROFILE
//!   ([`EmbedProfile`]), never hardcoded here: prefixes are applied BEFORE
//!   tokenization (profile rule 1), pooling is mean-over-attention-mask +
//!   client-side L2 (profile rule 2).
//! - Batching: `neurocode.rag.batch_size` (validated 16–128 at the config
//!   layer; defensively re-clamped here), padded `encode_batch` per chunk,
//!   rayon across texts for pooling; results are order-preserving
//!   (contract § Batching: response position `i` ↔ input `batch[i]`).
//!
//! T010 lifts [`LocalOnnxError`] into the `EmbedError` taxonomy and wraps
//! this in the `EmbeddingBackend` trait (`EmbedError::ModelFilesMissing` /
//! `ModelFilesCorrupt` / `DimensionMismatch` map 1:1 onto variants below).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use joey_neurocode::graph::GraphStore;
use ndarray::{Axis, Ix3};
use rayon::prelude::*;
use tokenizers::{PaddingParams, Tokenizer, TruncationParams};

use crate::config::{BATCH_SIZE_MAX, BATCH_SIZE_MIN, DEFAULT_BATCH_SIZE, RagConfig};
use crate::embed::artifacts::{
    compute_hashes, verify_or_register, ArtifactError, MODEL_FILE, TOKENIZER_FILE,
};
use crate::embed::profiles::EmbedProfile;

/// Environment variable `ort` consults (lazy `load-dynamic`) when no explicit
/// dylib is configured — the second rung of the ladder.
pub const ENV_ORT_DYLIB_PATH: &str = "ORT_DYLIB_PATH";

/// Model input names this backend feeds (contract §1 pins the BERT-style
/// two-input surface; a graph requiring anything else is refused at load).
const INPUT_IDS: &str = "input_ids";
const ATTENTION_MASK: &str = "attention_mask";

// ---------------------------------------------------------------------------
// Error taxonomy
// ---------------------------------------------------------------------------

/// Errors of the LocalOnnx backend, structured so T010 can lift each variant
/// into `EmbedError` verbatim (contracts/embedding-backend.md § Error
/// taxonomy).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalOnnxError {
    /// Artifact-integrity failure at the load-time gate — missing files lift
    /// to `EmbedError::ModelFilesMissing`, SHA-256 mismatch to
    /// `ModelFilesCorrupt`. Returned BEFORE any dylib/ort work, so a clean
    /// "no model present" state is indistinguishable from an ort failure.
    Artifacts(ArtifactError),
    /// The configured ONNX Runtime dylib could not be loaded
    /// (`ort::init_from(..).commit()` failed) — dylib load failure.
    DylibLoad(String),
    /// `tokenizer.json` missing/unloadable, or padding/truncation setup
    /// rejected — tokenizer load failure.
    TokenizerLoad(String),
    /// `model.onnx` unloadable by ort, or the graph's input surface is not
    /// `{input_ids, attention_mask}` — session load failure.
    SessionLoad(String),
    /// A batch inference failed at run time (tokenize/tensorize/run/extract).
    Inference(String),
    /// The model's hidden dimension != `profile.dim` — lifts to
    /// `EmbedError::DimensionMismatch` (profile identity is load-bearing).
    DimensionMismatch(String),
}

impl std::fmt::Display for LocalOnnxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocalOnnxError::Artifacts(e) => write!(f, "local-onnx artifacts: {}", e),
            LocalOnnxError::DylibLoad(m) => write!(f, "local-onnx dylib load: {}", m),
            LocalOnnxError::TokenizerLoad(m) => write!(f, "local-onnx tokenizer load: {}", m),
            LocalOnnxError::SessionLoad(m) => write!(f, "local-onnx session load: {}", m),
            LocalOnnxError::Inference(m) => write!(f, "local-onnx inference: {}", m),
            LocalOnnxError::DimensionMismatch(m) => write!(f, "local-onnx dimension mismatch: {}", m),
        }
    }
}

impl std::error::Error for LocalOnnxError {}

// ---------------------------------------------------------------------------
// Settings & dylib ladder
// ---------------------------------------------------------------------------

/// Constructor inputs for [`LocalOnnx::load`], sourced from
/// [`RagConfig`] (see [`LocalOnnxSettings::from_rag_config`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalOnnxSettings {
    /// `neurocode.rag.local.model_dir` (`~`-expanded by the config layer).
    pub model_dir: PathBuf,
    /// `neurocode.rag.local.ort_dylib_path` — empty string = unset (env /
    /// system rungs take over).
    pub ort_dylib_path: String,
    /// `neurocode.rag.batch_size` (validated 16–128 upstream; defensively
    /// re-checked by [`clamp_batch_size`]).
    pub batch_size: i64,
}

impl LocalOnnxSettings {
    /// Project the RAG config keys this backend consumes.
    pub fn from_rag_config(cfg: &RagConfig) -> Self {
        Self {
            model_dir: cfg.model_dir.clone(),
            ort_dylib_path: cfg.ort_dylib_path.clone(),
            batch_size: cfg.batch_size,
        }
    }
}

/// Defensive batch-size fallback mirroring the config-layer rule
/// (out-of-range → default 64, bounds 16–128). Never trusts the caller.
pub fn clamp_batch_size(v: i64) -> usize {
    if (BATCH_SIZE_MIN..=BATCH_SIZE_MAX).contains(&v) {
        v as usize
    } else {
        DEFAULT_BATCH_SIZE as usize
    }
}

/// Which rung of the dylib-resolution ladder won (contract §1: "resolved
/// through `neurocode.rag.local.ort_dylib_path` → env → system lookup").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DylibSource<'a> {
    /// Explicit path from `neurocode.rag.local.ort_dylib_path` — fed to
    /// `ort::init_from` eagerly so a bad path fails cleanly at load time.
    Config(&'a Path),
    /// `ORT_DYLIB_PATH` env var — ort's own lazy `load-dynamic` loading
    /// reads it; we do nothing and let ort resolve it.
    Env,
    /// Neither configured — ort falls back to the system library lookup.
    System,
}

/// Resolve the ONNX Runtime dylib ladder:
/// config (`neurocode.rag.local.ort_dylib_path`) → env (`ORT_DYLIB_PATH`) →
/// system. Blank/whitespace strings count as unset. Pure — testable without
/// touching process environment state.
pub fn resolve_dylib<'a>(configured: Option<&'a str>, env: Option<&str>) -> DylibSource<'a> {
    if let Some(p) = configured.filter(|p| !p.trim().is_empty()) {
        return DylibSource::Config(Path::new(p));
    }
    if env.is_some_and(|e| !e.trim().is_empty()) {
        return DylibSource::Env;
    }
    DylibSource::System
}

/// Ensure ort's global environment is initialized from `configured` exactly
/// once per process (the ORT environment is process-global; a second
/// `init_from` would fail spuriously on later constructions, so subsequent
/// calls are no-ops). The committed `Environment` is intentionally leaked —
/// the dylib must outlive every session for the process lifetime.
fn ensure_ort(configured: Option<&Path>) -> Result<(), LocalOnnxError> {
    static ORT_INITIALIZED: OnceLock<()> = OnceLock::new();
    if ORT_INITIALIZED.get().is_some() {
        return Ok(());
    }
    if let Some(p) = configured {
        // init_from loads the dylib (failure -> Err); commit() installs the
        // process-global config and returns false when ort is already
        // configured — not an error (idempotent re-load across sessions).
        let builder = ort::init_from(p).map_err(|e| {
            LocalOnnxError::DylibLoad(format!("ort::init_from({}): {}", p.display(), e))
        })?;
        if !builder.commit() {
            // Already configured (e.g. a prior LocalOnnx in-process); the
            // previously committed dylib stays authoritative.
        }
        let _ = ORT_INITIALIZED.set(());
    }
    // None => Env/System rung: ort lazy-loads ORT_DYLIB_PATH or the system
    // library; a failure there surfaces at session creation (SessionLoad).
    Ok(())
}

// ---------------------------------------------------------------------------
// Pooling (profile rule 2: mean over attention mask + client-side L2)
// ---------------------------------------------------------------------------

/// Mean-pool `[seq, dim]` hidden states over the attention mask, then
/// client-side L2-normalize when `l2_normalize` (both accepted profiles use
/// mean pooling + L2 — the backend never improvises; Pooling::Mean only).
///
/// Positions with a zero attention mask are EXCLUDED from both the sum and
/// the divisor (padding must not dilute the mean). A fully-masked sequence
/// or zero vector yields zeros (never NaN).
pub fn mean_pool_l2(hidden: ndarray::ArrayView2<'_, f32>, attention_mask: &[i64], l2_normalize: bool) -> Vec<f32> {
    let (seq, dim) = (hidden.nrows(), hidden.ncols());
    let mut acc = vec![0.0f32; dim];
    let mut count = 0usize;
    for i in 0..seq {
        let on = attention_mask.get(i).copied().unwrap_or(0) != 0;
        if !on {
            continue;
        }
        count += 1;
        for (j, a) in acc.iter_mut().enumerate() {
            *a += hidden[[i, j]];
        }
    }
    if count > 0 {
        let n = count as f32;
        for a in &mut acc {
            *a /= n;
        }
    }
    if l2_normalize {
        let norm = acc.iter().map(|v| v * v).sum::<f32>().sqrt();
        if norm > 0.0 {
            for a in &mut acc {
                *a /= norm;
            }
        }
    }
    acc
}

// ---------------------------------------------------------------------------
// Prefixes (profile rule 1: applied BEFORE tokenization, raw text in)
// ---------------------------------------------------------------------------

/// What kind of input a text is — selects the profile prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Query,
    Document,
}

/// Map raw texts to the exact strings the tokenizer must consume:
/// `profile.query_input` / `profile.document_input` (prefix ++ raw, verbatim
/// — no trimming, no case folding). Callers pass RAW text; the prefix lives
/// here so it can never be forgotten by a caller.
fn prefixed_inputs(profile: &EmbedProfile, texts: &[String], kind: InputKind) -> Vec<String> {
    texts
        .iter()
        .map(|t| match kind {
            InputKind::Query => profile.query_input(t),
            InputKind::Document => profile.document_input(t),
        })
        .collect()
}

/// Order-preserving split of `n` items into batch index ranges of at most
/// `batch_size` (defensive: a zero/nonsense size collapses to one chunk —
/// callers normally pass [`clamp_batch_size`] output).
pub fn batch_ranges(n: usize, batch_size: usize) -> Vec<(usize, usize)> {
    if n == 0 {
        return Vec::new();
    }
    // Zero/nonsense size → one chunk covering everything; saturating_add
    // keeps a huge batch_size overflow-safe.
    let bs = if batch_size == 0 { n } else { batch_size };
    let mut out = Vec::new();
    let mut start = 0;
    while start < n {
        let end = n.min(start.saturating_add(bs));
        out.push((start, end));
        start = end;
    }
    out
}

// ---------------------------------------------------------------------------
// The backend
// ---------------------------------------------------------------------------

/// Fully-local, in-process ONNX embedder (PRIMARY backend). No daemon, no
/// socket, no network — local by construction, consent-free by contract.
pub struct LocalOnnx {
    profile: &'static EmbedProfile,
    model_dir: PathBuf,
    batch_size: usize,
    tokenizer: Tokenizer,
    /// ort `run()` needs `&mut self`; `Session` is `Send + Sync`, so a mutex
    /// shares it across calls (embedding remains serialized per session,
    /// pooling parallelizes via rayon instead).
    session: Mutex<ort::session::Session>,
    /// First output name captured from the loaded graph (works for
    /// `last_hidden_state` and any other single-output export).
    output_name: String,
}

impl std::fmt::Debug for LocalOnnx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalOnnx")
            .field("profile", &self.profile.name)
            .field("model_dir", &self.model_dir)
            .field("batch_size", &self.batch_size)
            .field("output_name", &self.output_name)
            .finish_non_exhaustive()
    }
}

impl LocalOnnx {
    /// Load and verify everything, failing FAST and CLEAN in order:
    ///
    /// 1. Artifact gate ([`verify_or_register`] with `store`, or hash-only
    ///    [`compute_hashes`] when no store is at hand) — missing/tampered
    ///    files never reach ort.
    /// 2. Dylib ladder — an explicit config path is committed via
    ///    `ort::init_from` so a bad dylib is a clean [`LocalOnnxError::DylibLoad`];
    ///    env/system rungs defer to ort's lazy loading.
    /// 3. Offline tokenizer from `tokenizer.json` (padding to batch-longest
    ///    with the tokenizer's own pad/unk id; truncation to profile ctx).
    /// 4. ONNX session from `model.onnx`, input-surface-checked against
    ///    `{input_ids, attention_mask}`.
    pub fn load(
        profile: &'static EmbedProfile,
        settings: &LocalOnnxSettings,
        store: Option<&GraphStore>,
    ) -> Result<Self, LocalOnnxError> {
        // 1. Artifact gate — BEFORE any ort/dylib work.
        match store {
            Some(s) => {
                verify_or_register(s, profile.name, &settings.model_dir)
                    .map_err(LocalOnnxError::Artifacts)?;
            }
            None => {
                compute_hashes(&settings.model_dir).map_err(LocalOnnxError::Artifacts)?;
            }
        }

        // 2. Dylib ladder: config → env → system.
        let env_dylib = std::env::var(ENV_ORT_DYLIB_PATH).ok();
        match resolve_dylib(Some(settings.ort_dylib_path.as_str()), env_dylib.as_deref()) {
            DylibSource::Config(p) => ensure_ort(Some(p))?,
            DylibSource::Env | DylibSource::System => ensure_ort(None)?,
        }

        // 3. Offline tokenizer.
        let tokenizer = Self::load_tokenizer(profile, &settings.model_dir)?;

        // 4. ONNX session.
        let model_path = settings.model_dir.join(MODEL_FILE);
        let session = ort::session::Session::builder()
            .and_then(|mut b| b.commit_from_file(&model_path))
            .map_err(|e| {
                LocalOnnxError::SessionLoad(format!("{}: {}", model_path.display(), e))
            })?;
        for input in session.inputs() {
            if input.name() != INPUT_IDS && input.name() != ATTENTION_MASK {
                return Err(LocalOnnxError::SessionLoad(format!(
                    "graph input {:?} not supported (expected {{{}, {}}})",
                    input.name(), INPUT_IDS, ATTENTION_MASK
                )));
            }
        }
        let output_name = session
            .outputs()
            .first()
            .map(|o| o.name().to_string())
            .ok_or_else(|| LocalOnnxError::SessionLoad("graph has no outputs".into()))?;

        Ok(Self {
            profile,
            model_dir: settings.model_dir.clone(),
            batch_size: clamp_batch_size(settings.batch_size),
            tokenizer,
            session: Mutex::new(session),
            output_name,
        })
    }

    /// Offline tokenizer from `<model_dir>/tokenizer.json`, padded to
    /// batch-longest with the tokenizer's OWN pad id (falling back to unk,
    /// then 0) and truncated to the profile context window.
    fn load_tokenizer(profile: &'static EmbedProfile, model_dir: &Path) -> Result<Tokenizer, LocalOnnxError> {
        let path = model_dir.join(TOKENIZER_FILE);
        let mut tokenizer = Tokenizer::from_file(&path)
            .map_err(|e| LocalOnnxError::TokenizerLoad(format!("{}: {}", path.display(), e)))?;
        let pad_id = ["<pad>", "[PAD]", "<unk>", "[UNK]"]
            .iter()
            .find_map(|t| tokenizer.token_to_id(t))
            .unwrap_or(0);
        // with_padding is infallible in-place (&mut self); with_truncation
        // can reject invalid params (infallible here: ctx > 0 per profile).
        tokenizer.with_padding(Some(PaddingParams {
            pad_id,
            ..Default::default() // BatchLongest / right — contract §1
        }));
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: profile.ctx as usize,
                ..Default::default()
            }))
            .map_err(|e| LocalOnnxError::TokenizerLoad(format!("set truncation: {}", e)))?;
        Ok(tokenizer)
    }

    /// The profile this embedder is pinned to (identity: name + dim +
    /// pooling — persisted by T010 in `rag_index_meta`).
    pub fn profile(&self) -> &'static EmbedProfile {
        self.profile
    }

    /// Effective batch size (already clamped 16–128 / defaulted to 64).
    pub fn batch_size(&self) -> usize {
        self.batch_size
    }

    /// Embed raw DOCUMENT/CHUNK texts (profile document prefix applied
    /// pre-tokenization). Order-preserving: result `i` ↔ `texts[i]`.
    pub fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LocalOnnxError> {
        self.embed_texts(texts, InputKind::Document)
    }

    /// Embed raw QUERY texts (profile query prefix applied pre-tokenization).
    /// Order-preserving: result `i` ↔ `texts[i]`.
    pub fn embed_queries(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LocalOnnxError> {
        self.embed_texts(texts, InputKind::Query)
    }

    /// Embed raw texts of either kind, splitting into config-sized batches
    /// (contract § Batching). Empty input → empty output, no session touch.
    pub fn embed_texts(&self, texts: &[String], kind: InputKind) -> Result<Vec<Vec<f32>>, LocalOnnxError> {
        let mut out = Vec::with_capacity(texts.len());
        for (start, end) in batch_ranges(texts.len(), self.batch_size) {
            let prefixed = prefixed_inputs(self.profile, &texts[start..end], kind);
            out.extend(self.infer_batch(&prefixed)?);
        }
        Ok(out)
    }

    /// One padded `encode_batch` + session run + per-text pooling (rayon
    /// across the batch's texts). Order-preserving.
    fn infer_batch(&self, prefixed: &[String]) -> Result<Vec<Vec<f32>>, LocalOnnxError> {
        let encodings = self
            .tokenizer
            .encode_batch(prefixed.to_vec(), true)
            .map_err(|e| LocalOnnxError::Inference(format!("encode_batch: {}", e)))?;
        if encodings.is_empty() {
            return Ok(Vec::new());
        }
        let seq = encodings[0].get_ids().len();
        let b = encodings.len();
        let mut ids = Vec::with_capacity(b * seq);
        let mut mask = Vec::with_capacity(b * seq);
        for e in &encodings {
            if e.get_ids().len() != seq {
                return Err(LocalOnnxError::Inference(
                    "encode_batch returned unequal padded lengths".into(),
                ));
            }
            ids.extend(e.get_ids().iter().map(|&v| v as i64));
            mask.extend(e.get_attention_mask().iter().map(|&v| v as i64));
        }

        let ids_tensor = ort::value::Tensor::from_array(([b as i64, seq as i64], ids))
            .map_err(|e| LocalOnnxError::Inference(format!("input_ids tensor: {}", e)))?;
        let mask_tensor = ort::value::Tensor::from_array(([b as i64, seq as i64], mask.clone()))
            .map_err(|e| LocalOnnxError::Inference(format!("attention_mask tensor: {}", e)))?;

        // Session outputs borrow the (mutex-guarded) session; extract and
        // pool everything before releasing the guard.
        let pooled: Vec<Vec<f32>> = {
            let mut session = self.session.lock().unwrap_or_else(|p| p.into_inner());
            let outputs = session
                .run(ort::inputs![
                    INPUT_IDS => ids_tensor,
                    ATTENTION_MASK => mask_tensor,
                ])
                .map_err(|e| LocalOnnxError::Inference(format!("session run: {}", e)))?;
            let hidden = outputs[self.output_name.as_str()]
                .try_extract_array::<f32>()
                .map_err(|e| {
                    LocalOnnxError::Inference(format!("extract {}: {}", self.output_name, e))
                })?
                .into_dimensionality::<Ix3>()
                .map_err(|e| {
                    LocalOnnxError::Inference(format!("hidden states not [batch, seq, dim]: {}", e))
                })?;
            let (hb, hseq, hdim) = hidden.dim();
            if hb != b || hseq != seq {
                return Err(LocalOnnxError::Inference(format!(
                    "hidden shape [{}, {}, {}] != expected batch/seq [{}, {}]",
                    hb, hseq, hdim, b, seq
                )));
            }
            if hdim != self.profile.dim as usize {
                return Err(LocalOnnxError::DimensionMismatch(format!(
                    "model dim {} != profile {} dim {}",
                    hdim, self.profile.name, self.profile.dim
                )));
            }
            (0..b)
                .into_par_iter()
                .map(|i| {
                    let rows = hidden.index_axis(Axis(0), i);
                    let row_mask = &mask[i * seq..(i + 1) * seq];
                    mean_pool_l2(rows, row_mask, self.profile.l2_normalize)
                })
                .collect()
        };
        Ok(pooled)
    }
}

// ---------------------------------------------------------------------------
// Tests (inline per task assignment — no real ONNX binary or dylib needed;
// every ort-touching success path is unreachable without artifacts, so these
// pin the pure math, the prefix wiring, the split logic, the dylib ladder,
// and the clean load-time failure modes).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::profiles::{CODERANK_EMBED, NOMIC_EMBED_TEXT_V1_5};
    use ndarray::Array2;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    // ── Mean pooling + L2 (hand-computed) ──────────────────────────────────

    /// [[1,0],[0,1],[3,4]] with mask [1,0,1]: row 1 is attention-masked OUT.
    /// Mean over rows 0,2 = [(1+3)/2, (0+4)/2] = [2,2]; L2 → [1/√2, 1/√2].
    #[test]
    fn mean_pool_excludes_masked_positions_then_l2() {
        let hidden = Array2::from_shape_vec((3, 2), vec![1.0, 0.0, 0.0, 1.0, 3.0, 4.0]).unwrap();
        let v = mean_pool_l2(hidden.view(), &[1, 0, 1], true);
        assert_eq!(v.len(), 2);
        assert!(approx(v[0], std::f32::consts::FRAC_1_SQRT_2), "v0={}", v[0]);
        assert!(approx(v[1], std::f32::consts::FRAC_1_SQRT_2), "v1={}", v[1]);
    }

    /// Classic 3-4-5 triple: [[3,4]], mask [1], L2 → [0.6, 0.8].
    #[test]
    fn mean_pool_full_mask_classic_l2() {
        let hidden = Array2::from_shape_vec((1, 2), vec![3.0, 4.0]).unwrap();
        let v = mean_pool_l2(hidden.view(), &[1], true);
        assert!(approx(v[0], 0.6) && approx(v[1], 0.8), "{:?}", v);
    }

    /// Without L2 the raw mean survives: [[2,0],[0,2]] mask [1,1] → [1,1].
    #[test]
    fn mean_pool_without_l2_returns_raw_mean() {
        let hidden = Array2::from_shape_vec((2, 2), vec![2.0, 0.0, 0.0, 2.0]).unwrap();
        let v = mean_pool_l2(hidden.view(), &[1, 1], false);
        assert!(approx(v[0], 1.0) && approx(v[1], 1.0), "{:?}", v);
    }

    /// Zero / fully-masked inputs yield zeros, never NaN.
    #[test]
    fn mean_pool_zero_and_empty_mask_are_safe() {
        let zeros = Array2::zeros((2, 3));
        assert!(mean_pool_l2(zeros.view(), &[1, 1], true).iter().all(|&v| v == 0.0));
        let ones = Array2::from_shape_vec((2, 2), vec![1.0; 4]).unwrap();
        assert!(mean_pool_l2(ones.view(), &[0, 0], true).iter().all(|&v| v == 0.0));
    }

    // ── Prefix application (profile rule 1, pre-tokenization) ──────────────

    #[test]
    fn prefixes_applied_pre_tokenization_per_profile() {
        // Nomic: both prefixes.
        let q = prefixed_inputs(&NOMIC_EMBED_TEXT_V1_5, &["token validation".into()], InputKind::Query);
        assert_eq!(q, vec!["search_query: token validation".to_string()]);
        let d = prefixed_inputs(&NOMIC_EMBED_TEXT_V1_5, &["fn main()".into()], InputKind::Document);
        assert_eq!(d, vec!["search_document: fn main()".to_string()]);

        // CodeRankEmbed: query-only prefix; EMPTY document prefix is
        // load-bearing — documents pass through verbatim.
        let q = prefixed_inputs(&CODERANK_EMBED, &["parse the config".into()], InputKind::Query);
        assert_eq!(
            q,
            vec!["Represent this query for searching relevant code: parse the config".to_string()]
        );
        let d = prefixed_inputs(&CODERANK_EMBED, &["raw chunk".into()], InputKind::Document);
        assert_eq!(d, vec!["raw chunk".to_string()]);

        // Raw text is verbatim: no trimming.
        let d = prefixed_inputs(&NOMIC_EMBED_TEXT_V1_5, &["  spaced  ".into()], InputKind::Document);
        assert_eq!(d, vec!["search_document:   spaced  ".to_string()]);
    }

    // ── Batching split logic ────────────────────────────────────────────────

    #[test]
    fn batch_ranges_splits_and_preserves_order() {
        assert_eq!(batch_ranges(0, 64), vec![]);
        assert_eq!(batch_ranges(5, 64), vec![(0, 5)]);
        assert_eq!(batch_ranges(64, 64), vec![(0, 64)]);
        assert_eq!(batch_ranges(100, 64), vec![(0, 64), (64, 100)]);
        assert_eq!(batch_ranges(128, 64), vec![(0, 64), (64, 128), (128, 128)][..2]);
        // Defensive: zero size collapses to one chunk, never loops forever.
        assert_eq!(batch_ranges(3, 0), vec![(0, 3)]);
        // Ranges tile [0, n) contiguously in order (order-preservation base).
        for &(n, bs) in &[(0usize, 16usize), (1, 16), (17, 16), (200, 128)] {
            let rs = batch_ranges(n, bs);
            let mut pos = 0;
            for &(s, e) in &rs {
                assert_eq!(s, pos);
                assert!(e > s && e - s <= bs);
                pos = e;
            }
            assert_eq!(pos, n);
        }
    }

    #[test]
    fn clamp_batch_size_mirrors_config_rule() {
        assert_eq!(clamp_batch_size(16), 16);
        assert_eq!(clamp_batch_size(64), 64);
        assert_eq!(clamp_batch_size(128), 128);
        for bad in [0i64, -3, 15, 129, 10_000] {
            assert_eq!(clamp_batch_size(bad), 64, "expected default fallback for {}", bad);
        }
    }

    // ── Dylib ladder (contract §1: config → env → system) ──────────────────

    #[test]
    fn dylib_ladder_config_env_system() {
        use DylibSource::{Config, Env, System};
        assert_eq!(resolve_dylib(Some("/opt/ort/lib.so"), Some("/env/lib.so")), Config(Path::new("/opt/ort/lib.so")));
        assert_eq!(resolve_dylib(None, Some("/env/lib.so")), Env);
        assert_eq!(resolve_dylib(None, Some("  ")), System); // blank env = unset
        assert_eq!(resolve_dylib(None, None), System);
        // Blank config string = unset → env rung wins.
        assert_eq!(resolve_dylib(Some(""), Some("/env/lib.so")), Env);
        assert_eq!(resolve_dylib(Some("   "), None), System);
    }

    // ── Load-time gating (clean errors, no ONNX binary required) ───────────

    #[test]
    fn load_fails_cleanly_when_artifacts_missing() {
        let store = GraphStore::open_in_memory().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let settings = LocalOnnxSettings {
            model_dir: tmp.path().to_path_buf(),
            ort_dylib_path: String::new(),
            batch_size: 64,
        };
        // With the artifact-integrity store.
        let err = LocalOnnx::load(&NOMIC_EMBED_TEXT_V1_5, &settings, Some(&store)).unwrap_err();
        assert!(
            matches!(&err, LocalOnnxError::Artifacts(ArtifactError::ModelFilesMissing(_))),
            "wrong error: {:?}",
            err
        );
        // And hash-only (no store) — still a clean missing-artifacts error
        // BEFORE any dylib/ort work.
        let err = LocalOnnx::load(&NOMIC_EMBED_TEXT_V1_5, &settings, None).unwrap_err();
        assert!(matches!(&err, LocalOnnxError::Artifacts(ArtifactError::ModelFilesMissing(_))));
    }

    /// Garbage tokenizer.json passes the hash gate (bytes are hashable),
    /// then fails deterministically at TokenizerLoad — before any dylib or
    /// session work, so the test needs no ONNX Runtime.
    #[test]
    fn load_reports_tokenizer_failure_before_dylib_or_session() {
        let store = GraphStore::open_in_memory().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(MODEL_FILE), b"fake-model-bytes").unwrap();
        std::fs::write(tmp.path().join(TOKENIZER_FILE), b"{{{not json").unwrap();
        let settings = LocalOnnxSettings {
            model_dir: tmp.path().to_path_buf(),
            ort_dylib_path: String::new(), // env/system rungs: no eager init
            batch_size: 64,
        };
        let err = LocalOnnx::load(&CODERANK_EMBED, &settings, Some(&store)).unwrap_err();
        assert!(
            matches!(err, LocalOnnxError::TokenizerLoad(ref m) if m.contains(TOKENIZER_FILE)),
            "wrong error: {:?}",
            err
        );
    }

    /// Settings projection carries the config values unmolested.
    #[test]
    fn settings_project_from_rag_config() {
        let cfg = RagConfig::default();
        let s = LocalOnnxSettings::from_rag_config(&cfg);
        assert_eq!(s.model_dir, cfg.model_dir);
        assert_eq!(s.ort_dylib_path, cfg.ort_dylib_path);
        assert_eq!(s.batch_size, 64);
    }
}

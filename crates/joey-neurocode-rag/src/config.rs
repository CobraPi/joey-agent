//! `neurocode.rag.*` configuration keys — all 18, pinned by
//! `specs/021-please-enhance-neurocode/contracts/rag-config-keys.md` (T002).
//!
//! ## Key table
//!
//! [`RAG_CONFIG_KEYS`] enumerates the full contract table (18 keys, exact
//! defaults, types). It exists so the contract test
//! (`tests/config_keys.rs`) can pin the surface against silent additions,
//! renames, or default drift (constitution Principle VII).
//!
//! ## `neurocode.rag.api_key` and the `_KEY` rule
//!
//! joey-core's `is_env_config_key` routing rule (crates/joey-core/src/
//! config.rs, port of upstream `_is_env_config_key`) applies only to BARE
//! UPPERCASE keys (`OPENAI_API_KEY`) — dotted keys never route, so
//! `neurocode.rag.api_key` set through the generic layer would land in
//! `config.yaml`. The contract instead demands `.env` auto-routing (the
//! `_KEY` rule). This module therefore implements the routing at its own
//! save/set layer, exactly the way joey-core does:
//!
//! - **Write**: [`set_and_save`] intercepts `neurocode.rag.api_key` and
//!   persists it via `joey_core::config::save_env_value` under the env var
//!   name [`ENV_API_KEY_NAME`] = `JOEY_NEUROCODE_RAG_API_KEY` — written to
//!   `<joey home>/.env`, never `config.yaml`. All other keys delegate to
//!   `Config::set_and_save` (config.yaml).
//! - **Read**: [`RagConfig::load`] resolves the key as
//!   `JOEY_NEUROCODE_RAG_API_KEY` from the process environment (populated
//!   from `~/.joey/.env` at startup by `Config::load`'s dotenv step) if
//!   set and non-empty, else falls back to the ordinary dotted-path getter
//!   `config.get_str("neurocode.rag.api_key", "")`. Env wins; the dotted
//!   getter remains the fallback either way (contract: "reads use the same
//!   dotted-path getter either way").
//!
//! ## Validation & clamping (invalid never crashes — warn + fallback)
//!
//! - `neurocode.rag.batch_size`: validated range **16–128**; out-of-range
//!   (or non-integer) → config-load warning + fallback to the default `64`.
//! - `neurocode.rag.context_window_lines`: clamped **0–200** (default 20;
//!   the clamp is consumed by context expansion in T027).
//! - `neurocode.rag.relation_max_depth`: clamped **0–2** (default 2).
//! - `neurocode.rag.backend`: enum `auto | local_onnx | openai_compat |
//!   ollama`; unknown value → warning + fallback to `auto`.
//!
//! ## `~` expansion
//!
//! `neurocode.rag.local.model_dir` expands a leading `~` (or `~/`) to the
//! OS user home at load time. The default resolves through `joey_home()`
//! so profile scoping (`-p/--profile`) is honored; in a standard install
//! it equals the contract default `~/.joey/neurocode/models/<profile>/`.
//!
//! Unknown `neurocode.rag.*` keys follow the existing config-layer
//! behavior verbatim — no new strictness, no new errors. Changes take
//! effect at the next refresh/turn; no hot-reload contract.

use std::fmt;
use std::path::PathBuf;

use joey_core::config::{save_env_value, Config};
use joey_core::{joey_home, user_home_dir};

// ─── Key constants (all 18 dotted paths) ─────────────────────────────────────

/// Master switch; `false` = byte-identical parity (FR-009/SC-005).
pub const KEY_ENABLED: &str = "neurocode.rag.enabled";
/// Backend selection enum.
pub const KEY_BACKEND: &str = "neurocode.rag.backend";
/// Embedding service base URL for the secondary HTTP backends.
pub const KEY_BASE_URL: &str = "neurocode.rag.base_url";
/// Model profile name (R2).
pub const KEY_MODEL: &str = "neurocode.rag.model";
/// Bearer token for OpenAiCompat — `.env`-routed (see module docs).
pub const KEY_API_KEY: &str = "neurocode.rag.api_key";
/// Local ONNX model artifacts directory (`~` expanded at load time).
pub const KEY_MODEL_DIR: &str = "neurocode.rag.local.model_dir";
/// Mirror for `/neurocode model fetch`; empty = fetch disabled.
pub const KEY_MIRROR_URL: &str = "neurocode.rag.local.mirror_url";
/// ONNX Runtime dylib path for `ort` load-dynamic; empty = env/system.
pub const KEY_ORT_DYLIB_PATH: &str = "neurocode.rag.local.ort_dylib_path";
/// Embedding batch size; validated range 16–128.
pub const KEY_BATCH_SIZE: &str = "neurocode.rag.batch_size";
/// Default result limit for hybrid search.
pub const KEY_TOP_K: &str = "neurocode.rag.top_k";
/// ± context lines per result; clamped 0–200.
pub const KEY_CONTEXT_WINDOW_LINES: &str = "neurocode.rag.context_window_lines";
/// Max BFS depth for relation expansion; clamped 0–2.
pub const KEY_RELATION_MAX_DEPTH: &str = "neurocode.rag.relation_max_depth";
/// FallbackCoarse chunks participate in ranking (FR-014).
pub const KEY_INCLUDE_FALLBACK_CHUNKS: &str = "neurocode.rag.include_fallback_chunks";
/// Chunk count above which int8 quantization is applied.
pub const KEY_QUANTIZE_THRESHOLD: &str = "neurocode.rag.quantize_threshold";
/// Proactive pre-fetch into agent context (FR-015).
pub const KEY_PREFETCH_ENABLED: &str = "neurocode.rag.prefetch.enabled";
/// Refresh budget: max files processed per turn (FR-004).
pub const KEY_REFRESH_MAX_FILES_PER_TURN: &str = "neurocode.rag.refresh.max_files_per_turn";
/// Refresh budget: max bytes read per turn (FR-004).
pub const KEY_REFRESH_MAX_BYTES_PER_TURN: &str = "neurocode.rag.refresh.max_bytes_per_turn";
/// HTTP timeout for embedding calls and model-fetch downloads.
pub const KEY_TIMEOUT_SECS: &str = "neurocode.rag.timeout_secs";
/// Model id on the Copilot embeddings wire (Joey-native provider-following
/// extension; NOT part of the pinned 18-key contract table).
pub const KEY_COPILOT_MODEL: &str = "neurocode.rag.copilot.model";

/// Env var name `neurocode.rag.api_key` is persisted under (`.env`).
/// Matches the contract example: `JOEY_NEUROCODE_RAG_API_KEY=***`.
pub const ENV_API_KEY_NAME: &str = "JOEY_NEUROCODE_RAG_API_KEY";

// ─── Defaults & bounds (contract-pinned) ─────────────────────────────────────

pub const DEFAULT_BACKEND: &str = "auto";
pub const DEFAULT_BASE_URL: &str = "http://localhost:11434";
pub const DEFAULT_MODEL: &str = "nomic-embed-text-v1.5";
pub const DEFAULT_BATCH_SIZE: i64 = 64;
pub const BATCH_SIZE_MIN: i64 = 16;
pub const BATCH_SIZE_MAX: i64 = 128;
pub const DEFAULT_TOP_K: i64 = 10;
pub const DEFAULT_CONTEXT_WINDOW_LINES: i64 = 20;
pub const CONTEXT_WINDOW_LINES_MIN: i64 = 0;
pub const CONTEXT_WINDOW_LINES_MAX: i64 = 200;
pub const DEFAULT_RELATION_MAX_DEPTH: i64 = 2;
pub const RELATION_MAX_DEPTH_MIN: i64 = 0;
pub const RELATION_MAX_DEPTH_MAX: i64 = 2;
pub const DEFAULT_QUANTIZE_THRESHOLD: i64 = 100000;
pub const DEFAULT_REFRESH_MAX_FILES_PER_TURN: i64 = 50;
pub const DEFAULT_REFRESH_MAX_BYTES_PER_TURN: i64 = 52428800;
pub const DEFAULT_TIMEOUT_SECS: i64 = 30;
pub const DEFAULT_COPILOT_MODEL: &str = "metis-1024-I16-Binary";

// ─── Contract table ──────────────────────────────────────────────────────────

/// The value kind of a config key (used by the contract enumeration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RagValueKind {
    Bool,
    /// Backend enum `auto | local_onnx | openai_compat | ollama | copilot`.
    Backend,
    Str,
    /// Filesystem path with `~` expansion (`local.model_dir`).
    Path,
    Int,
}

/// One row of the contract key table: dotted key, kind, exact default.
#[derive(Debug, Clone, Copy)]
pub struct RagKeySpec {
    pub key: &'static str,
    pub kind: RagValueKind,
    /// Canonical default repr, exactly as written in the contract table.
    pub default: &'static str,
}

impl RagKeySpec {
    const fn new(key: &'static str, kind: RagValueKind, default: &'static str) -> Self {
        Self { key, kind, default }
    }
}

/// The full 18-key contract table (`contracts/rag-config-keys.md`).
/// Length is pinned by the type — additions require editing this table and
/// the contract together.
pub const RAG_CONFIG_KEYS: [RagKeySpec; 18] = [
    RagKeySpec::new(KEY_ENABLED, RagValueKind::Bool, "false"),
    RagKeySpec::new(KEY_BACKEND, RagValueKind::Backend, "auto"),
    RagKeySpec::new(KEY_BASE_URL, RagValueKind::Str, "http://localhost:11434"),
    RagKeySpec::new(KEY_MODEL, RagValueKind::Str, "nomic-embed-text-v1.5"),
    RagKeySpec::new(KEY_API_KEY, RagValueKind::Str, ""),
    RagKeySpec::new(
        KEY_MODEL_DIR,
        RagValueKind::Path,
        "~/.joey/neurocode/models/<profile>/",
    ),
    RagKeySpec::new(KEY_MIRROR_URL, RagValueKind::Str, ""),
    RagKeySpec::new(KEY_ORT_DYLIB_PATH, RagValueKind::Str, ""),
    RagKeySpec::new(KEY_BATCH_SIZE, RagValueKind::Int, "64"),
    RagKeySpec::new(KEY_TOP_K, RagValueKind::Int, "10"),
    RagKeySpec::new(KEY_CONTEXT_WINDOW_LINES, RagValueKind::Int, "20"),
    RagKeySpec::new(KEY_RELATION_MAX_DEPTH, RagValueKind::Int, "2"),
    RagKeySpec::new(KEY_INCLUDE_FALLBACK_CHUNKS, RagValueKind::Bool, "true"),
    RagKeySpec::new(KEY_QUANTIZE_THRESHOLD, RagValueKind::Int, "100000"),
    RagKeySpec::new(KEY_PREFETCH_ENABLED, RagValueKind::Bool, "false"),
    RagKeySpec::new(KEY_REFRESH_MAX_FILES_PER_TURN, RagValueKind::Int, "50"),
    RagKeySpec::new(KEY_REFRESH_MAX_BYTES_PER_TURN, RagValueKind::Int, "52428800"),
    RagKeySpec::new(KEY_TIMEOUT_SECS, RagValueKind::Int, "30"),
];

// ─── Backend enum ────────────────────────────────────────────────────────────

/// `neurocode.rag.backend` selection (contract enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RagBackend {
    /// Resolve `local_onnx` when `local.model_dir` artifacts verify, else
    /// keyword-only degradation — never an implicit network call.
    #[default]
    Auto,
    /// In-process ONNX embedder (primary, fully local).
    LocalOnnx,
    /// OpenAI-compatible `POST {base_url}/v1/embeddings`.
    OpenAiCompat,
    /// Ollama native `POST {base_url}/api/embed`.
    Ollama,
    /// GitHub Copilot `POST {base}/embeddings` (provider-following). Copilot,
    Copilot,
}

impl RagBackend {
    /// Parse the contract enum; `None` for unknown values (callers warn and
    /// fall back to `auto` — never a crash).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "auto" => Some(Self::Auto),
            "local_onnx" => Some(Self::LocalOnnx),
            "openai_compat" => Some(Self::OpenAiCompat),
            "ollama" => Some(Self::Ollama),
            "copilot" => Some(Self::Copilot),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::LocalOnnx => "local_onnx",
            Self::OpenAiCompat => "openai_compat",
            Self::Ollama => "ollama",
            Self::Copilot => "copilot",
        }
    }
}

impl fmt::Display for RagBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─── RagConfig ───────────────────────────────────────────────────────────────

/// Typed view of all 18 `neurocode.rag.*` keys, loaded from a
/// [`joey_core::Config`] via the existing dotted-path getters.
///
/// Loading is infallible by contract: invalid values produce a
/// config-load warning and fall back / clamp — never a crash.
#[derive(Debug, Clone, PartialEq)]
pub struct RagConfig {
    /// `neurocode.rag.enabled` (default `false`).
    pub enabled: bool,
    /// `neurocode.rag.backend` (default `auto`).
    pub backend: RagBackend,
    /// `neurocode.rag.base_url` (default `http://localhost:11434`).
    pub base_url: String,
    /// `neurocode.rag.model` profile name (default `nomic-embed-text-v1.5`).
    pub model: String,
    /// `neurocode.rag.api_key` (default empty; see module docs for the
    /// env-first resolution order).
    pub api_key: String,
    /// `neurocode.rag.local.model_dir`, `~`-expanded
    /// (default `~/.joey/neurocode/models/<profile>/`).
    pub model_dir: PathBuf,
    /// `neurocode.rag.local.mirror_url` (default empty = fetch disabled).
    pub mirror_url: String,
    /// `neurocode.rag.local.ort_dylib_path` (default empty).
    pub ort_dylib_path: String,
    /// `neurocode.rag.batch_size`, validated 16–128 (default 64).
    pub batch_size: i64,
    /// `neurocode.rag.top_k` (default 10).
    pub top_k: i64,
    /// `neurocode.rag.context_window_lines`, clamped 0–200 (default 20).
    pub context_window_lines: i64,
    /// `neurocode.rag.relation_max_depth`, clamped 0–2 (default 2).
    pub relation_max_depth: i64,
    /// `neurocode.rag.include_fallback_chunks` (default `true`).
    pub include_fallback_chunks: bool,
    /// `neurocode.rag.quantize_threshold` (default 100000).
    pub quantize_threshold: i64,
    /// `neurocode.rag.prefetch.enabled` (default `false`).
    pub prefetch_enabled: bool,
    /// `neurocode.rag.refresh.max_files_per_turn` (default 50).
    pub refresh_max_files_per_turn: i64,
    /// `neurocode.rag.refresh.max_bytes_per_turn` (default 52428800 = 50 MiB).
    pub refresh_max_bytes_per_turn: i64,
    /// `neurocode.rag.timeout_secs` (default 30).
    pub timeout_secs: i64,
    /// `neurocode.rag.copilot.model` (default `metis-1024-I16-Binary`,
    /// 1024-dim, served via the GitHub-native embeddings endpoint;
    /// `text-embedding-3-small` (1536-dim) remains supported via the
    /// OpenAI-style endpoint).
    pub copilot_model: String,
    /// Derived at load: `model.provider` selects a Copilot wire (see
    /// [`provider_selects_copilot`]) — the CLI wiring switches the `auto`
    /// embedding backend to the Copilot embeddings endpoint when true.
    pub copilot_provider_active: bool,
}

impl RagConfig {
    /// Load all 18 keys from `config` (layered defaults ← user, expanded).
    /// Invalid values warn and fall back / clamp per the contract.
    pub fn load(config: &Config) -> Self {
        let model = config.get_str(KEY_MODEL, DEFAULT_MODEL);

        let backend_raw = config.get_str(KEY_BACKEND, DEFAULT_BACKEND);
        let backend = match RagBackend::parse(&backend_raw) {
            Some(b) => b,
            None => {
                warn_config(&format!(
                    "invalid {} value {:?}; falling back to {:?} (expected one of \
                     auto | local_onnx | openai_compat | ollama | copilot)",
                    KEY_BACKEND, backend_raw, DEFAULT_BACKEND
                ));
                RagBackend::default()
            }
        };

        // api_key: env (`.env`-routed) first, dotted getter as fallback.
        let api_key = std::env::var(ENV_API_KEY_NAME)
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| config.get_str(KEY_API_KEY, ""));

        // model_dir: explicit (tilde-expanded) or profile-keyed default.
        let model_dir = {
            let raw = config.get_str(KEY_MODEL_DIR, "");
            if raw.trim().is_empty() {
                default_model_dir(&model)
            } else {
                expand_tilde(&raw)
            }
        };

        let batch_size = validate_batch_size(config.get_i64(KEY_BATCH_SIZE, DEFAULT_BATCH_SIZE));

        let copilot_model = config.get_str(KEY_COPILOT_MODEL, DEFAULT_COPILOT_MODEL);
        let copilot_provider_active =
            provider_selects_copilot(&config.get_str("model.provider", "auto"));

        Self {
            enabled: config.get_bool(KEY_ENABLED, false),
            backend,
            base_url: config.get_str(KEY_BASE_URL, DEFAULT_BASE_URL),
            model,
            api_key,
            model_dir,
            mirror_url: config.get_str(KEY_MIRROR_URL, ""),
            ort_dylib_path: config.get_str(KEY_ORT_DYLIB_PATH, ""),
            batch_size,
            top_k: config.get_i64(KEY_TOP_K, DEFAULT_TOP_K),
            context_window_lines: clamp_i64(
                config.get_i64(KEY_CONTEXT_WINDOW_LINES, DEFAULT_CONTEXT_WINDOW_LINES),
                CONTEXT_WINDOW_LINES_MIN,
                CONTEXT_WINDOW_LINES_MAX,
            ),
            relation_max_depth: clamp_i64(
                config.get_i64(KEY_RELATION_MAX_DEPTH, DEFAULT_RELATION_MAX_DEPTH),
                RELATION_MAX_DEPTH_MIN,
                RELATION_MAX_DEPTH_MAX,
            ),
            include_fallback_chunks: config.get_bool(KEY_INCLUDE_FALLBACK_CHUNKS, true),
            quantize_threshold: config.get_i64(KEY_QUANTIZE_THRESHOLD, DEFAULT_QUANTIZE_THRESHOLD),
            prefetch_enabled: config.get_bool(KEY_PREFETCH_ENABLED, false),
            refresh_max_files_per_turn: config
                .get_i64(KEY_REFRESH_MAX_FILES_PER_TURN, DEFAULT_REFRESH_MAX_FILES_PER_TURN),
            refresh_max_bytes_per_turn: config
                .get_i64(KEY_REFRESH_MAX_BYTES_PER_TURN, DEFAULT_REFRESH_MAX_BYTES_PER_TURN),
            timeout_secs: config.get_i64(KEY_TIMEOUT_SECS, DEFAULT_TIMEOUT_SECS),
            copilot_model,
            copilot_provider_active,
        }
    }

    /// Effective context window for one search (T027, FR-006): the
    /// per-request override wins when present, else the config default
    /// `context_window_lines`; the result is ALWAYS clamped 0–200.
    ///
    /// Single source of the precedence rule (request override > config
    /// default, clamped) shared by the CLI flag path (`--expand-lines`)
    /// and any other surface that resolves a window before building a
    /// [`crate::search::hybrid::SearchRequest`]; the pipeline clamps
    /// again at consumption as the last line of defense.
    pub fn effective_context_window(&self, requested: Option<u32>) -> u32 {
        let v = requested.map(i64::from).unwrap_or(self.context_window_lines);
        v.clamp(CONTEXT_WINDOW_LINES_MIN, CONTEXT_WINDOW_LINES_MAX) as u32
    }
}

impl Default for RagConfig {
    fn default() -> Self {
        Self::load(&Config::defaults())
    }
}

/// Contract default `~/.joey/neurocode/models/<profile>/` — resolved via
/// `joey_home()` so profile scoping is honored (standard install:
/// `~/.joey`).
pub fn default_model_dir(profile: &str) -> PathBuf {
    joey_home().join("neurocode").join("models").join(profile)
}

/// Whether `dotted` is `.env`-routed at this layer. Only
/// `neurocode.rag.api_key` — joey-core's generic `_KEY` rule covers bare
/// uppercase keys only, so the dotted key's routing is implemented here
/// (see module docs).
pub fn is_env_routed_key(dotted: &str) -> bool {
    dotted == KEY_API_KEY
}

/// Whether a `model.provider` value selects a Copilot wire — the
/// provider-following embedding switch. Aliases mirror joey-providers
/// profile.rs (`is_copilot_wire` + the github-* alias set).
pub fn provider_selects_copilot(provider: &str) -> bool {
    matches!(
        provider.trim().to_lowercase().as_str(),
        "copilot" | "github-copilot" | "github-models" | "github" | "ai-usage-hud"
    )
}

/// Set a `neurocode.rag.*` key and persist, routing `neurocode.rag.api_key`
/// to `.env` (env var [`ENV_API_KEY_NAME`]) the way joey-core's
/// `set_and_save` routes bare `_KEY` keys; every other key delegates to
/// `Config::set_and_save` (config.yaml).
pub fn set_and_save(config: &mut Config, dotted: &str, raw_value: &str) -> anyhow::Result<()> {
    if is_env_routed_key(dotted) {
        save_env_value(ENV_API_KEY_NAME, raw_value)
    } else {
        config.set_and_save(dotted, raw_value)
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Expand a leading `~` / `~/` to the OS user home; anything else is
/// returned unchanged (absolute and relative paths pass through).
fn expand_tilde(p: &str) -> PathBuf {
    let trimmed = p.trim();
    if trimmed == "~" {
        user_home_dir()
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        user_home_dir().join(rest)
    } else {
        PathBuf::from(p)
    }
}

/// batch_size rule: range 16–128; out-of-range → warning + default 64
/// (contract: fallback, NOT clamp).
fn validate_batch_size(v: i64) -> i64 {
    if (BATCH_SIZE_MIN..=BATCH_SIZE_MAX).contains(&v) {
        v
    } else {
        warn_config(&format!(
            "invalid {} value {}; must be {}–{}; falling back to default {}",
            KEY_BATCH_SIZE, v, BATCH_SIZE_MIN, BATCH_SIZE_MAX, DEFAULT_BATCH_SIZE
        ));
        DEFAULT_BATCH_SIZE
    }
}

fn clamp_i64(v: i64, lo: i64, hi: i64) -> i64 {
    v.clamp(lo, hi)
}

/// Config-load warning (stderr, matching the config layer's warning style —
/// invalid values never crash the load).
fn warn_config(msg: &str) {
    eprintln!("[joey config] warning: {}", msg);
}

// ─── Unit tests (pure helpers only — no env/home mutation) ───────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_parse_matrix() {
        assert_eq!(RagBackend::parse("auto"), Some(RagBackend::Auto));
        assert_eq!(RagBackend::parse("local_onnx"), Some(RagBackend::LocalOnnx));
        assert_eq!(
            RagBackend::parse("openai_compat"),
            Some(RagBackend::OpenAiCompat)
        );
        assert_eq!(RagBackend::parse("ollama"), Some(RagBackend::Ollama));
        assert_eq!(RagBackend::parse(" ollama "), Some(RagBackend::Ollama));
        assert_eq!(RagBackend::parse("LocalOnnx"), None);
        assert_eq!(RagBackend::parse("openai"), None);
        assert_eq!(RagBackend::parse(""), None);
        assert_eq!(RagBackend::default(), RagBackend::Auto);
        assert_eq!(RagBackend::Ollama.to_string(), "ollama");
    }

    #[test]
    fn copilot_extensions() {
        assert_eq!(RagBackend::parse("copilot"), Some(RagBackend::Copilot));
        assert_eq!(RagBackend::Copilot.as_str(), "copilot");
        assert!(provider_selects_copilot("Copilot"));
        assert!(provider_selects_copilot(" github-copilot "));
        assert!(provider_selects_copilot("ai-usage-hud"));
        assert!(!provider_selects_copilot("zai"));
        assert!(!provider_selects_copilot("openai"));
        let cfg = RagConfig::load(&Config::defaults());
        assert_eq!(cfg.copilot_model, DEFAULT_COPILOT_MODEL);
        assert!(!cfg.copilot_provider_active);
    }

    #[test]
    fn tilde_expansion() {
        let home = user_home_dir();
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("~/models/x"), home.join("models").join("x"));
        assert_eq!(expand_tilde("/abs/path"), PathBuf::from("/abs/path"));
        assert_eq!(expand_tilde("rel/path"), PathBuf::from("rel/path"));
        assert_eq!(expand_tilde(""), PathBuf::from(""));
        // `~otheruser` is NOT the current home — passed through literally.
        assert_eq!(expand_tilde("~root/x"), PathBuf::from("~root/x"));
    }

    #[test]
    fn batch_size_validation() {
        assert_eq!(validate_batch_size(16), 16);
        assert_eq!(validate_batch_size(64), 64);
        assert_eq!(validate_batch_size(128), 128);
        assert_eq!(validate_batch_size(15), DEFAULT_BATCH_SIZE);
        assert_eq!(validate_batch_size(129), DEFAULT_BATCH_SIZE);
        assert_eq!(validate_batch_size(-1), DEFAULT_BATCH_SIZE);
        assert_eq!(validate_batch_size(100000), DEFAULT_BATCH_SIZE);
    }

    #[test]
    fn clamping_bounds() {
        assert_eq!(clamp_i64(20, 0, 200), 20);
        assert_eq!(clamp_i64(-3, 0, 200), 0);
        assert_eq!(clamp_i64(999, 0, 200), 200);
        assert_eq!(clamp_i64(7, 0, 2), 2);
        assert_eq!(clamp_i64(-1, 0, 2), 0);
        assert_eq!(clamp_i64(1, 0, 2), 1);
    }

    #[test]
    fn effective_context_window_precedence_and_clamp() {
        let rag = RagConfig::default();
        assert_eq!(
            rag.context_window_lines, DEFAULT_CONTEXT_WINDOW_LINES,
            "config default 20"
        );
        // Request override wins over the config default.
        assert_eq!(rag.effective_context_window(Some(7)), 7);
        assert_eq!(rag.effective_context_window(Some(0)), 0);
        // No request → the config default.
        assert_eq!(rag.effective_context_window(None), 20);
        // Always clamped 0–200 — the request value…
        assert_eq!(rag.effective_context_window(Some(250)), 200);
        assert_eq!(rag.effective_context_window(Some(u32::MAX)), 200);
        // …and the config value alike (a loaded-clamped 200 → None uses it).
        let mut clamped = rag.clone();
        clamped.context_window_lines = CONTEXT_WINDOW_LINES_MAX;
        assert_eq!(clamped.effective_context_window(None), 200);
    }

    #[test]
    fn env_routing_rule_is_api_key_only() {
        assert!(is_env_routed_key("neurocode.rag.api_key"));
        for spec in RAG_CONFIG_KEYS.iter() {
            if spec.key != KEY_API_KEY {
                assert!(!is_env_routed_key(spec.key), "{}", spec.key);
            }
        }
        assert!(!is_env_routed_key("OPENAI_API_KEY"));
    }

    #[test]
    fn contract_table_shape() {
        assert_eq!(RAG_CONFIG_KEYS.len(), 18);
        let mut seen = std::collections::HashSet::new();
        for spec in RAG_CONFIG_KEYS.iter() {
            assert!(
                spec.key.starts_with("neurocode.rag."),
                "key {} must be namespaced",
                spec.key
            );
            assert!(seen.insert(spec.key), "duplicate key {}", spec.key);
            assert!(!spec.default.is_empty() || spec.kind == RagValueKind::Str);
        }
    }
}

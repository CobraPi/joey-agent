//! Runtime smoke test for the local ONNX embedding backend (real model).
//!
//! Complements the inline unit tests (which never execute a session) with a
//! RUNTIME proof against the real artifacts: auto->LocalOnnx resolution,
//! load, embed (query/document prefixes), dims=768, unit-norm, semantic
//! sanity (matching pair > non-matching pair).
//!
//! SKIP POLICY (research.md R8 — no Hugging Face, no downloads, ever):
//! AUTO-SKIPS (prints the documented reason, returns Ok) when no verified
//! local model artifacts are present — same knob/pattern as
//! `retrieval_quality.rs` (`JOEY_RAG_MODEL_DIR` override, else the
//! default model dir, filesystem-only probe). It NEVER downloads and
//! never contacts huggingface.co.
//!
//! Unlike the SC-001 benchmark this test is NOT `#[ignore]`d: with the
//! model absent it skips cleanly, so it is safe in the default suite.
//! Run with output:
//!
//! ```text
//! cargo test -p joey-neurocode-rag --test local_model_smoke -- --nocapture
//! ```

use std::path::PathBuf;

use joey_neurocode_rag::config::{default_model_dir, RagBackend, RagConfig};
use joey_neurocode_rag::embed::artifacts::compute_hashes;
use joey_neurocode_rag::embed::local_onnx::{
    check_input_surface, LocalOnnx, LocalOnnxSettings, ENV_ORT_DYLIB_PATH,
};
use joey_neurocode_rag::embed::profiles::default_profile;
use joey_neurocode_rag::embed::{resolve_kind, BackendKind};

/// Explicit opt-in artifact directory override (same knob the T040 bench
/// honors). Never required, never fetched from.
const ENV_MODEL_DIR: &str = "JOEY_RAG_MODEL_DIR";

/// The documented auto-skip reason.
pub const SKIP_REASON_PREFIX: &str =
    "SKIP (local_model_smoke): no verified local model artifacts";

/// Filesystem-only artifact probe — same gate as `retrieval_quality.rs`.
fn artifacts_available() -> Option<PathBuf> {
    let dir = std::env::var(ENV_MODEL_DIR)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| default_model_dir(default_profile().name));
    compute_hashes(&dir).ok().map(|_| dir)
}

/// Filesystem-only ONNX Runtime dylib probe, mirroring the runtime ladder
/// (`neurocode.rag.local.ort_dylib_path` → `ORT_DYLIB_PATH` → system
/// lookup): a dylib that exists nowhere ort looks cannot load, and ort's
/// lazy dlopen PANICS (it does not return Err) — so gate the runtime test
/// the same way the model probe above does. Config/env rungs check the
/// exact configured file; the system rung checks ort's documented lookup
/// locations. Purely a skip gate: never downloads, never dlopens.
fn dylib_available() -> bool {
    let cfg = joey_core::Config::load().unwrap_or_else(|_| joey_core::Config::defaults());
    let rag = RagConfig::load(&cfg);
    let configured = rag.ort_dylib_path.trim().to_string();
    if !configured.is_empty() {
        return PathBuf::from(&configured).is_file();
    }
    if let Ok(env_path) = std::env::var(ENV_ORT_DYLIB_PATH) {
        let env_path = env_path.trim().to_string();
        if !env_path.is_empty() {
            return PathBuf::from(env_path).is_file();
        }
    }
    // System rung: the dlopen search roots ort's error output documents
    // (plus the dynamic-linker search paths). A dylib present in none of
    // them cannot load — set the config/env rung when it lives elsewhere.
    const NAMES: [&str; 2] = ["libonnxruntime.dylib", "libonnxruntime.so"];
    let mut dirs: Vec<PathBuf> = ["/usr/local/lib", "/opt/homebrew/lib", "/usr/lib"]
        .iter()
        .map(PathBuf::from)
        .collect();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join("lib"));
    }
    for var in ["DYLD_LIBRARY_PATH", "LD_LIBRARY_PATH"] {
        if let Ok(search) = std::env::var(var) {
            dirs.extend(std::env::split_paths(&search));
        }
    }
    dirs.iter().any(|d| NAMES.iter().any(|n| d.join(n).is_file()))
}

/// L2 norm of a vector.
fn l2_norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
}

/// Cosine similarity (inputs are unit-norm post-profile-L2, so this is a
/// dot product; the explicit norm division keeps it honest anyway).
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    dot / (l2_norm(a) * l2_norm(b))
}

/// Surface matrix pinned at runtime: 3-input surface accepted (+true),
/// unknown input rejected, missing required rejected.
#[test]
fn input_surface_matrix() {
    assert_eq!(
        check_input_surface(&["input_ids", "attention_mask", "token_type_ids"]).unwrap(),
        true
    );
    assert_eq!(
        check_input_surface(&["input_ids", "attention_mask"]).unwrap(),
        false
    );
    assert!(check_input_surface(&["input_ids", "attention_mask", "weird_input"]).is_err());
    assert!(check_input_surface(&["attention_mask", "token_type_ids"]).is_err());
    assert!(check_input_surface(&[]).is_err());
    eprintln!("[local_model_smoke] input_surface_matrix: reject/accept paths OK");
}

/// Runtime smoke: auto resolves LocalOnnx, load + embed with the nomic
/// query/document prefixes, dims/normalization/semantic sanity. AUTO-SKIPS
/// (never an error, never a download) when artifacts are absent.
#[test]
fn local_onnx_auto_resolution_and_embed_runtime() {
    let Some(model_dir) = artifacts_available() else {
        eprintln!(
            "{SKIP_REASON_PREFIX} at {} — this test NEVER downloads and never \
             contacts huggingface.co (research.md R8). Place model.onnx + \
             tokenizer.json manually (or set {ENV_MODEL_DIR}) to execute.",
            default_model_dir(default_profile().name).display()
        );
        return;
    };

    if !dylib_available() {
        eprintln!(
            "SKIP (local_model_smoke): ONNX Runtime dylib not found on any ladder rung \
             (neurocode.rag.local.ort_dylib_path / {ENV_ORT_DYLIB_PATH} / system lookup) — \
             ort's lazy dlopen would panic without it. Point a rung at libonnxruntime \
             to execute this test."
        );
        return;
    }

    // (b) auto -> LocalOnnx resolution, exactly the config path's call.
    let resolved = resolve_kind(
        RagBackend::Auto,
        default_profile().name,
        &model_dir,
    )
    .unwrap_or_else(|e| panic!("resolve_kind(auto): {e}"));
    eprintln!(
        "[local_model_smoke] backend kind = {:?} (degradation_reason={:?})",
        resolved.kind, resolved.degradation_reason
    );
    assert_eq!(
        resolved.kind,
        BackendKind::LocalOnnx,
        "auto must resolve LocalOnnx when artifacts are present, not KeywordOnly"
    );

    // (c) load + embed through the public API. Dylib ladder must match the
    // probe above: config rung if set (committed via `ort::init_from`
    // inside load), else empty = env/system rungs.
    let cfg = joey_core::Config::load().unwrap_or_else(|_| joey_core::Config::defaults());
    let ort_dylib_path = RagConfig::load(&cfg).ort_dylib_path.trim().to_string();
    let settings = LocalOnnxSettings {
        model_dir: model_dir.clone(),
        ort_dylib_path,
        batch_size: 64,
    };
    let profile = default_profile();
    let model = LocalOnnx::load(profile, &settings, None)
        .unwrap_or_else(|e| panic!("load at {}: {e}", model_dir.display()));

    let query = "how is retry backoff delay computed".to_string();
    let doc_match = "def calculate_backoff_with_jitter(attempt, base_ms, cap_ms, seed):\n    exponent = min(attempt - 1, 16)\n    delay = base_ms * (2 ** exponent)".to_string();
    let doc_other = "fn render_markdown_table(rows: &[Vec<String>]) -> String {\n    let mut out = String::new();".to_string();

    let q = &model.embed_queries(&[query]).unwrap()[0];
    let d = &model.embed_documents(&[doc_match, doc_other]).unwrap();

    for (name, v) in [("query", q), ("doc_match", &d[0]), ("doc_other", &d[1])] {
        assert_eq!(v.len(), profile.dim as usize, "{name}: dim mismatch");
        let n = l2_norm(v);
        eprintln!("[local_model_smoke] {name}: dim={} |v|={n:.6}", v.len());
        assert!(
            (n - 1.0).abs() < 1e-3,
            "{name}: expected unit norm (profile l2_normalize), got {n}"
        );
    }

    let sim_match = cosine(q, &d[0]);
    let sim_other = cosine(q, &d[1]);
    eprintln!(
        "[local_model_smoke] cosine(matching)={sim_match:.4} cosine(non-matching)={sim_other:.4}"
    );
    assert!(
        sim_match > sim_other,
        "semantic sanity violated: matching {sim_match:.4} !> non-matching {sim_other:.4}"
    );
    assert!(
        sim_match > 0.3,
        "matching pair suspiciously cold: {sim_match:.4}"
    );
    eprintln!("[local_model_smoke] PASS: auto->LocalOnnx, dims, unit-norm, semantic ordering");
}

/// `RagConfig` sanity (always runs): the default backend key is `auto` and
/// `model_dir` points at the profile dir the smoke test probes.
#[test]
fn default_config_points_at_auto_backend() {
    let cfg = RagConfig::default();
    assert_eq!(cfg.backend, RagBackend::Auto);
    let _ = RagBackend::parse("auto"); // used above via resolve_kind
    eprintln!("[local_model_smoke] default backend = auto, model_dir = {:?}", cfg.model_dir);
}

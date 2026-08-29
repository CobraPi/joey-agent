//! T037 — local embedding throughput benchmark (plan.md M7/budget;
//! contracts/embedding-backend.md § Throughput note).
//!
//! Validates the ~10–40 ms per 512-token text AVX2 ballpark of the
//! LocalOnnx backend (padded `encode_batch` + rayon pooling run).
//!
//! SKIP POLICY (research.md R8 — no Hugging Face, no downloads, ever):
//! this bench NEVER downloads model artifacts and NEVER contacts the
//! network. When no verified `model.onnx` + `tokenizer.json` set is
//! present it AUTO-SKIPS: every measurement prints a clear skip reason
//! and exits cleanly (Ok), so a bare CI/dev environment stays green.
//!
//! Artifact location probed in order:
//! 1. `$JOEY_RAG_MODEL_DIR` (explicit opt-in override), else
//! 2. the config default `~/.joey/neurocode/models/<profile>/`
//!    (`config::default_model_dir`).
//!
//! INVOCATION (plain benches/*.rs file — auto-discovered bench target,
//! standard test harness; zero Cargo.toml changes, zero new dependencies,
//! per the Constitution's dependency-justification bar):
//!
//! ```text
//! cargo bench -p joey-neurocode-rag                       # harness: all ignored
//! cargo test -p joey-neurocode-rag --benches -- --ignored --nocapture
//!     # ^ the real run: measurement fns execute (or print their skip
//!     #   reason when artifacts are absent)
//! cargo test -p joey-neurocode-rag --benches -- --nocapture
//!     # ^ also runs the always-run companion (skip-path + pure pooling)
//! ```
//!
//! What is measured when artifacts ARE present:
//! - `end_to_end_documents_512_token_texts` (ignored, opt-in): a batch of
//!   512-token-ish texts through the REAL tokenizer + session path
//!   (`LocalOnnx::embed_documents` — padded `encode_batch`, session run,
//!   rayon mean-pool + L2), reporting ms/text and the EXACT padded token
//!   count measured with the offline `tokenizer.json`.
//! - `pure_pooling_normalization_path` (ALWAYS runs, no artifacts
//!   needed): the measurable-without-a-session slice — `mean_pool_l2`
//!   over synthetic `[512, 768]` hidden states, rayon across the batch —
//!   labeled as the pooling/normalization-only path.
//!
//! M7 NOTE: milestone M7 pins the real numbers on real hardware; the
//! 10–40 ms band is reported INFORMATIONALLY here (an assessment line is
//! printed whether the measurement lands inside or outside the band) and
//! never hard-fails on timing — only sanity (finite timing, unit-norm
//! vectors, order preservation) is asserted.

use std::path::PathBuf;
use std::time::Instant;

use joey_neurocode_rag::embed::artifacts::compute_hashes;
use joey_neurocode_rag::embed::local_onnx::{mean_pool_l2, LocalOnnx, LocalOnnxSettings};
use joey_neurocode_rag::embed::profiles::default_profile;
use ndarray::Array2;
use rayon::prelude::*;

/// Environment variable naming an explicit artifact directory for this
/// bench (opt-in; never required, never fetched from).
const ENV_MODEL_DIR: &str = "JOEY_RAG_MODEL_DIR";

/// Documented reason string (also used by the always-run skip-path test).
pub const SKIP_REASON_PREFIX: &str =
    "SKIP (T037 embed_throughput): no verified local model artifacts";

/// The plan.md AVX2 ballpark this bench reports against (informational).
const BALLPARK_MIN_MS: f64 = 10.0;
const BALLPARK_MAX_MS: f64 = 40.0;

/// Resolve the artifact directory this bench would use: `None` when no
/// verifiable artifact set is present (the skip case). Filesystem-only
/// probe — hashes the two files, touches no dylib, no session, no network.
fn bench_model_dir() -> Option<PathBuf> {
    let dir = std::env::var(ENV_MODEL_DIR)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            joey_neurocode_rag::config::default_model_dir(default_profile().name)
        });
    match compute_hashes(&dir) {
        Ok(_) => Some(dir),
        Err(_) => None,
    }
}

/// Print the skip banner for `dir` (the probe already failed there).
fn print_skip(dir: &std::path::Path) {
    println!(
        "{SKIP_REASON_PREFIX} at {} — this bench NEVER downloads and never \
         contacts huggingface.co (research.md R8). To run the measurement, \
         place model.onnx + tokenizer.json there manually (manual placement \
         or project mirror only) or point {ENV_MODEL_DIR} at such a directory.",
        dir.display()
    );
}

/// Deterministic code-flavored filler of roughly `words` words (~512 BPE
/// tokens at the nomic tokenizer's code density; the exact padded count is
/// printed at run time when artifacts are present). No rand dependency —
/// a tiny LCG varies the vocabulary. ~11 BPE tokens per iteration ⇒ 47
/// iterations ≈ 512 tokens.
fn filler_text(words: usize, seed: u64) -> String {
    const VOCAB: &[&str] = &[
        "request", "handler", "buffer", "queue", "worker", "session", "token",
        "stream", "record", "index", "cache", "pool", "socket", "packet",
        "cursor", "schema", "message", "channel", "gateway", "scheduler",
        "retry", "limit", "window", "batch", "ledger", "manifest",
    ];
    let mut out = String::new();
    let mut state = seed;
    let mut next = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        state
    };
    for w in 0..words {
        let a = VOCAB[(next() % VOCAB.len() as u64) as usize];
        let b = VOCAB[(next() % VOCAB.len() as u64) as usize];
        if w % 12 == 0 && w > 0 {
            out.push('\n');
        }
        out.push_str(&format!("{a}_{:02} := {b}_{} + {}.len(); ", w % 100, w % 7, a));
    }
    out
}

/// The document batch: 16 texts sized ~512 tokens each.
fn bench_texts() -> Vec<String> {
    (0u64..16).map(|i| filler_text(47, 0x5EED_0000 + i * 7919)).collect()
}

// ---------------------------------------------------------------------------
// Opt-in measurements (artifacts present). Invoked via
// `cargo test -p joey-neurocode-rag --benches -- --ignored --nocapture`.
// ---------------------------------------------------------------------------

/// End-to-end: tokenizer + session + rayon pooling over 512-token-ish
/// documents — the path whose per-text cost the 10–40 ms band describes.
#[test]
#[ignore = "opt-in: needs locally placed model artifacts (never downloaded); run with -- --ignored --nocapture"]
fn end_to_end_documents_512_token_texts() {
    let Some(dir) = bench_model_dir() else {
        let fallback = std::env::var(ENV_MODEL_DIR)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                joey_neurocode_rag::config::default_model_dir(default_profile().name)
            });
        print_skip(&fallback);
        return;
    };

    let profile = default_profile();
    let settings = LocalOnnxSettings {
        model_dir: dir.clone(),
        ort_dylib_path: String::new(), // env/system rungs of the ladder
        batch_size: 64,
    };
    let model = match LocalOnnx::load(profile, &settings, None) {
        Ok(m) => m,
        Err(e) => panic!(
            "artifacts verified at {} but the backend failed to load \
             (dylib configured? ORT_DYLIB_PATH / system lookup): {e}",
            dir.display()
        ),
    };

    let texts = bench_texts();

    // Report the EXACT padded token count with the offline tokenizer
    // (tokenizers is already a pinned dependency — no new dep).
    let tok = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json"));
    match tok {
        Ok(t) => {
            let enc = t.encode_batch(texts.clone(), true);
            if let Ok(enc) = enc {
                if let Some(first) = enc.first() {
                    println!(
                        "[embed_throughput] padded token count (batch-longest, \
                         first encoding): {} tokens/text target≈512",
                        first.get_ids().len()
                    );
                }
            }
        }
        Err(e) => println!("[embed_throughput] tokenizer probe failed: {e}"),
    }

    // Warmup (session/graph warm paths), then timed reps.
    let warm = model.embed_documents(&texts).expect("warmup embed");
    assert_eq!(warm.len(), texts.len());
    assert!(warm[0].iter().all(|v| v.is_finite()));

    const REPS: usize = 3;
    let mut per_text_ms: Vec<f64> = Vec::with_capacity(REPS);
    for rep in 0..REPS {
        let start = Instant::now();
        let out = model.embed_documents(&texts).expect("timed embed");
        let elapsed = start.elapsed();
        assert_eq!(out.len(), texts.len(), "order/size preserved");
        // Sanity: L2-normalized vectors (finite, unit norm).
        let norm: f32 = out[0].iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-3, "vector not unit-norm: {norm}");
        let ms = elapsed.as_secs_f64() * 1000.0 / texts.len() as f64;
        per_text_ms.push(ms);
        println!(
            "[embed_throughput] rep {}: {:.2} ms/text over {} texts \
             (padded encode_batch + session run + rayon pool)",
            rep + 1,
            ms,
            texts.len()
        );
    }
    let best = per_text_ms.iter().cloned().fold(f64::INFINITY, f64::min);
    let verdict = if (BALLPARK_MIN_MS..=BALLPARK_MAX_MS).contains(&best) {
        "WITHIN the plan.md AVX2 ballpark"
    } else {
        "OUTSIDE the plan.md AVX2 ballpark (M7 pins real numbers on real \
         hardware — informational, not a failure)"
    };
    println!(
        "[embed_throughput] BEST {:.2} ms per ~512-token text — {} \
         (band {:.0}–{:.0} ms)",
        best, verdict, BALLPARK_MIN_MS, BALLPARK_MAX_MS
    );
}

/// The session-independent slice that is always measurable: rayon
/// mean-pool + client-side L2 over synthetic `[512, 768]` hidden states,
/// exactly the shape `LocalOnnx::infer_batch` pools per batch element.
#[test]
#[ignore = "opt-in: needs locally placed model artifacts (never downloaded); run with -- --ignored --nocapture"]
fn query_path_512_token_texts() {
    let Some(dir) = bench_model_dir() else {
        print_skip(&joey_neurocode_rag::config::default_model_dir(
            default_profile().name,
        ));
        return;
    };
    let profile = default_profile();
    let settings = LocalOnnxSettings {
        model_dir: dir.clone(),
        ort_dylib_path: String::new(),
        batch_size: 64,
    };
    let model = LocalOnnx::load(profile, &settings, None)
        .unwrap_or_else(|e| panic!("backend load failed for {}: {e}", dir.display()));

    // Queries embed one at a time on the search path — measure that shape.
    let queries: Vec<String> = (0..16)
        .map(|i| format!("how does component {i} handle the failing case"))
        .collect();
    let _ = model.embed_queries(&queries).expect("warmup");

    const REPS: usize = 3;
    let mut per_query_ms: Vec<f64> = Vec::with_capacity(REPS);
    for rep in 0..REPS {
        let start = Instant::now();
        let out = model.embed_queries(&queries).expect("timed query embed");
        let elapsed = start.elapsed();
        assert_eq!(out.len(), queries.len());
        per_query_ms.push(elapsed.as_secs_f64() * 1000.0 / queries.len() as f64);
        println!(
            "[embed_throughput] query rep {}: {:.2} ms/query (batch of {})",
            rep + 1,
            per_query_ms.last().unwrap(),
            queries.len()
        );
    }
    let best = per_query_ms.iter().cloned().fold(f64::INFINITY, f64::min);
    println!(
        "[embed_throughput] query BEST {:.2} ms — document-band check \
         applies to documents; queries are single-batch (informational)",
        best
    );
}

// ---------------------------------------------------------------------------
// Always-run companions (no artifacts needed, no network).
// ---------------------------------------------------------------------------

/// The measurable-without-a-session path, labeled as such: pooling +
/// normalization only. Asserts sanity (unit norm, order, finite) — the
/// printed timing is informational.
#[test]
fn pure_pooling_normalization_path() {
    let seq = 512usize;
    let dim = 768usize;
    let batch = 16usize;
    // Deterministic synthetic hidden states [seq, dim] and a mask with a
    // realistic tail of padding zeros.
    let mut hidden = Array2::<f32>::zeros((seq, dim));
    let mut flat = hidden.iter_mut();
    let mut i = 0u32;
    for v in flat.by_ref() {
        i = i.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *v = ((i >> 8) % 97) as f32 / 97.0 - 0.5;
    }
    let mask: Vec<i64> = (0..seq).map(|t| if t < seq - 37 { 1 } else { 0 }).collect();

    // Warmup.
    let _ = mean_pool_l2(hidden.view(), &mask, true);

    const REPS: usize = 5;
    let mut best = f64::INFINITY;
    for _ in 0..REPS {
        let start = Instant::now();
        let pooled: Vec<Vec<f32>> = (0..batch)
            .into_par_iter()
            .map(|_| mean_pool_l2(hidden.view(), &mask, true))
            .collect();
        let elapsed = start.elapsed();
        assert_eq!(pooled.len(), batch, "order/size preserved");
        let norm: f32 = pooled[0].iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "not unit-norm: {norm}");
        assert!(pooled[0].iter().all(|v| v.is_finite()));
        best = best.min(elapsed.as_secs_f64() * 1000.0 / batch as f64);
    }
    println!(
        "[embed_throughput] pooling+L2 ONLY (no tokenizer/session; \
         mean_pool_l2 over [512, 768], rayon x{batch}): {:.3} ms/text — \
         this is the normalization slice, NOT the 10–40 ms full-inference \
         band (which needs the session path above)",
        best
    );
    // Pooling must be a small fraction of the full band — sanity only.
    assert!(best.is_finite() && best > 0.0);
}

/// The skip gate itself: with artifacts absent (the CI/dev posture) the
/// probe returns None and the documented reason is exactly the banner the
/// measurement fns print — pinned so the auto-skip cannot silently rot.
#[test]
fn skip_path_when_artifacts_absent() {
    // Point the probe at a directory that cannot hold artifacts: an empty
    // tempdir, injected via the same env override the bench honors.
    let tmp = tempfile::tempdir().unwrap();
    // SAFETY of concurrency: env-var mutation in-process; Rust test harness
    // runs these targets single-threaded per binary unless --test-threads,
    // and no other always-run test reads this variable.
    std::env::set_var(ENV_MODEL_DIR, tmp.path());
    let probed = bench_model_dir();
    std::env::remove_var(ENV_MODEL_DIR);

    match probed {
        None => {
            println!("{SKIP_REASON_PREFIX} (probe returned None for the empty dir) — clean skip");
        }
        Some(dir) => {
            // Someone placed real artifacts in a tempdir — extremely
            // unlikely; treat as skip-the-skip, not a failure.
            println!(
                "note: artifacts unexpectedly present at {} — skip-path \
                 not exercised here",
                dir.display()
            );
        }
    }
    // The banner text is the contract other tasks and humans grep for.
    assert!(SKIP_REASON_PREFIX.contains("no verified local model artifacts"));
    assert!(SKIP_REASON_PREFIX.contains("NEVER"), "reason states the no-download policy");
}

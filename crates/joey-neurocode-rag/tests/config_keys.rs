//! Contract test: the full 18-key `neurocode.rag.*` table with exact
//! defaults, `.env` routing for `neurocode.rag.api_key`, `~` expansion for
//! `local.model_dir`, and validation/clamp rules.
//!
//! Pins `specs/021-please-enhance-neurocode/contracts/rag-config-keys.md`
//! against silent additions, renames, or default drift (constitution
//! Principle VII regression obligation, T002).
//!
//! Every test takes `joey_core::constants::TEST_HOME_OVERRIDE_LOCK` and a
//! `HomeOverrideGuard` over a temp dir so `joey_home()` (model-dir default,
//! `.env` path) and the process env (`JOEY_NEUROCODE_RAG_API_KEY`) are
//! isolated and serialized — no cross-test or `~/.joey` pollution.

use std::path::Path;
use std::sync::MutexGuard;

use joey_core::config::{is_env_config_key, Config};
use joey_core::constants::{HomeOverrideGuard, TEST_HOME_OVERRIDE_LOCK};
use joey_neurocode_rag::config::{
    is_env_routed_key, set_and_save, RagBackend, RagConfig, RagValueKind, RAG_CONFIG_KEYS,
    ENV_API_KEY_NAME, KEY_API_KEY,
};
use tempfile::TempDir;

/// Serialized temp-home context: holds the shared lock + override guard +
/// tempdir for the test body.
///
/// Field order is load-bearing: Rust drops fields in declaration order, so
/// the guard restores the home override FIRST, then the tempdir is deleted,
/// and only then is the shared lock released — the critical section spans
/// install → body → restore, so no concurrent test can observe or clobber
/// this test's override.
struct TempHome {
    _guard: HomeOverrideGuard,
    _dir: TempDir,
    _lock: MutexGuard<'static, ()>,
}

fn temp_home() -> TempHome {
    // Poisoning only indicates a prior panicking test, not external input.
    let lock = TEST_HOME_OVERRIDE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("sub")).ok(); // ensure writable
    let guard = HomeOverrideGuard::new(dir.path().to_path_buf());
    TempHome {
        _lock: lock,
        _dir: dir,
        _guard: guard,
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// Config from a config.yaml written into the temp home.
fn config_with(ctx: &TempHome, yaml: &str) -> Config {
    let path = ctx._dir.path().join("config.yaml");
    std::fs::write(&path, yaml).expect("write config.yaml");
    Config::load_from(path).expect("load config")
}

fn fresh_config(ctx: &TempHome) -> Config {
    // Nonexistent path → empty user doc → pure defaults (no .env side
    // effects: load_from never touches the dotenv layer).
    Config::load_from(ctx._dir.path().join("does-not-exist.yaml")).expect("fresh config")
}

// ─── 1. Defaults: fresh config yields EXACTLY the contract table ─────────────

#[test]
fn fresh_config_yields_exact_contract_defaults() {
    let ctx = temp_home();
    std::env::remove_var(ENV_API_KEY_NAME);
    let cfg = fresh_config(&ctx);
    let rag = RagConfig::load(&cfg);

    // The 18-key enumeration: count, uniqueness, namespace, exact defaults.
    assert_eq!(RAG_CONFIG_KEYS.len(), 18, "contract table must have 18 keys");
    let mut seen = std::collections::HashSet::new();
    for spec in RAG_CONFIG_KEYS.iter() {
        assert!(
            spec.key.starts_with("neurocode.rag."),
            "key {} outside the neurocode.rag.* namespace",
            spec.key
        );
        assert!(seen.insert(spec.key), "duplicate key {}", spec.key);
    }

    // Scalar defaults, key by key, exactly as the contract table states.
    assert!(!rag.enabled, "neurocode.rag.enabled default false");
    assert_eq!(rag.backend, RagBackend::Auto, "backend default auto");
    assert_eq!(
        rag.base_url,
        "http://localhost:11434",
        "neurocode.rag.base_url default"
    );
    assert_eq!(
        rag.model,
        "nomic-embed-text-v1.5",
        "neurocode.rag.model default"
    );
    assert_eq!(rag.api_key, "", "neurocode.rag.api_key default empty");
    assert_eq!(
        rag.mirror_url, "",
        "neurocode.rag.local.mirror_url default empty (fetch disabled)"
    );
    assert_eq!(
        rag.ort_dylib_path, "",
        "neurocode.rag.local.ort_dylib_path default empty"
    );
    assert_eq!(rag.batch_size, 64, "neurocode.rag.batch_size default 64");
    assert_eq!(rag.top_k, 10, "neurocode.rag.top_k default 10");
    assert_eq!(
        rag.context_window_lines, 20,
        "neurocode.rag.context_window_lines default 20"
    );
    assert_eq!(
        rag.relation_max_depth, 2,
        "neurocode.rag.relation_max_depth default 2"
    );
    assert!(
        rag.include_fallback_chunks,
        "neurocode.rag.include_fallback_chunks default true"
    );
    assert_eq!(
        rag.quantize_threshold, 100000,
        "neurocode.rag.quantize_threshold default 100000"
    );
    assert!(
        !rag.prefetch_enabled,
        "neurocode.rag.prefetch.enabled default false"
    );
    assert_eq!(
        rag.refresh_max_files_per_turn, 50,
        "neurocode.rag.refresh.max_files_per_turn default 50"
    );
    assert_eq!(
        rag.refresh_max_bytes_per_turn, 52428800,
        "neurocode.rag.refresh.max_bytes_per_turn default 52428800 (50 MiB)"
    );
    assert_eq!(rag.timeout_secs, 30, "neurocode.rag.timeout_secs default 30");
}

#[test]
fn model_dir_default_is_profile_keyed_under_joey_home() {
    let ctx = temp_home();
    std::env::remove_var(ENV_API_KEY_NAME);
    let rag = RagConfig::load(&fresh_config(&ctx));
    // Contract default: ~/.joey/neurocode/models/<profile>/ — resolved via
    // joey_home() so profile scoping is honored (here: the temp override).
    let expected = ctx
        ._dir
        .path()
        .join("neurocode")
        .join("models")
        .join("nomic-embed-text-v1.5");
    assert_eq!(rag.model_dir, expected);
}

#[test]
fn every_table_row_maps_to_a_loaded_value_or_path_default() {
    let ctx = temp_home();
    std::env::remove_var(ENV_API_KEY_NAME);
    let cfg = config_with(
        &ctx,
        "neurocode:\n  rag:\n    enabled: true\n    top_k: 3\n",
    );
    // Round-trip a couple of overrides through the plain dotted getters to
    // prove readability via the existing accessor API (contract: Readability).
    assert_eq!(cfg.get_bool("neurocode.rag.enabled", false), true);
    assert_eq!(cfg.get_i64("neurocode.rag.top_k", 10), 3);
    let rag = RagConfig::load(&cfg);
    assert!(rag.enabled);
    assert_eq!(rag.top_k, 3);
}

// ─── 2. Validation / clamping: out-of-range never crashes ───────────────────

#[test]
fn batch_size_out_of_range_falls_back_to_default() {
    let ctx = temp_home();
    std::env::remove_var(ENV_API_KEY_NAME);
    // 15 (below 16) and 999 (above 128) → warning + fallback to 64 (the
    // contract says fallback, NOT clamp). Boundaries 16 and 128 pass.
    let rag = RagConfig::load(&config_with(
        &ctx,
        "neurocode:\n  rag:\n    batch_size: 15\n",
    ));
    assert_eq!(rag.batch_size, 64, "below-range batch_size falls back");
    let rag = RagConfig::load(&config_with(
        &ctx,
        "neurocode:\n  rag:\n    batch_size: 999\n",
    ));
    assert_eq!(rag.batch_size, 64, "above-range batch_size falls back");
    let rag = RagConfig::load(&config_with(
        &ctx,
        "neurocode:\n  rag:\n    batch_size: 16\n",
    ));
    assert_eq!(rag.batch_size, 16, "lower boundary accepted");
    let rag = RagConfig::load(&config_with(
        &ctx,
        "neurocode:\n  rag:\n    batch_size: 128\n",
    ));
    assert_eq!(rag.batch_size, 128, "upper boundary accepted");
}

#[test]
fn context_window_lines_clamps_0_200() {
    let ctx = temp_home();
    std::env::remove_var(ENV_API_KEY_NAME);
    let rag = RagConfig::load(&config_with(
        &ctx,
        "neurocode:\n  rag:\n    context_window_lines: 5000\n",
    ));
    assert_eq!(rag.context_window_lines, 200, "clamped to max");
    let rag = RagConfig::load(&config_with(
        &ctx,
        "neurocode:\n  rag:\n    context_window_lines: -7\n",
    ));
    assert_eq!(rag.context_window_lines, 0, "clamped to min");
    let rag = RagConfig::load(&config_with(
        &ctx,
        "neurocode:\n  rag:\n    context_window_lines: 137\n",
    ));
    assert_eq!(rag.context_window_lines, 137, "in-range value preserved");
}

#[test]
fn relation_max_depth_clamps_0_2() {
    let ctx = temp_home();
    std::env::remove_var(ENV_API_KEY_NAME);
    let rag = RagConfig::load(&config_with(
        &ctx,
        "neurocode:\n  rag:\n    relation_max_depth: 9\n",
    ));
    assert_eq!(rag.relation_max_depth, 2, "clamped to max");
    let rag = RagConfig::load(&config_with(
        &ctx,
        "neurocode:\n  rag:\n    relation_max_depth: -1\n",
    ));
    assert_eq!(rag.relation_max_depth, 0, "clamped to min");
}

#[test]
fn unknown_backend_falls_back_to_auto() {
    let ctx = temp_home();
    std::env::remove_var(ENV_API_KEY_NAME);
    let rag = RagConfig::load(&config_with(
        &ctx,
        "neurocode:\n  rag:\n    backend: \"gpt4all\"\n",
    ));
    assert_eq!(rag.backend, RagBackend::Auto, "unknown backend → auto");
    // All four contract enum members round-trip.
    for (raw, expect) in [
        ("auto", RagBackend::Auto),
        ("local_onnx", RagBackend::LocalOnnx),
        ("openai_compat", RagBackend::OpenAiCompat),
        ("ollama", RagBackend::Ollama),
    ] {
        let rag = RagConfig::load(&config_with(
            &ctx,
            &format!("neurocode:\n  rag:\n    backend: \"{}\"\n", raw),
        ));
        assert_eq!(rag.backend, expect, "backend {}", raw);
    }
}

// ─── 3. `.env` routing for `neurocode.rag.api_key` ───────────────────────────

#[test]
fn api_key_set_persists_to_env_not_config_yaml() {
    let ctx = temp_home();
    std::env::remove_var(ENV_API_KEY_NAME);
    let mut cfg = fresh_config(&ctx);

    set_and_save(&mut cfg, KEY_API_KEY, "sk-neurocode-test-123")
        .expect("set api_key routes to .env");

    let env_file = ctx._dir.path().join(".env");
    let env_body = read(&env_file);
    assert!(
        env_body.contains(&format!("{}=", ENV_API_KEY_NAME)),
        ".env must define {} — got: {:?}",
        ENV_API_KEY_NAME,
        env_body
    );
    assert!(
        env_body.contains("sk-neurocode-test-123"),
        ".env must carry the value"
    );

    // And it must NOT land in config.yaml.
    let yaml_body = read(&ctx._dir.path().join("does-not-exist.yaml"));
    assert_eq!(yaml_body, "", "fresh config never wrote config.yaml");

    // The generic joey-core rule does NOT route dotted keys — document why
    // this module implements the routing at its own save/set layer.
    assert!(
        !is_env_config_key("neurocode.rag.api_key"),
        "dotted keys never route in joey-core; routing is implemented here"
    );
    assert!(is_env_routed_key("neurocode.rag.api_key"));
}

#[test]
fn api_key_read_resolves_env_first_then_dotted_fallback() {
    let ctx = temp_home();
    // Route a value to .env (also sets the process env var) ��
    let mut cfg = fresh_config(&ctx);
    set_and_save(&mut cfg, KEY_API_KEY, "sk-from-env").expect("route to .env");
    let rag = RagConfig::load(&fresh_config(&ctx));
    assert_eq!(rag.api_key, "sk-from-env", "env value wins after routing");

    // … while a plain config.yaml value (no env var set) is still readable
    // through the same dotted-path getter fallback.
    std::env::remove_var(ENV_API_KEY_NAME);
    let rag = RagConfig::load(&config_with(
        &ctx,
        "neurocode:\n  rag:\n    api_key: \"sk-from-yaml\"\n",
    ));
    assert_eq!(rag.api_key, "sk-from-yaml", "dotted getter fallback");
    std::env::remove_var(ENV_API_KEY_NAME);
}

#[test]
fn sibling_keys_persist_to_config_yaml_not_env() {
    let ctx = temp_home();
    std::env::remove_var(ENV_API_KEY_NAME);
    let mut cfg = fresh_config(&ctx);
    set_and_save(&mut cfg, "neurocode.rag.top_k", "25").expect("set top_k");

    let yaml_path = ctx._dir.path().join("does-not-exist.yaml");
    let yaml_body = read(&yaml_path);
    assert!(
        yaml_body.contains("top_k: 25"),
        "non-api_key keys land in config.yaml — got: {:?}",
        yaml_body
    );
    let env_body = read(&ctx._dir.path().join(".env"));
    assert!(
        !env_body.contains("top_k"),
        "top_k must never land in .env — got: {:?}",
        env_body
    );
    // And the written value survives a reload.
    let rag = RagConfig::load(&Config::load_from(yaml_path).expect("reload"));
    assert_eq!(rag.top_k, 25);
}

// ─── 4. `~` expansion for local.model_dir ────────────────────────────────────

#[test]
fn model_dir_tilde_expands_at_load_time() {
    let ctx = temp_home();
    std::env::remove_var(ENV_API_KEY_NAME);
    let home = joey_core::user_home_dir();
    let rag = RagConfig::load(&config_with(
        &ctx,
        "neurocode:\n  rag:\n    local:\n      model_dir: \"~/custom/models\"\n",
    ));
    assert_eq!(
        rag.model_dir,
        home.join("custom").join("models"),
        "leading ~/ expands to the OS user home"
    );
    // Absolute paths pass through untouched.
    let rag = RagConfig::load(&config_with(
        &ctx,
        "neurocode:\n  rag:\n    local:\n      model_dir: \"/opt/neurocode/models\"\n",
    ));
    assert_eq!(rag.model_dir.to_str(), Some("/opt/neurocode/models"));
}

// ─── 5. Table-kind sanity: the enumeration is well-formed ────────────────────

#[test]
fn contract_table_kinds_match_expected_types() {
    let by_key = |k: &str| RAG_CONFIG_KEYS.iter().find(|s| s.key == k).unwrap();
    assert_eq!(by_key("neurocode.rag.enabled").kind, RagValueKind::Bool);
    assert_eq!(by_key("neurocode.rag.backend").kind, RagValueKind::Backend);
    assert_eq!(by_key("neurocode.rag.base_url").kind, RagValueKind::Str);
    assert_eq!(by_key("neurocode.rag.model").kind, RagValueKind::Str);
    assert_eq!(by_key("neurocode.rag.api_key").kind, RagValueKind::Str);
    assert_eq!(
        by_key("neurocode.rag.local.model_dir").kind,
        RagValueKind::Path
    );
    assert_eq!(
        by_key("neurocode.rag.local.mirror_url").kind,
        RagValueKind::Str
    );
    assert_eq!(
        by_key("neurocode.rag.local.ort_dylib_path").kind,
        RagValueKind::Str
    );
    assert_eq!(by_key("neurocode.rag.batch_size").kind, RagValueKind::Int);
    assert_eq!(by_key("neurocode.rag.top_k").kind, RagValueKind::Int);
    assert_eq!(
        by_key("neurocode.rag.context_window_lines").kind,
        RagValueKind::Int
    );
    assert_eq!(
        by_key("neurocode.rag.relation_max_depth").kind,
        RagValueKind::Int
    );
    assert_eq!(
        by_key("neurocode.rag.include_fallback_chunks").kind,
        RagValueKind::Bool
    );
    assert_eq!(
        by_key("neurocode.rag.quantize_threshold").kind,
        RagValueKind::Int
    );
    assert_eq!(
        by_key("neurocode.rag.prefetch.enabled").kind,
        RagValueKind::Bool
    );
    assert_eq!(
        by_key("neurocode.rag.refresh.max_files_per_turn").kind,
        RagValueKind::Int
    );
    assert_eq!(
        by_key("neurocode.rag.refresh.max_bytes_per_turn").kind,
        RagValueKind::Int
    );
    assert_eq!(by_key("neurocode.rag.timeout_secs").kind, RagValueKind::Int);
}

// ─── 6. Copilot extension default (Joey-native, outside the 18-key table) ───

#[test]
fn copilot_model_defaults_to_text_embedding_3_small() {
    let _ctx = temp_home();
    std::env::remove_var(ENV_API_KEY_NAME);
    let rag = RagConfig::load(&Config::defaults());
    assert_eq!(rag.copilot_model, "text-embedding-3-small");
}

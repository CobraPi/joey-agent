//! Contract test: the 5-key `neurocode.memory.*` table (feature 027) with
//! exact defaults, clamp bounds, and additivity vs `neurocode.rag.*`.
//!
//! Pins `specs/027-please-enhance-neurocode/contracts/
//! neurocode-memory-config-keys.md` against silent additions, renames, or
//! default drift (constitution Principle VII regression obligation).
//!
//! Follows the `tests/config_keys.rs` pattern: every test takes
//! `joey_core::constants::TEST_HOME_OVERRIDE_LOCK` and a `HomeOverrideGuard`
//! over a temp dir so `joey_home()` and the process env are isolated and
//! serialized — no cross-test or `~/.joey` pollution.

use std::sync::MutexGuard;

use joey_core::config::Config;
use joey_core::constants::{HomeOverrideGuard, TEST_HOME_OVERRIDE_LOCK};
use joey_neurocode_rag::config::{MemoryConfig, RagConfig, RagValueKind, MEMORY_CONFIG_KEYS};
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
fn defaults_load_from_empty_config() {
    let ctx = temp_home();
    let cfg = fresh_config(&ctx);
    let mem = MemoryConfig::load(&cfg);
    assert!(!mem.enabled, "neurocode.memory.enabled default false");
    assert_eq!(mem.top_k, 5, "neurocode.memory.top_k default 5");
    assert_eq!(
        mem.injection_char_limit, 2048,
        "neurocode.memory.injection_char_limit default 2048"
    );
    assert_eq!(
        mem.max_episodes, 500,
        "neurocode.memory.max_episodes default 500"
    );
    assert_eq!(
        mem.distill_model, "",
        "neurocode.memory.distill_model default empty (economical tier)"
    );
    // `Default` impl agrees with `load` on pure defaults.
    assert_eq!(mem, MemoryConfig::default());
}

// ─── 2. Validation / clamping: out-of-range never crashes ───────────────────

#[test]
fn ints_clamp_to_contract_bounds() {
    let ctx = temp_home();

    // top_k: 1..=20
    let mem = MemoryConfig::load(&config_with(
        &ctx,
        "neurocode:\n  memory:\n    top_k: 0\n",
    ));
    assert_eq!(mem.top_k, 1, "top_k 0 clamps up to 1");
    let mem = MemoryConfig::load(&config_with(
        &ctx,
        "neurocode:\n  memory:\n    top_k: 100\n",
    ));
    assert_eq!(mem.top_k, 20, "top_k 100 clamps down to 20");

    // injection_char_limit: 256..=8192
    let mem = MemoryConfig::load(&config_with(
        &ctx,
        "neurocode:\n  memory:\n    injection_char_limit: 10\n",
    ));
    assert_eq!(
        mem.injection_char_limit, 256,
        "injection_char_limit 10 clamps up to 256"
    );
    let mem = MemoryConfig::load(&config_with(
        &ctx,
        "neurocode:\n  memory:\n    injection_char_limit: 99999\n",
    ));
    assert_eq!(
        mem.injection_char_limit, 8192,
        "injection_char_limit 99999 clamps down to 8192"
    );

    // max_episodes: 50..=10000
    let mem = MemoryConfig::load(&config_with(
        &ctx,
        "neurocode:\n  memory:\n    max_episodes: 1\n",
    ));
    assert_eq!(mem.max_episodes, 50, "max_episodes 1 clamps up to 50");
    let mem = MemoryConfig::load(&config_with(
        &ctx,
        "neurocode:\n  memory:\n    max_episodes: 999999\n",
    ));
    assert_eq!(
        mem.max_episodes, 10000,
        "max_episodes 999999 clamps down to 10000"
    );
}

// ─── 3. Absent keys fall back to the documented defaults ────────────────────

#[test]
fn absent_keys_fall_back_to_defaults() {
    let ctx = temp_home();
    // A config with other keys set but NONE of the neurocode.memory.* keys.
    let cfg = config_with(
        &ctx,
        "neurocode:\n  rag:\n    enabled: true\n    top_k: 7\n",
    );
    let mem = MemoryConfig::load(&cfg);
    assert_eq!(mem, MemoryConfig::default(), "all five keys default");
}

// ─── 4. Table-kind sanity: the enumeration is well-formed ────────────────────

#[test]
fn contract_table_matches_keys_and_defaults() {
    assert_eq!(MEMORY_CONFIG_KEYS.len(), 5, "contract table must have 5 keys");
    let expected = [
        ("neurocode.memory.enabled", RagValueKind::Bool, "false"),
        ("neurocode.memory.top_k", RagValueKind::Int, "5"),
        ("neurocode.memory.injection_char_limit", RagValueKind::Int, "2048"),
        ("neurocode.memory.max_episodes", RagValueKind::Int, "500"),
        ("neurocode.memory.distill_model", RagValueKind::Str, ""),
    ];
    for (spec, (key, kind, default)) in MEMORY_CONFIG_KEYS.iter().zip(expected) {
        assert_eq!(spec.key, key, "key");
        assert_eq!(spec.kind, kind, "kind for {}", key);
        assert_eq!(spec.default, default, "default repr for {}", key);
    }
    // No duplicates.
    let mut seen = std::collections::HashSet::new();
    for spec in MEMORY_CONFIG_KEYS.iter() {
        assert!(seen.insert(spec.key), "duplicate key {}", spec.key);
    }
}

// ─── 5. Additivity: memory keys and rag keys never cross-contaminate ────────

#[test]
fn additive_no_cross_effect() {
    let ctx = temp_home();

    // Setting all five neurocode.memory.* keys leaves RagConfig untouched.
    let rag_baseline = RagConfig::load(&fresh_config(&ctx));
    let cfg = config_with(
        &ctx,
        concat!(
            "neurocode:\n",
            "  memory:\n",
            "    enabled: true\n",
            "    top_k: 20\n",
            "    injection_char_limit: 8192\n",
            "    max_episodes: 10000\n",
            "    distill_model: \"gpt-distill-x\"\n",
        ),
    );
    let rag = RagConfig::load(&cfg);
    assert_eq!(
        rag, rag_baseline,
        "neurocode.memory.* keys must not change any RagConfig field"
    );

    // The inverse: neurocode.rag.enabled=true leaves MemoryConfig.enabled
    // at its default false.
    let mem = MemoryConfig::load(&config_with(
        &ctx,
        "neurocode:\n  rag:\n    enabled: true\n",
    ));
    assert!(
        !mem.enabled,
        "neurocode.rag.enabled must not leak into MemoryConfig.enabled"
    );
}

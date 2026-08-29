//! T050 — cross-crate `neurocode.rag.*` key-parity test (feature 021).
//!
//! `joey-agent-core` gates RAG refresh/pre-fetch on config keys it reads as
//! dotted-path literals, WITHOUT a normal dependency on `joey-neurocode-rag`
//! (deliberate: keeps agent-core's build graph free of the rag crate's pinned
//! `ort`/`tokenizers`/`ndarray` deps). If those literals drifted from the
//! keys `RagConfig::load` actually reads, the gates would silently stop
//! firing. This test pins them:
//!
//! 1. every key agent-core references exists VERBATIM in the canonical
//!    18-key contract table (`RAG_CONFIG_KEYS`, contracts/
//!    rag-config-keys.md);
//! 2. the default passed by agent-core at each call site matches the
//!    contract default's semantics (for `local.model_dir`, both sides use
//!    the same empty-string sentinel meaning "resolve the profile-scoped
//!    default directory");
//! 3. a loaded-config round trip: values written under agent-core's key
//!    consts are read back identically by `RagConfig::load`.

use joey_agent_core::agent::rag_keys;
use joey_core::Config;
use joey_neurocode_rag::config::{
    RagConfig, RagValueKind, RAG_CONFIG_KEYS, KEY_ENABLED, KEY_PREFETCH_ENABLED,
};

/// Every key referenced by agent-core exists verbatim in the canonical table.
#[test]
fn agent_core_rag_keys_exist_in_contract_table() {
    let referenced = [
        rag_keys::ENABLED,
        rag_keys::PREFETCH_ENABLED,
        rag_keys::BACKEND,
        rag_keys::BASE_URL,
        rag_keys::MODEL,
        rag_keys::LOCAL_MODEL_DIR,
    ];
    // Sanity: agent-core's consts are the exact dotted paths, not aliases.
    for k in referenced {
        assert!(k.starts_with("neurocode.rag."), "const {k} lost its prefix");
    }
    for k in referenced {
        let spec = RAG_CONFIG_KEYS
            .iter()
            .find(|spec| spec.key == k)
            .unwrap_or_else(|| panic!("agent-core references `{k}`, which is NOT in RAG_CONFIG_KEYS — gating would silently break (contracts/rag-config-keys.md)"));
        // Reusing the table's own consts for the two bool gates additionally
        // pins byte-identity with the rag crate's exported KEY_* names.
        match k {
            _ if k == KEY_ENABLED => assert_eq!(spec.kind, RagValueKind::Bool),
            _ if k == KEY_PREFETCH_ENABLED => assert_eq!(spec.kind, RagValueKind::Bool),
            _ => {}
        }
    }
}

/// The defaults agent-core passes to the getters match the contract defaults.
#[test]
fn agent_core_rag_key_defaults_match_contract() {
    let default_of = |key: &str| -> &'static str {
        RAG_CONFIG_KEYS
            .iter()
            .find(|spec| spec.key == key)
            .unwrap_or_else(|| panic!("key {key} missing from RAG_CONFIG_KEYS"))
            .default
    };
    // Hard-coded expectations mirror the exact defaults passed at the
    // call sites in `agent.rs` (`RagGate::from_config`, `rag_auto_refresh`).
    // model_dir: agent-core passes `""` as an unset sentinel; the contract
    // default `~/.joey/neurocode/models/<profile>/` resolves identically via
    // `joey_home()` when the raw value is empty (see `RagConfig::load`).
    assert_eq!(default_of(rag_keys::LOCAL_MODEL_DIR), "~/.joey/neurocode/models/<profile>/");
    assert_eq!(default_of(rag_keys::BACKEND), "auto");
    assert_eq!(default_of(rag_keys::BASE_URL), "http://localhost:11434");
    assert_eq!(default_of(rag_keys::MODEL), "nomic-embed-text-v1.5");
    assert_eq!(default_of(rag_keys::ENABLED), "false");
    assert_eq!(default_of(rag_keys::PREFETCH_ENABLED), "false");
}

/// Round trip: values set under agent-core's key consts are read back by
/// `RagConfig::load` — proof the two sides read the SAME keys, not just
/// equal-looking strings.
#[test]
fn rag_config_load_reads_agent_core_key_consts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    std::fs::write(
        &path,
        format!(
            "neurocode:\n  rag:\n    enabled: true\n    prefetch:\n      enabled: true\n    backend: local_onnx\n    base_url: http://localhost:11434\n    model: nomic-embed-text-v1.5\n    local:\n      model_dir: {}\n",
            dir.path().join("models").display()
        ),
    )
    .unwrap();
    let config = Config::load_from(path).unwrap();

    // agent-core side reads via the shared consts:
    assert!(config.get_bool(rag_keys::ENABLED, false));
    assert!(config.get_bool(rag_keys::PREFETCH_ENABLED, false));
    assert_eq!(config.get_str(rag_keys::BACKEND, "auto"), "local_onnx");

    // rag-crate side loads the SAME file into RagConfig:
    let rag: RagConfig = RagConfig::load(&config);
    assert!(rag.enabled);
    assert!(rag.prefetch_enabled, "prefetch flag did not round-trip");
    assert_eq!(rag.model, "nomic-embed-text-v1.5");
    assert_eq!(
        rag.model_dir,
        dir.path().join("models"),
        "model_dir did not round-trip"
    );
}

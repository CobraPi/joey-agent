//! T013 — adaptive-memory injection tests (feature 027, spec
//! specs/027-please-enhance-neurocode, contracts/
//! neurocode-memory-injection.md).
//!
//! Two layers are pinned here without any provider transport:
//!
//! 1. `format_memory_block`/`clamp_char_limit` (the format-level contract:
//!    section order, whole-entry truncation, empty-input omission, bounds);
//! 2. the `effective_system_prompt` append path via `Agent::new` +
//!    `set_history` + `set_memory_runtime` (the double gate: config key +
//!    runtime-enabled, and the FR-010 byte-identical noop when disabled).
//!
//! Note: agent-core appends the runtime's block verbatim by design — the
//! char cap is enforced runtime-side in joey-cli (T012); tests 2/4/5 pin
//! that logic at the format level only.

use std::path::Path;
use std::sync::{Arc, Mutex};

use joey_agent_core::memory_hook::{clamp_char_limit, format_memory_block};
use joey_agent_core::{Agent, AgentConfig, MemoryRuntime, MemoryTurnSummary};
use joey_core::Config;
use joey_providers::Message;
use joey_tools::{ToolContext, ToolRegistry};

/// Serialize tests that install the process-global joey-home override
/// (same pattern as the in-file agent.rs test module).
fn lock() -> std::sync::MutexGuard<'static, ()> {
    joey_core::constants::TEST_HOME_OVERRIDE_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// A MemoryRuntime stub: always enabled, prefetch answers a fixed block
/// (None = omit), capture records clones of every summary.
struct StubRuntime {
    block: Option<String>,
    captured: Mutex<Vec<MemoryTurnSummary>>,
}

impl StubRuntime {
    fn new(block: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            block,
            captured: Mutex::new(Vec::new()),
        })
    }
}

impl MemoryRuntime for StubRuntime {
    fn enabled(&self) -> bool {
        true
    }
    fn prefetch_block(&self, _prompt: &str) -> Option<String> {
        self.block.clone()
    }
    fn capture_turn(&self, summary: &MemoryTurnSummary) {
        self.captured.lock().unwrap().push(summary.clone());
    }
}

/// Build an agent whose `ToolContext` config comes from YAML at `home`,
/// sharing caller-provided home + cwd paths so two agents can be compared
/// byte-for-byte (mirrors the in-file `rag_agent_at` precedent).
fn agent_at(_home: &Path, cwd: &Path, config: Config) -> Agent {
    let ctx = ToolContext::new(cwd.to_path_buf(), config, "memory-test-session");
    let registry = ToolRegistry::new();
    let agent_cfg = AgentConfig {
        model: "test-model".to_string(),
        provider: "openrouter".to_string(),
        base_url: "https://openrouter.ai/api/v1".to_string(),
        api_key: None,
        max_turns: 10,
        api_max_retries: 3,
        tool_delay: 0.0,
        reasoning: None,
        enabled_tools: vec![],
        max_tokens: None,
        stream: false,
        pass_session_id: false,
        model_pinned: false,
    };
    Agent::new(agent_cfg, registry, ctx).expect("agent")
}

/// Write YAML into `home`'s config.yaml and load it (the
/// `rag_config_key_parity.rs` / `Config::load_from` fixture pattern).
fn yaml_config(home: &Path, yaml: &str) -> Config {
    let path = home.join("config.yaml");
    std::fs::write(&path, yaml).unwrap();
    Config::load_from(path).unwrap()
}

fn seeded_history() -> Vec<Message> {
    vec![
        Message::user("what did we work on yesterday"),
        Message::assistant("we patched the parser"),
    ]
}

// ── format_memory_block / clamp_char_limit ───────────────────────────

/// Preferences section precedes episodes; entries render as "- " lines in
/// input order.
#[test]
fn block_formats_sections_and_entries() {
    let prefs = ["pref-alpha".to_string(), "pref-beta".to_string()];
    let eps = ["epi-one".to_string(), "epi-two".to_string()];
    let block = format_memory_block(&prefs, &eps, 8192);
    let prefs_at = block
        .find("## Learned preferences (applied automatically)")
        .expect("preferences header present");
    let eps_at = block
        .find("## Relevant past episodes")
        .expect("episodes header present");
    assert!(prefs_at < eps_at, "preferences section must come first");
    // Entries as "- " lines in input order.
    let a = block.find("- pref-alpha\n").expect("pref-alpha line");
    let b = block.find("- pref-beta\n").expect("pref-beta line");
    let c = block.find("- epi-one\n").expect("epi-one line");
    let d = block.find("- epi-two\n").expect("epi-two line");
    assert!(a < b && b < c && c < d, "entries in input order");
}

/// Truncation drops WHOLE entries: a cap admitting only the header + first
/// entry never leaks a fragment of the second.
#[test]
fn block_truncates_whole_entries_only() {
    let prefs = ["first-marker-AAA".to_string(), "second-marker-BBB".to_string()];
    // Cap = header line + first entry line, exactly.
    let cap = "## Learned preferences (applied automatically)".len()
        + 1
        + "- first-marker-AAA\n".len();
    let block = format_memory_block(&prefs, &[], cap);
    assert!(block.contains("- first-marker-AAA\n"), "first entry fits");
    assert!(
        !block.contains("second-marker-BBB"),
        "second entry dropped whole — no marker fragment anywhere"
    );
}

/// All-empty inputs yield an empty String (caller treats empty as omit).
#[test]
fn block_empty_inputs_yield_empty() {
    assert!(format_memory_block(&[], &[], 8192).is_empty());
}

/// A cap smaller than any entry omits the section entirely — the header
/// never leaks without content.
#[test]
fn block_section_omitted_when_no_entry_fits() {
    let eps = ["way-too-long-entry-marker".to_string()];
    let block = format_memory_block(&[], &eps, 10);
    assert!(
        !block.contains("## Learned preferences (applied automatically)"),
        "no preferences header without a fitting entry"
    );
    assert!(
        !block.contains("## Relevant past episodes"),
        "no episodes header without a fitting entry"
    );
    assert!(block.is_empty());
}

/// Contract bounds: 256 floor, 8192 ceiling, in-range passthrough.
#[test]
fn clamp_char_limit_bounds() {
    assert_eq!(clamp_char_limit(10), 256);
    assert_eq!(clamp_char_limit(99999), 8192);
    assert_eq!(clamp_char_limit(2048), 2048);
}

// ── effective_system_prompt injection gate ───────────────────────────

/// FR-010: with the config gate closed (`neurocode.memory.enabled` false
/// or absent), an installed+enabled runtime is a byte-identical noop —
/// the effective prompt equals the no-runtime agent's and carries no
/// "Learned preferences" text.
#[test]
fn disabled_config_is_byte_identical_noop() {
    let _l = lock();
    let home = tempfile::tempdir().unwrap();
    let guard = joey_core::constants::HomeOverrideGuard::new(home.path().to_path_buf());
    let cwd = tempfile::tempdir().unwrap();
    for config in [
        yaml_config(home.path(), "neurocode:\n  memory:\n    enabled: false\n"),
        Config::defaults(), // key absent entirely
    ] {
        let mut with_rt = agent_at(home.path(), cwd.path(), config.clone());
        with_rt.set_history(seeded_history());
        with_rt.set_memory_runtime(Some(StubRuntime::new(Some(
            "## Learned preferences (applied automatically)\n- MUST NOT APPEAR\n".to_string(),
        ))));
        let mut no_rt = agent_at(home.path(), cwd.path(), config);
        no_rt.set_history(seeded_history());
        let with = with_rt.effective_system_prompt();
        let without = no_rt.effective_system_prompt();
        assert_eq!(with, without, "disabled config: byte-identical noop");
        assert!(
            !with.contains("Learned preferences"),
            "no memory text with the gate closed"
        );
    }
    let _ = guard;
}

/// Enabled config: a Some(block) runtime appends its block verbatim; a
/// None runtime stays byte-identical to the no-runtime baseline.
#[test]
fn prefetch_block_appended_when_enabled() {
    let _l = lock();
    let home = tempfile::tempdir().unwrap();
    let guard = joey_core::constants::HomeOverrideGuard::new(home.path().to_path_buf());
    let cwd = tempfile::tempdir().unwrap();
    let config = yaml_config(home.path(), "neurocode:\n  memory:\n    enabled: true\n");

    let mut baseline = agent_at(home.path(), cwd.path(), config.clone());
    baseline.set_history(seeded_history());
    let baseline_prompt = baseline.effective_system_prompt();

    let mut with_block = agent_at(home.path(), cwd.path(), config.clone());
    with_block.set_history(seeded_history());
    with_block.set_memory_runtime(Some(StubRuntime::new(Some(
        "MEMBLOCK-XYZ".to_string(),
    ))));
    assert!(
        with_block.effective_system_prompt().contains("MEMBLOCK-XYZ"),
        "Some(block) runtime appends its block verbatim"
    );

    let mut with_none = agent_at(home.path(), cwd.path(), config);
    with_none.set_history(seeded_history());
    with_none.set_memory_runtime(Some(StubRuntime::new(None)));
    assert_eq!(
        with_none.effective_system_prompt(),
        baseline_prompt,
        "None runtime: byte-identical to the no-runtime baseline"
    );
    let _ = guard;
}

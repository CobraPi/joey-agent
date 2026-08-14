//! T028 — NeuroCode disabled regression test.
//!
//! With enabled=false: is_active() returns false, classify() still works,
//! assemble_context returns cold-mode context (empty graph), and no messages
//! are injected.

use std::path::PathBuf;

use joey_neurocode::classifier::ComplexityTier;
use joey_neurocode::config::NeuroCodeConfig;
use joey_neurocode::engine::{CodingRequest, DefaultEngine, NeuroCodeEngine};

fn make_request(text: &str) -> CodingRequest {
    CodingRequest {
        text: text.into(),
        active_file: None,
        active_symbols: vec![],
        project_root: PathBuf::from("/tmp/test-project"),
        token_budget_hint: 0,
    }
}

#[test]
fn disabled_engine_is_not_active() {
    let cfg = NeuroCodeConfig::default(); // enabled defaults to false
    assert!(!cfg.enabled);
    let engine = DefaultEngine::new(cfg, PathBuf::from("/tmp/test-project"));
    assert!(!engine.is_active(), "default config → disabled");
}

#[test]
fn disabled_engine_still_classifies() {
    // Even when disabled, classify() must still function — the agent-core
    // intercept simply chooses not to *call* it when is_active() is false.
    let cfg = NeuroCodeConfig::default();
    let engine = DefaultEngine::new(cfg, PathBuf::from("/tmp/test-project"));

    assert!(!engine.is_active());

    // But classify() still returns a valid route.
    let route = engine.classify(&make_request("write a JUnit test"));
    assert_eq!(route.tier, ComplexityTier::Economical);
    assert!(!route.overridden);

    // A frontier request still classifies.
    let route = engine.classify(&make_request("refactor architecture concurrency"));
    assert_eq!(route.tier, ComplexityTier::Frontier);
}

#[test]
fn disabled_engine_returns_cold_mode_context() {
    // The graph is lazily opened; with_graph returns None (Default) when the
    // graph is not yet initialized, so assemble_context yields a cold-mode
    // AssembledContext.
    //
    // T065 (FR-015): the project root must contain a Java artifact — a
    // non-Java project would now take the FR-015 fallback branch before the
    // graph check, which is a different (also degenerate) outcome.
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(
        project.path().join("src/App.java"),
        "public class App {}\n",
    )
    .unwrap();
    let cfg = NeuroCodeConfig::default();
    let engine = DefaultEngine::new(cfg, project.path().to_path_buf());

    let request = CodingRequest {
        text: "refactor UserServiceImpl".into(),
        active_file: None,
        active_symbols: vec![],
        project_root: project.path().to_path_buf(),
        token_budget_hint: 0,
    };
    let ctx = engine.assemble_context(&request, ComplexityTier::Frontier);

    // Cold mode because the graph was never opened / is empty.
    assert!(
        ctx.cold_mode,
        "un-indexed/empty graph → cold mode (got cold_mode={})",
        ctx.cold_mode
    );
    assert!(ctx.primary_nodes.is_empty());
    assert!(ctx.expanded_nodes.is_empty());
}

#[test]
fn disabled_engine_cold_context_formatted_text_present() {
    let cfg = NeuroCodeConfig::default();
    let engine = DefaultEngine::new(cfg, PathBuf::from("/tmp/test-project-disabled2"));

    let req = CodingRequest {
        text: "fix the bug".into(),
        active_file: None,
        active_symbols: vec![],
        project_root: PathBuf::from("/tmp/test-project-disabled2"),
        token_budget_hint: 0,
    };
    let ctx = engine.assemble_context(&req, ComplexityTier::Economical);

    // When disabled and un-indexed, the formatted_context should reflect
    // cold/degraded mode (or be empty for the None-graph branch).
    // The agent-core intercept never injects this when is_active() is false.
    assert!(!engine.is_active());
    // formatted_context may be empty (graph not initialized branch) or
    // contain cold-mode text — both are acceptable "no-op" outcomes.
    // The contract is: nothing useful is injected when disabled.
    assert!(
        ctx.cold_mode || ctx.formatted_context.is_empty(),
        "disabled engine should produce cold-mode or empty context"
    );
}

#[test]
fn explicit_disabled_config_is_not_active() {
    let mut cfg = NeuroCodeConfig::default();
    cfg.enabled = false;
    let engine = DefaultEngine::new(cfg, PathBuf::from("/tmp/test-project"));
    assert!(!engine.is_active());

    // Flipping to enabled makes it active.
    let mut cfg = NeuroCodeConfig::default();
    cfg.enabled = true;
    let engine2 = DefaultEngine::new(cfg, PathBuf::from("/tmp/test-project"));
    assert!(engine2.is_active());
}

#[test]
fn resolve_tier_model_none_when_unconfigured() {
    let cfg = NeuroCodeConfig::default();
    let engine = DefaultEngine::new(cfg, PathBuf::from("/tmp/test-project"));
    // No tier models configured → None.
    assert_eq!(engine.resolve_tier_model(), None);
}

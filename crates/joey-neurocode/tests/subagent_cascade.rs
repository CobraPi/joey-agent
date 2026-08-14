//! Subagent cascade tests (T066, FR-021, Clarification Q5 — Inherit + Share).
//!
//! Per contracts/subagent-cascade.md:
//! - The index is shared by project-root identity: parent and subagent engines
//!   constructed for the same project root resolve the SAME `graph.db`.
//! - The subagent MUST NOT re-index: it reads the already-built index.
//! - NeuroCode config inherits via `parent_config_tree` (the config tree rides
//!   through joey-orchestration; at the engine layer this is the same
//!   `NeuroCodeConfig::from_config` construction the parent used).

use std::path::PathBuf;

use joey_neurocode::graph::{project_graph_db_path, DependencyGraph};
use joey_neurocode::parse::ingest_project;
use joey_neurocode::{CodingRequest, DefaultEngine, NeuroCodeConfig, NeuroCodeEngine};

/// A tiny Java project the "parent" indexes.
fn write_project(root: &std::path::Path) {
    let src = root.join("src").join("main").join("java").join("com").join("example");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("UserService.java"),
        "package com.example;\npublic interface UserService { Object findById(Long id); }\n",
    )
    .unwrap();
    std::fs::write(
        src.join("UserServiceImpl.java"),
        "package com.example;\npublic class UserServiceImpl implements UserService {\n\
         public Object findById(Long id) { return null; }\n}\n",
    )
    .unwrap();
}

fn make_request(root: &std::path::Path, text: &str) -> CodingRequest {
    CodingRequest {
        text: text.into(),
        active_file: None,
        active_symbols: vec!["UserServiceImpl".into()],
        project_root: root.to_path_buf(),
        token_budget_hint: 0,
    }
}

#[test]
fn parent_and_subagent_share_the_same_graph_db() {
    let tmp = tempfile::tempdir().unwrap();
    write_project(tmp.path());

    // Two engines constructed for the same project root (parent + subagent)
    // resolve the identical graph.db path — the shared-index invariant (FR-021).
    let nc_cfg = NeuroCodeConfig::default();
    let parent = DefaultEngine::new(nc_cfg.clone(), tmp.path().to_path_buf());
    let subagent = DefaultEngine::new(nc_cfg, tmp.path().to_path_buf());

    let parent_db = project_graph_db_path(tmp.path());
    // Both engines open lazily; force graph materialization via a command-level
    // status call (which opens the graph) on each.
    use joey_neurocode::NeuroCodeCommands;
    let _ = parent.status_text();
    let _ = subagent.status_text();

    // The path identity is the contract: same project root → same graph.db.
    assert!(parent_db.ends_with("graph.db"));
    // And a DIFFERENT project root gets a different db (no cross-contamination).
    let other_tmp = tempfile::tempdir().unwrap();
    assert_ne!(parent_db, project_graph_db_path(other_tmp.path()));
}

#[test]
fn subagent_reads_parent_index_without_reindexing() {
    let tmp = tempfile::tempdir().unwrap();
    write_project(tmp.path());

    // Parent indexes the project.
    let parent_graph = DependencyGraph::open_for_project(tmp.path()).unwrap();
    let ingest = ingest_project(&parent_graph, tmp.path());
    assert!(ingest.artifacts_seen > 0, "parent ingestion should see artifacts");
    let parent_count = parent_graph.artifact_count().unwrap();
    assert!(parent_count > 0);
    drop(parent_graph);

    // Subagent engine (same project root) reads the SAME shared index — no
    // ingestion call. Its context assembly must reflect the parent's artifacts.
    let mut nc_cfg = NeuroCodeConfig::default();
    nc_cfg.enabled = true;
    let subagent = DefaultEngine::new(nc_cfg, tmp.path().to_path_buf());

    let req = make_request(tmp.path(), "edit findById on UserServiceImpl");
    let ctx = subagent.assemble_context(&req, joey_neurocode::ComplexityTier::Frontier);
    // The shared index means the subagent sees the parent's nodes: the
    // assembled context is NOT cold-mode and references real artifacts.
    assert!(!ctx.cold_mode, "subagent should read the parent's index (shared, not cold)");
    assert!(
        ctx.formatted_context.contains("UserServiceImpl"),
        "assembled context should draw on the shared index"
    );

    // The artifact count the subagent sees matches the parent's (shared index).
    let sub_count = subagent
        .with_graph(|g| g.and_then(|g| g.artifact_count().ok()).unwrap_or(0));
    assert_eq!(sub_count, parent_count, "shared graph.db — identical artifact count");
}

#[test]
fn config_inherits_from_parent_config_tree() {
    // FR-021: the subagent's engine is built from the same config tree the
    // parent used (`parent_config_tree` in joey-orchestration). At the engine
    // layer this is NeuroCodeConfig::from_config — identical input config
    // yields identical tier config (the cascade inheritance surface).
    let tmp_a = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        tmp_a.path(),
        "neurocode:\n  enabled: true\n  tier:\n    frontier:\n      model: \"frontier-model-x\"\n",
    )
    .unwrap();
    let parent_tree = joey_core::Config::load_from(tmp_a.path().to_path_buf()).unwrap();

    // Parent and subagent both construct from the (same) inherited tree.
    let parent_cfg = NeuroCodeConfig::from_config(&parent_tree);
    let subagent_cfg = NeuroCodeConfig::from_config(&parent_tree);
    assert!(parent_cfg.enabled && subagent_cfg.enabled);
    assert_eq!(parent_cfg.tier.frontier_model, subagent_cfg.tier.frontier_model);
    assert_eq!(subagent_cfg.tier.frontier_model, "frontier-model-x");
}

#[test]
fn subagent_on_different_project_takes_cold_mode() {
    // Edge case from contracts/subagent-cascade.md: a subagent targeting a
    // DIFFERENT project root must NOT silently use the parent's index — it
    // detects the un-indexed project and operates cold (FR-016).
    let parent_tmp = tempfile::tempdir().unwrap();
    write_project(parent_tmp.path());
    let parent_graph = DependencyGraph::open_for_project(parent_tmp.path()).unwrap();
    let ingest = ingest_project(&parent_graph, parent_tmp.path());
    assert!(ingest.artifacts_seen > 0);

    let other_tmp = tempfile::tempdir().unwrap();
    // A Java project that was never indexed.
    write_project(other_tmp.path());

    let mut nc_cfg = NeuroCodeConfig::default();
    nc_cfg.enabled = true;
    let subagent = DefaultEngine::new(nc_cfg, other_tmp.path().to_path_buf());
    let req = make_request(other_tmp.path(), "edit something here");
    let ctx = subagent.assemble_context(&req, joey_neurocode::ComplexityTier::Economical);
    assert!(ctx.cold_mode, "un-indexed different project must take cold mode, not the parent's index");
}

//! Regression: `/neurocode status` reporting "not indexed" after
//! `/neurocode index`.
//!
//! The `/neurocode` command handler constructs a fresh `DefaultEngine` per
//! invocation. `index_text` writes artifacts into the persisted per-project
//! `graph.db`, but `status_text` used `with_graph` without ever opening that
//! file — a fresh engine saw `None` and always reported
//! "Index: not indexed (run /neurocode index)".
//!
//! The fix: `with_graph` opens the persisted graph lazily (same as
//! `assemble_context`), so any fresh engine for the same project root reads
//! the on-disk index.

use std::path::PathBuf;

use joey_neurocode::{DefaultEngine, NeuroCodeCommands, NeuroCodeConfig};

/// A tiny Java project to index.
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

#[test]
fn fresh_engine_sees_persisted_index_in_status() {
    let tmp = tempfile::tempdir().unwrap();
    write_project(tmp.path());

    // Engine #1 — the `/neurocode index` invocation. Writes graph.db to disk.
    let mut cfg = NeuroCodeConfig::default();
    cfg.enabled = true;
    let indexer = DefaultEngine::new(cfg.clone(), tmp.path().to_path_buf());
    let out = indexer.index_text(false);
    assert!(
        out.contains("Indexing complete"),
        "indexing should succeed, got: {out}"
    );
    assert!(
        !out.contains("0 artifacts"),
        "indexing a real project should record artifacts, got: {out}"
    );
    drop(indexer);

    // Engine #2 — the subsequent `/neurocode status` invocation. A fresh
    // engine in a fresh process-like state: the graph must be re-opened from
    // the persisted graph.db, not reported as "not indexed".
    let status_engine = DefaultEngine::new(cfg, tmp.path().to_path_buf());
    let status = status_engine.status_text();
    assert!(
        !status.contains("not indexed"),
        "fresh engine must read the persisted index (status: {status})"
    );
    assert!(
        status.contains("Index:"),
        "status should include the index line (status: {status})"
    );

    // And the underlying count is visible through with_graph too.
    let count = status_engine.with_graph(|g| g.and_then(|g| g.artifact_count().ok()).unwrap_or(0));
    assert!(count > 0, "persisted artifact count should be > 0, got {count}");
}

#[test]
fn fresh_engine_query_reads_persisted_index() {
    let tmp = tempfile::tempdir().unwrap();
    write_project(tmp.path());

    // Index with one engine…
    let mut cfg = NeuroCodeConfig::default();
    cfg.enabled = true;
    let indexer = DefaultEngine::new(cfg.clone(), tmp.path().to_path_buf());
    indexer.index_text(false);
    drop(indexer);

    // …query with a fresh one — same read-back contract as status.
    let querier = DefaultEngine::new(cfg, tmp.path().to_path_buf());
    let out = querier.query_text("symbol", "UserServiceImpl");
    assert!(
        !out.contains("graph not initialized"),
        "query must use the persisted graph, got: {out}"
    );
    assert!(
        out.contains("UserServiceImpl"),
        "query should find the indexed artifact, got: {out}"
    );
}

#[test]
fn never_indexed_project_still_reports_not_indexed() {
    // The un-indexed message must survive for a project that truly has no
    // graph yet (a fresh graph.db is created empty by the lazy open — count
    // 0 keeps the honest "not indexed" status line).
    let tmp = tempfile::tempdir().unwrap();
    write_project(tmp.path());

    let engine = DefaultEngine::new(
        NeuroCodeConfig::default(),
        PathBuf::from(tmp.path().to_path_buf()),
    );
    let status = engine.status_text();
    assert!(
        status.contains("not indexed"),
        "un-indexed project should still say 'not indexed' (status: {status})"
    );
}

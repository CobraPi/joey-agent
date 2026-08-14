//! T065 — non-Java-project fallback (FR-015).
//!
//! (a) a temp project with only .py files → assemble_context returns the
//!     FR-015 notice and an EMPTY formatted_context even though the graph
//!     store has artifacts;
//! (b) a project with one .java file → normal assembly proceeds
//!     (non-empty context when the graph is seeded).
//!
//! Uses isolated JOEY_HOME + unique temp project roots so the per-project
//! `graph.db` never collides with other tests.

use std::path::PathBuf;
use std::sync::Mutex;

use joey_neurocode::classifier::ComplexityTier;
use joey_neurocode::config::NeuroCodeConfig;
use joey_neurocode::engine::{CodingRequest, DefaultEngine, NeuroCodeEngine};
use joey_neurocode::graph::node::{ArtifactKind, CodeArtifactNode};
use joey_neurocode::parse::project_has_java;

/// Serialize tests that touch JOEY_HOME (cargo test runs threads in one process).
static HOME_LOCK: Mutex<()> = Mutex::new(());

/// Set an isolated JOEY_HOME for this test.
fn set_test_home(tag: &str) {
    let home = std::env::temp_dir().join(format!(
        "joey-neurocode-t065-home-{}-{}",
        tag,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("JOEY_HOME", &home);
}

fn make_request(text: &str, project_root: &PathBuf) -> CodingRequest {
    CodingRequest {
        text: text.into(),
        active_file: Some(project_root.join("src/com/enterprise/auth/UserServiceImpl.java").to_string_lossy().to_string()),
        active_symbols: vec!["UserServiceImpl".to_string()],
        project_root: project_root.clone(),
        token_budget_hint: 0,
    }
}

fn seed_graph(engine: &DefaultEngine) {
    let result = engine.index_project();
    assert!(
        result.errors.is_empty(),
        "indexing the seeded Java project must succeed: {:?}",
        result.errors
    );
}

#[test]
fn a_python_only_project_falls_back_with_notice() {
    let _guard = HOME_LOCK.lock().unwrap();
    set_test_home("python-only");

    // A temp project with only .py files.
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/app.py"), "print('hello')\n").unwrap();

    // Build the engine against a Java-shaped project first so its graph
    // store gets seeded with real artifacts, then point the REQUEST at the
    // Python project — the FR-015 check must still fall back.
    let java_project = tempfile::tempdir().unwrap();
    let java_src = java_project.path().join("src/com/enterprise/auth");
    std::fs::create_dir_all(&java_src).unwrap();
    std::fs::write(
        java_src.join("UserServiceImpl.java"),
        "package com.enterprise.auth; public class UserServiceImpl { public void charge() {} }\n",
    )
    .unwrap();
    std::fs::write(java_project.path().join("build.gradle"), "dependencies { implementation 'com.pega:pega' }\n").unwrap();

    let mut cfg = NeuroCodeConfig::default();
    cfg.enabled = true;
    let engine = DefaultEngine::new(cfg, java_project.path().to_path_buf());
    seed_graph(&engine);

    // Sanity: the graph store really has artifacts.
    let count = engine.with_graph(|g| g.and_then(|g| g.artifact_count().ok()).unwrap_or(0));
    assert!(count > 0, "graph must be seeded for the fallback check to be meaningful");

    // Request against the Python project → FR-015 fallback.
    let ctx = engine.assemble_context(
        &make_request("refactor UserServiceImpl", &project.path().to_path_buf()),
        ComplexityTier::Frontier,
    );

    let notice = ctx.notice.as_deref().unwrap_or("");
    assert!(
        notice.contains("FR-015") && notice.contains("no Java/Pega artifacts"),
        "expected the FR-015 notice, got: {:?}",
        ctx.notice
    );
    assert!(
        ctx.formatted_context.is_empty(),
        "formatted_context must be empty in fallback mode, got:\n{}",
        ctx.formatted_context
    );
    assert!(ctx.primary_nodes.is_empty());
    assert!(!ctx.cold_mode, "FR-015 fallback is not cold mode");
}

#[test]
fn b_java_project_assembles_normally() {
    let _guard = HOME_LOCK.lock().unwrap();
    set_test_home("java-project");

    // A temp project with one .java file.
    let project = tempfile::tempdir().unwrap();
    let src = project.path().join("src/com/enterprise/auth");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(
        src.join("UserServiceImpl.java"),
        "package com.enterprise.auth; public class UserServiceImpl { public void charge() {} }\n",
    )
    .unwrap();

    assert!(project_has_java(project.path()), "one .java file → true");

    let mut cfg = NeuroCodeConfig::default();
    cfg.enabled = true;
    let engine = DefaultEngine::new(cfg, project.path().to_path_buf());
    seed_graph(&engine);

    let ctx = engine.assemble_context(
        &make_request("refactor UserServiceImpl", &project.path().to_path_buf()),
        ComplexityTier::Frontier,
    );
    assert!(
        !ctx.formatted_context.is_empty(),
        "Java project must get a normal assembled context"
    );
    assert!(!ctx.cold_mode);
    assert!(
        ctx.notice.is_none() || !ctx.notice.as_deref().unwrap_or("").contains("FR-015"),
        "Java project must not get the FR-015 notice"
    );
    assert!(ctx.formatted_context.contains("UserServiceImpl"));
}

#[test]
fn pega_marker_projects_are_in_scope() {
    // A project with no .java files but a com.pega build marker → in scope.
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/main.rb"), "puts 'ruby'\n").unwrap();
    std::fs::write(
        project.path().join("pom.xml"),
        "<project><dependency><groupId>com.pega</groupId></dependency></project>\n",
    )
    .unwrap();
    assert!(project_has_java(project.path()), "com.pega pom.xml → in scope");

    // Rule-* files are also Pega markers.
    let project2 = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project2.path().join("rules")).unwrap();
    std::fs::write(project2.path().join("rules/Rule-Obj-Flow.md"), "rule definition\n").unwrap();
    assert!(project_has_java(project2.path()), "Rule-* file → in scope");

    // And plain non-Java projects are out of scope.
    let project3 = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project3.path().join("src")).unwrap();
    std::fs::write(project3.path().join("src/app.py"), "print('x')\n").unwrap();
    assert!(!project_has_java(project3.path()), "plain Python project → out of scope");
}

//! T065 — non-source-project fallback (FR-015), generalized to all
//! languages (originally the Java-only gate).
//!
//! (a) a temp project with no supported source files at all (only assets)
//!     → assemble_context returns the FR-015 notice and an EMPTY
//!     formatted_context even though the graph store has artifacts;
//! (b) a Python-only project → NOW IN SCOPE: normal assembly proceeds
//!     (multi-language generalization);
//! (c) a Java project → normal assembly (unchanged behavior);
//! (d) multi-language projects (TS, Go, Rust, Ruby-heuristic) → in scope
//!     and indexable.
//!
//! Uses isolated JOEY_HOME + unique temp project roots so the per-project
//! `graph.db` never collides with other tests.

use std::path::PathBuf;
use std::sync::Mutex;

use joey_neurocode::classifier::ComplexityTier;
use joey_neurocode::config::NeuroCodeConfig;
use joey_neurocode::engine::{CodingRequest, DefaultEngine, NeuroCodeEngine};
use joey_neurocode::parse::{ingest_project, project_has_java, project_has_source};

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

fn make_request(text: &str, active_rel: &str, project_root: &PathBuf) -> CodingRequest {
    CodingRequest {
        text: text.into(),
        active_file: Some(project_root.join(active_rel).to_string_lossy().to_string()),
        active_symbols: vec![],
        project_root: project_root.clone(),
        token_budget_hint: 0,
    }
}

fn seed_graph(engine: &DefaultEngine) {
    let result = engine.index_project();
    assert!(
        result.errors.is_empty(),
        "indexing the seeded project must succeed: {:?}",
        result.errors
    );
}

#[test]
fn a_no_source_project_falls_back_with_notice() {
    let _guard = HOME_LOCK.lock().unwrap();
    set_test_home("no-source");

    // A temp project with only non-source assets.
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join("assets")).unwrap();
    std::fs::write(project.path().join("assets/logo.svg"), "<svg/>\n").unwrap();
    std::fs::write(project.path().join("README.md"), "# doc\n").unwrap();

    // Build the engine against a code-bearing project first so its graph
    // store gets seeded with real artifacts, then point the REQUEST at the
    // asset-only project — the FR-015 check must still fall back.
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

    // Request against the asset-only project → FR-015 fallback.
    let ctx = engine.assemble_context(
        &make_request(
            "refactor UserServiceImpl",
            "assets/logo.svg",
            &project.path().to_path_buf(),
        ),
        ComplexityTier::Frontier,
    );

    let notice = ctx.notice.as_deref().unwrap_or("");
    assert!(
        notice.contains("FR-015") && notice.contains("no supported source artifacts"),
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
fn b_python_project_is_now_in_scope() {
    let _guard = HOME_LOCK.lock().unwrap();
    set_test_home("python-in-scope");

    // A temp project with only .py files — in scope since generalization.
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join("app")).unwrap();
    std::fs::write(
        project.path().join("app/services.py"),
        "from app.repo import Repository\n\nclass UserService:\n    def __init__(self):\n        self.repo = Repository()\n\n    def find(self, id):\n        return self.repo.get(id)\n",
    )
    .unwrap();

    assert!(
        project_has_source(project.path()),
        "Python-only project must be in scope after generalization"
    );

    let mut cfg = NeuroCodeConfig::default();
    cfg.enabled = true;
    let engine = DefaultEngine::new(cfg, project.path().to_path_buf());
    seed_graph(&engine);

    let ctx = engine.assemble_context(
        &make_request("refactor UserService", "app/services.py", &project.path().to_path_buf()),
        ComplexityTier::Frontier,
    );
    assert!(
        !ctx.formatted_context.is_empty(),
        "Python project must get a normal assembled context, got notice: {:?}",
        ctx.notice
    );
    assert!(!ctx.cold_mode);
    assert!(
        ctx.notice.is_none()
            || !ctx.notice.as_deref().unwrap_or("").contains("FR-015"),
        "Python project must not get the FR-015 notice"
    );
    assert!(ctx.formatted_context.contains("UserService"));
}

#[test]
fn c_java_project_assembles_normally() {
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
        &make_request(
            "refactor UserServiceImpl",
            "src/com/enterprise/auth/UserServiceImpl.java",
            &project.path().to_path_buf(),
        ),
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
fn d_multi_language_projects_are_in_scope_and_indexable() {
    // Pega markers still work.
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    std::fs::write(project.path().join("src/main.rb"), "puts 'ruby'\n").unwrap();
    std::fs::write(
        project.path().join("pom.xml"),
        "<project><dependency><groupId>com.pega</groupId></dependency></project>\n",
    )
    .unwrap();
    assert!(project_has_source(project.path()), "com.pega pom.xml → in scope");

    let project2 = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project2.path().join("rules")).unwrap();
    std::fs::write(project2.path().join("rules/Rule-Obj-Flow.md"), "rule definition\n").unwrap();
    assert!(project_has_source(project2.path()), "Rule-* file → in scope");

    // Asset-only projects are out of scope.
    let project3 = tempfile::tempdir().unwrap();
    std::fs::write(project3.path().join("README.md"), "# doc\n").unwrap();
    assert!(
        !project_has_source(project3.path()),
        "asset-only project → out of scope"
    );

    // A polyglot project indexes artifacts from every language family.
    let poly = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(poly.path().join("src")).unwrap();
    std::fs::write(
        poly.path().join("src/service.ts"),
        "import { Repo } from './repo';\nexport class OrderService implements IOrder { place(o: Order): void {} }\n",
    )
    .unwrap();
    std::fs::write(
        poly.path().join("src/service.go"),
        "package svc\n\ntype OrderService struct { repo *Repo }\n\nfunc (s *OrderService) Place(o Order) {}\n",
    )
    .unwrap();
    std::fs::write(
        poly.path().join("src/service.rs"),
        "pub struct OrderService { repo: Repo }\n\nimpl OrderService {\n    pub fn place(&self, o: Order) {}\n}\n",
    )
    .unwrap();
    std::fs::write(
        poly.path().join("src/service.rb"),
        "class OrderService < BaseService\n  def place(o); end\nend\n",
    )
    .unwrap();

    let _guard = HOME_LOCK.lock().unwrap();
    set_test_home("polyglot");
    let graph = joey_neurocode::graph::DependencyGraph::open_for_project(poly.path()).unwrap();
    let result = ingest_project(&graph, poly.path());
    assert!(result.errors.is_empty(), "polyglot ingestion errors: {:?}", result.errors);
    assert_eq!(result.files_scanned, 4, "all four source files scanned");
    assert!(result.artifacts_seen > 0, "polyglot ingestion must produce artifacts");

    // Each language contributed its class node.
    for name in ["OrderService"] {
        let hits = graph.query_fts(name, 10).unwrap_or_default();
        assert!(
            hits.len() >= 4,
            "expected ≥4 OrderService nodes (TS, Go, Rust, Ruby), got {}: {:?}",
            hits.len(),
            hits.iter().map(|n| (n.kind.as_str().to_string(), n.fqcn.clone())).collect::<Vec<_>>()
        );
    }
}

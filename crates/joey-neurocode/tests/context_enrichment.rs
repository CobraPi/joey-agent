//! Context-enrichment integration tests — the "useful context for the LLM"
//! behaviors added on top of the original T021 assembly:
//!
//! 1. Discovery: free-text requests naming a symbol (plain, backticked,
//!    dotted) locate the right primary node WITHOUT explicit active_symbols.
//! 2. Rendering: the formatted context contains file paths, member
//!    signatures, and the target's roster — enough for the model to act
//!    without grepping.
//! 3. Tier routing: resolve_tier_model follows the CLASSIFIED tier, not
//!    just the ambiguous default (engine caches the last classification).
//! 4. Staleness: an index older than the file's mtime produces a warning.
//! 5. Hub warning: a high-fan-in target gets a blast-radius note.

use std::path::PathBuf;

use joey_neurocode::classifier::ComplexityTier;
use joey_neurocode::config::NeuroCodeConfig;
use joey_neurocode::context::ContextAssembler;
use joey_neurocode::engine::{CodingRequest, DefaultEngine, NeuroCodeEngine};
use joey_neurocode::graph::edge::EdgeKind;
use joey_neurocode::graph::node::{ArtifactKind, CodeArtifactNode};
use joey_neurocode::graph::DependencyGraph;

/// A method member node with a signature, attached to its type.
fn method_of(type_fqcn: &str, enclosing: &str, name: &str, sig: &str, file: &str) -> CodeArtifactNode {
    let mut m = CodeArtifactNode::new(
        ArtifactKind::Method,
        format!("{}.{}()", type_fqcn, name),
        type_fqcn.rsplit_once('.').map(|(p, _)| p.to_string()).unwrap_or_default(),
        file.to_string(),
    );
    m.enclosing_type = Some(enclosing.to_string());
    m.signature = Some(sig.to_string());
    m
}

fn seed_rich_graph(graph: &DependencyGraph) -> u64 {
    let mut svc = CodeArtifactNode::new(
        ArtifactKind::Class,
        "com.acme.user.UserServiceImpl".into(),
        "com.acme.user".into(),
        "src/main/java/com/acme/user/UserServiceImpl.java".into(),
    );
    svc.implemented_interfaces = vec!["UserService".into()];
    svc.declared_dependencies = vec!["UserRepository".into()];
    svc.annotations = vec!["Service".into()];

    let iface = CodeArtifactNode::new(
        ArtifactKind::Interface,
        "com.acme.user.UserService".into(),
        "com.acme.user".into(),
        "src/main/java/com/acme/user/UserService.java".into(),
    );

    let repo = CodeArtifactNode::new(
        ArtifactKind::Class,
        "com.acme.repo.UserRepository".into(),
        "com.acme.repo".into(),
        "src/main/java/com/acme/repo/UserRepository.java".into(),
    );

    let svc_id = graph.upsert_node(&svc).unwrap();
    let iface_id = graph.upsert_node(&iface).unwrap();
    let repo_id = graph.upsert_node(&repo).unwrap();

    graph.upsert_edge(svc_id, iface_id, EdgeKind::Implements).unwrap();
    graph.upsert_edge(svc_id, repo_id, EdgeKind::Injects).unwrap();

    // Members of UserServiceImpl.
    let m1 = method_of(
        "com.acme.user.UserServiceImpl",
        "UserServiceImpl",
        "findById",
        "public User findById(Long id)",
        "src/main/java/com/acme/user/UserServiceImpl.java",
    );
    let m2 = method_of(
        "com.acme.user.UserServiceImpl",
        "UserServiceImpl",
        "deleteUser",
        "public void deleteUser(Long id)",
        "src/main/java/com/acme/user/UserServiceImpl.java",
    );
    for m in [m1, m2] {
        let mid = graph.upsert_node(&m).unwrap();
        graph.upsert_edge(mid, svc_id, EdgeKind::Injects).unwrap();
    }

    // Five dependents on UserService (hub) via IsImplementedBy-ish edges.
    for i in 0..5 {
        let mut d = CodeArtifactNode::new(
            ArtifactKind::Class,
            format!("com.acme.client.Client{}", i),
            "com.acme.client".into(),
            format!("src/main/java/com/acme/client/Client{}.java", i),
        );
        d.declared_dependencies = vec!["UserServiceImpl".into()];
        let did = graph.upsert_node(&d).unwrap();
        graph.upsert_edge(did, svc_id, EdgeKind::Injects).unwrap();
    }

    svc_id
}

fn request(text: &str) -> CodingRequest {
    CodingRequest {
        text: text.into(),
        active_file: None,
        active_symbols: vec![],
        project_root: PathBuf::from("."),
        token_budget_hint: 0,
    }
}

#[test]
fn free_text_symbol_mention_locates_target() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_rich_graph(&graph);
    let assembler = ContextAssembler::new(&graph);

    // Plain CamelCase mention, no active symbols, no active file.
    let ctx = assembler.assemble(&request("fix the UserServiceImpl please"), ComplexityTier::Frontier);
    assert!(
        ctx.primary_nodes.iter().any(|n| n.fqcn == "com.acme.user.UserServiceImpl"),
        "plain mention should locate UserServiceImpl, got: {:?}",
        ctx.primary_nodes.iter().map(|n| &n.fqcn).collect::<Vec<_>>()
    );
}

#[test]
fn backtick_mention_locates_target() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_rich_graph(&graph);
    let assembler = ContextAssembler::new(&graph);

    let ctx = assembler.assemble(&request("refactor `UserServiceImpl` to use Optional"), ComplexityTier::Frontier);
    assert!(ctx.primary_nodes.iter().any(|n| n.fqcn == "com.acme.user.UserServiceImpl"));
}

#[test]
fn dotted_reference_locates_target() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_rich_graph(&graph);
    let assembler = ContextAssembler::new(&graph);

    let ctx = assembler.assemble(
        &request("update com.acme.user.UserServiceImpl for the audit"),
        ComplexityTier::Frontier,
    );
    assert!(
        ctx.primary_nodes.iter().any(|n| n.fqcn == "com.acme.user.UserServiceImpl"),
        "dotted mention should locate the FQCN, got: {:?}",
        ctx.primary_nodes.iter().map(|n| &n.fqcn).collect::<Vec<_>>()
    );
}

#[test]
fn context_renders_file_paths_and_signatures() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_rich_graph(&graph);
    let assembler = ContextAssembler::new(&graph);

    let ctx = assembler.assemble(&request("edit UserServiceImpl"), ComplexityTier::Frontier);
    let text = &ctx.formatted_context;

    // File path — the model can read_file this directly.
    assert!(
        text.contains("src/main/java/com/acme/user/UserServiceImpl.java"),
        "file path missing in:\n{}",
        text
    );
    // Member roster with real signatures.
    assert!(
        text.contains("public User findById(Long id)"),
        "findById signature missing in:\n{}",
        text
    );
    assert!(
        text.contains("public void deleteUser(Long id)"),
        "deleteUser signature missing in:\n{}",
        text
    );
}

#[test]
fn hub_target_gets_blast_radius_warning() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_rich_graph(&graph);
    let assembler = ContextAssembler::new(&graph);

    let ctx = assembler.assemble(&request("edit UserServiceImpl"), ComplexityTier::Frontier);
    assert!(
        ctx.formatted_context.contains("blast radius"),
        "hub warning missing in:\n{}",
        ctx.formatted_context
    );
}

#[test]
fn expanded_interface_ranked_first() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_rich_graph(&graph);
    let assembler = ContextAssembler::new(&graph);

    // Economical budget: 8 expanded nodes; the implemented interface must
    // make the cut BEFORE dependents do.
    let ctx = assembler.assemble(&request("UserServiceImpl"), ComplexityTier::Economical);
    assert!(
        ctx.expanded_nodes.iter().any(|e| e.node.fqcn == "com.acme.user.UserService"),
        "implemented interface must be expanded within budget, got: {:?}",
        ctx.expanded_nodes.iter().map(|e| e.node.fqcn.clone()).collect::<Vec<_>>()
    );
}

#[test]
fn tier_routing_follows_classification() {
    // resolve_tier_model must return the tier model for the tier the LAST
    // classify() chose — not always the ambiguous default.
    let tmp = tempfile::tempdir().unwrap();
    // A source-bearing project so assemble_context doesn't take the FR-015
    // shortcut (classify itself doesn't need it, but keep the engine honest).
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/App.java"), "public class App {}\n").unwrap();

    let mut cfg = NeuroCodeConfig::default();
    cfg.enabled = true;
    cfg.tier.economical_model = "eco-model".into();
    cfg.tier.frontier_model = "frontier-model".into();

    let engine = DefaultEngine::new(cfg, tmp.path().to_path_buf());

    // Economical classification ("write a test").
    let _ = engine.classify(&CodingRequest {
        text: "write a unit test for this".into(),
        active_file: None,
        active_symbols: vec![],
        project_root: tmp.path().to_path_buf(),
        token_budget_hint: 0,
    });
    assert_eq!(engine.resolve_tier_model().as_deref(), Some("eco-model"));

    // Frontier classification ("refactor the architecture").
    let _ = engine.classify(&CodingRequest {
        text: "refactor the architecture for concurrency".into(),
        active_file: None,
        active_symbols: vec![],
        project_root: tmp.path().to_path_buf(),
        token_budget_hint: 0,
    });
    assert_eq!(engine.resolve_tier_model().as_deref(), Some("frontier-model"));
}

#[test]
fn stale_index_produces_warning() {
    let project = tempfile::tempdir().unwrap();
    let src_dir = project.path().join("src/main/java/com/acme/user");
    std::fs::create_dir_all(&src_dir).unwrap();
    let file = src_dir.join("UserServiceImpl.java");
    std::fs::write(&file, "package com.acme.user; public class UserServiceImpl {}\n").unwrap();

    // Index the project so the graph has a node with indexed_at = now.
    let mut cfg = NeuroCodeConfig::default();
    cfg.enabled = true;
    let engine = DefaultEngine::new(cfg, project.path().to_path_buf());
    let result = engine.index_project();
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert!(result.artifacts_seen > 0);

    // Backdate the indexed_at timestamp via the store directly (std-only;
    // no filetime dependency): mtime (now) will then postdate indexed_at.
    engine.with_graph(|g| {
        if let Some(graph) = g {
            let _ = graph.store().conn().execute(
                "UPDATE code_artifacts SET indexed_at = '2000-01-01T00:00:00+00:00'",
                [],
            );
        }
    });

    let req = CodingRequest {
        text: "fix UserServiceImpl".into(),
        active_file: None,
        active_symbols: vec![],
        project_root: project.path().to_path_buf(),
        token_budget_hint: 0,
    };
    let ctx = engine.assemble_context(&req, ComplexityTier::Frontier);
    assert!(
        ctx.formatted_context.contains("Index Staleness"),
        "stale index should warn, got:\n{}",
        ctx.formatted_context
    );
}

#[test]
fn intercept_dedupe_key_new_turn_reassembles() {
    // Engine-level: classify + assemble are engine-agnostic; the dedupe key
    // lives in agent-core. Here we assert the assembler is deterministic
    // for identical requests (a precondition for dedupe being sound).
    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_rich_graph(&graph);
    let assembler = ContextAssembler::new(&graph);

    let a = assembler.assemble(&request("fix UserServiceImpl"), ComplexityTier::Frontier);
    let b = assembler.assemble(&request("fix UserServiceImpl"), ComplexityTier::Frontier);
    assert_eq!(a.formatted_context, b.formatted_context, "assembly must be deterministic");
    assert_eq!(a.token_estimate, b.token_estimate);
}

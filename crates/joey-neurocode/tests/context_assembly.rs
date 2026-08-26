//! T021 — context assembly integration test.
//!
//! Seed an in-memory DependencyGraph with UserServiceImpl → UserService →
//! UserRepository, create Implements/Injects edges, use ContextAssembler to
//! assemble context, verify all three nodes are included, ExpansionReason tags
//! are correct, and Economical vs Frontier budget sizing differs.

use std::path::PathBuf;

use joey_neurocode::classifier::ComplexityTier;
use joey_neurocode::context::{ContextAssembler, ExpansionReason};
use joey_neurocode::engine::CodingRequest;
use joey_neurocode::graph::edge::EdgeKind;
use joey_neurocode::graph::node::{ArtifactKind, CodeArtifactNode};
use joey_neurocode::graph::DependencyGraph;

/// Seed the graph with a three-node chain and return the primary node id.
fn seed_graph(graph: &DependencyGraph) -> u64 {
    let mut svc = CodeArtifactNode::new(
        ArtifactKind::Class,
        "com.enterprise.auth.service.UserServiceImpl".into(),
        "com.enterprise.auth.service".into(),
        "src/UserServiceImpl.java".into(),
    );
    svc.implemented_interfaces = vec!["UserService".into()];
    svc.declared_dependencies = vec!["UserRepository".into()];
    svc.annotations = vec!["Service".into(), "Transactional".into()];

    let mut iface = CodeArtifactNode::new(
        ArtifactKind::Interface,
        "com.enterprise.auth.service.UserService".into(),
        "com.enterprise.auth.service".into(),
        "src/UserService.java".into(),
    );
    iface.annotations = vec!["Service".into()];

    let mut repo = CodeArtifactNode::new(
        ArtifactKind::Class,
        "com.enterprise.auth.repo.UserRepository".into(),
        "com.enterprise.auth.repo".into(),
        "src/UserRepository.java".into(),
    );
    repo.annotations = vec!["Repository".into()];

    let svc_id = graph.upsert_node(&svc).unwrap();
    let iface_id = graph.upsert_node(&iface).unwrap();
    let repo_id = graph.upsert_node(&repo).unwrap();

    // UserServiceImpl implements UserService.
    graph
        .upsert_edge(svc_id, iface_id, EdgeKind::Implements)
        .unwrap();
    // UserServiceImpl injects UserRepository.
    graph
        .upsert_edge(svc_id, repo_id, EdgeKind::Injects)
        .unwrap();

    svc_id
}

fn make_request(symbol: &str) -> CodingRequest {
    CodingRequest {
        text: format!("refactor {}", symbol),
        active_file: Some("src/UserServiceImpl.java".into()),
        active_symbols: vec![symbol.to_string()],
        project_root: PathBuf::from("."),
        token_budget_hint: 0,
    }
}

#[test]
fn assembled_context_includes_all_three_nodes() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_graph(&graph);
    assert_eq!(graph.artifact_count().unwrap(), 3);

    let assembler = ContextAssembler::new(&graph);
    let ctx = assembler.assemble(&make_request("UserServiceImpl"), ComplexityTier::Frontier);

    // Not cold mode — graph is populated.
    assert!(!ctx.cold_mode);

    // Primary node found via FTS on the symbol.
    assert!(
        !ctx.primary_nodes.is_empty(),
        "primary node should be located via active symbol"
    );
    assert!(
        ctx.primary_nodes
            .iter()
            .any(|n| n.fqcn.contains("UserServiceImpl")),
        "primary node should be UserServiceImpl"
    );

    // Expanded nodes should include UserService (Implements) and UserRepository (Injects).
    let expanded_fqcns: Vec<&str> = ctx
        .expanded_nodes
        .iter()
        .map(|e| e.node.fqcn.as_str())
        .collect();
    assert!(
        expanded_fqcns.iter().any(|f| f.contains("UserService") && !f.contains("Impl")),
        "expanded nodes should include UserService interface, got: {:?}",
        expanded_fqcns
    );
    assert!(
        expanded_fqcns.iter().any(|f| f.contains("UserRepository")),
        "expanded nodes should include UserRepository, got: {:?}",
        expanded_fqcns
    );
}

#[test]
fn expansion_reason_tags_are_correct() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_graph(&graph);

    let assembler = ContextAssembler::new(&graph);
    let ctx = assembler.assemble(&make_request("UserServiceImpl"), ComplexityTier::Frontier);

    // There should be an ImplementsInterface reason (UserService) and an
    // InjectedByTarget reason (UserRepository).
    let reasons: Vec<&ExpansionReason> = ctx.expanded_nodes.iter().map(|e| &e.reason).collect();

    assert!(
        reasons
            .iter()
            .any(|r| { **r == ExpansionReason::ImplementsInterface || **r == ExpansionReason::InjectedByTarget }),
        "expected Implements/Injects expansion reasons, got: {:?}",
        reasons
    );

    // Specifically: the node with "UserService" should be tagged with an
    // Implements-related reason, and UserRepository with Injects.
    let iface_exp = ctx
        .expanded_nodes
        .iter()
        .find(|e| e.node.fqcn.contains("UserService") && !e.node.fqcn.contains("Impl"));
    if let Some(exp) = iface_exp {
        assert_eq!(exp.reason, ExpansionReason::ImplementsInterface);
    }

    let repo_exp = ctx
        .expanded_nodes
        .iter()
        .find(|e| e.node.fqcn.contains("UserRepository"));
    if let Some(exp) = repo_exp {
        assert_eq!(exp.reason, ExpansionReason::InjectedByTarget);
    }
}

#[test]
fn cold_mode_on_empty_graph() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    // No nodes seeded.
    let assembler = ContextAssembler::new(&graph);
    let ctx = assembler.assemble(&make_request("UserServiceImpl"), ComplexityTier::Economical);

    assert!(ctx.cold_mode, "empty graph → cold mode");
    assert!(ctx.primary_nodes.is_empty());
    assert!(ctx.expanded_nodes.is_empty());
    assert!(ctx.notice.is_some(), "cold mode should have a notice");
    assert!(ctx.formatted_context.contains("Cold-Mode") || ctx.formatted_context.contains("cold"));
}

#[test]
fn frontier_budget_expands_more_than_economical() {
    // Build a richer graph so Frontier's deeper expansion can pull in more
    // nodes than Economical's. The chain is three levels deep:
    // svc → repo → em → emFactory (plus audit → logsink at level 2).
    let graph = DependencyGraph::open_in_memory().unwrap();

    // Primary: UserServiceImpl.
    let mut svc = CodeArtifactNode::new(
        ArtifactKind::Class,
        "app.UserServiceImpl".into(),
        "app".into(),
        "a.java".into(),
    );
    svc.implemented_interfaces = vec!["UserService".into()];
    svc.declared_dependencies = vec!["UserRepository".into(), "AuditLogger".into()];

    let iface = CodeArtifactNode::new(
        ArtifactKind::Interface,
        "app.UserService".into(),
        "app".into(),
        "b.java".into(),
    );
    let mut repo = CodeArtifactNode::new(
        ArtifactKind::Class,
        "app.UserRepository".into(),
        "app".into(),
        "c.java".into(),
    );
    repo.declared_dependencies = vec!["EntityManager".into()];
    let mut audit = CodeArtifactNode::new(
        ArtifactKind::Class,
        "app.AuditLogger".into(),
        "app".into(),
        "d.java".into(),
    );
    audit.declared_dependencies = vec!["LogSink".into()];
    let mut em = CodeArtifactNode::new(
        ArtifactKind::Class,
        "app.EntityManager".into(),
        "app".into(),
        "e.java".into(),
    );
    em.declared_dependencies = vec!["EmFactory".into()];
    let logsink = CodeArtifactNode::new(
        ArtifactKind::Class,
        "app.LogSink".into(),
        "app".into(),
        "f.java".into(),
    );
    let em_factory = CodeArtifactNode::new(
        ArtifactKind::Class,
        "app.EmFactory".into(),
        "app".into(),
        "g.java".into(),
    );

    let svc_id = graph.upsert_node(&svc).unwrap();
    let iface_id = graph.upsert_node(&iface).unwrap();
    let repo_id = graph.upsert_node(&repo).unwrap();
    let audit_id = graph.upsert_node(&audit).unwrap();
    let em_id = graph.upsert_node(&em).unwrap();
    let logsink_id = graph.upsert_node(&logsink).unwrap();
    let em_factory_id = graph.upsert_node(&em_factory).unwrap();

    graph.upsert_edge(svc_id, iface_id, EdgeKind::Implements).unwrap();
    graph.upsert_edge(svc_id, repo_id, EdgeKind::Injects).unwrap();
    graph.upsert_edge(svc_id, audit_id, EdgeKind::Injects).unwrap();
    // depth-2 reachable: repo → em, audit → logsink.
    graph.upsert_edge(repo_id, em_id, EdgeKind::Injects).unwrap();
    graph.upsert_edge(audit_id, logsink_id, EdgeKind::Injects).unwrap();
    // depth-3 reachable: em → emFactory.
    graph.upsert_edge(em_id, em_factory_id, EdgeKind::Injects).unwrap();

    let assembler = ContextAssembler::new(&graph);

    let eco_ctx = assembler.assemble(&make_request("UserServiceImpl"), ComplexityTier::Economical);
    let frontier_ctx =
        assembler.assemble(&make_request("UserServiceImpl"), ComplexityTier::Frontier);

    // Frontier should expand at least as many nodes as Economical.
    assert!(
        frontier_ctx.expanded_nodes.len() >= eco_ctx.expanded_nodes.len(),
        "Frontier (depth 3, {}) should expand >= Economical (depth 2, {})",
        frontier_ctx.expanded_nodes.len(),
        eco_ctx.expanded_nodes.len()
    );

    // With the depth-3 edge (em→emFactory), Frontier should reach
    // strictly more nodes than Economical's depth-2 slice.
    assert!(
        frontier_ctx.expanded_nodes.len() > eco_ctx.expanded_nodes.len(),
        "Frontier depth-3 should reach more nodes than Economical depth-2 \
         (frontier={}, economical={})",
        frontier_ctx.expanded_nodes.len(),
        eco_ctx.expanded_nodes.len()
    );
}

#[test]
fn formatted_context_contains_artifacts() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_graph(&graph);

    let assembler = ContextAssembler::new(&graph);
    let ctx = assembler.assemble(&make_request("UserServiceImpl"), ComplexityTier::Frontier);

    assert!(
        !ctx.formatted_context.is_empty(),
        "formatted context should be non-empty"
    );
    assert!(ctx.token_estimate > 0);
    // The primary node's FQCN should appear in the output.
    assert!(ctx.formatted_context.contains("UserServiceImpl"));
}

/// Feature 015 follow-up (realtime feed): `assemble_with_progress` must
/// produce a result IDENTICAL to `assemble` (streaming must not change
/// bytes), and must invoke the progress callback at least for the
/// locate + expand + format stages.
#[test]
fn streaming_assembly_is_identical_and_reports_stages() {
    use std::sync::Mutex;

    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_graph(&graph);
    let assembler = ContextAssembler::new(&graph);
    let request = make_request("UserServiceImpl");

    let plain = assembler.assemble(&request, ComplexityTier::Frontier);
    let stages: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let streamed = assembler.assemble_with_progress(
        &request,
        ComplexityTier::Frontier,
        &|stage| stages.lock().unwrap().push(stage.to_string()),
    );

    // Byte-identical results (Constitution VII non-regression).
    assert_eq!(
        plain.formatted_context, streamed.formatted_context,
        "streaming assembly must not change the assembled context"
    );
    assert_eq!(plain.token_estimate, streamed.token_estimate);
    assert_eq!(plain.expanded_nodes.len(), streamed.expanded_nodes.len());

    // Stage coverage: locate, expand, format all fire.
    let stages = stages.into_inner().unwrap();
    let joined = stages.join("\n");
    assert!(
        joined.contains("locating target nodes"),
        "missing locate stage, got: {:?}",
        stages
    );
    assert!(
        joined.contains("expanded graph:"),
        "missing expand stage, got: {:?}",
        stages
    );
    assert!(
        joined.contains("formatting context for tier"),
        "missing format stage, got: {:?}",
        stages
    );
}

/// Cold mode must also stream (the degraded path reports itself).
#[test]
fn cold_mode_streams_stage() {
    use std::sync::Mutex;

    let graph = DependencyGraph::open_in_memory().unwrap(); // empty — cold
    let assembler = ContextAssembler::new(&graph);
    let stages: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let ctx = assembler.assemble_with_progress(
        &make_request("Nothing"),
        ComplexityTier::Frontier,
        &|stage| stages.lock().unwrap().push(stage.to_string()),
    );
    assert!(ctx.cold_mode);
    let stages = stages.into_inner().unwrap();
    assert!(
        stages.iter().any(|s| s.contains("cold mode")),
        "cold mode should report a stage, got: {:?}",
        stages
    );
}

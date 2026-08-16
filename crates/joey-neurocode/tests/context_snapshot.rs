//! Feature 015 follow-up — interactive-visualization snapshot integration.
//!
//! Seed an in-memory DependencyGraph with UserServiceImpl → UserService →
//! UserRepository (+ members), assemble a context, and verify the structured
//! snapshot carried alongside the formatted text: primaries tagged, expanded
//! nodes carry reason + via + depth, member rosters populated, fan-in
//! correct, edges among included nodes only, and the budget facts present.

use std::path::PathBuf;

use joey_neurocode::classifier::ComplexityTier;
use joey_neurocode::context::{ContextAssembler, EdgeSnapshot};
use joey_neurocode::engine::CodingRequest;
use joey_neurocode::graph::edge::EdgeKind;
use joey_neurocode::graph::node::{ArtifactKind, CodeArtifactNode};
use joey_neurocode::graph::DependencyGraph;

fn seed_graph(graph: &DependencyGraph) {
    let mut svc = CodeArtifactNode::new(
        ArtifactKind::Class,
        "com.enterprise.auth.service.UserServiceImpl".into(),
        "com.enterprise.auth.service".into(),
        "src/UserServiceImpl.java".into(),
    );
    svc.implemented_interfaces = vec!["UserService".into()];
    svc.declared_dependencies = vec!["UserRepository".into()];

    let iface = CodeArtifactNode::new(
        ArtifactKind::Interface,
        "com.enterprise.auth.service.UserService".into(),
        "com.enterprise.auth.service".into(),
        "src/UserService.java".into(),
    );

    let repo = CodeArtifactNode::new(
        ArtifactKind::Class,
        "com.enterprise.auth.repo.UserRepository".into(),
        "com.enterprise.auth.repo".into(),
        "src/UserRepository.java".into(),
    );

    // A member method of the service (enclosing_type set).
    let mut find_by_id = CodeArtifactNode::new(
        ArtifactKind::Method,
        "com.enterprise.auth.service.UserServiceImpl.findById()".into(),
        "com.enterprise.auth.service".into(),
        "src/UserServiceImpl.java".into(),
    );
    find_by_id.enclosing_type = Some("UserServiceImpl".into());
    find_by_id.signature = Some("public User findById(Long id)".into());

    // A second consumer of the interface (fan-in for UserService).
    let mut other = CodeArtifactNode::new(
        ArtifactKind::Class,
        "com.enterprise.auth.service.AdminServiceImpl".into(),
        "com.enterprise.auth.service".into(),
        "src/AdminServiceImpl.java".into(),
    );
    other.implemented_interfaces = vec!["UserService".into()];

    let svc_id = graph.upsert_node(&svc).unwrap();
    let iface_id = graph.upsert_node(&iface).unwrap();
    let repo_id = graph.upsert_node(&repo).unwrap();
    let member_id = graph.upsert_node(&find_by_id).unwrap();
    let other_id = graph.upsert_node(&other).unwrap();

    graph.upsert_edge(svc_id, iface_id, EdgeKind::Implements).unwrap();
    graph.upsert_edge(svc_id, repo_id, EdgeKind::Injects).unwrap();
    graph.upsert_edge(member_id, svc_id, EdgeKind::MemberOf).unwrap();
    graph.upsert_edge(other_id, iface_id, EdgeKind::Implements).unwrap();
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
fn snapshot_carries_structure_of_the_assembly() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_graph(&graph);

    let assembler = ContextAssembler::new(&graph);
    let ctx = assembler.assemble(&make_request("UserServiceImpl"), ComplexityTier::Frontier);

    let snap = ctx
        .snapshot
        .as_ref()
        .expect("populated graph assembly must carry a snapshot");
    assert_eq!(snap.tier, "Frontier");
    assert!(!snap.nodes.is_empty());
    assert!(!snap.edges.is_empty());
    assert_eq!(snap.cold_mode, false);

    // Exactly one primary, tagged as target with depth 0 and no reason.
    let primaries: Vec<_> = snap.nodes.iter().filter(|n| n.primary).collect();
    assert_eq!(primaries.len(), 1, "one primary target");
    assert_eq!(primaries[0].name, "UserServiceImpl");
    assert_eq!(primaries[0].depth, 0);
    assert!(primaries[0].reason.is_none());
    assert_eq!(primaries[0].kind, "Class");

    // The interface and repository are included as expanded nodes with
    // reason labels and via = the primary's name.
    let iface = snap
        .nodes
        .iter()
        .find(|n| n.name == "UserService")
        .expect("interface included");
    assert_eq!(iface.reason.as_deref(), Some("implements"));
    assert_eq!(iface.via.as_deref(), Some("UserServiceImpl"));
    assert!(iface.depth >= 1);

    let repo = snap
        .nodes
        .iter()
        .find(|n| n.name == "UserRepository")
        .expect("repository included");
    assert_eq!(repo.reason.as_deref(), Some("injects"));

    // Fan-in: UserService is implemented by both consumers.
    assert!(iface.fan_in >= 2, "interface fan-in counts both implementors");

    // Member roster on the primary: findById with its signature.
    let member = primaries[0]
        .members
        .iter()
        .find(|m| m.name == "findById")
        .expect("member roster includes findById");
    assert_eq!(member.kind, "method");
    assert!(member.signature.contains("public User findById"));

    // Edges reference only included nodes (indices in range) and carry
    // kind tags.
    for e in &snap.edges {
        assert!(e.from < snap.nodes.len());
        assert!(e.to < snap.nodes.len());
        assert!(!e.kind.is_empty());
    }
    let kinds: Vec<&str> = snap.edges.iter().map(|e| e.kind.as_str()).collect();
    assert!(kinds.contains(&"Implements"), "implements edge present: {:?}", kinds);
    assert!(kinds.contains(&"Injects"), "injects edge present: {:?}", kinds);

    // Budget facts for the stats bar.
    assert_eq!(snap.budget.max_expanded_nodes, 24, "frontier budget");
    assert!(snap.budget.max_expansion_depth >= 2);
}

#[test]
fn snapshot_via_resolves_to_names_not_ids() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_graph(&graph);

    let assembler = ContextAssembler::new(&graph);
    let ctx = assembler.assemble(&make_request("UserServiceImpl"), ComplexityTier::Economical);

    let snap = ctx.snapshot.as_ref().expect("snapshot present");
    // Every expanded node's via (when set) names an INCLUDED node.
    let names: Vec<&str> = snap.nodes.iter().map(|n| n.name.as_str()).collect();
    for n in snap.nodes.iter().filter(|n| !n.primary) {
        if let Some(via) = &n.via {
            assert!(
                names.contains(&via.as_str()),
                "via '{}' must reference an included node",
                via
            );
        }
    }
}

#[test]
fn cold_mode_assembly_has_no_snapshot() {
    // Empty graph → cold mode → snapshot is None (nothing to visualize).
    let graph = DependencyGraph::open_in_memory().unwrap();
    let assembler = ContextAssembler::new(&graph);
    let ctx = assembler.assemble(&make_request("Anything"), ComplexityTier::Frontier);
    assert!(ctx.cold_mode);
    assert!(ctx.snapshot.is_none(), "cold mode carries no snapshot");
}

#[test]
fn edge_snapshot_default_and_equality() {
    // Trivial but pins the public surface (constitution: regression tests
    // for public-surface changes).
    let a = EdgeSnapshot {
        from: 0,
        to: 1,
        kind: "Implements".into(),
    };
    let b = a.clone();
    assert_eq!(a, b);
    assert_eq!(EdgeSnapshot::default(), EdgeSnapshot {
        from: 0,
        to: 0,
        kind: String::new(),
    });
}

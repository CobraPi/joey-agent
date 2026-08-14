//! T008 — DependencyGraph round-trip integration test.
//!
//! Exercises the public graph API end-to-end: create nodes, upsert into a
//! temp on-disk graph.db, create edges, close/reopen, verify data preserved,
//! test update (re-upsert), and FTS search round-trip.

use std::path::PathBuf;

use joey_neurocode::*;
use joey_neurocode::graph::node::{ArtifactKind, ArtifactStatus, CodeArtifactNode};
use joey_neurocode::graph::edge::EdgeKind;
use joey_neurocode::graph::DependencyGraph;

/// Build a class node for UserServiceImpl.
fn make_service_impl() -> CodeArtifactNode {
    let mut node = CodeArtifactNode::new(
        ArtifactKind::Class,
        "com.enterprise.auth.service.UserServiceImpl".into(),
        "com.enterprise.auth.service".into(),
        "src/main/java/com/enterprise/auth/service/UserServiceImpl.java".into(),
    );
    node.implemented_interfaces = vec!["UserService".into()];
    node.annotations = vec!["Service".into(), "Transactional".into()];
    node.declared_dependencies = vec!["UserRepository".into()];
    node.framework_version = Some("Spring Boot 3.2".into());
    node
}

/// Build an interface node for UserService.
fn make_service_interface() -> CodeArtifactNode {
    CodeArtifactNode::new(
        ArtifactKind::Interface,
        "com.enterprise.auth.service.UserService".into(),
        "com.enterprise.auth.service".into(),
        "src/main/java/com/enterprise/auth/service/UserService.java".into(),
    )
}

/// Build a class node for UserRepository.
fn make_repository() -> CodeArtifactNode {
    let mut node = CodeArtifactNode::new(
        ArtifactKind::Class,
        "com.enterprise.auth.repo.UserRepository".into(),
        "com.enterprise.auth.repo".into(),
        "src/main/java/com/enterprise/auth/repo/UserRepository.java".into(),
    );
    node.annotations = vec!["Repository".into()];
    node
}

/// Build a method node.
fn make_method(enclosing: &str) -> CodeArtifactNode {
    let mut node = CodeArtifactNode::new(
        ArtifactKind::Method,
        "com.enterprise.auth.service.UserServiceImpl.findById()".into(),
        "com.enterprise.auth.service".into(),
        "src/main/java/com/enterprise/auth/service/UserServiceImpl.java".into(),
    );
    node.enclosing_type = Some(enclosing.into());
    node.annotations = vec!["Override".into()];
    node
}

#[test]
fn upsert_and_read_back_nodes() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path: PathBuf = tmp.path().join("graph.db");

    {
        let graph = DependencyGraph::open(&db_path).unwrap();
        let svc = make_service_impl();
        let iface = make_service_interface();
        let repo = make_repository();
        let method = make_method("UserServiceImpl");

        let svc_id = graph.upsert_node(&svc).unwrap();
        let iface_id = graph.upsert_node(&iface).unwrap();
        let repo_id = graph.upsert_node(&repo).unwrap();
        let method_id = graph.upsert_node(&method).unwrap();

        assert!(svc_id > 0);
        assert!(iface_id > 0);
        assert!(repo_id > 0);
        assert!(method_id > 0);
        assert_ne!(svc_id, iface_id);

        // All four should be countable active artifacts.
        assert_eq!(graph.artifact_count().unwrap(), 4);
    }
    // graph dropped → connection closed

    // Reopen and verify everything persisted.
    {
        let graph = DependencyGraph::open(&db_path).unwrap();
        assert_eq!(graph.artifact_count().unwrap(), 4);

        // Verify UserServiceImpl by unique key.
        let svc = graph
            .store()
            .find_node(
                "com.enterprise.auth.service.UserServiceImpl",
                &ArtifactKind::Class,
                "src/main/java/com/enterprise/auth/service/UserServiceImpl.java",
            )
            .unwrap()
            .expect("UserServiceImpl should persist after reopen");

        assert_eq!(svc.kind, ArtifactKind::Class);
        assert_eq!(svc.fqcn, "com.enterprise.auth.service.UserServiceImpl");
        assert_eq!(svc.package, "com.enterprise.auth.service");
        assert!(svc.implemented_interfaces.contains(&"UserService".to_string()));
        assert!(svc.annotations.contains(&"Service".to_string()));
        assert!(svc.annotations.contains(&"Transactional".to_string()));
        assert!(svc.declared_dependencies.contains(&"UserRepository".to_string()));
        assert_eq!(svc.framework_version.as_deref(), Some("Spring Boot 3.2"));
        assert_eq!(svc.status, ArtifactStatus::Active);

        // Verify interface.
        let iface = graph
            .store()
            .find_node(
                "com.enterprise.auth.service.UserService",
                &ArtifactKind::Interface,
                "src/main/java/com/enterprise/auth/service/UserService.java",
            )
            .unwrap()
            .expect("UserService interface should persist");
        assert_eq!(iface.kind, ArtifactKind::Interface);

        // Verify repository by rowid.
        let repo = graph
            .store()
            .get_node(
                graph
                    .store()
                    .find_node(
                        "com.enterprise.auth.repo.UserRepository",
                        &ArtifactKind::Class,
                        "src/main/java/com/enterprise/auth/repo/UserRepository.java",
                    )
                    .unwrap()
                    .unwrap()
                    .id,
            )
            .unwrap()
            .expect("repo by rowid");
        assert_eq!(repo.kind, ArtifactKind::Class);
        assert!(repo.annotations.contains(&"Repository".to_string()));
    }
}

#[test]
fn upsert_edges_and_traverse() {
    let graph = DependencyGraph::open_in_memory().unwrap();

    let svc_id = graph.upsert_node(&make_service_impl()).unwrap();
    let iface_id = graph.upsert_node(&make_service_interface()).unwrap();
    let repo_id = graph.upsert_node(&make_repository()).unwrap();

    // UserServiceImpl implements UserService.
    graph
        .upsert_edge(svc_id, iface_id, EdgeKind::Implements)
        .unwrap();
    // UserServiceImpl injects UserRepository.
    graph
        .upsert_edge(svc_id, repo_id, EdgeKind::Injects)
        .unwrap();

    // Traverse from UserServiceImpl.
    let edges = graph.traverse_edges(svc_id, None).unwrap();
    assert_eq!(edges.len(), 2);

    // Filter by Implements.
    let impl_edges = graph
        .traverse_edges(svc_id, Some(EdgeKind::Implements))
        .unwrap();
    assert_eq!(impl_edges.len(), 1);
    assert_eq!(impl_edges[0].0, iface_id);

    // Filter by Injects.
    let inject_edges = graph
        .traverse_edges(svc_id, Some(EdgeKind::Injects))
        .unwrap();
    assert_eq!(inject_edges.len(), 1);
    assert_eq!(inject_edges[0].0, repo_id);

    // Edge idempotency — re-inserting the same edge should be a no-op.
    graph
        .upsert_edge(svc_id, iface_id, EdgeKind::Implements)
        .unwrap();
    let edges2 = graph.traverse_edges(svc_id, None).unwrap();
    assert_eq!(edges2.len(), 2, "duplicate edge should be idempotent");

    // Traverse TO the interface (reverse lookup).
    let incoming = graph.traverse_to(iface_id, None).unwrap();
    assert_eq!(incoming.len(), 1);
    assert_eq!(incoming[0].0, svc_id);
}

#[test]
fn update_via_reupsert() {
    let graph = DependencyGraph::open_in_memory().unwrap();

    let mut node = make_service_impl();
    let id1 = graph.upsert_node(&node).unwrap();
    assert_eq!(graph.artifact_count().unwrap(), 1);

    // Change fields and re-upsert with the same unique key (status stays Active
    // so artifact_count — which counts Active — stays 1).
    node.annotations = vec!["Service".into(), "Primary".into()];
    node.declared_dependencies = vec!["UserRepository".into(), "AuditLogger".into()];

    let id2 = graph.upsert_node(&node).unwrap();

    // Same unique key → should not create a new row.
    assert_eq!(graph.artifact_count().unwrap(), 1);

    // Verify the updated fields were persisted.
    let read_back = graph
        .store()
        .find_node(&node.fqcn, &node.kind, &node.source_path)
        .unwrap()
        .unwrap();
    assert!(read_back.annotations.contains(&"Primary".to_string()));
    assert!(read_back
        .declared_dependencies
        .contains(&"AuditLogger".to_string()));
    assert_eq!(read_back.status, ArtifactStatus::Active);
    let _ = (id1, id2);

    // Now verify a status update (Stale) also persists — artifact_count
    // counts Active only, so it drops to 0 after marking Stale.
    node.status = ArtifactStatus::Stale;
    graph.upsert_node(&node).unwrap();
    assert_eq!(graph.artifact_count().unwrap(), 0, "Stale nodes are not counted as Active");
    let read_back2 = graph
        .store()
        .find_node(&node.fqcn, &node.kind, &node.source_path)
        .unwrap()
        .unwrap();
    assert_eq!(read_back2.status, ArtifactStatus::Stale);
}

#[test]
fn fts_search_round_trip() {
    let graph = DependencyGraph::open_in_memory().unwrap();

    graph.upsert_node(&make_service_impl()).unwrap();
    graph.upsert_node(&make_service_interface()).unwrap();
    graph.upsert_node(&make_repository()).unwrap();

    // Search by class name token.
    let results = graph.query_fts("UserServiceImpl", 10).unwrap();
    assert!(
        !results.is_empty(),
        "FTS should find UserServiceImpl"
    );
    assert!(results
        .iter()
        .any(|n| n.fqcn.contains("UserServiceImpl")));

    // Search by package token.
    let results = graph.query_fts("UserRepository", 10).unwrap();
    assert!(
        !results.is_empty(),
        "FTS should find UserRepository"
    );

    // Search for the interface.
    let results = graph.query_fts("UserService", 10).unwrap();
    assert!(!results.is_empty());

    // Non-matching query returns empty.
    let results = graph.query_fts("NonexistentClassXYZ", 10).unwrap();
    assert!(results.is_empty());
}

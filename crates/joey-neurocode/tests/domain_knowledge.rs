//! T063 — domain-knowledge ingestion + retrieval integration tests (FR-013/014).
//!
//! (a) ingest a temp framework-docs file → retrieve finds it by keyword;
//! (b) ingest an entity catalog → assembling context for a matching node
//!     includes real field names that only exist in the ingested doc;
//! (c) ingest a postmortem mentioning a class name → assembling context for
//!     that class surfaces the postmortem text as a warning-ish note;
//! (d) provenance + version tag appear in the assembled context.
//!
//! T064 — conflict resolution:
//! (e) two framework_docs with the same version_tag → retrieve returns only
//!     the newest content; resolve_conflicts reports the pair; removing one
//!     via remove clears the conflict.

use std::path::PathBuf;

use joey_neurocode::classifier::ComplexityTier;
use joey_neurocode::context::ContextAssembler;
use joey_neurocode::engine::CodingRequest;
use joey_neurocode::graph::node::{ArtifactKind, CodeArtifactNode};
use joey_neurocode::graph::DependencyGraph;
use joey_neurocode::memory::domain::{
    ingest_source, resolve_conflicts, retrieve, KnowledgeCategory, KnowledgeSource,
};

fn make_source(category: KnowledgeCategory, path: &str, version: Option<&str>, prov: &str) -> KnowledgeSource {
    KnowledgeSource {
        category,
        source_path: path.to_string(),
        version_tag: version.map(str::to_string),
        provenance: prov.to_string(),
    }
}

fn make_request(symbol: &str, file: &str) -> CodingRequest {
    CodingRequest {
        text: format!("refactor {}", symbol),
        active_file: Some(file.into()),
        active_symbols: vec![symbol.to_string()],
        project_root: PathBuf::from("."),
        token_budget_hint: 0,
    }
}

/// Seed a UserServiceImpl node with annotations for domain-knowledge joins.
fn seed_user_service(graph: &DependencyGraph) -> u64 {
    let mut svc = CodeArtifactNode::new(
        ArtifactKind::Class,
        "com.enterprise.auth.UserServiceImpl".into(),
        "com.enterprise.auth".into(),
        "src/UserServiceImpl.java".into(),
    );
    svc.implemented_interfaces = vec!["UserService".into()];
    svc.annotations = vec!["Transactional".into(), "Service".into()];
    graph.upsert_node(&svc).unwrap()
}

#[test]
fn a_ingest_framework_docs_and_retrieve_by_keyword() {
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("spring-boot-3.2.md");
    std::fs::write(
        &doc,
        "# Spring Boot 3.2 configuration\nUse spring.datasource.hikari.maximum-pool-size for pooling.\n",
    )
    .unwrap();
    let doc_str = doc.to_str().unwrap();

    let graph = DependencyGraph::open_in_memory().unwrap();
    let id = ingest_source(
        graph.store(),
        &make_source(
            KnowledgeCategory::FrameworkDocs,
            doc_str,
            Some("3.2"),
            "Enterprise Wiki / Spring Boot 3.2",
        ),
    )
    .unwrap();
    assert!(id > 0);

    // Retrieved by keyword present only in the doc.
    let hits = retrieve(graph.store(), "hikari", None, 5);
    assert!(!hits.is_empty(), "keyword 'hikari' should hit the doc");
    assert!(hits[0].content.contains("hikari"));
    assert_eq!(hits[0].provenance, "Enterprise Wiki / Spring Boot 3.2");
    assert_eq!(hits[0].version_tag.as_deref(), Some("3.2"));

    // Category-filtered retrieval.
    let fw = retrieve(
        graph.store(),
        "configuration",
        Some(&KnowledgeCategory::FrameworkDocs),
        5,
    );
    assert!(!fw.is_empty());
    let pm = retrieve(
        graph.store(),
        "configuration",
        Some(&KnowledgeCategory::Postmortem),
        5,
    );
    assert!(pm.is_empty(), "Postmortem filter should exclude the doc");
}

#[test]
fn ingest_missing_path_errors() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    let result = ingest_source(
        graph.store(),
        &make_source(
            KnowledgeCategory::FrameworkDocs,
            "/nonexistent/does-not-exist-xyz.md",
            None,
            "nowhere",
        ),
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not readable"));
}

#[test]
fn ingest_directory_concatenates_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.md"), "alpha entity notes\n").unwrap();
    std::fs::write(dir.path().join("b.md"), "beta entity notes\n").unwrap();
    let dir_str = dir.path().to_str().unwrap().to_string();

    let graph = DependencyGraph::open_in_memory().unwrap();
    let id = ingest_source(
        graph.store(),
        &make_source(KnowledgeCategory::EntityCatalog, &dir_str, None, "Entities dir"),
    )
    .unwrap();
    assert!(id > 0);
    // Both files' content is retrievable from one ingestion.
    assert!(!retrieve(graph.store(), "alpha", None, 5).is_empty());
    assert!(!retrieve(graph.store(), "beta", None, 5).is_empty());
}

#[test]
fn b_entity_catalog_fields_surface_in_assembled_context() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("entities.md");
    // Field name that exists ONLY in this doc — nothing in the graph has it.
    std::fs::write(
        &catalog,
        "Entity UserService fields:\n- customerLoyaltyTierCode (String, max 16)\n- riskScoreBucket (enum)\n",
    )
    .unwrap();

    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_user_service(&graph);
    ingest_source(
        graph.store(),
        &make_source(
            KnowledgeCategory::EntityCatalog,
            catalog.to_str().unwrap(),
            Some("v2"),
            "Enterprise Entity Catalog",
        ),
    )
    .unwrap();

    let assembler = ContextAssembler::new(&graph);
    let ctx = assembler.assemble(&make_request("UserServiceImpl", "src/UserServiceImpl.java"), ComplexityTier::Frontier);

    assert!(
        ctx.formatted_context.contains("### Domain Knowledge"),
        "domain section missing:\n{}",
        ctx.formatted_context
    );
    assert!(
        ctx.formatted_context.contains("customerLoyaltyTierCode"),
        "real entity field from the ingested catalog must surface:\n{}",
        ctx.formatted_context
    );
}

#[test]
fn c_postmortem_surfaces_as_warning_note() {
    let dir = tempfile::tempdir().unwrap();
    let postmortem = dir.path().join("pm-2024-11.md");
    std::fs::write(
        &postmortem,
        "Postmortem UserServiceImpl outage 2024-11: deadlock in UserServiceImpl.charge() due to nested @Transactional; keep transactions short.\n",
    )
    .unwrap();

    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_user_service(&graph);
    ingest_source(
        graph.store(),
        &make_source(
            KnowledgeCategory::Postmortem,
            postmortem.to_str().unwrap(),
            None,
            "Incident Review 2024-11",
        ),
    )
    .unwrap();

    let assembler = ContextAssembler::new(&graph);
    let ctx = assembler.assemble(&make_request("UserServiceImpl", "src/UserServiceImpl.java"), ComplexityTier::Frontier);

    assert!(ctx.formatted_context.contains("### Domain Knowledge"));
    assert!(
        ctx.formatted_context.contains("postmortem"),
        "postmortem hit should be flagged as a postmortem note:\n{}",
        ctx.formatted_context
    );
    assert!(
        ctx.formatted_context.contains("deadlock"),
        "postmortem text should surface for the mentioned class:\n{}",
        ctx.formatted_context
    );
}

#[test]
fn d_provenance_and_version_surface_in_assembled_context() {
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("docs.md");
    std::fs::write(&doc, "UserServiceImpl notes and guidance.\n").unwrap();

    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_user_service(&graph);
    ingest_source(
        graph.store(),
        &make_source(
            KnowledgeCategory::FrameworkDocs,
            doc.to_str().unwrap(),
            Some("3.2"),
            "Enterprise Docs Hub",
        ),
    )
    .unwrap();

    let assembler = ContextAssembler::new(&graph);
    let ctx = assembler.assemble(&make_request("UserServiceImpl", "src/UserServiceImpl.java"), ComplexityTier::Economical);

    assert!(ctx.formatted_context.contains("Enterprise Docs Hub"), "provenance missing:\n{}", ctx.formatted_context);
    assert!(ctx.formatted_context.contains("3.2"), "version tag missing:\n{}", ctx.formatted_context);
    assert!(ctx.formatted_context.contains("provenance:"), "provenance label missing:\n{}", ctx.formatted_context);
}

// ── T064 conflict resolution ─────────────────────────────────────────────

#[test]
fn e_conflicting_versions_newest_wins_for_retrieval() {
    let dir = tempfile::tempdir().unwrap();
    let old_doc = dir.path().join("old.md");
    let new_doc = dir.path().join("new.md");
    std::fs::write(&old_doc, "Spring Boot 2.7: use legacy configuration properties.\n").unwrap();
    std::fs::write(&new_doc, "Spring Boot 3.2: use the new observation-based configuration.\n").unwrap();

    let graph = DependencyGraph::open_in_memory().unwrap();
    let old_id = ingest_source(
        graph.store(),
        &make_source(
            KnowledgeCategory::FrameworkDocs,
            old_doc.to_str().unwrap(),
            Some("3.2"),
            "Old Docs",
        ),
    )
    .unwrap();
    let new_id = ingest_source(
        graph.store(),
        &make_source(
            KnowledgeCategory::FrameworkDocs,
            new_doc.to_str().unwrap(),
            Some("3.2"),
            "New Docs",
        ),
    )
    .unwrap();
    assert_ne!(old_id, new_id);

    // Same category + same version_tag → conflict. Only the NEWEST content
    // is retrievable (most-recently-ingested wins).
    let hits = retrieve(graph.store(), "configuration", None, 10);
    assert!(!hits.is_empty(), "newest source should still be retrievable");
    assert!(
        hits.iter().all(|h| h.content.contains("3.2: use the new")),
        "only the newest content may be returned, got: {:?}",
        hits.iter().map(|h| h.content.clone()).collect::<Vec<_>>()
    );
    assert!(
        retrieve(graph.store(), "legacy", None, 10).is_empty(),
        "the older source's content must be hidden by conflict resolution"
    );

    // resolve_conflicts reports the pair.
    let conflicts = resolve_conflicts(graph.store());
    assert_eq!(conflicts.len(), 1, "expected exactly one conflict report");
    assert_eq!(conflicts[0].category, "FrameworkDocs");
    assert_eq!(conflicts[0].version_tag.as_deref(), Some("3.2"));
    assert_eq!(conflicts[0].sources.len(), 2);

    // Removing one source clears the conflict and restores retrieval of the
    // remaining one.
    assert!(graph.store().remove_domain_knowledge(new_id).unwrap());
    assert!(resolve_conflicts(graph.store()).is_empty());
    let hits = retrieve(graph.store(), "legacy", None, 10);
    assert!(
        !hits.is_empty() && hits[0].content.contains("legacy"),
        "after removing the winner, the remaining source is retrievable"
    );

    // And removing the old one too leaves nothing.
    assert!(graph.store().remove_domain_knowledge(old_id).unwrap());
    assert!(resolve_conflicts(graph.store()).is_empty());
    assert!(retrieve(graph.store(), "configuration", None, 10).is_empty());
    assert!(retrieve(graph.store(), "legacy", None, 10).is_empty());
}

#[test]
fn none_version_overlaps_everything_in_category() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.md");
    let b = dir.path().join("b.md");
    std::fs::write(&a, "Postmortem alpha incident summary.\n").unwrap();
    std::fs::write(&b, "Postmortem beta incident summary.\n").unwrap();

    let graph = DependencyGraph::open_in_memory().unwrap();
    // One None-tagged + one Some-tagged source in the same category →
    // None overlaps everything: conflict, newest wins.
    ingest_source(
        graph.store(),
        &make_source(KnowledgeCategory::Postmortem, a.to_str().unwrap(), None, "PM A"),
    )
    .unwrap();
    let b_id = ingest_source(
        graph.store(),
        &make_source(KnowledgeCategory::Postmortem, b.to_str().unwrap(), Some("v1"), "PM B"),
    )
    .unwrap();

    let conflicts = resolve_conflicts(graph.store());
    assert_eq!(conflicts.len(), 1, "None tag must overlap Some(v1)");
    // Newest (b) wins; the older None-tagged content is hidden.
    let hits = retrieve(graph.store(), "incident", None, 10);
    assert!(!hits.is_empty());
    assert!(
        hits.iter().all(|h| h.content.contains("beta")),
        "newest source must win over the older None-tagged one"
    );

    // Removing the winner dissolves the conflict.
    assert!(graph.store().remove_domain_knowledge(b_id).unwrap());
    assert!(resolve_conflicts(graph.store()).is_empty());
    assert!(!retrieve(graph.store(), "alpha", None, 10).is_empty());
}

#[test]
fn different_categories_do_not_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.md");
    let b = dir.path().join("b.md");
    std::fs::write(&a, "framework docs content.\n").unwrap();
    std::fs::write(&b, "entity catalog content.\n").unwrap();

    let graph = DependencyGraph::open_in_memory().unwrap();
    ingest_source(
        graph.store(),
        &make_source(KnowledgeCategory::FrameworkDocs, a.to_str().unwrap(), None, "A"),
    )
    .unwrap();
    ingest_source(
        graph.store(),
        &make_source(KnowledgeCategory::EntityCatalog, b.to_str().unwrap(), None, "B"),
    )
    .unwrap();
    assert!(resolve_conflicts(graph.store()).is_empty());
    // Both retrievable.
    assert!(!retrieve(graph.store(), "framework", None, 10).is_empty());
    assert!(!retrieve(graph.store(), "entity", None, 10).is_empty());
}

//! T062 — anti-pattern surfacing integration test.
//!
//! Seed a graph, record an anti-pattern attached to node X's id, assemble
//! context for X, verify:
//! (a) the warning text appears in formatted_context,
//! (b) hit_count was incremented (query the store),
//! (c) assembling for an unrelated node Y does NOT surface the warning.

use std::path::PathBuf;

use joey_neurocode::classifier::ComplexityTier;
use joey_neurocode::context::ContextAssembler;
use joey_neurocode::engine::CodingRequest;
use joey_neurocode::graph::node::{ArtifactKind, CodeArtifactNode};
use joey_neurocode::graph::DependencyGraph;

fn make_request(symbol: &str, file: &str) -> CodingRequest {
    CodingRequest {
        text: format!("refactor {}", symbol),
        active_file: Some(file.into()),
        active_symbols: vec![symbol.to_string()],
        project_root: PathBuf::from("."),
        token_budget_hint: 0,
    }
}

/// Seed node X (PaymentService) and unrelated node Y (AuditService).
fn seed(graph: &DependencyGraph) -> (u64, u64) {
    let x = CodeArtifactNode::new(
        ArtifactKind::Class,
        "com.enterprise.pay.PaymentService".into(),
        "com.enterprise.pay".into(),
        "src/PaymentService.java".into(),
    );
    let y = CodeArtifactNode::new(
        ArtifactKind::Class,
        "com.enterprise.audit.AuditService".into(),
        "com.enterprise.audit".into(),
        "src/AuditService.java".into(),
    );
    let x_id = graph.upsert_node(&x).unwrap();
    let y_id = graph.upsert_node(&y).unwrap();
    (x_id, y_id)
}

#[test]
fn anti_pattern_surfaced_and_hit_count_bumped_for_attached_node() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    let (x_id, _y_id) = seed(&graph);

    graph
        .store()
        .record_anti_pattern(
            "NPE:PaymentService*charge",
            "NullPointerException at line 42",
            "add null-check on amount before dereferencing",
            &[x_id],
        )
        .unwrap();

    // Look up the recorded anti-pattern's row id via the new API.
    let matches = graph.store().anti_patterns_for_artifacts(&[x_id]).unwrap();
    assert_eq!(matches.len(), 1);
    let (ap_id, sig, resolution) = &matches[0];
    assert_eq!(sig, "NPE:PaymentService*charge");
    assert!(resolution.contains("null-check"));
    assert_eq!(graph.store().get_anti_pattern_hit_count(*ap_id).unwrap(), 0);

    // Assemble context for X.
    let assembler = ContextAssembler::new(&graph);
    let ctx = assembler.assemble(
        &make_request("PaymentService", "src/PaymentService.java"),
        ComplexityTier::Frontier,
    );

    // (a) The warning section appears with signature + resolution.
    assert!(
        ctx.formatted_context
            .contains("### Known Anti-Patterns (prior failures in this area)"),
        "anti-pattern section missing:\n{}",
        ctx.formatted_context
    );
    assert!(ctx.formatted_context.contains("NPE:PaymentService*charge"));
    assert!(ctx.formatted_context.contains("add null-check"));
    assert!(ctx.formatted_context.contains("WARNING"));

    // (b) hit_count was incremented by the assembly.
    assert_eq!(graph.store().get_anti_pattern_hit_count(*ap_id).unwrap(), 1);

    // A second assembly bumps it again.
    let _ = assembler.assemble(
        &make_request("PaymentService", "src/PaymentService.java"),
        ComplexityTier::Frontier,
    );
    assert_eq!(graph.store().get_anti_pattern_hit_count(*ap_id).unwrap(), 2);
}

#[test]
fn unrelated_node_does_not_surface_warning() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    let (x_id, y_id) = seed(&graph);

    graph
        .store()
        .record_anti_pattern(
            "NPE:PaymentService*charge",
            "NullPointerException at line 42",
            "add null-check on amount before dereferencing",
            &[x_id],
        )
        .unwrap();
    let ap_id = graph.store().anti_patterns_for_artifacts(&[x_id]).unwrap()[0].0;

    // Assemble context for Y — must NOT surface X's anti-pattern.
    let assembler = ContextAssembler::new(&graph);
    let ctx = assembler.assemble(
        &make_request("AuditService", "src/AuditService.java"),
        ComplexityTier::Frontier,
    );

    assert!(
        !ctx.formatted_context.contains("Known Anti-Patterns"),
        "unrelated node must not surface the warning:\n{}",
        ctx.formatted_context
    );
    assert!(!ctx.formatted_context.contains("NPE:PaymentService"));

    // (c, hit side) hit_count unchanged for Y's assembly.
    assert_eq!(graph.store().get_anti_pattern_hit_count(ap_id).unwrap(), 0);
    // Y's id has no anti-patterns attached.
    assert!(graph.store().anti_patterns_for_artifacts(&[y_id]).unwrap().is_empty());
}

#[test]
fn multi_artifact_anti_pattern_matches_on_any_intersection() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    let (x_id, y_id) = seed(&graph);

    // Attached to BOTH X and Y — assembling for either surfaces it.
    graph
        .store()
        .record_anti_pattern("Compile:Foo.java:10", "';' expected", "add semicolon", &[x_id, y_id])
        .unwrap();

    let assembler = ContextAssembler::new(&graph);
    let ctx = assembler.assemble(
        &make_request("AuditService", "src/AuditService.java"),
        ComplexityTier::Economical,
    );
    assert!(ctx.formatted_context.contains("Known Anti-Patterns"));
    assert!(ctx.formatted_context.contains("add semicolon"));
}

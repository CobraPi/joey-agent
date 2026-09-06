//! Feature-scoped enrichment integration tests (feature 026 US7,
//! FR-010/FR-011; T027/T028/T029/T031).
//!
//! Scoping is strictly additive: a request with an empty `scope_files`
//! must behave exactly as the pre-scope code path did, while a request
//! with a scope pulls the scoped files' nodes into the assembled context
//! on top of the text-derived targets.

use std::path::PathBuf;

use joey_neurocode::auto_index::AutoIndexState;
use joey_neurocode::classifier::ComplexityTier;
use joey_neurocode::config::AutoIndexConfig;
use joey_neurocode::context::ContextAssembler;
use joey_neurocode::engine::CodingRequest;
use joey_neurocode::graph::edge::EdgeKind;
use joey_neurocode::graph::node::{ArtifactKind, CodeArtifactNode};
use joey_neurocode::graph::DependencyGraph;
use joey_neurocode::verification_plan::{VerificationPlan, VerificationStep};

/// Seed a two-file fixture "index" (mirrors the in-memory fixture style of
/// tests/context_enrichment.rs): an interface file and its implementing
/// class file, linked by an Implements edge.
fn seed_two_file_fixture(graph: &DependencyGraph) {
    let iface = CodeArtifactNode::new(
        ArtifactKind::Interface,
        "com.acme.user.UserService".into(),
        "com.acme.user".into(),
        "src/user/UserService.java".into(),
    );
    let imp = CodeArtifactNode::new(
        ArtifactKind::Class,
        "com.acme.user.UserServiceImpl".into(),
        "com.acme.user".into(),
        "src/user/UserServiceImpl.java".into(),
    );
    let iface_id = graph.upsert_node(&iface).unwrap();
    let imp_id = graph.upsert_node(&imp).unwrap();
    graph
        .upsert_edge(imp_id, iface_id, EdgeKind::Implements)
        .unwrap();
}

fn request(text: &str, scope_files: Vec<String>) -> CodingRequest {
    CodingRequest {
        text: text.into(),
        active_file: None,
        active_symbols: vec![],
        project_root: PathBuf::from("."),
        token_budget_hint: 0,
        scope_files,
    }
}

/// (a) Scoped enrichment: the request text names a symbol from ONE file
/// while the scope names the OTHER file — the assembled context must
/// include nodes from BOTH the scoped file and the text-hint symbol.
#[test]
fn scoped_request_pulls_in_scope_file_nodes() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_two_file_fixture(&graph);
    let assembler = ContextAssembler::new(&graph);

    // Text names the impl; scope names the interface's file.
    let req = request(
        "refactor UserServiceImpl",
        vec!["src/user/UserService.java".to_string()],
    );
    let ctx = assembler.assemble(&req, ComplexityTier::Frontier);

    let fqs: Vec<&str> = ctx.primary_nodes.iter().map(|n| n.fqcn.as_str()).collect();
    assert!(
        fqs.contains(&"com.acme.user.UserServiceImpl"),
        "text-hint symbol still resolved, got: {:?}",
        fqs
    );
    assert!(
        fqs.contains(&"com.acme.user.UserService"),
        "scoped file's interface must be a primary target, got: {:?}",
        fqs
    );
}

/// (b) Empty-scope no-regression: two assemblies of the same unscoped
/// request are byte-identical, the request is reported unscoped
/// (`scope_files` empty — the pre-T027 construction), and the scoped
/// file's node is NOT promoted to a primary target from the text alone
/// (it only enters via graph expansion, as before).
#[test]
fn empty_scope_is_byte_identical_across_assemblies() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_two_file_fixture(&graph);
    let assembler = ContextAssembler::new(&graph);

    let req = request("refactor UserServiceImpl", vec![]);
    assert!(req.scope_files.is_empty(), "scope path is unscoped");

    let a1 = assembler.assemble(&req, ComplexityTier::Frontier);
    let a2 = assembler.assemble(&req, ComplexityTier::Frontier);
    assert_eq!(
        a1.formatted_context, a2.formatted_context,
        "identical unscoped requests must assemble byte-identically"
    );
    assert_eq!(a1.token_estimate, a2.token_estimate);
    let fq1: Vec<&str> = a1.primary_nodes.iter().map(|n| n.fqcn.as_str()).collect();
    let fq2: Vec<&str> = a2.primary_nodes.iter().map(|n| n.fqcn.as_str()).collect();
    assert_eq!(fq1, fq2);

    // Pre-scope behavior: the text names only the impl, so the interface
    // is not a primary target (it arrives via Implements expansion).
    assert!(
        !fq1.contains(&"com.acme.user.UserService"),
        "unscoped request must not promote the interface file to primary, got: {:?}",
        fq1
    );
    assert!(fq1.contains(&"com.acme.user.UserServiceImpl"));
}

/// (c) acceptance_criteria survive `scoped()` narrowing.
#[test]
fn acceptance_criteria_survive_scoped() {
    let plan = VerificationPlan {
        steps: vec![VerificationStep {
            name: "core tests".to_string(),
            command: "cargo test -p joey-core".to_string(),
            parse: "exit_code".to_string(),
            timeout_sec: 300,
            required: false,
        }],
        risk_triggered_review: false,
        acceptance_criteria: vec![],
    }
    .with_acceptance_criteria(vec![
        "given a scoped plan".to_string(),
        "then criteria survive narrowing".to_string(),
    ]);

    let scoped = plan.scoped(&["joey-core".to_string()]);
    assert_eq!(scoped.steps.len(), 1);
    assert!(scoped.steps[0].required);
    assert_eq!(
        scoped.acceptance_criteria,
        vec![
            "given a scoped plan".to_string(),
            "then criteria survive narrowing".to_string(),
        ]
    );
}

/// (d) pending_prioritized orders the edited set scope-first (path-suffix
/// match), remainder in existing BTreeSet order.
#[test]
fn pending_prioritized_scope_first() {
    let mut s = AutoIndexState::new(&AutoIndexConfig::default());
    s.record_edit("z.rs", 1, 1);
    s.record_edit("a/scope.rs", 1, 1);
    s.record_edit("b.rs", 1, 1);

    let ordered = s.pending_prioritized(&["a/scope.rs".to_string()]);
    assert_eq!(
        ordered.first().map(String::as_str),
        Some("a/scope.rs"),
        "scope entry must be first, got: {:?}",
        ordered
    );
    assert_eq!(
        &ordered[1..],
        &["b.rs".to_string(), "z.rs".to_string()],
        "remainder stays in BTreeSet order"
    );
}

/// (e) SC-005a timing: scoped assembly on the fixture stays well under
/// the 3-second bar; the measured duration is printed for recording.
#[test]
fn scoped_assembly_under_3_seconds() {
    let graph = DependencyGraph::open_in_memory().unwrap();
    seed_two_file_fixture(&graph);
    let assembler = ContextAssembler::new(&graph);

    let req = request(
        "refactor UserServiceImpl",
        vec!["src/user/UserService.java".to_string()],
    );
    let start = std::time::Instant::now();
    let ctx = assembler.assemble(&req, ComplexityTier::Frontier);
    let elapsed = start.elapsed();

    eprintln!(
        "SC-005a scoped assembly on fixture: {} ms",
        elapsed.as_millis()
    );
    assert!(!ctx.primary_nodes.is_empty());
    assert!(
        elapsed.as_secs_f64() < 3.0,
        "scoped assembly took {:?} — over the 3s bar",
        elapsed
    );
}

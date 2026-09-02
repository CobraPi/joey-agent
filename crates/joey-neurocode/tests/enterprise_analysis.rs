//! Spec 023 T010 [US1]: end-to-end integration test for the enterprise
//! analysis plane (quickstart §2 — "Analysis plane on").
//!
//! One `AnalysisEngine::analyze()` call over a fixture repo with layered
//! instruction files and a hub-type change must report: target +
//! impacted-closure artifacts, combined effective policies with surfaced
//! conflicts, GraphHub complexity signal + tier, risk factors + execution
//! hint, and the module-scoped verification plan.

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use joey_neurocode::config::VerifyStepConfig;
use joey_neurocode::{
    AnalysisEngine, ArtifactKind, CodeArtifactNode, ComplexityClassifier, ComplexityTier,
    DependencyGraph, EdgeKind, EnterpriseTaskAnalyzer, NeuroCodeConfig, PolicyLayer, SignalKind,
};

/// Fixture repo: root `AGENTS.md` (Repository layer, default `**` glob)
/// carrying a directive that polarity-conflicts with the nested
/// `joey-core/AGENTS.md` (Module layer, directory-scoped `joey-core/**`
/// glob), plus the plain hub source file the request targets.
fn fixture_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("fixture tempdir");
    let root = dir.path();
    fs::write(
        root.join("AGENTS.md"),
        concat!(
            "# Repo conventions\n",
            "- Run cargo fmt before committing\n",
            "- Always use raw SQL in migrations\n",
        ),
    )
    .expect("write root AGENTS.md");
    fs::create_dir_all(root.join("joey-core/src")).expect("mkdir joey-core/src");
    fs::write(
        root.join("joey-core/AGENTS.md"),
        "- Never use raw SQL in migrations\n",
    )
    .expect("write joey-core/AGENTS.md");
    fs::write(root.join("joey-core/src/Hub.java"), "").expect("write Hub.java");
    dir
}

/// Seed the structural graph: one hub class plus 4 clients (each in its
/// own `com.ex.<letter>` package) injecting the hub.
/// Returns `(graph, hub id, client ids)`.
fn seed_graph() -> (DependencyGraph, u64, Vec<u64>) {
    let graph = DependencyGraph::open_in_memory().expect("open in-memory graph");
    let hub_id = graph
        .upsert_node(&CodeArtifactNode::new(
            ArtifactKind::Class,
            "com.ex.Hub".to_string(),
            "com.ex".to_string(),
            "joey-core/src/Hub.java".to_string(),
        ))
        .expect("upsert hub node");
    let mut clients = Vec::new();
    for i in 0..4u32 {
        let package = format!("com.ex.{}", (b'a' + i as u8) as char);
        let client_id = graph
            .upsert_node(&CodeArtifactNode::new(
                ArtifactKind::Class,
                format!("com.ex.c{}", i),
                package,
                format!("joey-core/src/c{}.java", i),
            ))
            .expect("upsert client node");
        graph
            .upsert_edge(client_id, hub_id, EdgeKind::Injects)
            .expect("upsert Injects edge");
        clients.push(client_id);
    }
    (graph, hub_id, clients)
}

/// The shared request: refactor the hub service with the hub file active.
fn hub_request(root: &Path) -> joey_neurocode::CodingRequest {
    joey_neurocode::CodingRequest {
        text: "refactor the hub service".to_string(),
        active_file: Some("joey-core/src/Hub.java".to_string()),
        active_symbols: vec![],
        project_root: root.to_path_buf(),
        token_budget_hint: 0,
    }
}

/// Build the analysis engine over the fixture repo + the seeded shared
/// graph, with the two-step base verification config. The SAME
/// `Arc<Mutex<DependencyGraph>>` is shared by the classifier and the
/// engine (`DependencyGraph` is not `Clone`): seed once, wrap, clone the
/// `Arc` for both.
fn make_engine(root: &Path) -> (AnalysisEngine, u64, Vec<u64>) {
    let (graph, hub_id, clients) = seed_graph();
    let shared = Arc::new(Mutex::new(graph));
    let mut config = NeuroCodeConfig::default();
    config.verify.steps = vec![
        VerifyStepConfig {
            name: "core-tests".to_string(),
            command: "cargo test -p joey-core".to_string(),
            parse: "plain".to_string(),
            timeout_sec: 120,
        },
        VerifyStepConfig {
            name: "cli-tests".to_string(),
            command: "cargo test -p joey-cli".to_string(),
            parse: "plain".to_string(),
            timeout_sec: 120,
        },
    ];
    let engine = AnalysisEngine::new(
        ComplexityClassifier::default().with_graph(shared.clone()),
        Some(shared),
        root.to_path_buf(),
        &config,
    );
    (engine, hub_id, clients)
}

/// 1. Targets resolve from the active file; the impacted closure (BFS over
/// incoming edges) finds all four clients that inject the hub.
#[test]
fn analysis_reports_targets_and_impacted_closure() {
    let dir = fixture_repo();
    let (engine, hub_id, clients) = make_engine(dir.path());
    let analysis = engine.analyze(&hub_request(dir.path()));

    assert_eq!(analysis.target_artifacts, vec![hub_id]);
    for client_id in &clients {
        assert!(
            analysis.impacted_artifacts.contains(client_id),
            "client {} missing from impacted closure {:?}",
            client_id,
            analysis.impacted_artifacts
        );
    }
}

/// 2. Effective policies combine the root Repository directive with the
/// nested Module directive, and the polarity clash on migrations
/// (Always vs Never) across DIFFERENT layers is surfaced as a conflict —
/// never silently dropped.
#[test]
fn analysis_combines_policies_and_surfaces_conflicts() {
    let dir = fixture_repo();
    let (engine, _hub_id, _clients) = make_engine(dir.path());
    let analysis = engine.analyze(&hub_request(dir.path()));

    assert!(
        analysis.effective_policies.iter().any(|b| b.directive
            == "Run cargo fmt before committing"
            && b.layer == PolicyLayer::Repository),
        "missing Repository directive in {:?}",
        analysis.effective_policies
    );
    assert!(
        analysis
            .effective_policies
            .iter()
            .any(|b| b.directive == "Never use raw SQL in migrations"
                && b.layer == PolicyLayer::Module),
        "missing Module directive in {:?}",
        analysis.effective_policies
    );
    assert!(
        analysis.policy_conflicts.iter().any(|c| {
            c.directive_a.contains("migrations") && c.directive_b.contains("migrations")
        }),
        "expected the migrations polarity conflict, got {:?}",
        analysis.policy_conflicts
    );
}

/// 3. Graph evidence participates: the fan-in hub fires a GraphHub signal
/// and the route lands on the Frontier tier.
#[test]
fn analysis_scores_graph_hub_and_tier() {
    let dir = fixture_repo();
    let (engine, _hub_id, _clients) = make_engine(dir.path());
    let analysis = engine.analyze(&hub_request(dir.path()));

    assert!(
        analysis
            .complexity
            .signals
            .iter()
            .any(|s| s.kind == SignalKind::GraphHub),
        "no GraphHub signal in {:?}",
        analysis.complexity.signals
    );
    assert_eq!(analysis.model_tier, ComplexityTier::Frontier);
}

/// 4. Risk assessment carries the fan-in factor with hub evidence, and the
/// provisional execution hint reports at least one independent component.
#[test]
fn analysis_risk_and_hint() {
    let dir = fixture_repo();
    let (engine, _hub_id, _clients) = make_engine(dir.path());
    let analysis = engine.analyze(&hub_request(dir.path()));

    assert!(!analysis.risk.factors.is_empty());
    assert!(
        analysis
            .risk
            .factors
            .iter()
            .any(|f| f.evidence.contains("com.ex.Hub")),
        "no fan-in factor evidence mentioning the hub in {:?}",
        analysis.risk.factors
    );
    assert!(analysis.execution_hint.independent_components >= 1);
}

/// 5. Verification is scoped to the impacted modules: every source path's
/// first component is `joey-core`, so only the `core-tests` step (its
/// command contains "joey-core") survives, forced `required = true`;
/// `cli-tests` is dropped (its command lacks "joey-core").
#[test]
fn analysis_scopes_verification() {
    let dir = fixture_repo();
    let (engine, _hub_id, _clients) = make_engine(dir.path());
    let analysis = engine.analyze(&hub_request(dir.path()));

    assert_eq!(
        analysis.verification.steps.len(),
        1,
        "scoped steps: {:?}",
        analysis.verification.steps
    );
    assert_eq!(analysis.verification.steps[0].name, "core-tests");
    assert!(analysis.verification.steps[0].required);
}

/// 6. Flag-off parity (SC-001 flavor): a classifier with NO graph attached
/// produces zero GraphHub signals for the same request.
#[test]
fn flag_off_classifier_has_no_graph_signals() {
    let dir = fixture_repo();
    let classifier = ComplexityClassifier::default();
    let route = classifier.classify(&hub_request(dir.path()));

    assert_eq!(
        route
            .signals
            .iter()
            .filter(|s| s.kind == SignalKind::GraphHub)
            .count(),
        0,
        "unexpected GraphHub signals without a graph: {:?}",
        route.signals
    );
}

/// 7. Revision binding: the fixture tempdir is not a git repository, so
/// the analysis binds revision "unknown".
#[test]
fn analysis_revision_unknown_without_git() {
    let dir = fixture_repo();
    let (engine, _hub_id, _clients) = make_engine(dir.path());
    let analysis = engine.analyze(&hub_request(dir.path()));

    assert_eq!(analysis.revision, "unknown");
}

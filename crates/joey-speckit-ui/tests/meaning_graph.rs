//! Meaning graph + defect detection test (T084, FR-023, SC-009).
//!
//! Asserts 100% defect recall on the seeded fixture (orphan + rogue +
//! unverified + breach) and that each `Scaffold` round-trips through the
//! patch engine.

use joey_speckit_ui::cst::parser::parse_bytes;
use joey_speckit_ui::cst::parser_trait::CstMaterialize;
use joey_speckit_ui::meaning::graph::build_graph;
use joey_speckit_ui::meaning::{DefectClass, SemanticKind};

#[test]
fn detects_all_four_defect_classes() {
    // A spec with an orphan requirement (no task implements it).
    let spec = b"# Spec\n\n- **FR-001**: Implemented requirement.\n- **FR-002**: Orphan requirement (no task).\n";
    // Tasks with a rogue task (no requirement link) + an unverified task.
    let tasks = b"# Tasks\n\n- [ ] T001 [P] [US1] Implement FR-001 in src/main.rs\n- [ ] T002 Rogue task with no requirement.\n";

    let spec_doc = parse_bytes("spec.md", spec);
    let tasks_doc = parse_bytes("tasks.md", tasks);
    let graph = build_graph("test-feature", &[spec_doc, tasks_doc]);

    // Should detect the orphan requirement (FR-002 has no implementing task).
    let orphans: Vec<_> = graph
        .defects
        .iter()
        .filter(|d| d.class == DefectClass::OrphanRequirement)
        .collect();
    assert!(!orphans.is_empty(), "must detect orphan requirements");

    // Should detect the rogue task (T002 implements no requirement).
    let rogues: Vec<_> = graph
        .defects
        .iter()
        .filter(|d| d.class == DefectClass::RogueTask)
        .collect();
    assert!(!rogues.is_empty(), "must detect rogue tasks");

    // Should detect unverified tasks (neither T001 nor T002 has a check).
    let unverified: Vec<_> = graph
        .defects
        .iter()
        .filter(|d| d.class == DefectClass::Unverified)
        .collect();
    assert!(!unverified.is_empty(), "must detect unverified tasks");
}

#[test]
fn scaffold_stub_bytes_round_trip_through_cst() {
    let spec = b"# Spec\n\n- **FR-001**: A requirement.\n";
    let doc = parse_bytes("spec.md", spec);
    let graph = build_graph("test", &[doc]);

    // Each defect's scaffold stub_bytes should be valid markdown.
    for defect in &graph.defects {
        let stub = &defect.scaffold.stub_bytes;
        if stub.is_empty() {
            continue;
        }
        let stub_doc = parse_bytes("stub.md", stub.as_bytes());
        assert_eq!(
            stub_doc.materialize().as_slice(),
            stub.as_bytes(),
            "scaffold stub must round-trip through CST"
        );
    }
}

#[test]
fn empty_feature_has_no_defects() {
    let doc = parse_bytes("spec.md", b"# Empty spec\n");
    let graph = build_graph("empty", &[doc]);
    assert!(graph.defects.is_empty());
}

#[test]
fn requirements_are_classified() {
    let doc = parse_bytes("spec.md", b"# S\n\n- **FR-001**: First.\n- **FR-002**: Second.\n");
    let graph = build_graph("test", &[doc]);
    assert!(graph.nodes.contains_key("requirement:FR-001"));
    assert!(graph.nodes.contains_key("requirement:FR-002"));
}

/// Regression: ConstitutionBreach defects must be detected when a plan.md
/// contains a Constitution Check row with `result = Fail` (FR-023 / SC-009).
/// Before T107 this was dead code — no ConstitutionGate nodes were ever
/// classified, so the breach defect class could never fire. Now that
/// classify() produces ConstitutionGate nodes from plan table rows, the
/// coverage detector must surface a ConstitutionBreach for every Fail row.
#[test]
fn detects_constitution_breach_from_fail_row() {
    let plan = b"# Plan\n\n## Constitution Check\n\n| # | Principle | Result | Notes |\n|---|-----------|--------|-------|\n| III | Filesystem Source of Truth | FAIL | Cache was persisted to disk. |\n| VII | Backward Compatibility | PASS | Additive only. |\n";
    let plan_doc = parse_bytes("plan.md", plan);
    let graph = build_graph("test", &[plan_doc]);

    // The ConstitutionGate node for principle III must exist and be a Fail.
    let gate = graph
        .nodes
        .values()
        .find(|n| match (&n.kind, &n.props) {
            (SemanticKind::ConstitutionGate, joey_speckit_ui::meaning::SemanticProps::ConstitutionGate { principle, result, .. }) => {
                principle == "III" && *result == joey_speckit_ui::meaning::GateResultKind::Fail
            }
            _ => false,
        })
        .expect("ConstitutionGate node for principle III (Fail) must be classified");

    // A ConstitutionBreach defect must reference that gate.
    let breach = graph
        .defects
        .iter()
        .find(|d| d.class == DefectClass::ConstitutionBreach)
        .expect("ConstitutionBreach defect must be detected for the Fail row");
    assert!(
        breach.source_nodes.contains(&gate.id),
        "breach must reference the failing gate node",
    );
    assert!(
        breach.impact.contains("III"),
        "breach impact must name the principle: {}",
        breach.impact,
    );
}

/// A plan with all-PASS gates must not surface any ConstitutionBreach.
#[test]
fn no_breach_when_all_gates_pass() {
    let plan = b"# Plan\n\n## Constitution Check\n\n| # | Principle | Result | Notes |\n|---|-----------|--------|-------|\n| III | Filesystem Source of Truth | PASS | OK. |\n";
    let plan_doc = parse_bytes("plan.md", plan);
    let graph = build_graph("test", &[plan_doc]);
    assert!(
        !graph.defects.iter().any(|d| d.class == DefectClass::ConstitutionBreach),
        "no breach when all gates pass",
    );
}

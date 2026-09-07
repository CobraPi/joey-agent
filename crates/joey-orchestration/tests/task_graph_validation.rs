//! Spec 023, T014 (US2): integration test for task-graph validation.
//!
//! Exercises the public `joey_orchestration::task_graph` surface end to
//! end: strict planner JSON acceptance (the quickstart §3 contract
//! example), the six structural rejections with their rule constants and
//! offending task ids, the sequenced/legal controls for the WRITE_OVERLAP
//! and UNVERIFIED_HIGH_RISK rules, and the FR-007 legacy
//! `<workstreams>` conversion equivalence.
//!
//! Plain `#[test]` functions only — no async, no tokio runtime needed.

use std::path::{Path, PathBuf};

use joey_orchestration::evaluator::VerificationPlanView;
use joey_orchestration::task_graph::{
    rules, AcceptanceCriterion, IsolationMode, LegacyWorkstream, ModelTier, RiskLevel, TaskGraph,
    TaskId, TaskNode, TaskStatus, ValidationError, WorkerRole,
};

fn id(s: &str) -> TaskId {
    TaskId::new(s).expect("valid id")
}

/// A minimal valid node: one acceptance criterion, low risk, no
/// verification steps, shared checkout, empty read/write sets.
fn node(id_str: &str, deps: &[&str]) -> TaskNode {
    TaskNode {
        id: id(id_str),
        objective: format!("do {}", id_str),
        dependencies: deps.iter().map(|d| id(d)).collect(),
        read_set: vec![],
        write_set: vec![],
        artifact_ids: vec![],
        role: WorkerRole::Implementor,
        model_tier: ModelTier::Economical,
        risk: RiskLevel::Low,
        acceptance: vec![AcceptanceCriterion {
            criterion: "tests pass".to_string(),
            kind: "command".to_string(),
        }],
        verification: VerificationPlanView::default(),
        isolation: IsolationMode::SharedCheckout,
        status: TaskStatus::Pending,
        attempts: 0,
    }
}

fn graph(nodes: Vec<TaskNode>) -> TaskGraph {
    TaskGraph {
        nodes: nodes.into_iter().map(|n| (n.id.clone(), n)).collect(),
        ..TaskGraph::default()
    }
}

fn rule_errors<'a>(errs: &'a [ValidationError], rule: &str) -> Vec<&'a ValidationError> {
    errs.iter().filter(|e| e.rule == rule).collect()
}

fn names_both(err: &ValidationError, first: &str, second: &str) {
    assert_eq!(
        err.task_ids.len(),
        2,
        "expected both offending ids, got {:?}",
        err.task_ids
    );
    assert!(
        err.task_ids.contains(&first.to_string()),
        "id {} missing from {:?}",
        first,
        err.task_ids
    );
    assert!(
        err.task_ids.contains(&second.to_string()),
        "id {} missing from {:?}",
        second,
        err.task_ids
    );
}

// ---- 1. quickstart §3 happy path: the contract example parses ----

/// The strict planner-JSON contract example (quickstart §3): format tag,
/// baseline, single `task-auth` node with a scoped verification step.
const PLAN: &str = r#"{"format":"joey-taskgraph/1","baseline_revision":"abc123","tasks":[{"id":"task-auth","objective":"Implement token refresh","dependencies":[],"read_set":[],"write_set":["src/auth.rs"],"artifact_ids":[42,1337],"role":"implementor","model_tier":"economical","risk":"medium","acceptance":[{"criterion":"cargo test -p joey-core auth","kind":"command"}],"verification":{"steps":[{"name":"scoped-tests","command":"cargo test -p joey-core auth","parse":"plain","timeout_sec":300,"required":true}],"risk_triggered_review":false},"isolation":"isolated_worktree"}]}"#;

#[test]
fn strict_json_acceptance() {
    let g = TaskGraph::from_strict_json(PLAN, Path::new("/tmp/project"))
        .expect("contract example must parse and validate");
    let n = g.node(&id("task-auth")).expect("task-auth present");
    assert_eq!(n.isolation, IsolationMode::IsolatedWorktree);
    assert_eq!(n.model_tier, ModelTier::Economical);
    assert_eq!(n.risk, RiskLevel::Medium);
    assert_eq!(n.artifact_ids, vec![42, 1337]);
    assert!(
        n.verification.steps[0].required,
        "scoped-tests step must be required"
    );
}

// ---- 2..7. the six rejections (quickstart §3) ----

#[test]
fn reject_dependency_cycle() {
    // a → b → a.
    let g = graph(vec![node("task-a", &["task-b"]), node("task-b", &["task-a"])]);
    let errs = g.validate().unwrap_err();
    assert_eq!(errs.len(), 1, "only the cycle violation: {:?}", errs);
    assert_eq!(errs[0].rule, rules::CYCLE);
    names_both(&errs[0], "task-a", "task-b");
}

#[test]
fn reject_unknown_dependency() {
    // task-b depends on the nonexistent task-zzz.
    let g = graph(vec![node("task-b", &["task-zzz"])]);
    let errs = g.validate().unwrap_err();
    assert_eq!(errs.len(), 1, "only the unknown-dep violation: {:?}", errs);
    assert_eq!(errs[0].rule, rules::UNKNOWN_DEP);
    names_both(&errs[0], "task-zzz", "task-b");
}

#[test]
fn reject_concurrent_write_overlap_and_sequenced_control() {
    // Two independent tasks both writing src/main.rs.
    let mut a = node("task-a", &[]);
    a.write_set = vec![PathBuf::from("src/main.rs")];
    let mut b = node("task-b", &[]);
    b.write_set = vec![PathBuf::from("src/main.rs")];
    let errs = graph(vec![a, b]).validate().unwrap_err();
    let overlap = rule_errors(&errs, rules::WRITE_OVERLAP);
    assert_eq!(overlap.len(), 1, "exactly one WRITE_OVERLAP: {:?}", errs);
    names_both(overlap[0], "task-a", "task-b");

    // Control: b depends on a ⇒ sequenced ⇒ the shared path is legal.
    let mut a = node("task-a", &[]);
    a.write_set = vec![PathBuf::from("src/main.rs")];
    let mut b = node("task-b", &["task-a"]);
    b.write_set = vec![PathBuf::from("src/main.rs")];
    assert_eq!(graph(vec![a, b]).validate(), Ok(()));
}

#[test]
fn reject_path_escape() {
    // Parent traversal in the write set.
    let mut a = node("task-a", &[]);
    a.write_set = vec![PathBuf::from("../outside.rs")];
    let errs = graph(vec![a]).validate().unwrap_err();
    assert_eq!(errs.len(), 1, "only the escape violation: {:?}", errs);
    assert_eq!(errs[0].rule, rules::PATH_ESCAPE);
    assert_eq!(errs[0].task_ids, vec!["task-a".to_string()]);

    // Absolute path in the read set.
    let mut a = node("task-a", &[]);
    a.read_set = vec![PathBuf::from("/abs/x")];
    let errs = graph(vec![a]).validate().unwrap_err();
    assert_eq!(errs.len(), 1, "only the escape violation: {:?}", errs);
    assert_eq!(errs[0].rule, rules::PATH_ESCAPE);
    assert_eq!(errs[0].task_ids, vec!["task-a".to_string()]);
}

#[test]
fn reject_missing_acceptance() {
    let mut a = node("task-a", &[]);
    a.acceptance = vec![];
    let errs = graph(vec![a]).validate().unwrap_err();
    assert_eq!(errs.len(), 1, "only the acceptance violation: {:?}", errs);
    assert_eq!(errs[0].rule, rules::NO_ACCEPTANCE);
    assert_eq!(errs[0].task_ids, vec!["task-a".to_string()]);
}

#[test]
fn reject_unverified_high_risk_and_reviewed_control() {
    // High risk, zero verification steps, no risk-triggered review.
    let mut a = node("task-a", &[]);
    a.risk = RiskLevel::High;
    a.verification = VerificationPlanView {
        steps: vec![],
        risk_triggered_review: false,
    };
    let errs = graph(vec![a]).validate().unwrap_err();
    assert_eq!(errs.len(), 1, "only the high-risk violation: {:?}", errs);
    assert_eq!(errs[0].rule, rules::UNVERIFIED_HIGH_RISK);
    assert_eq!(errs[0].task_ids, vec!["task-a".to_string()]);

    // Control: same plan but a risk-triggered review ⇒ accepted.
    let mut a = node("task-a", &[]);
    a.risk = RiskLevel::High;
    a.verification = VerificationPlanView {
        steps: vec![],
        risk_triggered_review: true,
    };
    assert_eq!(graph(vec![a]).validate(), Ok(()));
}

// ---- 8. FR-007 legacy <workstreams> conversion equivalence ----

#[test]
fn legacy_conversion_equivalence() {
    let legacy = vec![
        LegacyWorkstream {
            id: "1".to_string(),
            focus: "Refactor auth".to_string(),
        },
        LegacyWorkstream {
            id: "2".to_string(),
            focus: "Update docs".to_string(),
        },
    ];
    let g = TaskGraph::from_workstreams(&legacy, "base");
    // Restored contract: empty write sets are undeclared — the
    // scheduler's ConflictAnalyzer sequences them at dispatch — so the
    // converted graph (two unrelated empty-write-set tasks) validates.
    assert_eq!(g.validate(), Ok(()), "empty×empty pair is legal");
    assert_eq!(g.nodes.len(), 2);

    // objective = focus, 1:1.
    assert_eq!(
        g.node(&id("workstream-1")).unwrap().objective,
        "Refactor auth"
    );
    assert_eq!(
        g.node(&id("workstream-2")).unwrap().objective,
        "Update docs"
    );

    // Undeclared writes ⇒ shared checkout and empty write sets; empty
    // write sets are sequenced at dispatch, not enforced at validation …
    for key in ["workstream-1", "workstream-2"] {
        let n = g.node(&id(key)).unwrap();
        assert_eq!(n.isolation, IsolationMode::SharedCheckout);
        assert!(n.write_set.is_empty(), "{} writes undeclared", key);
    }

    // … while an EXPLICIT overlapping pair is not: two tasks both writing
    // the same path ⇒ WRITE_OVERLAP naming both.
    let mut a = node("task-a", &[]);
    a.write_set = vec![PathBuf::from("src/lib.rs")];
    let mut b = node("task-b", &[]);
    b.write_set = vec![PathBuf::from("src/lib.rs")];
    let errs = graph(vec![a, b]).validate().unwrap_err();
    let overlap = rule_errors(&errs, rules::WRITE_OVERLAP);
    assert_eq!(overlap.len(), 1, "exactly one WRITE_OVERLAP: {:?}", errs);
    names_both(overlap[0], "task-a", "task-b");
}

// ---- 9. FR-010: rejections name the rule and the offending ids ----

#[test]
fn rejection_messages_name_rule_and_ids() {
    // ValidationError carries no Display impl; the rule constant lives in
    // the `rule` field and the offending ids in `task_ids`/`detail`.
    // Asserted programmatically on the fields.

    // Cycle: a → b → a.
    let g = graph(vec![node("task-a", &["task-b"]), node("task-b", &["task-a"])]);
    let errs = g.validate().unwrap_err();
    let e = rule_errors(&errs, rules::CYCLE)
        .into_iter()
        .next()
        .expect("CYCLE error present");
    assert_eq!(e.rule, rules::CYCLE, "rule field carries the constant");
    assert!(!e.detail.is_empty(), "detail must explain the rejection");
    for offending in ["task-a", "task-b"] {
        assert!(
            e.task_ids.contains(&offending.to_string()),
            "offending id {} must appear in task_ids {:?} (or detail {:?})",
            offending,
            e.task_ids,
            e.detail
        );
    }

    // Overlap: two independent writers of src/main.rs.
    let mut a = node("task-a", &[]);
    a.write_set = vec![PathBuf::from("src/main.rs")];
    let mut b = node("task-b", &[]);
    b.write_set = vec![PathBuf::from("src/main.rs")];
    let errs = graph(vec![a, b]).validate().unwrap_err();
    let e = rule_errors(&errs, rules::WRITE_OVERLAP)
        .into_iter()
        .next()
        .expect("WRITE_OVERLAP error present");
    assert_eq!(e.rule, rules::WRITE_OVERLAP, "rule field carries the constant");
    assert!(!e.detail.is_empty(), "detail must explain the rejection");
    for offending in ["task-a", "task-b"] {
        assert!(
            e.task_ids.contains(&offending.to_string())
                || e.detail.contains(offending),
            "offending id {} must appear in task_ids {:?} or detail {:?}",
            offending,
            e.task_ids,
            e.detail
        );
    }
    // The overlap detail additionally names the shared path.
    assert!(
        e.detail.contains("src/main.rs"),
        "detail names the shared path: {}",
        e.detail
    );
}

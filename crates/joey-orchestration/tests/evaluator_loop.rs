//! Spec 023 — T024 (US5): evaluation-loop integration tests.
//!
//! Exercises the committed evaluator API end-to-end over the four
//! quickstart §7 scenarios:
//!
//! 1. `repaired_success` — one failure then repair then pass,
//! 2. `escalation_then_eventual_pass` — the FR-021 tier ladder,
//! 3. `terminal_failure` — exhaustion at the highest tier,
//! 4. `degraded_blocked_until_acknowledgment` — FR-031 Degraded never
//!    touches the repair ledger.
//!
//! All gates are local `VerificationGate` impls; the task node is a
//! minimal 14-field literal per `task_graph.rs`.

use std::path::Path;
use std::sync::Mutex;

use joey_orchestration::evaluator::{
    CommandFailure, DefectBundle, EvaluationDirective, Evaluator, GateOutcome, RepairLedger,
    VerificationGate, VerificationPlanView,
};
use joey_orchestration::task_graph::{
    AcceptanceCriterion, IsolationMode, ModelTier, RiskLevel, TaskId, TaskNode, TaskStatus,
    WorkerRole,
};

/// Task id shared by every scenario's node (each test uses its own ledger).
const TASK_ID: &str = "t-eval-loop";
/// The failing command every gate reports in its defect bundle.
const FAIL_COMMAND: &str = "cargo test -p x";

fn node(tier: ModelTier) -> TaskNode {
    TaskNode {
        id: TaskId::new(TASK_ID).unwrap(),
        objective: "integration: evaluator loop".to_string(),
        dependencies: vec![],
        read_set: vec![],
        write_set: vec![],
        artifact_ids: vec![],
        role: WorkerRole::Implementor,
        model_tier: tier,
        risk: RiskLevel::Low,
        acceptance: vec![AcceptanceCriterion {
            criterion: "cargo test -p x passes".to_string(),
            kind: "command".to_string(),
        }],
        verification: VerificationPlanView::default(),
        isolation: IsolationMode::SharedCheckout,
        status: TaskStatus::Evaluating,
        attempts: 0,
    }
}

/// Short cause tag for sequence assertions.
fn tag(d: &EvaluationDirective) -> &'static str {
    match d {
        EvaluationDirective::Complete => "Complete",
        EvaluationDirective::Repair { .. } => "Repair",
        EvaluationDirective::Escalate { .. } => "Escalate",
        EvaluationDirective::Fail { .. } => "Fail",
        EvaluationDirective::MarkDegraded => "MarkDegraded",
    }
}

/// The defect bundle every failing gate reports (FR-020).
fn failed_bundle() -> GateOutcome {
    GateOutcome::Failed(DefectBundle {
        task_id: TASK_ID.to_string(),
        failed_commands: vec![CommandFailure {
            command: FAIL_COMMAND.to_string(),
            exit: 1,
            errors: vec!["assertion failed".to_string()],
        }],
        ..Default::default()
    })
}

/// Fails while an inner counter is > 0 (decrementing), then passes.
struct FailNTimes {
    remaining: Mutex<usize>,
}

impl FailNTimes {
    fn new(remaining: usize) -> Self {
        Self {
            remaining: Mutex::new(remaining),
        }
    }
}

#[async_trait::async_trait]
impl VerificationGate for FailNTimes {
    async fn run(&self, _plan: &VerificationPlanView, _workdir: &Path) -> GateOutcome {
        let mut remaining = self.remaining.lock().unwrap();
        if *remaining > 0 {
            *remaining -= 1;
            failed_bundle()
        } else {
            GateOutcome::Passed
        }
    }
}

/// Fails the first `n` evaluations, then passes forever.
struct AlwaysFailUntil {
    n: usize,
    calls: Mutex<usize>,
}

impl AlwaysFailUntil {
    fn new(n: usize) -> Self {
        Self {
            n,
            calls: Mutex::new(0),
        }
    }
}

#[async_trait::async_trait]
impl VerificationGate for AlwaysFailUntil {
    async fn run(&self, _plan: &VerificationPlanView, _workdir: &Path) -> GateOutcome {
        let mut calls = self.calls.lock().unwrap();
        *calls += 1;
        if *calls <= self.n {
            failed_bundle()
        } else {
            GateOutcome::Passed
        }
    }
}

/// Fails every evaluation.
struct AlwaysFailGate;

#[async_trait::async_trait]
impl VerificationGate for AlwaysFailGate {
    async fn run(&self, _plan: &VerificationPlanView, _workdir: &Path) -> GateOutcome {
        failed_bundle()
    }
}

/// Returns Degraded while the flag is set, Passed once it is cleared
/// (simulating explicit acknowledgment — the command becomes runnable).
struct ToggleGate {
    degraded: Mutex<bool>,
}

#[async_trait::async_trait]
impl VerificationGate for ToggleGate {
    async fn run(&self, _plan: &VerificationPlanView, _workdir: &Path) -> GateOutcome {
        if *self.degraded.lock().unwrap() {
            GateOutcome::Degraded
        } else {
            GateOutcome::Passed
        }
    }
}

// --- Scenario 1: repaired_success -------------------------------------

#[tokio::test]
async fn repaired_success() {
    let ev = Evaluator::new(2);
    let task = node(ModelTier::Economical);
    let gate = FailNTimes::new(1);
    let mut ledger = RepairLedger::default();

    // Evaluation 1: gate fails once -> Repair carrying the defect.
    let first = ev.evaluate(&task, &gate, Path::new("."), &mut ledger).await;
    match first.directive {
        EvaluationDirective::Repair { defect } => {
            assert_eq!(defect.task_id, TASK_ID);
            assert_eq!(defect.failed_commands.len(), 1);
            assert_eq!(defect.failed_commands[0].command, FAIL_COMMAND);
            assert_eq!(defect.failed_commands[0].exit, 1);
            assert_eq!(
                defect.failed_commands[0].errors,
                vec!["assertion failed".to_string()]
            );
        }
        other => panic!("expected Repair, got {:?}", other),
    }

    // Evaluation 2 (same ledger): gate passes -> Complete.
    let second = ev.evaluate(&task, &gate, Path::new("."), &mut ledger).await;
    assert_eq!(second.directive, EvaluationDirective::Complete);
    assert_eq!(second.detail, "gate passed");

    // The committed evaluator bumps `attempts` only on Failed outcomes,
    // so exactly one failure was recorded for this two-evaluation loop.
    let entry = ledger.get(TASK_ID).unwrap();
    assert_eq!(entry.attempts, 1);
    assert_eq!(entry.tier_attempts, 1);
}

// --- Scenario 2: escalation_then_eventual_pass ------------------------

#[tokio::test]
async fn escalation_then_eventual_pass() {
    let ev = Evaluator::new(1);
    let task = node(ModelTier::Economical);
    // The asserted sequence Repair, Escalate, Repair, Complete requires
    // gate failures at evaluations 1..=3 (pass on #4).
    let gate = AlwaysFailUntil::new(3);
    let mut ledger = RepairLedger::default();

    // #1: per-tier budget 1/1 available -> Repair at Economical.
    let out1 = ev.evaluate(&task, &gate, Path::new("."), &mut ledger).await;
    assert!(matches!(out1.directive, EvaluationDirective::Repair { .. }));
    assert_eq!(out1.detail, "repair attempt 1/1 at tier Economical");

    // #2: tier budget exhausted below the ladder top -> Escalate.
    let out2 = ev.evaluate(&task, &gate, Path::new("."), &mut ledger).await;
    assert!(matches!(out2.directive, EvaluationDirective::Escalate { .. }));
    assert_eq!(out2.detail, "tier escalated after repair exhaustion (FR-021)");
    {
        let e = ledger.get(TASK_ID).unwrap();
        assert!(e.escalated);
        assert_eq!(e.tier_attempts, 0);
    }

    // Simulate re-dispatch at the higher tier: same ledger key, Frontier.
    let frontier_task = node(ModelTier::Frontier);

    // #3: fresh tier budget -> Repair at Frontier.
    let out3 = ev
        .evaluate(&frontier_task, &gate, Path::new("."), &mut ledger)
        .await;
    assert!(matches!(out3.directive, EvaluationDirective::Repair { .. }));
    assert_eq!(out3.detail, "repair attempt 1/1 at tier Frontier");

    // #4: gate passes -> Complete.
    let out4 = ev
        .evaluate(&frontier_task, &gate, Path::new("."), &mut ledger)
        .await;
    assert_eq!(out4.directive, EvaluationDirective::Complete);

    let causes = vec![
        tag(&out1.directive),
        tag(&out2.directive),
        tag(&out3.directive),
        tag(&out4.directive),
    ];
    assert_eq!(causes, vec!["Repair", "Escalate", "Repair", "Complete"]);
}

// --- Scenario 3: terminal_failure --------------------------------------

#[tokio::test]
async fn terminal_failure() {
    let ev = Evaluator::new(1);
    let task = node(ModelTier::Frontier);
    let gate = AlwaysFailGate;
    let mut ledger = RepairLedger::default();

    // #1: budget 1/1 available -> Repair.
    let first = ev.evaluate(&task, &gate, Path::new("."), &mut ledger).await;
    assert!(matches!(first.directive, EvaluationDirective::Repair { .. }));

    // #2: budget exhausted at the highest tier -> terminal Fail.
    let second = ev.evaluate(&task, &gate, Path::new("."), &mut ledger).await;
    match second.directive {
        EvaluationDirective::Fail { defect } => assert!(!defect.is_empty()),
        other => panic!("expected Fail, got {:?}", other),
    }
    assert!(second.detail.contains("exhausted"));
    assert!(second.detail.contains("FR-021"));

    let e = ledger.get(TASK_ID).unwrap();
    assert_eq!(e.attempts, 2);
    assert!(!e.escalated);
}

// --- Scenario 4: degraded_blocked_until_acknowledgment (FR-031) --------

#[tokio::test]
async fn degraded_blocked_until_acknowledgment() {
    let ev = Evaluator::new(1);
    let task = node(ModelTier::Economical);
    let gate = ToggleGate {
        degraded: Mutex::new(true),
    };
    let mut ledger = RepairLedger::default();

    // Degraded evaluation: no ledger entry may exist (degraded never
    // counts as a repair attempt).
    let out1 = ev.evaluate(&task, &gate, Path::new("."), &mut ledger).await;
    assert_eq!(out1.directive, EvaluationDirective::MarkDegraded);
    assert!(ledger.get(TASK_ID).is_none());

    // Still degraded: still no ledger entry.
    let out2 = ev.evaluate(&task, &gate, Path::new("."), &mut ledger).await;
    assert_eq!(out2.directive, EvaluationDirective::MarkDegraded);
    assert!(ledger.get(TASK_ID).is_none());

    // Explicit acknowledgment: the verification command is runnable now.
    *gate.degraded.lock().unwrap() = false;

    let out3 = ev.evaluate(&task, &gate, Path::new("."), &mut ledger).await;
    assert_eq!(out3.directive, EvaluationDirective::Complete);
    assert!(ledger.get(TASK_ID).is_none());

    let causes = vec![
        tag(&out1.directive),
        tag(&out2.directive),
        tag(&out3.directive),
    ];
    assert_eq!(causes, vec!["MarkDegraded", "MarkDegraded", "Complete"]);
}

//! Spec 023 — verification-gate contract types (T003, type layer only).
//!
//! This module defines the orchestration-side contract for verification
//! gates: the [`VerificationGate`] trait and its supporting view/outcome
//! types. The evaluation loop that drives gates (T021) is a later task —
//! nothing here schedules, executes, or repairs anything.
//!
//! These types are pure orchestration-side contracts and deliberately have
//! NO neurocode imports.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// A single failed command recorded in a [`DefectBundle`] (FR-020).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandFailure {
    /// The command line that was executed and failed.
    pub command: String,
    /// Non-zero exit code the command terminated with.
    pub exit: i64,
    /// Human-readable error lines captured from the command's output.
    pub errors: Vec<String>,
}

/// The set of defects observed when a verification gate fails (FR-020).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DefectBundle {
    /// Identifier of the task the failing verification was run against.
    pub task_id: String,
    /// Commands from the verification plan that failed.
    pub failed_commands: Vec<CommandFailure>,
    /// Policy violations detected during verification.
    pub policy_violations: Vec<String>,
    /// Findings surfaced by a reviewer pass (risk-triggered review).
    pub reviewer_findings: Vec<String>,
    /// Repository paths touched by the work under verification.
    pub changed_paths: Vec<String>,
}

impl DefectBundle {
    /// Returns `true` iff the bundle carries no information at all:
    /// `task_id` is empty and all four collections are empty.
    pub fn is_empty(&self) -> bool {
        self.task_id.is_empty()
            && self.failed_commands.is_empty()
            && self.policy_violations.is_empty()
            && self.reviewer_findings.is_empty()
            && self.changed_paths.is_empty()
    }
}

/// A read-only view of one step of a verification plan (FR-019).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationStepView {
    /// Short human-readable name of the step.
    pub name: String,
    /// Shell command the step runs.
    pub command: String,
    /// How the command's output should be parsed (e.g. `plain`, `junit`).
    pub parse: String,
    /// Per-step timeout in seconds.
    pub timeout_sec: u64,
    /// Whether the overall gate fails when this step fails. Non-required
    /// steps may degrade the outcome instead (FR-031).
    pub required: bool,
}

/// A read-only view of a verification plan handed to a verification gate
/// (FR-019).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VerificationPlanView {
    /// Ordered steps composing the plan.
    pub steps: Vec<VerificationStepView>,
    /// Whether a risk condition triggered an additional reviewer pass.
    pub risk_triggered_review: bool,
}

impl VerificationPlanView {
    /// Returns only the steps marked as `required`.
    pub fn required_steps(&self) -> Vec<&VerificationStepView> {
        self.steps.iter().filter(|s| s.required).collect()
    }
}

/// The outcome of running a [`VerificationGate`] over a
/// [`VerificationPlanView`] (FR-019 gate semantics, FR-031 Degraded).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GateOutcome {
    /// Every required verification step passed.
    Passed,
    /// At least one required verification step failed; the bundle carries
    /// the observed defects (FR-020).
    Failed(DefectBundle),
    /// A verification command was unavailable (e.g. missing binary), so
    /// the gate neither passed nor found a code defect (FR-031).
    Degraded,
}

/// A verification gate: executes a verification plan against a working
/// directory and reports a [`GateOutcome`] (FR-019 gate semantics).
///
/// The evaluation loop that invokes gates is implemented in a later task;
/// this trait is only the contract.
#[async_trait::async_trait]
pub trait VerificationGate: Send + Sync {
    /// Run the verification `plan` inside `workdir` and report the outcome.
    async fn run(&self, plan: &VerificationPlanView, workdir: &Path) -> GateOutcome;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_failure_round_trips_with_snake_case_fields() {
        let cf = CommandFailure {
            command: "cargo test -p x".to_string(),
            exit: 101,
            errors: vec!["test foo failed".to_string()],
        };
        let json = serde_json::to_value(&cf).unwrap();
        assert!(json.is_object());
        let obj = json.as_object().unwrap();
        // Exact snake_case field names.
        assert_eq!(
            obj.keys().cloned().collect::<Vec<_>>(),
            vec!["command", "exit", "errors"]
        );
        let back: CommandFailure = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back, cf);
        // Spot-check values survive the round trip.
        assert_eq!(json["exit"], serde_json::json!(101));
    }

    #[test]
    fn defect_bundle_round_trips_with_snake_case_fields() {
        let bundle = DefectBundle {
            task_id: "T003".to_string(),
            failed_commands: vec![CommandFailure {
                command: "cargo test".to_string(),
                exit: 1,
                errors: vec!["e".to_string()],
            }],
            policy_violations: vec!["no-tests".to_string()],
            reviewer_findings: vec!["finding".to_string()],
            changed_paths: vec!["src/lib.rs".to_string()],
        };
        let json = serde_json::to_value(&bundle).unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(
            obj.keys().cloned().collect::<Vec<_>>(),
            vec![
                "task_id",
                "failed_commands",
                "policy_violations",
                "reviewer_findings",
                "changed_paths"
            ]
        );
        let back: DefectBundle = serde_json::from_value(json).unwrap();
        assert_eq!(back, bundle);
    }

    #[test]
    fn verification_step_view_round_trips_with_snake_case_fields() {
        let step = VerificationStepView {
            name: "unit-tests".to_string(),
            command: "cargo test".to_string(),
            parse: "plain".to_string(),
            timeout_sec: 300,
            required: true,
        };
        let json = serde_json::to_value(&step).unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(
            obj.keys().cloned().collect::<Vec<_>>(),
            vec!["name", "command", "parse", "timeout_sec", "required"]
        );
        let back: VerificationStepView = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back, step);
        assert_eq!(json["timeout_sec"], serde_json::json!(300));
    }

    #[test]
    fn verification_plan_view_round_trips_with_snake_case_fields() {
        let plan = VerificationPlanView {
            steps: vec![VerificationStepView {
                name: "build".to_string(),
                command: "cargo build".to_string(),
                parse: "plain".to_string(),
                timeout_sec: 600,
                required: false,
            }],
            risk_triggered_review: true,
        };
        let json = serde_json::to_value(&plan).unwrap();
        let obj = json.as_object().unwrap();
        assert_eq!(
            obj.keys().cloned().collect::<Vec<_>>(),
            vec!["steps", "risk_triggered_review"]
        );
        let back: VerificationPlanView = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back, plan);
        assert_eq!(json["risk_triggered_review"], serde_json::json!(true));
    }

    #[test]
    fn gate_outcome_round_trips() {
        for (outcome, expected) in [
            (GateOutcome::Passed, serde_json::json!("Passed")),
            (GateOutcome::Degraded, serde_json::json!("Degraded")),
        ] {
            let json = serde_json::to_value(&outcome).unwrap();
            assert_eq!(json, expected);
            let back: GateOutcome = serde_json::from_value(json).unwrap();
            assert_eq!(back, outcome);
        }
        let failed = GateOutcome::Failed(DefectBundle {
            task_id: "T9".to_string(),
            ..Default::default()
        });
        let json = serde_json::to_value(&failed).unwrap();
        assert_eq!(json, serde_json::json!({"Failed": {"task_id": "T9", "failed_commands": [], "policy_violations": [], "reviewer_findings": [], "changed_paths": []}}));
        let back: GateOutcome = serde_json::from_value(json).unwrap();
        assert_eq!(back, failed);
    }

    #[test]
    fn defect_bundle_is_empty_semantics() {
        assert!(DefectBundle::default().is_empty());
        let with_failure = DefectBundle {
            failed_commands: vec![CommandFailure {
                command: "cargo test".to_string(),
                exit: 1,
                errors: vec![],
            }],
            ..Default::default()
        };
        assert!(!with_failure.is_empty());
        // task_id alone makes it non-empty.
        let with_id = DefectBundle {
            task_id: "T1".to_string(),
            ..Default::default()
        };
        assert!(!with_id.is_empty());
    }

    #[test]
    fn required_steps_filters_on_required_flag() {
        let plan = VerificationPlanView {
            steps: vec![
                VerificationStepView {
                    name: "required-1".to_string(),
                    command: "true".to_string(),
                    parse: "plain".to_string(),
                    timeout_sec: 10,
                    required: true,
                },
                VerificationStepView {
                    name: "optional".to_string(),
                    command: "true".to_string(),
                    parse: "plain".to_string(),
                    timeout_sec: 10,
                    required: false,
                },
                VerificationStepView {
                    name: "required-2".to_string(),
                    command: "true".to_string(),
                    parse: "plain".to_string(),
                    timeout_sec: 10,
                    required: true,
                },
            ],
            risk_triggered_review: false,
        };
        let required = plan.required_steps();
        assert_eq!(required.len(), 2);
        assert_eq!(required[0].name, "required-1");
        assert_eq!(required[1].name, "required-2");
        assert!(VerificationPlanView::default().required_steps().is_empty());
    }

    #[derive(Default)]
    struct NullGate;

    #[async_trait::async_trait]
    impl VerificationGate for NullGate {
        async fn run(&self, _plan: &VerificationPlanView, _workdir: &Path) -> GateOutcome {
            GateOutcome::Passed
        }
    }

    #[tokio::test]
    async fn verification_gate_is_usable_as_trait_object() {
        let plan = VerificationPlanView::default();
        let g: Box<dyn VerificationGate> = Box::new(NullGate);
        let outcome = g.run(&plan, Path::new(".")).await;
        assert_eq!(outcome, GateOutcome::Passed);
    }
}

// ---------------------------------------------------------------------------
// T021 — evaluation loop + repair ledger (US5)
// ---------------------------------------------------------------------------

use std::collections::HashMap;

use crate::task_graph::{ModelTier, TaskNode};

/// Per-task repair bookkeeping kept in the [`RepairLedger`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LedgerEntry {
    /// Total failed-gate evaluations observed for the task (any tier).
    pub attempts: u32,
    /// Failed-gate evaluations at the *current* tier; a tier bump resets it.
    pub tier_attempts: u32,
    /// Whether this task has ever been escalated up the tier ladder.
    pub escalated: bool,
}

/// Tracks repair attempts per task id.
#[derive(Debug, Clone, Default)]
pub struct RepairLedger {
    entries: HashMap<String, LedgerEntry>,
}

impl RepairLedger {
    /// Mutable access to `task_id`'s entry, inserting a default on miss.
    pub fn entry_mut(&mut self, task_id: &str) -> &mut LedgerEntry {
        self.entries.entry(task_id.to_string()).or_default()
    }

    /// Read access to `task_id`'s entry, or `None` if it was never touched.
    pub fn get(&self, task_id: &str) -> Option<&LedgerEntry> {
        self.entries.get(task_id)
    }
}

/// What the evaluation loop tells the scheduler to do next.
#[derive(Debug, Clone, PartialEq)]
pub enum EvaluationDirective {
    /// Gate passed — the task may be marked Completed.
    Complete,
    /// Gate failed and per-tier repair budget remains — redispatch the
    /// defect to a repair worker (FR-020).
    Repair { defect: DefectBundle },
    /// Per-tier repair budget exhausted below the top of the ladder —
    /// redispatch at a higher tier (FR-021).
    Escalate { defect: DefectBundle },
    /// Repair exhausted at the highest tier — the task is terminally Failed
    /// (FR-021).
    Fail { defect: DefectBundle },
    /// Verification command unavailable (FR-031) — mark the task Degraded.
    MarkDegraded,
}

/// The chosen directive plus a human-readable explanation.
#[derive(Debug, Clone, PartialEq)]
pub struct EvaluationOutcome {
    /// Next action for the scheduler.
    pub directive: EvaluationDirective,
    /// Why the directive was chosen.
    pub detail: String,
}

/// Runs the evaluate→decide loop: invokes a verification gate over a task's
/// plan and converts the [`GateOutcome`] into an [`EvaluationDirective`],
/// consulting the [`RepairLedger`] for the FR-021 exhaustion ladder.
pub struct Evaluator {
    max_repair_attempts: u32,
}

impl Evaluator {
    /// FR-021 exhaustion ladder (per Q5): attempts are counted PER TIER —
    /// a tier bump resets the tier counter — and terminal failure occurs
    /// only when the highest tier (Frontier, rank 1) is exhausted.
    pub fn new(max_repair_attempts: u32) -> Self {
        Self {
            max_repair_attempts,
        }
    }

    /// Evaluates `task` against `gate` in `workdir` and decides the next
    /// directive.
    ///
    /// - FR-019: the awaited gate decides completion.
    /// - FR-020: a failing gate's `DefectBundle` feeds repair dispatch.
    /// - FR-021: per-tier repair budget with escalation up the ladder.
    /// - FR-031: `Degraded` never routes to code-defect repair.
    pub async fn evaluate(
        &self,
        task: &TaskNode,
        gate: &dyn VerificationGate,
        workdir: &Path,
        ledger: &mut RepairLedger,
    ) -> EvaluationOutcome {
        let outcome = gate.run(&task.verification, workdir).await;
        match outcome {
            GateOutcome::Passed => EvaluationOutcome {
                directive: EvaluationDirective::Complete,
                detail: "gate passed".to_string(),
            },
            GateOutcome::Degraded => EvaluationOutcome {
                directive: EvaluationDirective::MarkDegraded,
                detail: "verification command unavailable (FR-031: neither passed nor code defect; never routes to code-defect repair)".to_string(),
            },
            GateOutcome::Failed(defect) => {
                let e = ledger.entry_mut(task.id.as_str());
                e.attempts += 1;
                if e.tier_attempts < self.max_repair_attempts {
                    e.tier_attempts += 1;
                    EvaluationOutcome {
                        directive: EvaluationDirective::Repair { defect },
                        detail: format!(
                            "repair attempt {}/{} at tier {:?}",
                            e.tier_attempts, self.max_repair_attempts, task.model_tier
                        ),
                    }
                } else if task.model_tier.rank() < ModelTier::Frontier.rank() {
                    e.tier_attempts = 0;
                    e.escalated = true;
                    EvaluationOutcome {
                        directive: EvaluationDirective::Escalate { defect },
                        detail: "tier escalated after repair exhaustion (FR-021)".to_string(),
                    }
                } else {
                    EvaluationOutcome {
                        directive: EvaluationDirective::Fail { defect },
                        detail: "repair exhausted at highest tier (FR-021)".to_string(),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod t021_tests {
    use super::*;
    use crate::task_graph::{
        AcceptanceCriterion, IsolationMode, ModelTier, RiskLevel, TaskId, TaskNode, TaskStatus,
        WorkerRole,
    };

    struct PassGate;

    #[async_trait::async_trait]
    impl VerificationGate for PassGate {
        async fn run(&self, _plan: &VerificationPlanView, _workdir: &Path) -> GateOutcome {
            GateOutcome::Passed
        }
    }

    struct FailGate(String);

    #[async_trait::async_trait]
    impl VerificationGate for FailGate {
        async fn run(&self, _plan: &VerificationPlanView, _workdir: &Path) -> GateOutcome {
            GateOutcome::Failed(DefectBundle {
                task_id: "t-eval".to_string(),
                failed_commands: vec![CommandFailure {
                    command: self.0.clone(),
                    exit: 1,
                    errors: vec![],
                }],
                ..Default::default()
            })
        }
    }

    struct DegradeGate;

    #[async_trait::async_trait]
    impl VerificationGate for DegradeGate {
        async fn run(&self, _plan: &VerificationPlanView, _workdir: &Path) -> GateOutcome {
            GateOutcome::Degraded
        }
    }

    struct FlakyGate {
        fails: std::sync::Mutex<usize>,
    }

    #[async_trait::async_trait]
    impl VerificationGate for FlakyGate {
        async fn run(&self, _plan: &VerificationPlanView, _workdir: &Path) -> GateOutcome {
            let mut fails = self.fails.lock().unwrap();
            if *fails > 0 {
                *fails -= 1;
                GateOutcome::Failed(DefectBundle::default())
            } else {
                GateOutcome::Passed
            }
        }
    }

    fn node(tier: ModelTier) -> TaskNode {
        TaskNode {
            id: TaskId::new("t-eval").unwrap(),
            objective: "implement the thing".to_string(),
            dependencies: vec![],
            read_set: vec![],
            write_set: vec![],
            artifact_ids: vec![],
            role: WorkerRole::Implementor,
            model_tier: tier,
            risk: RiskLevel::Low,
            acceptance: vec![AcceptanceCriterion {
                criterion: "tests pass".to_string(),
                kind: "command".to_string(),
            }],
            verification: VerificationPlanView::default(),
            isolation: IsolationMode::SharedCheckout,
            status: TaskStatus::Evaluating,
            attempts: 0,
        }
    }

    #[tokio::test]
    async fn pass_completes() {
        let ev = Evaluator::new(2);
        let task = node(ModelTier::Economical);
        let mut ledger = RepairLedger::default();
        let out = ev.evaluate(&task, &PassGate, Path::new("."), &mut ledger).await;
        assert_eq!(out.directive, EvaluationDirective::Complete);
        assert_eq!(out.detail, "gate passed");
        assert!(ledger.get("t-eval").is_none());
    }

    #[tokio::test]
    async fn fail_then_repair() {
        let ev = Evaluator::new(2);
        let task = node(ModelTier::Economical);
        let mut ledger = RepairLedger::default();
        let out = ev
            .evaluate(&task, &FailGate("cargo test".to_string()), Path::new("."), &mut ledger)
            .await;
        assert!(matches!(out.directive, EvaluationDirective::Repair { .. }));
        assert_eq!(out.detail, "repair attempt 1/2 at tier Economical");
        let e = ledger.get("t-eval").unwrap();
        assert_eq!(e.attempts, 1);
        assert_eq!(e.tier_attempts, 1);
        assert!(!e.escalated);
    }

    #[tokio::test]
    async fn repair_exhausts_then_escalates() {
        let ev = Evaluator::new(1);
        let task = node(ModelTier::Economical);
        let mut ledger = RepairLedger::default();
        let gate = FailGate("cargo build".to_string());
        let first = ev.evaluate(&task, &gate, Path::new("."), &mut ledger).await;
        assert!(matches!(first.directive, EvaluationDirective::Repair { .. }));
        let second = ev.evaluate(&task, &gate, Path::new("."), &mut ledger).await;
        assert!(matches!(second.directive, EvaluationDirective::Escalate { .. }));
        assert_eq!(second.detail, "tier escalated after repair exhaustion (FR-021)");
        let e = ledger.get("t-eval").unwrap();
        assert!(e.escalated);
        assert_eq!(e.tier_attempts, 0);
        assert_eq!(e.attempts, 2);
    }

    #[tokio::test]
    async fn frontier_exhaustion_fails() {
        let ev = Evaluator::new(1);
        let task = node(ModelTier::Frontier);
        let mut ledger = RepairLedger::default();
        let gate = FailGate("cargo test".to_string());
        let first = ev.evaluate(&task, &gate, Path::new("."), &mut ledger).await;
        assert!(matches!(first.directive, EvaluationDirective::Repair { .. }));
        let second = ev.evaluate(&task, &gate, Path::new("."), &mut ledger).await;
        assert!(matches!(second.directive, EvaluationDirective::Fail { .. }));
        assert_eq!(second.detail, "repair exhausted at highest tier (FR-021)");
        let e = ledger.get("t-eval").unwrap();
        assert_eq!(e.attempts, 2);
        assert!(!e.escalated);
    }

    #[tokio::test]
    async fn degraded_never_repairs() {
        let ev = Evaluator::new(3);
        let task = node(ModelTier::Economical);
        let mut ledger = RepairLedger::default();
        let out = ev.evaluate(&task, &DegradeGate, Path::new("."), &mut ledger).await;
        assert_eq!(out.directive, EvaluationDirective::MarkDegraded);
        assert_eq!(
            out.detail,
            "verification command unavailable (FR-031: neither passed nor code defect; never routes to code-defect repair)"
        );
        let e_attempts = ledger.get("t-eval").map(|e| e.attempts).unwrap_or(0);
        assert_eq!(e_attempts, 0);
        let e_tier = ledger.get("t-eval").map(|e| e.tier_attempts).unwrap_or(0);
        assert_eq!(e_tier, 0);
    }

    #[tokio::test]
    async fn flaky_gate_recovers() {
        let ev = Evaluator::new(2);
        let task = node(ModelTier::Economical);
        let mut ledger = RepairLedger::default();
        let gate = FlakyGate {
            fails: std::sync::Mutex::new(1),
        };
        let first = ev.evaluate(&task, &gate, Path::new("."), &mut ledger).await;
        assert!(matches!(first.directive, EvaluationDirective::Repair { .. }));
        let second = ev.evaluate(&task, &gate, Path::new("."), &mut ledger).await;
        assert_eq!(second.directive, EvaluationDirective::Complete);
        assert_eq!(second.detail, "gate passed");
        // One failure was recorded before recovery (attempts increments only
        // on Failed per the implementation contract).
        let e = ledger.get("t-eval").unwrap();
        assert_eq!(e.attempts, 1);
        assert_eq!(e.tier_attempts, 1);
    }

    #[tokio::test]
    async fn defect_carries_command() {
        let ev = Evaluator::new(2);
        let task = node(ModelTier::Economical);
        let mut ledger = RepairLedger::default();
        let out = ev
            .evaluate(&task, &FailGate("cargo test -p x".to_string()), Path::new("."), &mut ledger)
            .await;
        match out.directive {
            EvaluationDirective::Repair { defect } => {
                assert_eq!(defect.failed_commands.len(), 1);
                assert_eq!(defect.failed_commands[0].command, "cargo test -p x");
                assert_eq!(defect.failed_commands[0].exit, 1);
            }
            other => panic!("expected Repair, got {:?}", other),
        }
    }

    #[test]
    fn ledger_entry_mut_inserts_default_and_get_misses() {
        let mut ledger = RepairLedger::default();
        assert!(ledger.get("never-touched").is_none());
        {
            let e = ledger.entry_mut("never-touched");
            assert_eq!(*e, LedgerEntry::default());
            e.attempts = 5;
            e.tier_attempts = 2;
            e.escalated = true;
        }
        let e = ledger.get("never-touched").unwrap();
        assert_eq!(e.attempts, 5);
        assert_eq!(e.tier_attempts, 2);
        assert!(e.escalated);
    }
}

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

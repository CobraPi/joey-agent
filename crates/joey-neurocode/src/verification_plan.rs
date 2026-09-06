//! Scoped verification plan derivation (spec 023).
//!
//! FR-001 (scoped derivation): a `VerificationPlan` can be narrowed to the
//! modules actually impacted by a change, keeping only the verification
//! steps that target those modules.
//!
//! FR-009 (high-risk satisfaction): a high-risk plan is considered
//! satisfied when it either contains at least one required step or has
//! triggered risk review.

use serde::{Deserialize, Serialize};

/// A single verification step (planner JSON contract:
/// contracts/planner-json-format.md — field names are load-bearing).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerificationStep {
    pub name: String,
    pub command: String,
    pub parse: String,
    pub timeout_sec: u64,
    pub required: bool,
}

/// A plan of verification steps for a change (planner JSON contract:
/// `risk_triggered_review` field name is load-bearing).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct VerificationPlan {
    pub steps: Vec<VerificationStep>,
    pub risk_triggered_review: bool,
    /// Acceptance criteria (T029, feature 026 US7): the spec's
    /// Given/When/Then list carried alongside the steps so downstream
    /// consumers can check the change against what the spec required.
    /// Serialized with a default so planner JSON predating the field
    /// deserializes cleanly.
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
}

impl VerificationPlan {
    /// True if any step in this plan is required (FR-009).
    pub fn has_required_step(&self) -> bool {
        self.steps.iter().any(|step| step.required)
    }

    /// Derive a module-scoped plan (FR-001): keep only the steps whose
    /// `command` contains any of `impacted_modules` as a substring, mark
    /// every kept step `required = true`, and drop the rest.
    /// `risk_triggered_review` and `acceptance_criteria` are carried over
    /// unchanged. An empty `impacted_modules` yields a plan with no steps.
    pub fn scoped(&self, impacted_modules: &[String]) -> VerificationPlan {
        VerificationPlan {
            steps: self
                .steps
                .iter()
                .filter(|step| {
                    impacted_modules
                        .iter()
                        .any(|module| step.command.contains(module.as_str()))
                })
                .map(|step| VerificationStep {
                    required: true,
                    ..step.clone()
                })
                .collect(),
            risk_triggered_review: self.risk_triggered_review,
            acceptance_criteria: self.acceptance_criteria.clone(),
        }
    }

    /// True iff this plan satisfies the high-risk bar (FR-009): it has at
    /// least one required step, or risk-triggered review was requested.
    pub fn is_satisfied_for_high_risk(&self) -> bool {
        self.has_required_step() || self.risk_triggered_review
    }

    /// Builder (T029): attach acceptance criteria. Consuming builder —
    /// `plan.with_acceptance_criteria(vec![...])`. APPENDS to any
    /// existing criteria, deduplicating while preserving first-seen
    /// order (additive only — never replaces).
    pub fn with_acceptance_criteria(mut self, criteria: Vec<String>) -> Self {
        for criterion in criteria {
            if !self.acceptance_criteria.contains(&criterion) {
                self.acceptance_criteria.push(criterion);
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(name: &str, command: &str, required: bool) -> VerificationStep {
        VerificationStep {
            name: name.to_string(),
            command: command.to_string(),
            parse: "exit_code".to_string(),
            timeout_sec: 120,
            required,
        }
    }

    #[test]
    fn serde_round_trip_asserts_exact_json_keys() {
        let plan = VerificationPlan {
            steps: vec![VerificationStep {
                name: "core tests".to_string(),
                command: "cargo test -p joey-core".to_string(),
                parse: "exit_code".to_string(),
                timeout_sec: 300,
                required: true,
            }],
            risk_triggered_review: false,
            acceptance_criteria: vec![],
        };

        let json = serde_json::to_value(&plan).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "steps": [
                    {
                        "name": "core tests",
                        "command": "cargo test -p joey-core",
                        "parse": "exit_code",
                        "timeout_sec": 300,
                        "required": true
                    }
                ],
                "risk_triggered_review": false,
                "acceptance_criteria": []
            })
        );

        let round_trip: VerificationPlan = serde_json::from_value(json).unwrap();
        assert_eq!(round_trip, plan);
    }

    #[test]
    fn serde_deserializes_json_without_acceptance_criteria_key() {
        // Planner JSON predating T029 has no `acceptance_criteria` key —
        // `#[serde(default)]` must absorb that.
        let json = serde_json::json!({
            "steps": [],
            "risk_triggered_review": true
        });
        let plan: VerificationPlan = serde_json::from_value(json).unwrap();
        assert!(plan.risk_triggered_review);
        assert!(plan.acceptance_criteria.is_empty());
    }

    #[test]
    fn scoped_keeps_matching_steps_marked_required() {
        let plan = VerificationPlan {
            steps: vec![
                step("a", "cargo test -p joey-core", false),
                step("b", "cargo test -p joey-tools", false),
                step("c", "echo hi", false),
            ],
            risk_triggered_review: false,
            acceptance_criteria: vec![],
        };

        let scoped = plan.scoped(&["joey-core".to_string()]);

        assert_eq!(scoped.steps.len(), 1);
        assert_eq!(scoped.steps[0].command, "cargo test -p joey-core");
        assert!(scoped.steps[0].required);
    }

    #[test]
    fn scoped_with_empty_modules_yields_no_steps() {
        let plan = VerificationPlan {
            steps: vec![step("a", "cargo test -p joey-core", false)],
            risk_triggered_review: true,
            acceptance_criteria: vec![],
        };

        let scoped = plan.scoped(&[]);

        assert!(scoped.steps.is_empty());
    }

    #[test]
    fn scoped_preserves_risk_triggered_review() {
        let plan = VerificationPlan {
            steps: vec![step("a", "cargo test -p joey-core", false)],
            risk_triggered_review: true,
            acceptance_criteria: vec![],
        };

        let scoped = plan.scoped(&["joey-core".to_string()]);
        assert!(scoped.risk_triggered_review);

        let scoped_none = plan.scoped(&[]);
        assert!(scoped_none.risk_triggered_review);
    }

    #[test]
    fn scoped_preserves_acceptance_criteria() {
        let plan = VerificationPlan {
            steps: vec![step("a", "cargo test -p joey-core", false)],
            risk_triggered_review: false,
            acceptance_criteria: vec!["given a scoped change".to_string()],
        }
        .with_acceptance_criteria(vec!["given a scoped change".to_string()]);

        let scoped = plan.scoped(&["joey-core".to_string()]);
        assert_eq!(
            scoped.acceptance_criteria,
            vec!["given a scoped change".to_string()]
        );

        let scoped_none = plan.scoped(&[]);
        assert_eq!(
            scoped_none.acceptance_criteria,
            vec!["given a scoped change".to_string()]
        );
    }

    #[test]
    fn with_acceptance_criteria_sets_field() {
        let plan = VerificationPlan::default()
            .with_acceptance_criteria(vec!["c1".to_string(), "c2".to_string()]);
        assert_eq!(plan.acceptance_criteria, vec!["c1".to_string(), "c2".to_string()]);
    }

    #[test]
    fn with_acceptance_criteria_appends_and_dedups_preserving_order() {
        // Appends to existing criteria instead of replacing them, dedups
        // preserving first-seen order.
        let plan = VerificationPlan::default()
            .with_acceptance_criteria(vec!["a".to_string(), "b".to_string()])
            .with_acceptance_criteria(vec!["b".to_string(), "c".to_string(), "a".to_string()]);
        assert_eq!(plan.acceptance_criteria, vec!["a", "b", "c"]);
    }

    #[test]
    fn has_required_step_true_and_false() {
        let with_required = VerificationPlan {
            steps: vec![step("a", "cargo test -p joey-core", true)],
            risk_triggered_review: false,
            acceptance_criteria: vec![],
        };
        assert!(with_required.has_required_step());

        let without_required = VerificationPlan {
            steps: vec![step("a", "cargo test -p joey-core", false)],
            risk_triggered_review: false,
            acceptance_criteria: vec![],
        };
        assert!(!without_required.has_required_step());
    }

    #[test]
    fn is_satisfied_for_high_risk_cases() {
        // Empty plan ⇒ false.
        let empty = VerificationPlan::default();
        assert!(!empty.is_satisfied_for_high_risk());

        // risk_triggered_review = true only ⇒ true.
        let review_only = VerificationPlan {
            steps: vec![],
            risk_triggered_review: true,
            acceptance_criteria: vec![],
        };
        assert!(review_only.is_satisfied_for_high_risk());

        // One required step ⇒ true.
        let required_step = VerificationPlan {
            steps: vec![step("a", "cargo test -p joey-core", true)],
            risk_triggered_review: false,
            acceptance_criteria: vec![],
        };
        assert!(required_step.is_satisfied_for_high_risk());

        // Only non-required steps ⇒ false.
        let non_required = VerificationPlan {
            steps: vec![step("a", "cargo test -p joey-core", false)],
            risk_triggered_review: false,
            acceptance_criteria: vec![],
        };
        assert!(!non_required.is_satisfied_for_high_risk());
    }
}

//! VerifyLoop → VerificationGate adapter (spec 023 T023, FR-019/FR-020).
//! Maps orchestration VerificationPlanView steps onto neurocode's VerifyConfig,
//! awaits the outcome, and routes failures into a DefectBundle queue that the
//! HypercodeDispatcher consumes to drive repair re-dispatch (the scheduler's
//! Repair directive owns re-execution; detached verify stays informational).
pub struct VerifyLoopGate {
    project_root: std::path::PathBuf,
    pending_repairs: std::sync::Arc<std::sync::Mutex<Vec<joey_orchestration::evaluator::DefectBundle>>>,
}

use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use joey_neurocode::config::{VerifyConfig, VerifyStepConfig};
use joey_neurocode::graph::DependencyGraph;
use joey_neurocode::verify::VerifyLoop;
use joey_orchestration::evaluator::{
    CommandFailure, DefectBundle, GateOutcome, VerificationGate, VerificationPlanView,
};

impl VerifyLoopGate {
    pub fn new(project_root: impl Into<std::path::PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            pending_repairs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The shared repair queue (clone of the Arc — the dispatcher pops from
    /// the same Vec the gate pushes into).
    pub fn repair_queue(&self) -> Arc<Mutex<Vec<DefectBundle>>> {
        Arc::clone(&self.pending_repairs)
    }
}

/// FR-019 (awaited gate decides completion), FR-020 (DefectBundle feeds
/// repair), FR-031 (Degraded = command unavailable, neither passed nor a
/// code defect — never routes to repair).
#[async_trait::async_trait]
impl VerificationGate for VerifyLoopGate {
    async fn run(&self, plan: &VerificationPlanView, workdir: &Path) -> GateOutcome {
        // Empty plan ⇒ nothing to verify (invariant 6: high-risk tasks
        // carry steps).
        if plan.steps.is_empty() {
            return GateOutcome::Passed;
        }

        // step_name → command map (VerifyResult carries step_name, not the
        // command — build the map up front and fall back to the step name).
        let command_by_name: std::collections::HashMap<&str, &str> = plan
            .steps
            .iter()
            .map(|s| (s.name.as_str(), s.command.as_str()))
            .collect();

        let config = VerifyConfig {
            steps: plan
                .steps
                .iter()
                .map(|s| VerifyStepConfig {
                    name: s.name.clone(),
                    command: s.command.clone(),
                    parse: s.parse.clone(),
                    timeout_sec: s.timeout_sec,
                })
                .collect(),
            max_fix_iterations: 1,
        };

        // The verify orchestrator requires a graph handle; in-memory is a
        // no-op substrate (the fix callback returns false so no reindex
        // ever runs against it).
        let graph = Arc::new(
            DependencyGraph::open_in_memory().expect("in-memory graph for verification"),
        );
        let orchestrator = VerifyLoop::new(config, graph);
        let outcome = orchestrator.run_with_fixes(workdir, |results| {
            for r in results.iter().filter(|r| !r.passed && !r.skipped) {
                eprintln!(
                    "hypercode-gate: step '{}' failed — queued for repair dispatch",
                    r.step_name
                );
            }
            false // repair is dispatched by the scheduler after the gate
                  // returns, never inline
        });

        let failed: Vec<_> = outcome
            .results
            .iter()
            .filter(|r| !r.passed && !r.skipped)
            .collect();

        if !failed.is_empty() {
            // task_id is unknown at gate level — the dispatcher stamps the
            // task id when it pops the queue.
            let bundle = DefectBundle {
                task_id: String::new(),
                failed_commands: failed
                    .iter()
                    .map(|r| CommandFailure {
                        command: command_by_name
                            .get(r.step_name.as_str())
                            .copied()
                            .unwrap_or(r.step_name.as_str())
                            .to_string(),
                        exit: 1,
                        // StructuredError implements no Display — use the
                        // Debug rendering (signature/file/line/message).
                        errors: r
                            .errors
                            .iter()
                            .map(|e| format!("{e:?}"))
                            .collect(),
                    })
                    .collect(),
                policy_violations: vec![],
                reviewer_findings: vec![],
                changed_paths: vec![],
            };
            self.pending_repairs
                .lock()
                .expect("repair queue lock")
                .push(bundle.clone());
            return GateOutcome::Failed(bundle);
        }

        // FR-031: a REQUIRED step that was skipped (command unavailable —
        // e.g. missing binary) degrades the gate; it neither passed nor
        // produced a code defect, so it never routes to repair.
        let required_skipped = plan.steps.iter().any(|s| {
            s.required && outcome.results.iter().any(|r| r.skipped && r.step_name == s.name)
        });
        if required_skipped {
            return GateOutcome::Degraded;
        }

        // Skipped OPTIONAL steps don't gate.
        GateOutcome::Passed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use joey_orchestration::evaluator::VerificationStepView;

    fn step(name: &str, command: &str, required: bool) -> VerificationStepView {
        VerificationStepView {
            name: name.to_string(),
            command: command.to_string(),
            parse: "plain".to_string(),
            timeout_sec: 10,
            required,
        }
    }

    fn plan(steps: Vec<VerificationStepView>) -> VerificationPlanView {
        VerificationPlanView {
            steps,
            risk_triggered_review: false,
        }
    }

    #[tokio::test]
    async fn passing_plan_yields_passed() {
        let dir = tempfile::tempdir().unwrap();
        let gate = VerifyLoopGate::new(dir.path().to_path_buf());
        let outcome = gate
            .run(&plan(vec![step("ok", "true", true)]), dir.path())
            .await;
        assert_eq!(outcome, GateOutcome::Passed);
        assert!(gate.repair_queue().lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn failing_plan_yields_failed_and_queues_defect() {
        let dir = tempfile::tempdir().unwrap();
        let gate = VerifyLoopGate::new(dir.path().to_path_buf());
        let outcome = gate
            .run(&plan(vec![step("bad", "false", true)]), dir.path())
            .await;
        match outcome {
            GateOutcome::Failed(bundle) => {
                assert!(!bundle.failed_commands.is_empty());
                assert_eq!(bundle.failed_commands[0].command, "false");
                assert_eq!(bundle.failed_commands[0].exit, 1);
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(gate.repair_queue().lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn unavailable_required_step_yields_degraded() {
        let dir = tempfile::tempdir().unwrap();
        let gate = VerifyLoopGate::new(dir.path().to_path_buf());
        let outcome = gate
            .run(
                &plan(vec![step("gone", "nonexistent-verify-cmd-xyz", true)]),
                dir.path(),
            )
            .await;
        assert_eq!(outcome, GateOutcome::Degraded);
        // Degraded never routes to repair (FR-031).
        assert!(gate.repair_queue().lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unavailable_optional_step_still_passes() {
        let dir = tempfile::tempdir().unwrap();
        let gate = VerifyLoopGate::new(dir.path().to_path_buf());
        let outcome = gate
            .run(
                &plan(vec![step("opt", "nonexistent-verify-cmd-xyz", false)]),
                dir.path(),
            )
            .await;
        assert_eq!(outcome, GateOutcome::Passed);
    }

    #[tokio::test]
    async fn empty_plan_passes() {
        let dir = tempfile::tempdir().unwrap();
        let gate = VerifyLoopGate::new(dir.path().to_path_buf());
        let outcome = gate.run(&plan(vec![]), dir.path()).await;
        assert_eq!(outcome, GateOutcome::Passed);
    }
}

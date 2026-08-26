//! Build/verify feedback loop (FR-010/011/012, T037-T042).

pub mod parse;
pub mod runner;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::VerifyConfig;
use crate::graph::DependencyGraph;

/// The verification result for a single step.
#[derive(Debug, Clone)]
pub struct VerifyResult {
    pub step_name: String,
    pub passed: bool,
    /// True when the step was skipped (tool absent / timed out — FR-012
    /// graceful degradation) rather than executed and failed. A skipped
    /// step is not counted as a failure and never triggers a fix
    /// iteration or escalation.
    pub skipped: bool,
    pub output: String,
    pub errors: Vec<crate::verify::parse::StructuredError>,
    pub duration_ms: u64,
}

/// The outcome of a full verify (with fix iterations) run (T061, T070).
#[derive(Debug, Clone)]
pub struct VerifyOutcome {
    /// Results for all steps, in configured order (failed steps re-run in
    /// place after each fix iteration).
    pub results: Vec<VerifyResult>,
    /// Whether every step ultimately passed.
    pub all_passed: bool,
    /// How many fix iterations were consumed.
    pub fix_iterations_used: u32,
    /// The configured fix-iteration ceiling (needed by [`VerifyOutcome::should_escalate`]).
    pub max_fix_iterations: u32,
    /// Whether the run exhausted its fix budget while still failing
    /// (i.e. `should_escalate()` was true at the end of the run).
    pub escalated: bool,
    /// The model tier this verification served, if known
    /// ("economical" / "frontier") — set by the agent layer (T070).
    pub served_tier: Option<String>,
}

impl VerifyOutcome {
    /// Economical→Frontier escalation predicate (T070): true when the run
    /// still fails AND the fix-iteration budget is exhausted.
    pub fn should_escalate(&self) -> bool {
        !self.all_passed && self.fix_iterations_used >= self.max_fix_iterations
    }

    /// Escalation hint (T070): `Some("frontier")` when the failing run was
    /// served by the economical tier and its fix budget is exhausted.
    ///
    /// The agent layer reads this hint to re-dispatch the request on the
    /// frontier tier — the spec's "router/developer disagree" edge case
    /// (economical tier was judged sufficient, but verification proved
    /// otherwise).
    pub fn escalate_hint(&self) -> Option<String> {
        if self.should_escalate() && self.served_tier.as_deref() == Some("economical") {
            Some("frontier".to_string())
        } else {
            None
        }
    }
}

/// The orchestrator for the detached build/verify feedback loop.
///
/// Runs verification steps in order, feeds failures back for a correction pass
/// (up to `max_fix_iterations`), never blocks the interactive turn (FR-010, FR-017).
pub struct VerifyLoop {
    config: VerifyConfig,
    graph: Arc<DependencyGraph>,
}

impl VerifyLoop {
    pub fn new(config: VerifyConfig, graph: Arc<DependencyGraph>) -> Self {
        Self { config, graph }
    }

    /// Construct from a solely-owned graph (used by [`VerifyLoop::run_detached`],
    /// which must move the graph across a thread boundary —
    /// `Arc<DependencyGraph>` is not `Send` because rusqlite's statement
    /// cache is `!Sync`).
    pub fn from_graph(config: VerifyConfig, graph: DependencyGraph) -> Self {
        Self {
            config,
            graph: Arc::new(graph),
        }
    }

    /// Run all configured verification steps. Returns results in order.
    ///
    /// This is synchronous (the caller spawns it on a detached task via
    /// `tokio::spawn` — see `run_detached`).
    pub fn run_steps(&self, project_root: &Path) -> Vec<VerifyResult> {
        self.config
            .steps
            .iter()
            .map(|step_cfg| self.run_step(step_cfg, project_root))
            .collect()
    }

    /// Run a single configured step (shared by `run_steps` and the
    /// fix-iteration re-runs in `run_with_fixes`).
    fn run_step(&self, step_cfg: &crate::config::VerifyStepConfig, project_root: &Path) -> VerifyResult {
        let runner = crate::verify::runner::VerifyStep::new(
            step_cfg.name.clone(),
            step_cfg.command.clone(),
            step_cfg.timeout_sec,
        );
        let raw_output = runner.run(project_root);
        let passed = raw_output.exit_code == 0;
        let errors = if passed {
            Vec::new()
        } else {
            crate::verify::parse::parse_errors(&raw_output.output, &step_cfg.parse)
        };
        VerifyResult {
            step_name: step_cfg.name.clone(),
            passed,
            skipped: raw_output.skipped,
            output: raw_output.output,
            errors,
            duration_ms: raw_output.duration_ms,
        }
    }

    /// Run the verify loop with a correction pass (T061, FR-010/011).
    ///
    /// Runs `run_steps`; while not everything passed and the fix budget is
    /// not exhausted, invokes `fix(&results)` — the callback performs the
    /// correction and returns whether it changed anything — then re-runs
    /// only the failed steps and increments the iteration counter. A `fix`
    /// callback returning `false` (nothing changed) breaks the loop early.
    pub fn run_with_fixes(
        &self,
        project_root: &Path,
        mut fix: impl FnMut(&[VerifyResult]) -> bool,
    ) -> VerifyOutcome {
        let mut results = self.run_steps(project_root);
        let mut iterations: u32 = 0;

        // A step only needs a correction pass when it actually RAN and
        // failed; skipped steps (FR-012: tool absent / timeout) are neither
        // failures nor fix targets.
        let has_real_failure = |rs: &[VerifyResult]| rs.iter().any(|r| !r.passed && !r.skipped);

        loop {
            if !has_real_failure(&results) || iterations >= self.config.max_fix_iterations {
                break;
            }
            if !fix(&results) {
                // The correction pass changed nothing — stop early.
                break;
            }
            iterations += 1;
            // Re-run only the failed steps, updating results in place.
            for step_cfg in &self.config.steps {
                let needs_rerun = results
                    .iter()
                    .any(|r| r.step_name == step_cfg.name && !r.passed && !r.skipped);
                if needs_rerun {
                    let fresh = self.run_step(step_cfg, project_root);
                    if let Some(slot) = results.iter_mut().find(|r| r.step_name == step_cfg.name) {
                        *slot = fresh;
                    }
                }
            }
        }

        // All-passed means "every step that ran passed"; skipped steps are
        // informative, not failing (FR-012).
        let all_passed = results
            .iter()
            .all(|r| r.passed || r.skipped);
        let mut outcome = VerifyOutcome {
            results,
            all_passed,
            fix_iterations_used: iterations,
            max_fix_iterations: self.config.max_fix_iterations,
            escalated: false,
            served_tier: None,
        };
        outcome.escalated = outcome.should_escalate();
        outcome
    }

    /// Spawn the verify loop detached from the interactive turn (T061,
    /// FR-017: never blocks the turn).
    ///
    /// Runs a plain `run_with_fixes` with a no-op fix closure on a background
    /// thread (the correction pass is driven by the agent layer in a later
    /// feature; here the orchestrator machinery must exist and be
    /// exercised). Returns a oneshot `Receiver` the caller can await.
    ///
    /// When called inside a tokio runtime the blocking subprocess work is
    /// offloaded via `spawn_blocking`; when no runtime is present the work
    /// still runs on a plain `std::thread` and the outcome is delivered
    /// through the same oneshot channel — this function never panics and
    /// never blocks the caller.
    ///
    /// Note: `DependencyGraph` is `Send` but `!Sync` (rusqlite's statement
    /// cache uses a `RefCell`), so an `Arc<DependencyGraph>` cannot cross a
    /// thread boundary while shared. This function takes sole ownership of
    /// the graph via `Arc::try_unwrap`; if the caller retains other strong
    /// references, an error outcome is delivered instead of panicking.
    pub fn run_detached(
        config: VerifyConfig,
        graph: Arc<DependencyGraph>,
        project_root: PathBuf,
    ) -> tokio::sync::oneshot::Receiver<VerifyOutcome> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        match Arc::try_unwrap(graph) {
            Ok(graph) => match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        let joined = tokio::task::spawn_blocking(move || {
                            let verify = VerifyLoop::from_graph(config, graph);
                            verify.run_with_fixes(&project_root, |_| false)
                        })
                        .await;
                        match joined {
                            Ok(outcome) => {
                                let _ = tx.send(outcome);
                            }
                            Err(join_err) => {
                                // The blocking task panicked — deliver a failed
                                // outcome (with a synthetic failed step carrying
                                // the join error) rather than dropping the channel.
                                let _ = tx.send(VerifyOutcome {
                                    results: vec![VerifyResult {
                                        step_name: "(detached-verify)".into(),
                                        passed: false,
                                        skipped: true,
                                        output: format!(
                                            "detached verify task failed: {}",
                                            join_err
                                        ),
                                        errors: Vec::new(),
                                        duration_ms: 0,
                                    }],
                                    all_passed: false,
                                    fix_iterations_used: 0,
                                    max_fix_iterations: 0,
                                    escalated: true,
                                    served_tier: None,
                                });
                            }
                        }
                    });
                }
                Err(_) => {
                    // No tokio runtime: run on a plain OS thread, still deliver
                    // through the oneshot channel (FR-017 — non-blocking).
                    std::thread::spawn(move || {
                        let verify = VerifyLoop::from_graph(config, graph);
                        let outcome = verify.run_with_fixes(&project_root, |_| false);
                        let _ = tx.send(outcome);
                    });
                }
            },
            Err(shared) => {
                // The graph is shared with other strong references and cannot
                // be moved to a background thread — report instead of panicking.
                let output = format!(
                    "cannot detach: DependencyGraph is shared \
                     (Arc strong count {}); pass sole ownership",
                    Arc::strong_count(&shared)
                );
                drop(shared);
                let _ = tx.send(VerifyOutcome {
                    results: vec![VerifyResult {
                        step_name: "(detached-verify)".into(),
                        passed: false,
                        skipped: true,
                        output,
                        errors: Vec::new(),
                        duration_ms: 0,
                    }],
                    all_passed: false,
                    fix_iterations_used: 0,
                    max_fix_iterations: 0,
                    escalated: true,
                    served_tier: None,
                });
            }
        }

        rx
    }

    /// The maximum number of fix iterations (FR-010).
    pub fn max_fix_iterations(&self) -> u32 {
        self.config.max_fix_iterations
    }

    /// Borrow the graph (for pattern recording).
    pub fn graph(&self) -> &DependencyGraph {
        &self.graph
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(
        all_passed: bool,
        used: u32,
        max: u32,
        served_tier: Option<&str>,
    ) -> VerifyOutcome {
        VerifyOutcome {
            results: Vec::new(),
            all_passed,
            fix_iterations_used: used,
            max_fix_iterations: max,
            escalated: false,
            served_tier: served_tier.map(String::from),
        }
    }

    #[test]
    fn should_escalate_true_only_when_failed_and_exhausted() {
        // Failed + exhausted → escalate.
        assert!(outcome(false, 3, 3, None).should_escalate());
        // Failed but budget not exhausted → no.
        assert!(!outcome(false, 2, 3, None).should_escalate());
        // Passed (even at exhaustion) → no.
        assert!(!outcome(true, 3, 3, None).should_escalate());
        assert!(!outcome(true, 0, 3, None).should_escalate());
        // Zero-budget edge: a failing run with max=0 counts as exhausted.
        assert!(outcome(false, 0, 0, None).should_escalate());
    }

    #[test]
    fn escalate_hint_requires_economical_tier() {
        // Router/developer disagree: economical tier exhausted its fixes
        // while still failing → hint to re-dispatch on frontier.
        let o = outcome(false, 3, 3, Some("economical"));
        assert_eq!(o.escalate_hint().as_deref(), Some("frontier"));

        // Already on frontier → no further hint.
        assert_eq!(outcome(false, 3, 3, Some("frontier")).escalate_hint(), None);
        // No tier recorded → no hint.
        assert_eq!(outcome(false, 3, 3, None).escalate_hint(), None);
        // Passed → no hint regardless of tier.
        assert_eq!(outcome(true, 3, 3, Some("economical")).escalate_hint(), None);
        // Failed but not exhausted → no hint.
        assert_eq!(outcome(false, 1, 3, Some("economical")).escalate_hint(), None);
    }
}

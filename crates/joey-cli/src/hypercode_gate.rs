//! VerifyLoop → VerificationGate adapter (spec 023 T023, FR-019/FR-020).
//! Maps orchestration VerificationPlanView steps onto neurocode's VerifyConfig,
//! awaits the outcome, and routes failures into a DefectBundle queue that the
//! HypercodeDispatcher consumes to drive repair re-dispatch (the scheduler's
//! Repair directive owns re-execution; detached verify stays informational).
//!
//! T029 (US8, FR-022): risk-triggered specialist review. When the plan view
//! carries `risk_triggered_review` (forced on by the Evaluator for High-risk
//! tasks), the gate invokes an independent reviewer AFTER all steps pass;
//! rejection findings enter the SAME DefectBundle/repair path as command
//! failures, and a missing reviewer records a notice-and-proceed.
pub struct VerifyLoopGate {
    // Retained for forthcoming verification-loop wiring; not yet read by
    // gate logic.
    #[allow(dead_code)]
    project_root: std::path::PathBuf,
    pending_repairs: std::sync::Arc<std::sync::Mutex<Vec<joey_orchestration::evaluator::DefectBundle>>>,
    /// T029 (US8): reviewer invoked when the plan requests risk review;
    /// None ⇒ notice-and-proceed (spec: skip gracefully, record notice).
    reviewer: Option<std::sync::Arc<dyn RiskReviewer>>,
    /// T029: audit trail drained into run evidence by execute_graph_run.
    review_events: std::sync::Arc<std::sync::Mutex<Vec<ReviewEvent>>>,
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
            reviewer: None,
            review_events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The shared repair queue (clone of the Arc — the dispatcher pops from
    /// the same Vec the gate pushes into).
    pub fn repair_queue(&self) -> Arc<Mutex<Vec<DefectBundle>>> {
        Arc::clone(&self.pending_repairs)
    }

    /// T029: attach the production reviewer (chainable).
    pub fn with_reviewer(mut self, reviewer: std::sync::Arc<dyn RiskReviewer>) -> Self {
        self.reviewer = Some(reviewer);
        self
    }

    /// T029: audit trail of review outcomes/notices (drained by
    /// execute_graph_run into EvidenceKind::ReviewOutcome records).
    pub fn review_events(&self) -> std::sync::Arc<std::sync::Mutex<Vec<ReviewEvent>>> {
        self.review_events.clone()
    }

    /// T036: test observability for reviewer attachment (both runtime
    /// paths construct the gate through the shared `graph_gate` helper).
    #[cfg(test)]
    pub(crate) fn has_reviewer(&self) -> bool {
        self.reviewer.is_some()
    }

    /// T029: append one event to the review audit trail.
    fn record_review_event(
        &self,
        workdir: &Path,
        verdict: &str,
        findings: Vec<String>,
        detail: &str,
    ) {
        self.review_events
            .lock()
            .expect("review events")
            .push(ReviewEvent {
                workdir: workdir.display().to_string(),
                verdict: verdict.to_string(),
                findings,
                detail: detail.to_string(),
            });
    }
}

/// FR-019 (awaited gate decides completion), FR-020 (DefectBundle feeds
/// repair), FR-031 (Degraded = command unavailable, neither passed nor a
/// code defect — never routes to repair).
#[async_trait::async_trait]
impl VerificationGate for VerifyLoopGate {
    async fn run(&self, plan: &VerificationPlanView, workdir: &Path) -> GateOutcome {
        // Empty plan with no review request ⇒ nothing to verify (invariant
        // 6: high-risk tasks carry steps). T029 (FR-022): an empty-steps
        // plan that requests risk review (legal for High-risk tasks per the
        // planner contract) falls through to the review tail instead of
        // passing outright.
        if plan.steps.is_empty() && !plan.risk_triggered_review {
            return GateOutcome::Passed;
        }

        if !plan.steps.is_empty() {
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
                // T029: a Failed bundle returns WITHOUT review — the defect
                // loop already owns it; review would duplicate.
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
            // produced a code defect, so it never routes to repair. T029:
            // Degraded returns WITHOUT review (degraded never completes and
            // never triggers code-defect repair; review is moot).
            let required_skipped = plan.steps.iter().any(|s| {
                s.required && outcome.results.iter().any(|r| r.skipped && r.step_name == s.name)
            });
            if required_skipped {
                return GateOutcome::Degraded;
            }
        }

        // T029 (FR-022): risk-triggered specialist review before
        // approval. Findings block completion and enter the defect
        // loop; a missing/unavailable reviewer records a notice and the
        // run proceeds (spec assumption: graceful skip).
        if plan.risk_triggered_review {
            let objective = plan
                .steps
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let objective = if objective.is_empty() {
                "risk-flagged task (no verification steps)".to_string()
            } else {
                format!("risk-flagged task; verification steps: {objective}")
            };
            match self.reviewer.as_ref() {
                None => {
                    self.record_review_event(
                        workdir,
                        "notice",
                        vec![],
                        "no reviewer configured (FR-022 notice-and-proceed)",
                    );
                }
                Some(reviewer) => {
                    let verdict = reviewer.review(&objective, workdir).await;
                    match verdict {
                        ReviewVerdict::Approve => {
                            self.record_review_event(
                                workdir,
                                "approve",
                                vec![],
                                "specialist review approved",
                            );
                        }
                        ReviewVerdict::Reject(findings) => {
                            self.record_review_event(
                                workdir,
                                "reject",
                                findings.clone(),
                                "specialist review rejected; findings enter the defect loop",
                            );
                            let bundle = DefectBundle {
                                task_id: String::new(), // dispatcher stamps the real task id (T023)
                                failed_commands: Vec::new(),
                                policy_violations: Vec::new(),
                                reviewer_findings: findings,
                                changed_paths: Vec::new(),
                            };
                            self.pending_repairs
                                .lock()
                                .expect("pending repairs")
                                .push(bundle.clone());
                            return GateOutcome::Failed(bundle);
                        }
                        ReviewVerdict::Notice(detail) => {
                            self.record_review_event(workdir, "notice", vec![], &detail);
                        }
                    }
                }
            }
        }

        // Skipped OPTIONAL steps don't gate.
        GateOutcome::Passed
    }
}

/// T029 (US8): outcome of one specialist review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReviewVerdict {
    Approve,
    Reject(Vec<String>),
    Notice(String),
}

/// T029 (FR-022): parse the reviewer child's summary for the mandated
/// VERDICT protocol. Case-insensitive; REJECT extracts findings from
/// `VERDICT: REJECT: <text>` plus any `FINDING:` lines. None when no
/// verdict is present (treated as a notice downstream).
pub(crate) fn parse_verdict(summary: &str) -> Option<ReviewVerdict> {
    // ASCII-safe line-prefix match: a line "starts with" the prefix (after
    // trim_start) iff its first bytes equal the prefix case-insensitively.
    // Comparing the ASCII prefix bytewise keeps slicing at a char boundary.
    fn line_prefix<'a>(line: &'a str, prefix: &[u8]) -> Option<&'a str> {
        let t = line.trim_start();
        let b = t.as_bytes();
        if b.len() >= prefix.len() && b[..prefix.len()].eq_ignore_ascii_case(prefix) {
            Some(&t[prefix.len()..])
        } else {
            None
        }
    }

    // Scan lines in REVERSE: only the LAST verdict-bearing line counts (a
    // review that merely quotes the protocol mid-body — "the reviewer must
    // emit `VERDICT: APPROVE`" — must not be classified by the quote).
    let mut verdict_word: Option<&str> = None;
    let mut verdict_rest: Option<String> = None;
    for line in summary.lines().rev() {
        if let Some(rest) = line_prefix(line, b"verdict:") {
            let rest = rest.trim();
            // Leading word, tolerant of the `REJECT:`/`REJECT` spellings.
            let word = rest.split_whitespace().next().unwrap_or("").trim_end_matches(':');
            let classified = if word.eq_ignore_ascii_case("approve") {
                Some("approve")
            } else if word.eq_ignore_ascii_case("reject") {
                Some("reject")
            } else {
                None
            };
            if let Some(w) = classified {
                verdict_word = Some(w);
                verdict_rest = Some(rest.to_string());
                break;
            }
        }
    }

    match verdict_word? {
        "approve" => Some(ReviewVerdict::Approve),
        _ => {
            let mut findings: Vec<String> = summary
                .lines()
                .filter_map(|l| line_prefix(l, b"finding:").map(|r| r.trim().to_string()))
                .filter(|f| !f.is_empty())
                .collect();
            // The verdict-line remainder (after the leading word) is a
            // finding too, when non-empty.
            if let Some(rest) = verdict_rest {
                let after_word = rest.split_whitespace().skip(1).collect::<Vec<_>>().join(" ");
                let after_word = after_word.trim().trim_start_matches(':').trim().to_string();
                if !after_word.is_empty() && !findings.contains(&after_word) {
                    findings.push(after_word);
                }
            }
            if findings.is_empty() {
                findings.push("reviewer rejected the change without stating a finding".to_string());
            }
            Some(ReviewVerdict::Reject(findings))
        }
    }
}

/// One recorded review event (audit trail; drained into the run's
/// evidence by execute_graph_run as EvidenceKind::ReviewOutcome).
#[derive(Debug, Clone)]
pub(crate) struct ReviewEvent {
    pub workdir: String,
    pub verdict: String,
    pub findings: Vec<String>,
    pub detail: String,
}

/// T029 (FR-022): the specialist reviewer invocation, behind a
/// crate-internal trait so the gate's trigger/block/notice paths are
/// unit-testable without a live model.
#[async_trait::async_trait]
pub(crate) trait RiskReviewer: Send + Sync {
    async fn review(&self, objective: &str, workdir: &Path) -> ReviewVerdict;
}

/// Production reviewer: dispatches the existing `momus` persona via
/// category delegation (R12 — reviewers are never team members). A
/// failed dispatch or unparseable verdict degrades to Notice (spec
/// assumption L263: skip gracefully with a recorded notice).
pub(crate) struct MomusReviewer {
    ctx: crate::hypercode::HypercodeContext,
}

impl MomusReviewer {
    pub(crate) fn new(ctx: crate::hypercode::HypercodeContext) -> Self {
        Self { ctx }
    }
}

/// Resolve the reviewer subagent's model WITHOUT dispatching (pure, unit-testable).
///
/// Chain (FR: never silently the orchestrator's model): reviewer table →
/// implementor table → explorer table → None (None = inherit the parent, the
/// same default children use). Derives the provider key the same way the
/// dispatch call sites of `get_implementor_config` do (agent_config.provider).
pub(crate) fn reviewer_request_model(ctx: &crate::hypercode::HypercodeContext) -> Option<String> {
    let cfg = crate::hypercode::HyperCodeConfig::from_config(&ctx.config);
    // Same provider derivation the dispatch call sites use
    // (hypercode.rs: provider = ctx.agent_config.provider.clone()).
    let provider = ctx.agent_config.provider.clone();
    let reviewer = cfg.get_reviewer_config(&provider);
    if !reviewer.model.is_empty() {
        return Some(reviewer.model);
    }
    let implementor = cfg.get_implementor_config(&provider);
    if !implementor.model.is_empty() {
        return Some(implementor.model);
    }
    let explorer = cfg.get_explorer_config(&provider);
    if !explorer.model.is_empty() {
        return Some(explorer.model);
    }
    None
}

/// Parse a reasoning-level string ("none"|"low"|"medium"|"high"|"") the same
/// way joey-cli's hypercode module does (empty/inherit ⇒ None).
fn parse_role_reasoning_level(level: &str) -> Option<joey_providers::ReasoningEffort> {
    crate::hypercode::parse_reasoning_level(level)
}

/// 0 ⇒ None (unset), else Some(n) — mirrors hypercode.rs's `nonzero`.
fn nonzero_u32(n: usize) -> Option<u32> {
    if n == 0 {
        None
    } else {
        Some(n as u32)
    }
}

#[async_trait::async_trait]
impl RiskReviewer for MomusReviewer {
    async fn review(&self, objective: &str, workdir: &Path) -> ReviewVerdict {
        use joey_orchestration::types::{DelegationRequest, SubagentRole};
        let goal = format!(
            "Independent risk review (FR-022). A worker completed this task in {workdir}:\n{objective}\n\n\
             Inspect the uncommitted working-tree changes in {workdir} (git status / git diff). \
             You are an independent reviewer, not the author. Judge correctness, security, \
             concurrency and interface risk. Be strict but fair.\n\
             List each concern as a line starting with `FINDING: `. \
             End your reply with exactly one final line: `VERDICT: APPROVE` or `VERDICT: REJECT: <primary reason>`.",
            workdir = workdir.display(),
            objective = objective,
        );
        // Opt-in reviewer config (hypercode.reviewer.<provider>): model chain
        // (reviewer → implementor → explorer → parent) plus optional
        // max_turns/max_tokens/reasoning overrides, mirroring the field forms
        // apply_hyper_role uses (delegation_tool.rs).
        let provider = self.ctx.agent_config.provider.clone();
        let hc = crate::hypercode::HyperCodeConfig::from_config(&self.ctx.config);
        let reviewer_cfg = hc.get_reviewer_config(&provider);
        let req = DelegationRequest {
            // Mirror the field forms used by HypercodeDispatcher::dispatch
            // in hypercode.rs (~L1235-1257); only goal/category/toolsets/
            // workdir/max_turns/prompt_append differ.
            goal,
            context: None,
            tasks: Vec::new(),
            model: reviewer_request_model(&self.ctx),
            toolsets: vec!["file".to_string(), "terminal".to_string()],
            max_turns: Some(if reviewer_cfg.max_turns > 0 {
                reviewer_cfg.max_turns
            } else {
                8
            }),
            reasoning: parse_role_reasoning_level(&reviewer_cfg.reasoning_level),
            max_tokens: nonzero_u32(reviewer_cfg.max_tokens),
            persist: false,
            role: SubagentRole::Leaf,
            workdir: Some(workdir.to_path_buf()),
            category: Some("momus".to_string()),
            subagent_type: None,
            load_skills: Vec::new(),
            prompt_append: None,
            team: None,
            name: None,
        };
        let results = self
            .ctx
            .manager
            .dispatch_requests(
                &[req],
                &self.ctx.agent_config,
                &self.ctx.config,
                &self.ctx.base_registry,
                None,
            )
            .await;
        match results.first() {
            Some(r) if r.success => match parse_verdict(&r.summary) {
                Some(v) => v,
                None => ReviewVerdict::Notice(format!(
                    "reviewer returned no parseable verdict (summary: {})",
                    r.summary.chars().take(200).collect::<String>()
                )),
            },
            Some(r) => ReviewVerdict::Notice(format!(
                "reviewer dispatch failed (momus): {}",
                r.error.clone().unwrap_or_else(|| "unknown error".to_string())
            )),
            None => ReviewVerdict::Notice("reviewer dispatch returned no result".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use joey_orchestration::evaluator::VerificationStepView;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    /// Build a Config from raw YAML via a temp file (the config layer has no
    /// in-memory constructor for user values) — same pattern hypercode.rs's
    /// tests use.
    fn config_with_yaml(yaml: &str) -> joey_core::Config {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), yaml).unwrap();
        joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap()
    }

    /// Minimal HypercodeContext shaped like hypercode.rs's model_ctx fixture
    /// (provider + config tree; the manager/registry are inert for the pure
    /// chain fn and never dispatched).
    fn chain_ctx(provider: &str, yaml: &str) -> crate::hypercode::HypercodeContext {
        use joey_agent_core::AgentConfig;
        use joey_orchestration::{ManagerConfig, SubagentManager};
        use joey_tools::ToolRegistry;
        crate::hypercode::HypercodeContext {
            agent_config: AgentConfig {
                model: "parent-model".to_string(),
                provider: provider.to_string(),
                base_url: String::new(),
                api_key: None,
                max_turns: 5,
                api_max_retries: 1,
                tool_delay: 0.0,
                reasoning: None,
                enabled_tools: Vec::new(),
                max_tokens: None,
                stream: false,
                pass_session_id: false,
                model_pinned: false,
            },
            config: config_with_yaml(yaml),
            base_registry: ToolRegistry::new(),
            manager: std::sync::Arc::new(SubagentManager::new(ManagerConfig::default())),
            cwd: std::path::PathBuf::from("/tmp"),
            parent_effective_model: None,
            execution_graph: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Reviewer model chain, case 1: an explicit reviewer-table model wins
    /// over implementor and explorer entries.
    #[test]
    fn reviewer_model_chain_reviewer_table_wins() {
        let ctx = chain_ctx(
            "zai",
            "hypercode:\n  reviewer:\n    zai:\n      model: reviewer-model\n  implementor:\n    zai:\n      model: impl-model\n  explorer:\n    zai:\n      model: explore-model\n",
        );
        assert_eq!(
            reviewer_request_model(&ctx).as_deref(),
            Some("reviewer-model"),
            "reviewer-table model must win over implementor/explorer"
        );
    }

    /// Case 2: no reviewer entry → falls back to the implementor table.
    #[test]
    fn reviewer_model_chain_falls_back_to_implementor() {
        let ctx = chain_ctx(
            "zai",
            "hypercode:\n  implementor:\n    zai:\n      model: impl-model\n  explorer:\n    zai:\n      model: explore-model\n",
        );
        assert_eq!(reviewer_request_model(&ctx).as_deref(), Some("impl-model"));
    }

    /// Case 3: no reviewer/implementor entries → falls back to explorer.
    #[test]
    fn reviewer_model_chain_falls_back_to_explorer() {
        let ctx = chain_ctx(
            "zai",
            "hypercode:\n  explorer:\n    zai:\n      model: explore-model\n",
        );
        assert_eq!(
            reviewer_request_model(&ctx).as_deref(),
            Some("explore-model")
        );
    }

    /// Case 4: no tables configured → None (the request inherits the parent,
    /// never a silently-resolved orchestrator model).
    #[test]
    fn reviewer_model_chain_none_when_no_tables() {
        let ctx = chain_ctx("zai", "hypercode:\n  enabled: true\n");
        assert_eq!(reviewer_request_model(&ctx), None);
    }

    /// A reviewer-table entry for a DIFFERENT provider must not leak into
    /// this provider's chain (provider-keyed lookup).
    #[test]
    fn reviewer_model_chain_is_provider_keyed() {
        let ctx = chain_ctx(
            "zai",
            "hypercode:\n  reviewer:\n    openai:\n      model: other-provider-model\n",
        );
        assert_eq!(reviewer_request_model(&ctx), None);
    }

    /// from_config: the `hypercode.reviewer` provider table parses, and a
    /// top-level `enabled: true` under it is the bool GATE — not a provider.
    #[test]
    fn reviewer_table_parses_and_skips_enabled_gate_key() {
        let tree = config_with_yaml(
            "hypercode:\n  reviewer:\n    enabled: true\n    zai:\n      model: reviewer-model\n      max_turns: 5\n      max_tokens: 4096\n      reasoning_level: high\n",
        );
        let hc = crate::hypercode::HyperCodeConfig::from_config(&tree);
        assert!(!hc.reviewer_configs.contains_key("enabled"));
        let rc = hc.get_reviewer_config("zai");
        assert_eq!(rc.model, "reviewer-model");
        assert_eq!(rc.max_turns, 5);
        assert_eq!(rc.max_tokens, 4096);
        assert_eq!(rc.reasoning_level, "high");
        // Default (unknown provider) shape mirrors explorer's: empty model,
        // max_turns 8, empty reasoning.
        let dflt = hc.get_reviewer_config("unknown-provider");
        assert_eq!(dflt.model, "");
        assert_eq!(dflt.max_turns, 8);
        assert_eq!(dflt.reasoning_level, "");
    }

    /// T029: empty-steps plan with the review flag (the simplest
    /// review-path fixture — legal for High-risk tasks).
    fn review_plan(review: bool) -> VerificationPlanView {
        VerificationPlanView {
            steps: vec![],
            risk_triggered_review: review,
        }
    }

    /// T029: programmable reviewer that counts invocations.
    struct FakeReviewer {
        verdict: ReviewVerdict,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl RiskReviewer for FakeReviewer {
        async fn review(&self, _objective: &str, _workdir: &Path) -> ReviewVerdict {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.verdict.clone()
        }
    }

    /// T029: reviewer whose invocation panics — proves non-invocation.
    struct PanicReviewer {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl RiskReviewer for PanicReviewer {
        async fn review(&self, _objective: &str, _workdir: &Path) -> ReviewVerdict {
            self.calls.fetch_add(1, Ordering::SeqCst);
            panic!("reviewer must not be invoked when no review is requested");
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

    // ── T029 (US8, FR-022): verdict parsing ─────────────────────────

    #[test]
    fn parse_verdict_approve() {
        assert_eq!(
            parse_verdict("stuff\nVERDICT: APPROVE"),
            Some(ReviewVerdict::Approve)
        );
    }

    #[test]
    fn parse_verdict_reject_extracts_findings() {
        let v = parse_verdict("FINDING: off by one\nVERDICT: REJECT: wrong math");
        match v {
            Some(ReviewVerdict::Reject(findings)) => {
                assert!(
                    findings.contains(&"off by one".to_string()),
                    "FINDING: line extracted: {findings:?}"
                );
                assert!(
                    findings.contains(&"wrong math".to_string()),
                    "REJECT reason extracted: {findings:?}"
                );
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn parse_verdict_none_when_missing() {
        assert_eq!(parse_verdict("no verdict here"), None);
    }

    /// A review that QUOTES the protocol mid-body must not be classified by
    /// the quote — only a real verdict line (the last one) counts.
    #[test]
    fn parse_verdict_protocol_quoting_then_real_reject() {
        let summary = "The reviewer must end with `VERDICT: APPROVE` per protocol.\n\
                       Everything looked acceptable in the quoted instructions.\n\
                       VERDICT: REJECT: bad";
        let v = parse_verdict(summary);
        match v {
            Some(ReviewVerdict::Reject(findings)) => {
                assert!(
                    findings.contains(&"bad".to_string()),
                    "verdict-line remainder extracted: {findings:?}"
                );
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    /// A quoted protocol mention followed by an APPROVE verdict line: the
    /// real (last) verdict line wins, not the quote.
    #[test]
    fn parse_verdict_protocol_quoting_then_real_approve() {
        let summary = "Protocol says emit `VERDICT: REJECT` on failure.\nVERDICT: APPROVE";
        assert_eq!(parse_verdict(summary), Some(ReviewVerdict::Approve));
    }

    /// 'İ' (dotted capital I) lowercases to TWO chars — the old byte-index
    /// math sliced mid-char and panicked. Must not panic, must classify.
    #[test]
    fn parse_verdict_turkish_i_no_panic() {
        let summary = "İİİ review body\nVERDICT: APPROVE";
        assert_eq!(parse_verdict(summary), Some(ReviewVerdict::Approve));
        let summary = "İİİ review body\nFINDING: broken thing\nVERDICT: REJECT: wrong";
        match parse_verdict(summary) {
            Some(ReviewVerdict::Reject(findings)) => {
                assert!(findings.contains(&"broken thing".to_string()));
                assert!(findings.contains(&"wrong".to_string()));
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    /// The LAST verdict-bearing line wins when several appear.
    #[test]
    fn parse_verdict_last_line_wins() {
        let summary = "VERDICT: APPROVE\nsecond thoughts\nVERDICT: REJECT: reverted";
        match parse_verdict(summary) {
            Some(ReviewVerdict::Reject(findings)) => {
                assert!(findings.contains(&"reverted".to_string()));
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    // ── T029 (US8, FR-022): gate review paths ───────────────────────

    #[tokio::test]
    async fn review_rejection_blocks_approval() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = VerifyLoopGate::new(dir.path().to_path_buf()).with_reviewer(Arc::new(
            FakeReviewer {
                verdict: ReviewVerdict::Reject(vec!["finding X".to_string()]),
                calls: calls.clone(),
            },
        ));
        let outcome = gate.run(&review_plan(true), dir.path()).await;
        match outcome {
            GateOutcome::Failed(bundle) => {
                assert_eq!(bundle.reviewer_findings, vec!["finding X".to_string()]);
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(gate.repair_queue().lock().unwrap().len(), 1);
        let events = gate.review_events();
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].verdict, "reject");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn review_approval_passes() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = VerifyLoopGate::new(dir.path().to_path_buf()).with_reviewer(Arc::new(
            FakeReviewer {
                verdict: ReviewVerdict::Approve,
                calls: calls.clone(),
            },
        ));
        let outcome = gate.run(&review_plan(true), dir.path()).await;
        assert_eq!(outcome, GateOutcome::Passed);
        assert!(gate.repair_queue().lock().unwrap().is_empty());
        let events = gate.review_events();
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].verdict, "approve");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn missing_reviewer_records_notice_and_proceeds() {
        let dir = tempfile::tempdir().unwrap();
        // No with_reviewer — reviewer stays None.
        let gate = VerifyLoopGate::new(dir.path().to_path_buf());
        let outcome = gate.run(&review_plan(true), dir.path()).await;
        assert_eq!(outcome, GateOutcome::Passed);
        assert!(gate.repair_queue().lock().unwrap().is_empty());
        let events = gate.review_events();
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].verdict, "notice");
        assert!(
            events[0].detail.contains("no reviewer"),
            "notice must mention the missing reviewer: {}",
            events[0].detail
        );
    }

    #[tokio::test]
    async fn review_skipped_when_not_requested() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = VerifyLoopGate::new(dir.path().to_path_buf()).with_reviewer(Arc::new(
            PanicReviewer {
                calls: calls.clone(),
            },
        ));
        // risk_triggered_review=false — the reviewer must never run.
        let outcome = gate
            .run(&plan(vec![step("ok", "true", true)]), dir.path())
            .await;
        assert_eq!(outcome, GateOutcome::Passed);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(gate.review_events().lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn empty_steps_with_review_still_reviewed() {
        let dir = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = VerifyLoopGate::new(dir.path().to_path_buf()).with_reviewer(Arc::new(
            FakeReviewer {
                verdict: ReviewVerdict::Approve,
                calls: calls.clone(),
            },
        ));
        // Zero steps + review flag: the old empty-plan early return is
        // bypassed and the review still runs (T029).
        let outcome = gate.run(&review_plan(true), dir.path()).await;
        assert_eq!(outcome, GateOutcome::Passed);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// T036: a bare gate has no reviewer; attaching one flips
    /// `has_reviewer`. (The graph_gate helper lives in hypercode.rs and
    /// needs a HypercodeContext — its own test covers it there.)
    #[test]
    fn bare_gate_has_no_reviewer_graph_gate_does() {
        assert!(!VerifyLoopGate::new(std::path::PathBuf::from(".")).has_reviewer());
        let calls = Arc::new(AtomicUsize::new(0));
        let gate =
            VerifyLoopGate::new(std::path::PathBuf::from(".")).with_reviewer(Arc::new(FakeReviewer {
                verdict: ReviewVerdict::Approve,
                calls,
            }));
        assert!(gate.has_reviewer());
    }
}

//! Enterprise task analysis plane (spec 023, FR-001/FR-005).
//!
//! FR-001 (unified analysis): one `analyze()` call produces the complete
//! per-request analysis — revision binding, target/impacted artifacts,
//! combined effective policies with surfaced conflicts, complexity route,
//! risk assessment, recommended model tier, execution hint, and the scoped
//! verification plan.
//!
//! FR-005 (additive): everything here is new public surface; the existing
//! `NeuroCodeEngine` trait and its implementations are untouched.
//!
//! FR-025 (record path): `record_outcome` is the only legal write into
//! outcome memory, and only accepts a [`VerifiedOutcome`] (produced when a
//! task Completes through a passed gate).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::classifier::{
    ComplexityClassifier, ComplexityRoute, ComplexityTier, GRAPH_HUB_DEPENDENTS_THRESHOLD,
};
use crate::config::NeuroCodeConfig;
use crate::engine::CodingRequest;
use crate::graph::{DependencyGraph, EdgeKind, NodeId};
use crate::memory::outcomes::{OutcomeMemory, OutcomeMemoryBuffer, VerifiedOutcome};
use crate::policy::resolver::{combine, PolicyConflict};
use crate::policy::sources::collect_policies;
use crate::policy::PolicyBinding;
use crate::risk::{assess, RiskAssessment, RiskFactor, RiskFactorKind, RiskLevel};
use crate::verification_plan::{VerificationPlan, VerificationStep};

/// FR-023 routing inputs. Analysis returns a provisional hint derived from
/// impact shape; the authoritative hint is computed from the validated
/// TaskGraph at planning time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct ExecutionHint {
    pub write_overlap: bool,
    pub strict_dependency_depth: u32,
    pub independent_components: u32,
    pub cross_component_coordination: bool,
}

/// DAG-safe task descriptor mirroring orchestration's TaskNode for
/// context/verification derivation. `joey-cli` adapts `TaskNode` →
/// `AnalysisTask` at the boundary. This is a deliberate stand-in for the
/// contract's `&TaskNode` — orchestration cannot be imported here without
/// a dependency cycle (orchestration is a higher crate).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AnalysisTask {
    pub id: String,
    pub objective: String,
    pub dependencies: Vec<String>,
    pub read_set: Vec<PathBuf>,
    pub write_set: Vec<PathBuf>,
    pub risk: RiskLevel,
    pub verification: VerificationPlan,
}

/// Task-scoped context (planner input): effective directives in force,
/// surfaced policy conflicts, impacted artifacts, spanning modules, and
/// consulted outcome-memory lessons.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TaskContext {
    pub task_id: String,
    pub objective: String,
    pub effective_directives: Vec<String>,
    pub policy_conflicts: Vec<PolicyConflict>,
    pub impacted_artifacts: Vec<u64>,
    pub modules: Vec<String>,
    pub lessons: Vec<OutcomeMemory>,
}

/// Unified per-request analysis (data-model.md field set + surfaced
/// conflicts), produced by [`EnterpriseTaskAnalyzer::analyze`].
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TaskAnalysis {
    pub revision: String,
    pub target_artifacts: Vec<NodeId>,
    pub impacted_artifacts: Vec<NodeId>,
    pub effective_policies: Vec<PolicyBinding>,
    pub policy_conflicts: Vec<PolicyConflict>,
    pub complexity: ComplexityRoute,
    pub risk: RiskAssessment,
    pub model_tier: ComplexityTier,
    pub execution_hint: ExecutionHint,
    pub verification: VerificationPlan,
}

/// The enterprise analysis-plane contract (public-api.md). All methods are
/// synchronous and deterministic — no LLM calls, no network (FR-017).
/// `Send + Sync` per the contract.
pub trait EnterpriseTaskAnalyzer: Send + Sync {
    /// Produce the unified analysis for a coding request (FR-001).
    fn analyze(&self, request: &CodingRequest) -> TaskAnalysis;

    /// Produce the task-scoped context for a planned task.
    fn context_for(&self, task: &AnalysisTask) -> TaskContext;

    /// Derive the scoped verification plan for a planned task.
    fn verification_for(&self, task: &AnalysisTask) -> VerificationPlan;

    /// Record a verified outcome into outcome memory (FR-025) — the only
    /// legal write path.
    fn record_outcome(&self, outcome: &VerifiedOutcome);
}

/// The analysis-plane engine. The contract names it `DefaultEngine`; it is
/// renamed `AnalysisEngine` here to avoid colliding with
/// `engine::DefaultEngine` — a deliberate, documented divergence.
pub struct AnalysisEngine {
    classifier: ComplexityClassifier,
    graph: Option<Arc<Mutex<DependencyGraph>>>,
    project_root: PathBuf,
    base_verification: VerificationPlan,
    outcomes: Mutex<OutcomeMemoryBuffer>,
    /// T028 (US7): optional SQLite outcome store attached by the runtime
    /// — `record_outcome` writes through to it and `context_for` consults
    /// it in addition to the in-memory buffer. `None` keeps the engine
    /// behaving exactly as before (additive).
    attached_store: Option<Arc<Mutex<crate::memory::outcomes::OutcomeStore>>>,
    /// Engine-level acceptance criteria (feature-scope wiring): appended
    /// (additive, deduped) to every verification plan this engine builds.
    /// Default empty — an empty store leaves plans unchanged.
    acceptance_criteria: Vec<String>,
    /// The tier AmbiguousDefault resolves to, captured from
    /// `config.ambiguous_default_tier()` at construction so `analyze`
    /// honors the configured default (like `engine.rs`) instead of
    /// hardcoding Economical.
    ambiguous_default_tier: ComplexityTier,
}

impl AnalysisEngine {
    /// Build from a classifier (carrying the same shared graph), the
    /// shared graph handle, the project root, and NeuroCode config. The
    /// base verification plan comes from `config.verify.steps` (every
    /// configured step `required: true`); empty when unconfigured.
    pub fn new(
        classifier: ComplexityClassifier,
        graph: Option<Arc<Mutex<DependencyGraph>>>,
        project_root: PathBuf,
        config: &NeuroCodeConfig,
    ) -> Self {
        let base_verification = VerificationPlan {
            steps: config
                .verify
                .steps
                .iter()
                .map(|step| VerificationStep {
                    name: step.name.clone(),
                    command: step.command.clone(),
                    parse: step.parse.clone(),
                    timeout_sec: step.timeout_sec,
                    required: true,
                })
                .collect(),
            risk_triggered_review: false,
            acceptance_criteria: Vec::new(),
        };
        Self {
            classifier,
            graph,
            project_root,
            base_verification,
            outcomes: Mutex::new(OutcomeMemoryBuffer::default()),
            attached_store: None,
            acceptance_criteria: Vec::new(),
            ambiguous_default_tier: config.ambiguous_default_tier(),
        }
    }

    /// Set the engine-level acceptance criteria store (feature-scope
    /// wiring): these are appended to every verification plan built by
    /// [`Self::analyze`] / [`Self::verification_for`]. Replaces the
    /// previous store contents.
    pub fn set_acceptance_criteria(&mut self, criteria: Vec<String>) {
        self.acceptance_criteria = criteria;
    }

    /// The stored engine-level acceptance criteria (empty by default).
    pub fn acceptance_criteria(&self) -> &[String] {
        &self.acceptance_criteria
    }

    /// T028 (US7): attach the SQLite outcome store. record_outcome then
    /// writes through to it, and consults read from it in addition to the
    /// in-memory buffer. Additive: engines without a store behave exactly
    /// as before.
    pub fn attach_outcome_store(
        &mut self,
        store: std::sync::Arc<std::sync::Mutex<crate::memory::outcomes::OutcomeStore>>,
    ) {
        self.attached_store = Some(store);
    }
}

impl EnterpriseTaskAnalyzer for AnalysisEngine {
    fn analyze(&self, request: &CodingRequest) -> TaskAnalysis {
        // (i) Revision binding: `git rev-parse HEAD` in the project root;
        // "unknown" on any failure (not a repo, git missing, non-zero).
        let revision = git_head_revision(&self.project_root);

        // (ii)+(iii)+(vi, graph part): ONE locked graph snapshot serves
        // targets, impacted closure, and graph-derived risk factors. The
        // guard drops at the end of this block, before classify() locks
        // the same shared graph (no deadlock, no torn snapshot).
        let mut targets: Vec<NodeId> = Vec::new();
        let mut impacted: Vec<NodeId> = Vec::new();
        let mut graph_factors: Vec<RiskFactor> = Vec::new();
        let mut target_packages: Vec<String> = Vec::new();
        let mut target_source_paths: Vec<String> = Vec::new();
        if let Some(graph) = &self.graph {
            if let Ok(graph) = graph.lock() {
                // Targets: type-level nodes declared in the active file.
                let target_nodes: Vec<crate::graph::CodeArtifactNode> = request
                    .active_file
                    .as_deref()
                    .map(|path| graph.store().nodes_by_source_path(path).unwrap_or_default())
                    .unwrap_or_default();
                targets = target_nodes.iter().map(|node| node.id).collect();

                // Impacted: BFS over incoming edges from each target,
                // skipping MemberOf (membership is not dependency), dedup
                // via BTreeSet, excluding the targets themselves. All
                // graph errors degrade to empty — analysis never panics.
                impacted = impacted_closure(&graph, &targets);

                // Risk factors rebuilt from the SAME locked snapshot.
                graph_factors = graph_risk_factors(&graph, &target_nodes);

                for node in &target_nodes {
                    if !target_packages.contains(&node.package) {
                        target_packages.push(node.package.clone());
                    }
                    if !target_source_paths.contains(&node.source_path) {
                        target_source_paths.push(node.source_path.clone());
                    }
                }
            }
        }
        // (+ active_file if the graph resolved nothing).
        if target_source_paths.is_empty() {
            if let Some(active_file) = request.active_file.as_deref() {
                target_source_paths.push(active_file.to_string());
            }
        }

        let mut factors = graph_factors;

        // Ownership boundary: targets span >= 2 distinct packages.
        if target_packages.len() >= 2 {
            factors.push(RiskFactor {
                kind: RiskFactorKind::OwnershipBoundary,
                evidence: format!(
                    "targets span {} distinct packages: {}",
                    target_packages.len(),
                    target_packages.join(", ")
                ),
                weight: target_packages.len() as u8,
            });
        }

        // Request-text factors (graph-independent, case-insensitive):
        // security / concurrency mentions.
        let text_lower = request.text.to_lowercase();
        if ["auth", "security", "token", "credential"]
            .iter()
            .any(|kw| text_lower.contains(kw))
        {
            factors.push(RiskFactor {
                kind: RiskFactorKind::SecuritySensitive,
                evidence: "request mentions auth/security/token/credential".to_string(),
                weight: 1,
            });
        }
        if ["concurrent", "mutex", "race"]
            .iter()
            .any(|kw| text_lower.contains(kw))
        {
            factors.push(RiskFactor {
                kind: RiskFactorKind::Concurrency,
                evidence: "request mentions concurrent/mutex/race".to_string(),
                weight: 1,
            });
        }

        // (iv) Policies over the task paths.
        let bindings = collect_policies(&self.project_root);
        let path_refs: Vec<&str> = target_source_paths.iter().map(String::as_str).collect();
        let combined = combine(&bindings, &path_refs);

        // (v) Complexity: the classifier carries the same shared graph, so
        // GraphHub signals participate.
        let complexity = self.classifier.classify(request);

        // (vi) Assess the accumulated risk factors.
        let risk = assess(factors);

        // (vii) Model tier: resolve AmbiguousDefault → the configured
        // default tier (captured at construction from config).
        let model_tier = complexity.tier.resolve_ambiguous(self.ambiguous_default_tier);

        // (viii) Modules: distinct first path components of impacted +
        // target source_paths (non-empty, forward slashes). Provisional
        // ExecutionHint from impact shape — the authoritative hint comes
        // from the validated TaskGraph at planning time (FR-023).
        let modules = modules_of_impacted(&self.graph, &targets, &impacted);
        let execution_hint = ExecutionHint {
            write_overlap: false,
            strict_dependency_depth: 0,
            independent_components: modules.len() as u32,
            cross_component_coordination: modules.len() >= 2,
        };

        // (ix) Scoped verification; the review flag follows the ASSESSED
        // risk level, set explicitly after scoping. When target resolution
        // produced NO modules (no active_file / nothing resolved — the
        // common path), `scoped(&[])` would drop every step; distinguish
        // that from a genuine narrow-to-none resolution by falling back to
        // the unscoped base plan. Acceptance criteria from the engine-level
        // store are appended (additive only, deduped).
        let mut verification = if modules.is_empty() {
            self.base_verification.clone()
        } else {
            self.base_verification.scoped(&modules)
        };
        verification = verification.with_acceptance_criteria(self.acceptance_criteria.clone());
        verification.risk_triggered_review = risk.level == RiskLevel::High;

        TaskAnalysis {
            revision,
            target_artifacts: targets,
            impacted_artifacts: impacted,
            effective_policies: combined.bindings,
            policy_conflicts: combined.conflicts,
            complexity,
            risk,
            model_tier,
            execution_hint,
            verification,
        }
    }

    fn context_for(&self, task: &AnalysisTask) -> TaskContext {
        // Task paths: read_set ∪ write_set, stringified with forward
        // slashes.
        let mut task_paths: Vec<String> = Vec::new();
        for path in task.read_set.iter().chain(task.write_set.iter()) {
            let s = path.to_string_lossy().replace('\\', "/");
            if !task_paths.contains(&s) {
                task_paths.push(s);
            }
        }

        let bindings = collect_policies(&self.project_root);
        let path_refs: Vec<&str> = task_paths.iter().map(String::as_str).collect();
        let combined = combine(&bindings, &path_refs);

        // Impacted: write_set path nodes + incoming BFS closure (same
        // helper as analyze), then modules over the combined source paths.
        let mut impacted: Vec<NodeId> = Vec::new();
        let mut modules: Vec<String> = Vec::new();
        if let Some(graph) = &self.graph {
            if let Ok(graph) = graph.lock() {
                let mut target_ids: Vec<NodeId> = Vec::new();
                let mut source_paths: Vec<String> = Vec::new();
                for write_path in &task.write_set {
                    let path_str = write_path.to_string_lossy().replace('\\', "/");
                    for node in graph
                        .store()
                        .nodes_by_source_path(&path_str)
                        .unwrap_or_default()
                    {
                        if !target_ids.contains(&node.id) {
                            target_ids.push(node.id);
                        }
                        if !source_paths.contains(&node.source_path) {
                            source_paths.push(node.source_path.clone());
                        }
                    }
                }
                impacted = impacted_closure(&graph, &target_ids);
                for id in &impacted {
                    if let Ok(Some(node)) = graph.store().get_node(*id) {
                        source_paths.push(node.source_path);
                    }
                }
                modules = distinct_first_components(&source_paths);
            }
        }

        // Lessons: consult outcome memory by task signature.
        let mut lessons = self
            .outcomes
            .lock()
            .expect("outcome memory buffer poisoned")
            .consult_by_signature(&task_signature(task));
        // T028 (US7): also consult the attached SQLite store by the SAME
        // signature, surfacing store lessons exactly like buffer lessons
        // (buffer lessons first, store lessons after).
        if let Some(store) = &self.attached_store {
            if let Ok(guard) = store.lock() {
                // T035 (FR-026/SC-008): before surfacing store lessons,
                // expire those whose referenced artifacts changed
                // materially since confirmation — the re-check runs on
                // every consult, so a changed lesson can never surface
                // as guidance.
                if let Some(graph) = self.graph.as_ref() {
                    if let Ok(g) = graph.lock() {
                        let root = &self.project_root;
                        let expired =
                            crate::memory::outcomes::expire_stale_lessons(&guard, |id| {
                                g.store()
                                    .get_node(id)
                                    .ok()
                                    .flatten()
                                    .map(|n| root.join(n.source_path))
                            });
                        if expired > 0 {
                            eprintln!(
                                "neurocode: expired {expired} outcome lesson(s) whose artifacts changed (FR-026)"
                            );
                        }
                    }
                }
                if let Ok(store_lessons) = guard.consult_by_signature(&task_signature(task)) {
                    lessons.extend(store_lessons);
                }
            }
        }

        TaskContext {
            task_id: task.id.clone(),
            objective: task.objective.clone(),
            effective_directives: combined
                .bindings
                .iter()
                .map(|b| b.directive.clone())
                .collect(),
            policy_conflicts: combined.conflicts,
            impacted_artifacts: impacted,
            modules,
            lessons,
        }
    }

    fn verification_for(&self, task: &AnalysisTask) -> VerificationPlan {
        let modules = modules_of_paths(&task.write_set);
        let mut plan = if modules.is_empty() {
            // No resolvable modules (empty/unresolved write set): the
            // unscoped base plan, not `scoped(&[])` which drops every step.
            self.base_verification.clone()
        } else {
            self.base_verification.scoped(&modules)
        };
        plan = plan.with_acceptance_criteria(self.acceptance_criteria.clone());
        plan.risk_triggered_review = task.risk == RiskLevel::High;
        plan
    }

    fn record_outcome(&self, outcome: &VerifiedOutcome) {
        let _ = self
            .outcomes
            .lock()
            .expect("outcome memory buffer poisoned")
            .record(outcome);
        // T028 (US7): write through to the attached SQLite store when one
        // is present (best-effort — store errors degrade to buffer-only).
        if let Some(store) = &self.attached_store {
            if let Ok(guard) = store.lock() {
                let _ = guard.record(outcome);
            }
        }
    }
}

/// Stable v1 task signature: `id|objective|write_set joined by ','`.
/// Matches T027's store convention.
fn task_signature(task: &AnalysisTask) -> String {
    let writes = task
        .write_set
        .iter()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect::<Vec<_>>()
        .join(",");
    format!("{}|{}|{}", task.id, task.objective, writes)
}

/// `git rev-parse HEAD` in `root`; "unknown" on any failure (missing git,
/// non-zero exit, not a repository).
fn git_head_revision(root: &std::path::Path) -> String {
    std::process::Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(root)
        .output()
        .map(|out| {
            if out.status.success() {
                String::from_utf8_lossy(&out.stdout).trim().to_string()
            } else {
                "unknown".to_string()
            }
        })
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Incoming-dependency closure: BFS over `traverse_to` from each seed,
/// skipping `MemberOf` edges (membership is not dependency), deduped via
/// BTreeSet, excluding the seeds themselves. Graph errors degrade to
/// partial results — never panic.
fn impacted_closure(graph: &DependencyGraph, seeds: &[NodeId]) -> Vec<NodeId> {
    let seed_set: BTreeSet<NodeId> = seeds.iter().copied().collect();
    let mut seen: BTreeSet<NodeId> = seed_set.clone();
    let mut queue: Vec<NodeId> = seeds.to_vec();
    let mut impacted: BTreeSet<NodeId> = BTreeSet::new();
    while let Some(current) = queue.pop() {
        let edges = match graph.traverse_to(current, None) {
            Ok(edges) => edges,
            Err(_) => continue,
        };
        for (from_id, kind) in edges {
            if kind == EdgeKind::MemberOf {
                continue;
            }
            if seen.insert(from_id) {
                impacted.insert(from_id);
                queue.push(from_id);
            }
        }
    }
    impacted
        .into_iter()
        .filter(|id| !seed_set.contains(id))
        .collect()
}

/// Graph-derived risk factors (analyze step vi): fan-in/fan-out hubs,
/// public-API exposure, and prior anti-pattern hits — all read from the
/// same locked snapshot. Anti-pattern hits carry the generic wide-impact
/// kind (`FanOut`) with weight `min(hits, 3)`; there is no dedicated
/// AntiPattern variant in RiskFactorKind.
fn graph_risk_factors(
    graph: &DependencyGraph,
    target_nodes: &[crate::graph::CodeArtifactNode],
) -> Vec<RiskFactor> {
    use crate::graph::ArtifactKind;

    let mut factors: Vec<RiskFactor> = Vec::new();

    // Fan-in hub: >= threshold dependents.
    for node in target_nodes {
        if let Ok(count) = graph.store().dependents_count(node.id) {
            if count >= GRAPH_HUB_DEPENDENTS_THRESHOLD {
                factors.push(RiskFactor {
                    kind: RiskFactorKind::FanOut,
                    evidence: format!("fan-in {} dependents on {}", count, node.fqcn),
                    weight: count.min(5) as u8,
                });
            }
        }
    }

    // Fan-out hub: >= threshold outgoing non-MemberOf edges.
    for node in target_nodes {
        let fan_out = graph
            .traverse_edges(node.id, None)
            .map(|edges| {
                edges
                    .iter()
                    .filter(|(_, kind)| *kind != EdgeKind::MemberOf)
                    .count()
            })
            .unwrap_or(0);
        if fan_out >= GRAPH_HUB_DEPENDENTS_THRESHOLD {
            factors.push(RiskFactor {
                kind: RiskFactorKind::FanOut,
                evidence: format!("fan-out {} dependencies from {}", fan_out, node.fqcn),
                weight: fan_out.min(5) as u8,
            });
        }
    }

    // Public-API exposure: an interface target, or one implementing
    // interfaces.
    for node in target_nodes {
        if node.kind == ArtifactKind::Interface || !node.implemented_interfaces.is_empty() {
            factors.push(RiskFactor {
                kind: RiskFactorKind::PublicApiExposure,
                evidence: format!("{} is public API surface", node.fqcn),
                weight: 1,
            });
        }
    }

    // Prior anti-pattern hits attached to the targets.
    let ids: Vec<NodeId> = target_nodes.iter().map(|n| n.id).collect();
    if let Ok(anti_patterns) = graph.store().anti_patterns_for_artifacts(&ids) {
        if !anti_patterns.is_empty() {
            factors.push(RiskFactor {
                kind: RiskFactorKind::FanOut,
                evidence: format!(
                    "{} learned anti-pattern(s) attached to targets",
                    anti_patterns.len()
                ),
                weight: anti_patterns.len().min(3) as u8,
            });
        }
    }

    factors
}

/// Distinct, non-empty first path components (forward slashes).
fn distinct_first_components(source_paths: &[String]) -> Vec<String> {
    let mut modules: Vec<String> = Vec::new();
    for path in source_paths {
        if let Some(first) = path.split('/').find(|s| !s.is_empty()) {
            if !modules.iter().any(|m| m == first) {
                modules.push(first.to_string());
            }
        }
    }
    modules
}

/// Modules of a path list: first path components, forward slashes.
fn modules_of_paths(paths: &[PathBuf]) -> Vec<String> {
    let strings: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();
    distinct_first_components(&strings)
}

/// Modules from the impacted + target closure: re-resolves each NodeId's
/// source_path under one fresh lock on the shared graph.
fn modules_of_impacted(
    graph: &Option<Arc<Mutex<DependencyGraph>>>,
    targets: &[NodeId],
    impacted: &[NodeId],
) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    if let Some(graph) = graph {
        if let Ok(graph) = graph.lock() {
            for id in targets.iter().chain(impacted.iter()) {
                if let Ok(Some(node)) = graph.store().get_node(*id) {
                    paths.push(node.source_path);
                }
            }
        }
    }
    distinct_first_components(&paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classifier::SignalKind;
    use crate::graph::{ArtifactKind, CodeArtifactNode};
    use std::fs;
    use std::path::Path;

    fn make_request(root: &Path, text: &str, active_file: Option<&str>) -> CodingRequest {
        CodingRequest {
            text: text.to_string(),
            active_file: active_file.map(|s| s.to_string()),
            active_symbols: vec![],
            project_root: root.to_path_buf(),
            token_budget_hint: 0,
            scope_files: vec![],
        }
    }

    fn make_engine(root: &Path) -> (AnalysisEngine, Arc<Mutex<DependencyGraph>>) {
        let graph = DependencyGraph::open_in_memory().unwrap();
        let shared = Arc::new(Mutex::new(graph));
        let engine = AnalysisEngine::new(
            ComplexityClassifier::default().with_graph(shared.clone()),
            Some(shared.clone()),
            root.to_path_buf(),
            &NeuroCodeConfig::default(),
        );
        (engine, shared)
    }

    /// Hub node (Class, com.ex.Hub, src/Hub.java) + 4 clients
    /// Injects→hub. Returns the hub's NodeId.
    fn seed_hub_graph(graph: &DependencyGraph) -> NodeId {
        let hub_id = graph
            .upsert_node(&CodeArtifactNode::new(
                ArtifactKind::Class,
                "com.ex.Hub".into(),
                "com.ex".into(),
                "src/Hub.java".into(),
            ))
            .unwrap();
        for i in 0..4 {
            let client_id = graph
                .upsert_node(&CodeArtifactNode::new(
                    ArtifactKind::Class,
                    format!("com.ex.Client{}", i),
                    "com.ex".into(),
                    format!("src/Client{}.java", i),
                ))
                .unwrap();
            graph
                .upsert_edge(client_id, hub_id, EdgeKind::Injects)
                .unwrap();
        }
        hub_id
    }

    #[test]
    fn analyze_populates_all_fields() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("AGENTS.md"), "- Run cargo fmt\n").unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/Hub.java"), "public class Hub {}\n").unwrap();

        let (engine, shared) = make_engine(root);
        let hub_id = {
            let graph = shared.lock().unwrap();
            seed_hub_graph(&graph)
        };

        let analysis =
            engine.analyze(&make_request(root, "refactor the hub service", Some("src/Hub.java")));

        assert_eq!(analysis.revision, "unknown"); // tempdir is not a git repo
        assert_eq!(analysis.target_artifacts, vec![hub_id]);
        assert_eq!(analysis.impacted_artifacts.len(), 4); // the 4 clients
        assert!(!analysis.effective_policies.is_empty());
        assert!(
            analysis
                .effective_policies
                .iter()
                .any(|b| b.directive == "Run cargo fmt")
        );
        assert_eq!(analysis.complexity.tier, ComplexityTier::Frontier);
        assert!(analysis
            .complexity
            .signals
            .iter()
            .any(|s| s.kind == SignalKind::GraphHub));
        assert!(!analysis.risk.factors.is_empty()); // fan-in 4 ⇒ FanOut factor
        assert_eq!(analysis.model_tier, ComplexityTier::Frontier);
        assert!(analysis.execution_hint.independent_components >= 1); // "src"
        // Base verification unconfigured (Default config) ⇒ empty steps;
        // review flag reflects the assessed risk level.
        assert!(analysis.verification.steps.is_empty());
        assert_eq!(
            analysis.verification.risk_triggered_review,
            analysis.risk.level == RiskLevel::High
        );
    }

    #[test]
    fn analyze_without_graph_degrades() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let engine = AnalysisEngine::new(
            ComplexityClassifier::default(),
            None,
            root.to_path_buf(),
            &NeuroCodeConfig::default(),
        );
        let analysis =
            engine.analyze(&make_request(root, "refactor the hub service", Some("src/Hub.java")));
        assert!(analysis.target_artifacts.is_empty());
        assert!(analysis.impacted_artifacts.is_empty());
        assert!(analysis.effective_policies.is_empty());
        // Complexity from keywords only — no GraphHub signals.
        assert!(analysis.complexity.signals.iter().all(|s| s.kind
            == SignalKind::Keyword
            || s.kind == SignalKind::ScopeFanOut));
        assert_eq!(analysis.model_tier, ComplexityTier::Frontier);
    }

    /// Bug: `analyze` hardcoded `resolve_ambiguous(Economical)` while
    /// `engine.rs` honored `config.ambiguous_default_tier()`. The analysis
    /// plane must resolve AmbiguousDefault to the CONFIGURED default tier.
    #[test]
    fn analyze_resolves_ambiguous_default_to_configured_tier() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        // Neutral request text → AmbiguousDefault classification.
        let mut cfg = NeuroCodeConfig::default();
        cfg.tier.ambiguous_default = "frontier".to_string();
        let engine = AnalysisEngine::new(
            ComplexityClassifier::default(),
            None,
            root.to_path_buf(),
            &cfg,
        );
        let analysis = engine.analyze(&make_request(root, "help me with this code", None));
        assert_eq!(
            analysis.complexity.tier,
            ComplexityTier::AmbiguousDefault,
            "neutral request must classify as AmbiguousDefault"
        );
        assert_eq!(
            analysis.model_tier, ComplexityTier::Frontier,
            "AmbiguousDefault must resolve to the configured frontier default"
        );

        // Default config still resolves to Economical.
        let engine_eco = AnalysisEngine::new(
            ComplexityClassifier::default(),
            None,
            root.to_path_buf(),
            &NeuroCodeConfig::default(),
        );
        let analysis_eco =
            engine_eco.analyze(&make_request(root, "help me with this code", None));
        assert_eq!(
            analysis_eco.model_tier, ComplexityTier::Economical,
            "default config resolves AmbiguousDefault to Economical"
        );
    }

    #[test]
    fn context_for_surfaces_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("AGENTS.md"),
            "- Always use raw SQL in migrations\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(
            root.join("sub/AGENTS.md"),
            "- Never use raw SQL in migrations\n",
        )
        .unwrap();

        let (engine, _shared) = make_engine(root);
        let task = AnalysisTask {
            id: "t1".to_string(),
            objective: "migrate the schema".to_string(),
            dependencies: vec![],
            read_set: vec![],
            write_set: vec![PathBuf::from("sub/x.rs")],
            risk: RiskLevel::Low,
            verification: VerificationPlan::default(),
        };
        let context = engine.context_for(&task);
        assert_eq!(context.task_id, "t1");
        assert_eq!(context.effective_directives.len(), 2);
        assert!(context.effective_directives.contains(&"Always use raw SQL in migrations".to_string()));
        assert!(context.effective_directives.contains(&"Never use raw SQL in migrations".to_string()));
        // Polarity differs on the shared subject ("sql"/"migrations").
        assert!(context.policy_conflicts.len() >= 1);
    }

    #[test]
    fn verification_for_high_risk_sets_review() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, _shared) = make_engine(dir.path());
        let task = |risk: RiskLevel| AnalysisTask {
            id: "t1".to_string(),
            objective: "o".to_string(),
            dependencies: vec![],
            read_set: vec![],
            write_set: vec![PathBuf::from("src/a.rs")],
            risk,
            verification: VerificationPlan::default(),
        };
        assert!(engine.verification_for(&task(RiskLevel::High)).risk_triggered_review);
        assert!(!engine.verification_for(&task(RiskLevel::Low)).risk_triggered_review);
    }

    /// An engine with configured verify steps but NO resolvable modules
    /// (no active_file, empty graph targets) must fall back to the
    /// UNSCOPED base plan — `scoped(&[])` would drop every step.
    #[test]
    fn analyze_with_no_resolvable_modules_keeps_base_steps() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut config = NeuroCodeConfig::default();
        config.verify.steps = vec![crate::config::VerifyStepConfig {
            name: "core-tests".to_string(),
            command: "cargo test -p joey-core".to_string(),
            parse: "plain".to_string(),
            timeout_sec: 120,
        }];
        let (mut engine, _shared) = make_engine(root);
        // Rebuild with the step config (make_engine uses Default config).
        engine = AnalysisEngine::new(
            ComplexityClassifier::default().with_graph(Arc::new(Mutex::new(
                DependencyGraph::open_in_memory().unwrap(),
            ))),
            None,
            root.to_path_buf(),
            &config,
        );

        // No active_file and no graph → no modules resolvable.
        let analysis = engine.analyze(&make_request(root, "refactor something", None));
        assert_eq!(analysis.verification.steps.len(), 1, "unscoped base plan keeps its steps");
        assert_eq!(analysis.verification.steps[0].name, "core-tests");
    }

    /// Engine-level criteria flow into built plans (analyze + verification_for),
    /// appended additively; an empty store leaves plans unchanged.
    #[test]
    fn acceptance_criteria_store_flows_into_built_plans() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let mut config = NeuroCodeConfig::default();
        config.verify.steps = vec![crate::config::VerifyStepConfig {
            name: "core-tests".to_string(),
            command: "cargo test -p joey-core".to_string(),
            parse: "plain".to_string(),
            timeout_sec: 120,
        }];
        let graph = Arc::new(Mutex::new(DependencyGraph::open_in_memory().unwrap()));
        let mut engine = AnalysisEngine::new(
            ComplexityClassifier::default().with_graph(graph.clone()),
            Some(graph),
            root.to_path_buf(),
            &config,
        );

        // Empty store (default): plan unchanged — no criteria, base steps.
        assert!(engine.acceptance_criteria().is_empty());
        let analysis = engine.analyze(&make_request(root, "refactor the hub service", None));
        assert!(analysis.verification.acceptance_criteria.is_empty());
        assert_eq!(analysis.verification.steps.len(), 1);

        // Stored criteria flow into the analyzed plan.
        engine.set_acceptance_criteria(vec![
            "given a scoped change".to_string(),
            "then criteria ride along".to_string(),
        ]);
        assert_eq!(engine.acceptance_criteria().len(), 2);
        let analysis = engine.analyze(&make_request(root, "refactor the hub service", None));
        assert_eq!(
            analysis.verification.acceptance_criteria,
            vec![
                "given a scoped change".to_string(),
                "then criteria ride along".to_string(),
            ]
        );

        // ...and into verification_for plans as well.
        let task = AnalysisTask {
            id: "t1".to_string(),
            objective: "o".to_string(),
            dependencies: vec![],
            read_set: vec![],
            write_set: vec![PathBuf::from("src/a.rs")],
            risk: RiskLevel::Low,
            verification: VerificationPlan::default(),
        };
        let plan = engine.verification_for(&task);
        assert_eq!(plan.acceptance_criteria.len(), 2);
    }

    #[test]
    fn record_outcome_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let (engine, _shared) = make_engine(dir.path());
        let task = AnalysisTask {
            id: "t9".to_string(),
            objective: "fix the NPE".to_string(),
            dependencies: vec![],
            read_set: vec![],
            write_set: vec![PathBuf::from("src/hub.rs")],
            risk: RiskLevel::Medium,
            verification: VerificationPlan::default(),
        };
        let outcome = || VerifiedOutcome {
            task_signature: task_signature(&task),
            repository_revision: "rev1".to_string(),
            artifact_ids: vec![5],
            policy_ids: vec!["p".to_string()],
            failure_signature: Some("NPE at Hub".to_string()),
            resolution: Some("guard the lookup".to_string()),
            evidence_ids: vec!["e1".to_string()],
            confidence: 90,
        };
        engine.record_outcome(&outcome());
        engine.record_outcome(&outcome());
        // The buffer's consult returns ALL matching lessons.
        let context = engine.context_for(&task);
        assert!(context.lessons.len() >= 1);
        let lesson = &context.lessons[0];
        assert_eq!(lesson.repository_revision, "rev1");
        assert_eq!(lesson.artifact_ids, vec![5]);
        assert_eq!(lesson.failure_signature.as_deref(), Some("NPE at Hub"));
        assert_eq!(lesson.resolution.as_deref(), Some("guard the lookup"));
        assert_eq!(lesson.confidence, 90);
        assert_eq!(lesson.hit_count, 0);
        assert!(!lesson.last_confirmed_at.is_empty());
    }

    /// T028 (US7): record_outcome writes through to an attached SQLite
    /// outcome store (exactly one row carrying the outcome's signature).
    #[test]
    fn attached_store_receives_record_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let (mut engine, _shared) = make_engine(dir.path());
        let store = Arc::new(Mutex::new(
            crate::memory::outcomes::OutcomeStore::open_in_memory().unwrap(),
        ));
        engine.attach_outcome_store(store.clone());
        let outcome = VerifiedOutcome {
            task_signature: "t1|fix the NPE|src/hub.rs".to_string(),
            repository_revision: "rev1".to_string(),
            artifact_ids: vec![5],
            policy_ids: vec!["p".to_string()],
            failure_signature: Some("NPE at Hub".to_string()),
            resolution: Some("guard the lookup".to_string()),
            evidence_ids: vec!["e1".to_string()],
            confidence: 90,
        };
        engine.record_outcome(&outcome);
        let rows = store.lock().unwrap().all_rows().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].task_signature, "t1|fix the NPE|src/hub.rs");
    }

    /// T028 (US7): context_for surfaces lessons recorded in an attached
    /// store whose signature matches the task (TaskContext's lesson-bearing
    /// field is `lessons: Vec<OutcomeMemory>`).
    #[test]
    fn context_for_surfaces_store_lesson() {
        let dir = tempfile::tempdir().unwrap();
        let (mut engine, _shared) = make_engine(dir.path());
        let store = Arc::new(Mutex::new(
            crate::memory::outcomes::OutcomeStore::open_in_memory().unwrap(),
        ));
        let task = AnalysisTask {
            id: "t1".to_string(),
            objective: "migrate the schema".to_string(),
            dependencies: vec![],
            read_set: vec![],
            write_set: vec![PathBuf::from("sub/x.rs")],
            risk: RiskLevel::Low,
            verification: VerificationPlan::default(),
        };
        // Record a lesson into the attached store with the task's signature.
        store
            .lock()
            .unwrap()
            .record(&VerifiedOutcome {
                task_signature: task_signature(&task),
                repository_revision: "rev1".to_string(),
                artifact_ids: vec![5],
                policy_ids: vec![],
                failure_signature: Some("NPE at Hub".to_string()),
                resolution: Some("guard the lookup".to_string()),
                evidence_ids: vec!["e1".to_string()],
                confidence: 90,
            })
            .unwrap();
        engine.attach_outcome_store(store);
        let context = engine.context_for(&task);
        assert!(context.lessons.len() >= 1);
        assert!(
            context
                .lessons
                .iter()
                .any(|l| l.resolution.as_deref() == Some("guard the lookup"))
        );
    }

    /// T035 (FR-026/SC-008): context_for expires attached-store lessons
    /// whose referenced graph artifact changed materially (future-mtime
    /// file) before the consult, while unchanged lessons still surface.
    /// Uses the real path: a seeded in-memory graph (`upsert_node`) whose
    /// `source_path` is project-relative, joined with project_root.
    #[test]
    fn context_for_expires_changed_artifact_lessons() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();

        // One materially-changed file (future mtime), one unchanged (past).
        let set_mtime = |rel: &str, mtime: std::time::SystemTime| {
            let p = root.join(rel);
            fs::write(&p, "x\n").unwrap();
            let mut f = std::fs::File::options().write(true).open(&p).unwrap();
            f.set_modified(mtime).unwrap();
        };
        let now = std::time::SystemTime::now();
        set_mtime("src/Changed.java", now + std::time::Duration::from_secs(3600));
        set_mtime("src/Stable.java", now - std::time::Duration::from_secs(3600));

        let (mut engine, shared) = make_engine(root);
        let (changed_id, stable_id) = {
            let graph = shared.lock().unwrap();
            let changed_id = graph
                .upsert_node(&CodeArtifactNode::new(
                    ArtifactKind::Class,
                    "com.ex.Changed".into(),
                    "com.ex".into(),
                    "src/Changed.java".into(),
                ))
                .unwrap();
            let stable_id = graph
                .upsert_node(&CodeArtifactNode::new(
                    ArtifactKind::Class,
                    "com.ex.Stable".into(),
                    "com.ex".into(),
                    "src/Stable.java".into(),
                ))
                .unwrap();
            (changed_id, stable_id)
        };

        let store = Arc::new(Mutex::new(
            crate::memory::outcomes::OutcomeStore::open_in_memory().unwrap(),
        ));
        let task = AnalysisTask {
            id: "t1".to_string(),
            objective: "migrate the schema".to_string(),
            dependencies: vec![],
            read_set: vec![],
            write_set: vec![PathBuf::from("sub/x.rs")],
            risk: RiskLevel::Low,
            verification: VerificationPlan::default(),
        };
        let lesson = |artifact: u64| VerifiedOutcome {
            task_signature: task_signature(&task),
            repository_revision: "rev1".to_string(),
            artifact_ids: vec![artifact],
            policy_ids: vec![],
            failure_signature: None,
            resolution: Some(format!("lesson for artifact {artifact}")),
            evidence_ids: vec![],
            confidence: 90,
        };
        {
            let mut guard = store.lock().unwrap();
            guard.record(&lesson(changed_id)).unwrap();
            guard.record(&lesson(stable_id)).unwrap();
        }
        engine.attach_outcome_store(store);

        let context = engine.context_for(&task);
        // The changed artifact's lesson is expired pre-consult; the stable
        // artifact's lesson surfaces.
        assert_eq!(context.lessons.len(), 1);
        assert_eq!(
            context.lessons[0].resolution.as_deref(),
            Some(&*format!("lesson for artifact {stable_id}"))
        );
    }

    #[test]
    fn serde_round_trips_analysis_types_snake_case() {
        // ExecutionHint keys.
        let hint = ExecutionHint {
            write_overlap: true,
            strict_dependency_depth: 3,
            independent_components: 2,
            cross_component_coordination: true,
        };
        let json = serde_json::to_string(&hint).unwrap();
        assert!(json.contains("\"write_overlap\""));
        assert!(json.contains("\"strict_dependency_depth\""));
        assert!(json.contains("\"independent_components\""));
        assert!(json.contains("\"cross_component_coordination\""));
        let back: ExecutionHint = serde_json::from_str(&json).unwrap();
        assert_eq!(back, hint);

        // TaskContext keys.
        let context = TaskContext {
            task_id: "t1".to_string(),
            objective: "o".to_string(),
            effective_directives: vec!["d".to_string()],
            policy_conflicts: vec![],
            impacted_artifacts: vec![1, 2],
            modules: vec!["src".to_string()],
            lessons: vec![],
        };
        let json = serde_json::to_string(&context).unwrap();
        assert!(json.contains("\"task_id\""));
        assert!(json.contains("\"effective_directives\""));
        assert!(json.contains("\"policy_conflicts\""));
        assert!(json.contains("\"impacted_artifacts\""));
        assert!(json.contains("\"lessons\""));
        let back: TaskContext = serde_json::from_str(&json).unwrap();
        assert_eq!(back, context);

        // TaskAnalysis keys.
        let analysis = TaskAnalysis {
            revision: "r".to_string(),
            target_artifacts: vec![1],
            impacted_artifacts: vec![2, 3],
            effective_policies: vec![],
            policy_conflicts: vec![],
            complexity: ComplexityRoute {
                tier: ComplexityTier::Frontier,
                reasoning: "kw".to_string(),
                overridden: false,
                override_tier: None,
                signals: vec![],
            },
            risk: RiskAssessment::default(),
            model_tier: ComplexityTier::Frontier,
            execution_hint: hint,
            verification: VerificationPlan {
                steps: vec![],
                risk_triggered_review: true,
                acceptance_criteria: Vec::new(),
            },
        };
        let json = serde_json::to_string(&analysis).unwrap();
        assert!(json.contains("\"model_tier\""));
        assert!(json.contains("\"risk_triggered_review\""));
        assert!(json.contains("\"execution_hint\""));
        assert!(json.contains("\"target_artifacts\""));
        let back: TaskAnalysis = serde_json::from_str(&json).unwrap();
        assert_eq!(back, analysis);
    }
}

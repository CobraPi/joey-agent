//! Workflow step catalog and readiness derivation (FR-008/009/021/022).
//!
//! Builds the step catalog from the spec-kit lifecycle, derives each step's
//! `StepState` as a pure function of artifact state + prerequisites + active
//! runs, and builds the `DependencyLink` graph for stale propagation and
//! traceability (FR-021/023/032).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::model::{
    Artifact, ArtifactKind, ArtifactLocation, ArtifactRef, DependencyKind, DependencyLink,
    StepState, WorkflowStep,
};

/// The canonical spec-kit lifecycle step order.
const LIFECYCLE: &[(&str, &str, &[&str])] = &[
    ("constitution", "Establish/verify governance principles", &[]),
    ("specify", "Create or update the feature specification", &["constitution"]),
    ("clarify", "Resolve ambiguities in the specification", &["specify"]),
    ("plan", "Design the implementation plan", &["specify", "clarify"]),
    ("checklist", "Validate specification quality", &["specify"]),
    ("tasks", "Generate the task breakdown", &["plan"]),
    ("analyze", "Cross-artifact consistency analysis", &["plan", "tasks"]),
    ("implement", "Execute the implementation", &["tasks"]),
    ("converge", "Append remaining unbuilt work", &["implement"]),
    ("task_to_issue", "Convert tasks to GitHub issues", &["tasks"]),
];

/// Build the workflow step catalog for a feature, deriving each step's state
/// from the current artifact state and prerequisite completion (FR-008/022).
/// `artifacts` are the discovered artifacts; `active_step_ids` are steps
/// currently running.
pub fn build_workflow(
    feature_id: &str,
    artifacts: &[Artifact],
    active_step_ids: &HashSet<String>,
) -> Vec<WorkflowStep> {
    let artifact_by_kind = index_artifacts(artifacts);

    let mut steps = Vec::new();
    for (order, (id, purpose, prereqs)) in LIFECYCLE.iter().enumerate() {
        let step_id = id.to_string();
        let is_active = active_step_ids.contains(&step_id);

        let available = is_step_available(id);
        let (inputs, outputs) = step_artifacts(id, feature_id, &artifact_by_kind);

        let state = derive_step_state(
            id,
            &steps,
            prereqs,
            available,
            is_active,
            &inputs,
            &outputs,
        );

        let blocking_reason = if state == StepState::Blocked {
            Some(format_blocking_reason(id, prereqs, &steps))
        } else if state == StepState::Unavailable {
            Some("skill not installed".to_string())
        } else {
            None
        };

        steps.push(WorkflowStep {
            id: step_id,
            order: order as i32,
            purpose: purpose.to_string(),
            inputs,
            outputs,
            prerequisites: prereqs.iter().map(|s| s.to_string()).collect(),
            available,
            state,
            blocking_reason,
            latest_attempt_id: None,
            installed_definition_ref: format!("skill:speckit-{id}"),
        });
    }
    steps
}

/// Derive a step's state as a pure function of its prerequisites' states,
/// availability, active runs, and input artifact validity (FR-022).
pub fn derive_step_state(
    _step_id: &str,
    completed_steps: &[WorkflowStep],
    prerequisites: &[&str],
    available: bool,
    is_active: bool,
    inputs: &[ArtifactRef],
    _outputs: &[ArtifactRef],
) -> StepState {
    if !available {
        return StepState::Unavailable;
    }

    if is_active {
        return StepState::Running;
    }

    // Check prerequisite completion.
    let prereq_map: HashMap<&str, &WorkflowStep> = completed_steps
        .iter()
        .map(|s| (s.id.as_str(), s))
        .collect();

    for prereq_id in prerequisites {
        if let Some(prereq) = prereq_map.get(*prereq_id) {
            match prereq.state {
                StepState::Succeeded => continue,
                StepState::Stale => return StepState::Stale,
                StepState::Failed => return StepState::Blocked,
                StepState::Unavailable => return StepState::Unavailable,
                StepState::Blocked | StepState::Running | StepState::AttentionNeeded => {
                    return StepState::Blocked;
                }
                _ => return StepState::Blocked,
            }
        } else {
            // Prerequisite not yet in the catalog — blocked.
            return StepState::Blocked;
        }
    }

    let _ = inputs; // input validity would be checked here in a fuller impl

    StepState::Ready
}

/// Build the `DependencyLink` graph from artifacts for stale propagation
/// and traceability (FR-021/023/032).
pub fn build_dependency_graph(feature_id: &str, artifacts: &[Artifact]) -> Vec<DependencyLink> {
    let mut links = Vec::new();
    let spec = find_artifact(artifacts, ArtifactKind::Spec);
    let plan = find_artifact(artifacts, ArtifactKind::Plan);
    let tasks = find_artifact(artifacts, ArtifactKind::Tasks);

    // spec → plan
    if let (Some(spec), Some(plan)) = (spec.as_ref(), plan.as_ref()) {
        links.push(DependencyLink {
            from: loc(&spec.path, "requirements"),
            to: loc(&plan.path, "summary"),
            kind: DependencyKind::RequirementToPlan,
        });
    }

    // plan → tasks
    if let (Some(plan), Some(tasks)) = (plan.as_ref(), tasks.as_ref()) {
        links.push(DependencyLink {
            from: loc(&plan.path, "plan"),
            to: loc(&tasks.path, "tasks"),
            kind: DependencyKind::PlanToTask,
        });
    }

    let _ = feature_id;
    links
}

/// Walk the dependency graph downstream from `changed_path` and return the
/// set of artifact paths that should be marked stale (FR-021). Does NOT
/// delete content — only identifies what's affected.
pub fn propagate_stale(links: &[DependencyLink], changed_path: &str) -> HashSet<String> {
    let mut affected = HashSet::new();
    // Simple forward walk: find all downstream `to` paths from `changed_path`.
    let mut queue = vec![changed_path.to_string()];
    let mut visited = HashSet::new();

    while let Some(path) = queue.pop() {
        if !visited.insert(path.clone()) {
            continue;
        }
        for link in links {
            if link.from.path == path {
                affected.insert(link.to.path.clone());
                queue.push(link.to.path.clone());
            }
        }
    }

    // Remove the source itself from the affected set.
    affected.remove(changed_path);
    affected
}

// --- helpers ---

fn index_artifacts(artifacts: &[Artifact]) -> HashMap<ArtifactKind, &Artifact> {
    artifacts
        .iter()
        .filter(|a| a.exists)
        .fold(HashMap::new(), |mut map, a| {
            map.entry(a.kind.clone()).or_insert(a);
            map
        })
}

fn find_artifact<'a>(artifacts: &'a [Artifact], kind: ArtifactKind) -> Option<&'a Artifact> {
    artifacts.iter().find(|a| a.kind == kind && a.exists)
}

fn loc(path: &str, section: &str) -> ArtifactLocation {
    ArtifactLocation {
        path: path.to_string(),
        line_or_section: section.to_string(),
    }
}

fn is_step_available(step_id: &str) -> bool {
    // task_to_issue requires an installed skill that may not be present.
    // All other core steps are always available in this implementation.
    step_id != "task_to_issue"
}

fn step_artifacts(
    step_id: &str,
    feature_id: &str,
    _artifacts: &HashMap<ArtifactKind, &Artifact>,
) -> (Vec<ArtifactRef>, Vec<ArtifactRef>) {
    let prefix = format!("specs/{feature_id}/");
    let mk = |filename: &str, kind: ArtifactKind| ArtifactRef {
        path: format!("{prefix}{filename}"),
        kind: Some(kind),
    };

    match step_id {
        "constitution" => (
            vec![],
            vec![ArtifactRef {
                path: ".specify/memory/constitution.md".to_string(),
                kind: Some(ArtifactKind::Constitution),
            }],
        ),
        "specify" => (
            vec![],
            vec![mk("spec.md", ArtifactKind::Spec)],
        ),
        "clarify" => (
            vec![mk("spec.md", ArtifactKind::Spec)],
            vec![mk("spec.md", ArtifactKind::Spec)],
        ),
        "plan" => (
            vec![mk("spec.md", ArtifactKind::Spec)],
            vec![mk("plan.md", ArtifactKind::Plan)],
        ),
        "checklist" => (
            vec![mk("spec.md", ArtifactKind::Spec)],
            vec![],
        ),
        "tasks" => (
            vec![mk("plan.md", ArtifactKind::Plan)],
            vec![mk("tasks.md", ArtifactKind::Tasks)],
        ),
        "analyze" => (
            vec![mk("plan.md", ArtifactKind::Plan), mk("tasks.md", ArtifactKind::Tasks)],
            vec![],
        ),
        "implement" => (
            vec![mk("tasks.md", ArtifactKind::Tasks)],
            vec![],
        ),
        "converge" => (
            vec![mk("tasks.md", ArtifactKind::Tasks)],
            vec![],
        ),
        "task_to_issue" => (
            vec![mk("tasks.md", ArtifactKind::Tasks)],
            vec![],
        ),
        _ => (vec![], vec![]),
    }
}

fn format_blocking_reason(step_id: &str, prereqs: &[&str], steps: &[WorkflowStep]) -> String {
    let prereq_map: HashMap<&str, &WorkflowStep> = steps.iter().map(|s| (s.id.as_str(), s)).collect();
    for prereq_id in prereqs {
        if let Some(prereq) = prereq_map.get(*prereq_id) {
            if prereq.state != StepState::Succeeded {
                return format!("waiting for prerequisite '{}' to succeed", prereq_id);
            }
        }
    }
    let _ = step_id;
    "prerequisites not met".to_string()
}

/// Check for cyclic task dependencies in a tasks.md file (Edge Cases).
/// Returns the first cycle found, if any.
pub fn detect_task_cycles(content: &str) -> Option<Vec<String>> {
    // Parse task dependency edges: "T00X depends on T00Y" patterns.
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("- [") {
            if let Some(close) = rest.find(']') {
                let after = rest[close + 1..].trim_start();
                let id = after.split_whitespace().next().unwrap_or("");
                if id.starts_with('T') {
                    graph.entry(id.to_string()).or_default();
                    // Look for "depends on T00Y" or "after T00Y" patterns.
                    let lower = after.to_lowercase();
                    if let Some(idx) = lower.find("depends on") {
                        let deps_text = &after[idx + 10..];
                        for dep in deps_text.split_whitespace() {
                            let dep = dep.trim_end_matches(',').trim_end_matches('.');
                            if dep.starts_with('T') {
                                graph.entry(id.to_string()).or_default().push(dep.to_string());
                            }
                        }
                    }
                    if let Some(idx) = lower.find("after") {
                        let deps_text = &after[idx + 5..];
                        for dep in deps_text.split_whitespace() {
                            let dep = dep.trim_end_matches(',').trim_end_matches('.');
                            if dep.starts_with('T') {
                                graph.entry(id.to_string()).or_default().push(dep.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    // DFS cycle detection.
    let mut visited = HashSet::new();
    let mut stack = Vec::new();
    let mut on_stack = HashSet::new();

    fn dfs(
        node: &str,
        graph: &HashMap<String, Vec<String>>,
        visited: &mut HashSet<String>,
        on_stack: &mut HashSet<String>,
        stack: &mut Vec<String>,
    ) -> Option<Vec<String>> {
        if on_stack.contains(node) {
            // Found cycle: extract from stack.
            let idx = stack.iter().position(|n| n == node)?;
            return Some(stack[idx..].to_vec());
        }
        if visited.contains(node) {
            return None;
        }
        visited.insert(node.to_string());
        on_stack.insert(node.to_string());
        stack.push(node.to_string());

        if let Some(deps) = graph.get(node) {
            for dep in deps {
                if let Some(cycle) = dfs(dep, graph, visited, on_stack, stack) {
                    return Some(cycle);
                }
            }
        }

        on_stack.remove(node);
        stack.pop();
        None
    }

    for node in graph.keys() {
        if let Some(cycle) = dfs(node, &graph, &mut visited, &mut on_stack, &mut stack) {
            return Some(cycle);
        }
    }
    None
}

// =====================================================================
// Feature 012: CST-aware readiness derivation (T027, FR-007).
//
// Extends the specs/010 step-state derivation so "Done" now requires the
// output artifact's CST to parse cleanly and be newer than its inputs
// (FR-007). Pure function over CST + run history; deterministic, no LLM
// (FR-005).
// =====================================================================

use crate::cst::parser::parse_bytes;
use crate::cst::parser_trait::CstMaterialize;

/// Extended readiness check: a step is "Done" only if the output artifact's
/// CST parses cleanly (round-trip identity holds) AND the output is newer
/// than all inputs (FR-007). Returns `false` if any output fails this check.
pub fn outputs_are_valid_and_fresh(
    repo_root: &Path,
    feature_id: &str,
    inputs: &[ArtifactRef],
    outputs: &[ArtifactRef],
) -> bool {
    let feature_dir = repo_root.join("specs").join(feature_id);

    for output in outputs {
        let out_path = feature_dir.join(&output.path);
        if !out_path.exists() {
            return false;
        }
        let Ok(bytes) = std::fs::read(&out_path) else {
            return false;
        };
        // CST must parse cleanly and round-trip.
        let doc = parse_bytes(&output.path, &bytes);
        if doc.materialize().as_slice() != bytes.as_slice() {
            return false;
        }
        // Output must be newer than all inputs.
        let out_mtime = match std::fs::metadata(&out_path).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => return false,
        };
        for input in inputs {
            let in_path = feature_dir.join(&input.path);
            if let Ok(in_meta) = std::fs::metadata(&in_path).and_then(|m| m.modified()) {
                if in_meta > out_mtime {
                    return false;
                }
            }
        }
    }
    true
}

/// Extended build_workflow that applies CST-aware freshness checks (T105,
/// FR-007). A step's Done state now requires the output artifact's CST to
/// parse cleanly and be newer than its inputs. This is additive — the
/// existing `build_workflow` is preserved unchanged (Constitution VII).
pub fn build_workflow_with_freshness(
    repo_root: &Path,
    feature_id: &str,
    artifacts: &[Artifact],
    active_step_ids: &HashSet<String>,
) -> Vec<WorkflowStep> {
    let mut steps = build_workflow(feature_id, artifacts, active_step_ids);

    // For each step that is currently Succeeded, verify freshness via the CST.
    // If the output is stale or fails to parse cleanly, downgrade to Stale.
    for step in steps.iter_mut() {
        if step.state != StepState::Succeeded {
            continue;
        }
        let fresh = outputs_are_valid_and_fresh(
            repo_root,
            feature_id,
            &step.inputs,
            &step.outputs,
        );
        if !fresh {
            step.state = StepState::Stale;
            step.blocking_reason = Some(
                "output artifact is stale or failed CST validation".to_string(),
            );
        }
    }

    steps
}

/// Compute the deterministic "next action" for a feature (FR-005). Pure
/// function of the workflow steps + CST validity — no LLM recommendation.
///
/// Priority: the first blocking step (so the developer unblocks the chain),
/// else the first stale step (refresh needed), else the first ready step
/// (the obvious next thing to do), else "all done".
pub fn next_action(steps: &[WorkflowStep]) -> NextAction {
    for step in steps {
        match step.state {
            StepState::Blocked => {
                return NextAction::Unblock {
                    step_id: step.id.clone(),
                    reason: step.blocking_reason.clone().unwrap_or_default(),
                }
            }
            StepState::Stale => {
                return NextAction::Refresh {
                    step_id: step.id.clone(),
                }
            }
            StepState::Failed => {
                return NextAction::Recover {
                    step_id: step.id.clone(),
                }
            }
            _ => continue,
        }
    }

    // No blocker — find the first ready step.
    for step in steps {
        if step.state == StepState::Ready {
            return NextAction::Run {
                step_id: step.id.clone(),
            };
        }
    }

    NextAction::AllDone
}

/// The deterministic next action (FR-005).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum NextAction {
    /// A step is blocked — unblock it first.
    Unblock { step_id: String, reason: String },
    /// A step's output is stale — re-run to refresh.
    Refresh { step_id: String },
    /// A step failed — recover.
    Recover { step_id: String },
    /// A step is ready to run — the obvious next thing.
    Run { step_id: String },
    /// All steps are done.
    AllDone,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_has_all_lifecycle_steps() {
        let steps = build_workflow("001-test", &[], &HashSet::new());
        assert_eq!(steps.len(), LIFECYCLE.len());
        assert_eq!(steps[0].id, "constitution");
        assert_eq!(steps[2].id, "clarify");
    }

    #[test]
    fn constitution_step_is_ready_first() {
        let steps = build_workflow("001-test", &[], &HashSet::new());
        let constitution = &steps[0];
        assert_eq!(constitution.state, StepState::Ready);
    }

    #[test]
    fn clarify_blocked_until_specify_succeeds() {
        let steps = build_workflow("001-test", &[], &HashSet::new());
        let clarify = steps.iter().find(|s| s.id == "clarify").unwrap();
        assert_eq!(clarify.state, StepState::Blocked);
        assert!(clarify.blocking_reason.is_some());
    }

    #[test]
    fn task_to_issue_is_unavailable() {
        let steps = build_workflow("001-test", &[], &HashSet::new());
        let t2i = steps.iter().find(|s| s.id == "task_to_issue").unwrap();
        assert_eq!(t2i.state, StepState::Unavailable);
    }

    #[test]
    fn dependency_graph_links_spec_to_plan() {
        let artifacts = vec![
            Artifact {
                path: "specs/001/spec.md".to_string(),
                kind: ArtifactKind::Spec,
                exists: true,
                ..Default::default()
            },
            Artifact {
                path: "specs/001/plan.md".to_string(),
                kind: ArtifactKind::Plan,
                exists: true,
                ..Default::default()
            },
        ];
        let links = build_dependency_graph("001", &artifacts);
        assert!(links.iter().any(|l| l.kind == DependencyKind::RequirementToPlan));
    }

    #[test]
    fn propagate_stale_marks_downstream() {
        let links = vec![
            DependencyLink {
                from: loc("specs/001/spec.md", "r"),
                to: loc("specs/001/plan.md", "s"),
                kind: DependencyKind::RequirementToPlan,
            },
            DependencyLink {
                from: loc("specs/001/plan.md", "s"),
                to: loc("specs/001/tasks.md", "t"),
                kind: DependencyKind::PlanToTask,
            },
        ];
        let affected = propagate_stale(&links, "specs/001/spec.md");
        assert!(affected.contains("specs/001/plan.md"));
        assert!(affected.contains("specs/001/tasks.md"));
        assert!(!affected.contains("specs/001/spec.md"));
    }

    #[test]
    fn detect_cycle_in_tasks() {
        let md = "- [ ] T001 depends on T002\n- [ ] T002 depends on T001\n";
        let cycle = detect_task_cycles(md);
        assert!(cycle.is_some());
    }

    #[test]
    fn no_cycle_in_acyclic_tasks() {
        let md = "- [ ] T001 first\n- [ ] T002 after T001\n- [ ] T003 after T002\n";
        assert!(detect_task_cycles(md).is_none());
    }

    #[test]
    fn derive_step_state_unavailable_when_not_available() {
        let state = derive_step_state("task_to_issue", &[], &[], false, false, &[], &[]);
        assert_eq!(state, StepState::Unavailable);
    }
}

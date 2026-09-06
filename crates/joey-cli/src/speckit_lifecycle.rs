//! Spec-Kit lifecycle state derivation (feature 026, T004 + T025).
//!
//! Contract: `specs/026-please-fully-integrate/contracts/lifecycle-state.md`.
//!
//! T004 — derive the current spec-kit lifecycle step as a pure function of
//! the on-disk artifacts (constitution III: `.specify/feature.json` points at
//! the active feature directory; the presence of `spec.md` / `plan.md` /
//! `tasks.md` plus an unchecked-checkbox scan determines the step) and render
//! the injected context block in the exact contract shape.
//!
//! T025 — adapt parsed `tasks.md` lines into orchestration
//! [`TaskNode`]s: `## Phase` headings become dependency tiers (a task
//! depends on every task in strictly earlier phases; same-phase tasks are
//! mutually independent so parallel-eligible `[P]` tasks fan out), and
//! same-phase write-set collisions are surfaced explicitly (edge case 4) so
//! the conductor can demote them to sequential instead of building a graph
//! `TaskGraph::validate` would reject (rule `concurrent_write_overlap`).

use std::path::{Path, PathBuf};

use joey_orchestration::evaluator::VerificationPlanView;
use joey_orchestration::task_graph::{
    AcceptanceCriterion, IsolationMode, ModelTier, RiskLevel, TaskId, TaskNode, TaskStatus,
    WorkerRole,
};
use joey_speckit_ui::parser::spec::parse_spec;
use joey_speckit_ui::parser::tasks::parse_tasks;

// ---------------------------------------------------------------------
// T004: lifecycle step + state derivation + context block
// ---------------------------------------------------------------------

/// One step of the spec-kit lifecycle, derived from disk artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleStep {
    /// No active feature selected.
    None,
    /// `spec.md` not yet authored.
    Specify,
    /// `spec.md` exists but open questions remain.
    Clarify,
    /// Spec is clarify-clean; `plan.md` not yet authored.
    Plan,
    /// `plan.md` exists; `tasks.md` not yet authored.
    Tasks,
    /// `tasks.md` exists with unchecked boxes.
    Implement,
    /// All tasks checked; final verification remains.
    Acceptance,
}

impl LifecycleStep {
    /// The step as it appears in the context block's `Step:` line.
    pub fn as_str(&self) -> &'static str {
        match self {
            LifecycleStep::None => "None",
            LifecycleStep::Specify => "Specify",
            LifecycleStep::Clarify => "Clarify",
            LifecycleStep::Plan => "Plan",
            LifecycleStep::Tasks => "Tasks",
            LifecycleStep::Implement => "Implement",
            LifecycleStep::Acceptance => "Acceptance",
        }
    }

    /// One-line guidance for the step (context block parenthetical).
    pub fn guidance(&self) -> &'static str {
        match self {
            LifecycleStep::None => {
                "no active feature — run /speckit-specify <description> or reselect a feature"
            }
            LifecycleStep::Specify => "author specs/<feature>/spec.md via /speckit-specify",
            LifecycleStep::Clarify => "resolve open questions then encode answers into the spec",
            LifecycleStep::Plan => "run /speckit-plan to design the implementation",
            LifecycleStep::Tasks => "run /speckit-tasks to decompose the plan",
            LifecycleStep::Implement => {
                "fan out implementors for unblocked tasks in parallel, exclusive write sets"
            }
            LifecycleStep::Acceptance => {
                "run exactly ONE final full-suite verification, then report"
            }
        }
    }
}

/// Derived lifecycle state for the active feature (pure function of disk).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleState {
    /// Value of `.specify/feature.json`'s `feature_directory` ("" when no
    /// active feature).
    pub feature_directory: String,
    /// Derived current step.
    pub step: LifecycleStep,
    /// `spec.md` exists in the feature directory.
    pub has_spec: bool,
    /// `plan.md` exists in the feature directory.
    pub has_plan: bool,
    /// `tasks.md` exists in the feature directory.
    pub has_tasks: bool,
    /// `feature.json` points at a directory that does not exist (edge
    /// case 5: reselect guidance).
    pub stale_pointer: bool,
}

impl Default for LifecycleState {
    fn default() -> Self {
        LifecycleState {
            feature_directory: String::new(),
            step: LifecycleStep::None,
            has_spec: false,
            has_plan: false,
            has_tasks: false,
            stale_pointer: false,
        }
    }
}

/// Marker scanned for in raw `spec.md` text to decide Clarify vs Plan.
const NEEDS_CLARIFICATION: &str = "[NEEDS CLARIFICATION]";

/// Derive the lifecycle state from the repository's on-disk artifacts.
///
/// Pure function of disk (constitution III): reads
/// `<repo_root>/.specify/feature.json` and existence/content of the feature
/// directory's `spec.md` / `plan.md` / `tasks.md`. Missing/invalid pointer
/// file → step `None`; pointer to a nonexistent directory → step `None`
/// with `stale_pointer = true`.
pub fn derive_state(repo_root: &Path) -> LifecycleState {
    // Active-feature pointer: .specify/feature.json → feature_directory.
    let feature_directory = std::fs::read_to_string(repo_root.join(".specify/feature.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| {
            v.get("feature_directory")
                .and_then(|f| f.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();

    if feature_directory.is_empty() {
        return LifecycleState::default();
    }

    let feature_dir = repo_root.join(&feature_directory);
    if !feature_dir.is_dir() {
        // Edge case 5: stale pointer → reselect guidance.
        return LifecycleState {
            feature_directory,
            step: LifecycleStep::None,
            has_spec: false,
            has_plan: false,
            has_tasks: false,
            stale_pointer: true,
        };
    }

    let spec_path = feature_dir.join("spec.md");
    let plan_path = feature_dir.join("plan.md");
    let tasks_path = feature_dir.join("tasks.md");

    let has_spec = spec_path.is_file();
    let has_plan = plan_path.is_file();
    let has_tasks = tasks_path.is_file();

    // Ordered rules, first match wins.
    let step = if !has_spec {
        LifecycleStep::Specify
    } else if !has_plan {
        let spec_raw = std::fs::read_to_string(&spec_path).unwrap_or_default();
        if spec_raw.contains(NEEDS_CLARIFICATION) {
            LifecycleStep::Clarify
        } else {
            LifecycleStep::Plan
        }
    } else if !has_tasks {
        LifecycleStep::Tasks
    } else if tasks_md_has_unchecked(
        &std::fs::read_to_string(&tasks_path).unwrap_or_default(),
    ) {
        LifecycleStep::Implement
    } else {
        LifecycleStep::Acceptance
    };

    LifecycleState {
        feature_directory,
        step,
        has_spec,
        has_plan,
        has_tasks,
        stale_pointer: false,
    }
}

/// True if any line of `tasks_md` is a top-level unchecked checkbox
/// (`- [ ]` after `trim_start`).
fn tasks_md_has_unchecked(tasks_md: &str) -> bool {
    tasks_md
        .lines()
        .any(|line| line.trim_start().starts_with("- [ ]"))
}

/// Render the injected context block in the exact contract shape
/// (contracts/lifecycle-state.md). Rendered once per session, pre-first-turn;
/// `/speckit-status` renders the same derivation on demand.
pub fn context_block(state: &LifecycleState) -> String {
    let feature = if state.feature_directory.is_empty() {
        "(none)"
    } else {
        state.feature_directory.as_str()
    };
    format!(
        "## Spec-Kit Lifecycle Context\n\
         Feature: {}\n\
         Step: {} ({})\n\
         Artifacts: spec.md {}, plan.md {}, tasks.md {}\n\
         Note: detected automatically; refresh by restarting the session or running /speckit-status.\n",
        feature,
        state.step.as_str(),
        state.step.guidance(),
        present(state.has_spec),
        present(state.has_plan),
        present(state.has_tasks),
    )
}

fn present(flag: bool) -> &'static str {
    if flag {
        "present"
    } else {
        "absent"
    }
}

// ---------------------------------------------------------------------
// T004 (scope): FeatureScope for conductor dispatch
// ---------------------------------------------------------------------

/// Files and criteria delimiting the active feature's blast radius.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeatureScope {
    /// Repo-relative paths the feature may read.
    pub read_files: Vec<String>,
    /// Repo-relative paths the feature may write (union of task target files).
    pub write_files: Vec<String>,
    /// Acceptance scenarios + success criteria from `spec.md` (deduped).
    pub acceptance_criteria: Vec<String>,
}

/// Compute the feature's read/write scope from disk.
///
/// Missing feature directory → all-empty scope. Otherwise: `write_files` is
/// the order-preserving deduped union of `parse_tasks` target files from
/// `tasks.md`; `read_files` adds the feature's `spec.md` / `plan.md` /
/// `tasks.md` / `research.md` paths that exist; `acceptance_criteria` is the
/// deduped union of the spec's user-story acceptance scenarios and success
/// criteria. Any parse/read failure skips that source gracefully.
pub fn feature_scope(repo_root: &Path, state: &LifecycleState) -> FeatureScope {
    if state.feature_directory.is_empty() {
        return FeatureScope::default();
    }
    let feature_dir = repo_root.join(&state.feature_directory);
    if !feature_dir.is_dir() {
        return FeatureScope::default();
    }

    // write_files: union of task target files (dedup, preserve order).
    let mut write_files: Vec<String> = Vec::new();
    if let Ok(tasks_md) = std::fs::read_to_string(feature_dir.join("tasks.md")) {
        for task in parse_tasks(&tasks_md) {
            push_dedup(&mut write_files, task.target_files.iter());
        }
    }

    // read_files: write_files ∪ existing feature artifact paths.
    let mut read_files: Vec<String> = write_files.clone();
    for artifact in ["spec.md", "plan.md", "tasks.md", "research.md"] {
        let rel = format!("{}/{}", state.feature_directory, artifact);
        if feature_dir.join(artifact).is_file() {
            push_dedup(&mut read_files, std::iter::once(&rel));
        }
    }

    // acceptance_criteria: spec acceptance scenarios + success criteria.
    let mut acceptance_criteria: Vec<String> = Vec::new();
    if let Ok(spec_md) = std::fs::read_to_string(feature_dir.join("spec.md")) {
        let spec = parse_spec(&spec_md);
        for story in &spec.user_stories {
            push_dedup(&mut acceptance_criteria, story.acceptance_scenarios.iter());
        }
        push_dedup(&mut acceptance_criteria, spec.success_criteria.iter());
    }

    FeatureScope {
        read_files,
        write_files,
        acceptance_criteria,
    }
}

/// Push items not already present, preserving first-seen order.
fn push_dedup<'a, I: IntoIterator<Item = &'a String>>(vec: &mut Vec<String>, items: I) {
    for item in items {
        if !vec.contains(item) {
            vec.push(item.clone());
        }
    }
}

// ---------------------------------------------------------------------
// T025: tasks.md → TaskNode adapter
// ---------------------------------------------------------------------

/// Two same-phase tasks that both write the same file (edge case 4).
///
/// Surfaced (rather than encoded as a dependency) so the conductor can
/// demote the pair to sequential dispatch; a graph containing the pair with
/// no sequencing edge would fail `TaskGraph::validate` with rule
/// `concurrent_write_overlap`.
// T025 adapter surface: consumed by the speckit_slash prepare-step wiring
// (task-graph embedding) and the unit tests below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFileCollision {
    /// First task's id as written in `tasks.md`.
    pub task_a: String,
    /// Second task's id as written in `tasks.md`.
    pub task_b: String,
    /// The shared target file.
    pub file: String,
}

/// Result of adapting `tasks.md` into orchestration task nodes.
#[derive(Debug, Clone, Default)]
pub struct TaskNodeBundle {
    /// Adapted nodes in document order.
    pub nodes: Vec<TaskNode>,
    /// Same-phase write-set collisions (empty when phases are exclusive).
    pub collisions: Vec<TaskFileCollision>,
}

/// Adapt parsed `tasks.md` content into orchestration [`TaskNode`]s.
///
/// Phase rule: `## Phase` headings partition the file into dependency tiers;
/// a task depends on ALL tasks under strictly earlier phase headings, and
/// tasks before any heading land in phase 0. Same-phase tasks (the
/// parallel-eligible `[P]` ones) get no mutual dependencies. Same-phase
/// pairs sharing a target file are reported in `collisions` instead of
/// being sequenced.
// Wired into speckit_slash::prepare_step_opts (T025 production wiring).
pub fn tasks_to_task_nodes(tasks_md: &str) -> TaskNodeBundle {
    let tasks = parse_tasks(tasks_md);
    let phases = task_line_phases(tasks_md);

    let entries: Vec<(usize, &joey_speckit_ui::model::Task)> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (phases.get(i).copied().unwrap_or(0), t))
        .collect();

    let mut nodes: Vec<TaskNode> = Vec::new();
    let mut collisions: Vec<TaskFileCollision> = Vec::new();

    for (idx, (phase, task)) in entries.iter().enumerate() {
        let id = sanitized_task_id(&task.id);

        // Depend on every task in strictly earlier phases (document order).
        let dependencies = entries
            .iter()
            .filter(|(p, _)| *p < *phase)
            .map(|(_, t)| sanitized_task_id(&t.id))
            .collect::<Vec<TaskId>>();

        let write_set: Vec<PathBuf> =
            task.target_files.iter().map(PathBuf::from).collect();
        let risk = if task.target_files.len() >= 3 {
            RiskLevel::Medium
        } else {
            RiskLevel::Low
        };

        nodes.push(TaskNode {
            id: id.clone(),
            objective: task.description.clone(),
            dependencies,
            read_set: write_set.clone(),
            write_set,
            artifact_ids: vec![],
            role: WorkerRole::Implementor,
            model_tier: ModelTier::Economical,
            risk,
            acceptance: vec![AcceptanceCriterion {
                criterion: task.description.clone(),
                kind: "description".to_string(),
            }],
            verification: VerificationPlanView::default(),
            isolation: IsolationMode::default(),
            status: TaskStatus::default(),
            attempts: 0,
        });

        // Same-phase write collisions (pairwise, one entry per shared file).
        for (other_phase, other) in entries.iter().take(idx) {
            if other_phase != phase {
                continue;
            }
            for file in &task.target_files {
                if other.target_files.contains(file) {
                    collisions.push(TaskFileCollision {
                        task_a: other.id.clone(),
                        task_b: task.id.clone(),
                        file: file.clone(),
                    });
                }
            }
        }
    }

    TaskNodeBundle { nodes, collisions }
}

/// Phase index per parsed task line (document order). Lines under the Nth
/// `## Phase` heading get phase N (1-based); lines before any heading get 0.
// Helper of [`tasks_to_task_nodes`].
#[allow(dead_code)]
fn task_line_phases(content: &str) -> Vec<usize> {
    let mut phases = Vec::new();
    let mut current = 0usize;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("## Phase") {
            current += 1;
        } else if is_task_line(trimmed) {
            phases.push(current);
        }
    }
    phases
}

/// Mirrors `parse_task_line`'s recognition: `- [` prefix plus a closing `]`.
// Helper of [`tasks_to_task_nodes`].
#[allow(dead_code)]
fn is_task_line(trimmed: &str) -> bool {
    match trimmed.strip_prefix("- [") {
        Some(rest) => rest.contains(']'),
        None => false,
    }
}

/// Lowercase a task id into `TaskId`'s `[a-z0-9-]` charset (e.g. `T001` → `t001`).
// Public shim over the internal helper so dispatch surfaces (speckit_slash
// T025 wiring) can map `tasks.md` ids onto node ids identically.
pub(crate) fn sanitized_task_id_pub(raw: &str) -> TaskId {
    sanitized_task_id(raw)
}

// Helper of [`tasks_to_task_nodes`].
fn sanitized_task_id(raw: &str) -> TaskId {
    let mut s = String::with_capacity(raw.len());
    for c in raw.chars() {
        let lower = c.to_ascii_lowercase();
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() || lower == '-' {
            s.push(lower);
        } else {
            s.push('-');
        }
    }
    TaskId::new(&s).unwrap_or_else(|| TaskId::from("unknown".to_string()))
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use joey_orchestration::task_graph::{rules, TaskGraph};

    fn write(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn activate(root: &Path, feature: &str) {
        write(
            root,
            ".specify/feature.json",
            &format!("{{\"feature_directory\":\"{feature}\"}}"),
        );
    }

    // ---- 1. derive_state truth table ---------------------------------

    #[test]
    fn no_feature_json_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let s = derive_state(tmp.path());
        assert_eq!(s.step, LifecycleStep::None);
        assert!(s.feature_directory.is_empty());
        assert!(!s.stale_pointer);
        assert!(!(s.has_spec || s.has_plan || s.has_tasks));
    }

    #[test]
    fn invalid_feature_json_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), ".specify/feature.json", "{\"bad");
        let s = derive_state(tmp.path());
        assert_eq!(s.step, LifecycleStep::None);
        assert!(s.feature_directory.is_empty());
        assert!(!s.stale_pointer);
    }

    #[test]
    fn stale_pointer_is_none_with_flag() {
        let tmp = tempfile::tempdir().unwrap();
        activate(tmp.path(), "specs/000-gone");
        let s = derive_state(tmp.path());
        assert_eq!(s.step, LifecycleStep::None);
        assert_eq!(s.feature_directory, "specs/000-gone");
        assert!(s.stale_pointer);
        assert!(!(s.has_spec || s.has_plan || s.has_tasks));
    }

    #[test]
    fn empty_feature_dir_is_specify() {
        let tmp = tempfile::tempdir().unwrap();
        activate(tmp.path(), "specs/001-demo");
        std::fs::create_dir_all(tmp.path().join("specs/001-demo")).unwrap();
        let s = derive_state(tmp.path());
        assert_eq!(s.step, LifecycleStep::Specify);
        assert!(!s.stale_pointer);
    }

    #[test]
    fn spec_with_open_questions_is_clarify() {
        let tmp = tempfile::tempdir().unwrap();
        activate(tmp.path(), "specs/001-demo");
        write(
            tmp.path(),
            "specs/001-demo/spec.md",
            "# Spec\n\n- Q [NEEDS CLARIFICATION]\n",
        );
        let s = derive_state(tmp.path());
        assert_eq!(s.step, LifecycleStep::Clarify);
        assert!(s.has_spec && !s.has_plan && !s.has_tasks);
    }

    #[test]
    fn clean_spec_is_plan() {
        let tmp = tempfile::tempdir().unwrap();
        activate(tmp.path(), "specs/001-demo");
        write(tmp.path(), "specs/001-demo/spec.md", "# Spec\n\nAll clear.\n");
        let s = derive_state(tmp.path());
        assert_eq!(s.step, LifecycleStep::Plan);
    }

    #[test]
    fn spec_and_plan_is_tasks() {
        let tmp = tempfile::tempdir().unwrap();
        activate(tmp.path(), "specs/001-demo");
        write(tmp.path(), "specs/001-demo/spec.md", "# Spec\n");
        write(tmp.path(), "specs/001-demo/plan.md", "# Plan\n");
        let s = derive_state(tmp.path());
        assert_eq!(s.step, LifecycleStep::Tasks);
    }

    #[test]
    fn tasks_with_unchecked_box_is_implement() {
        let tmp = tempfile::tempdir().unwrap();
        activate(tmp.path(), "specs/001-demo");
        write(tmp.path(), "specs/001-demo/spec.md", "# Spec\n");
        write(tmp.path(), "specs/001-demo/plan.md", "# Plan\n");
        write(
            tmp.path(),
            "specs/001-demo/tasks.md",
            "# Tasks\n\n- [X] T001 Done thing\n- [ ] T002 Open thing\n",
        );
        let s = derive_state(tmp.path());
        assert_eq!(s.step, LifecycleStep::Implement);
    }

    #[test]
    fn all_checked_is_acceptance() {
        let tmp = tempfile::tempdir().unwrap();
        activate(tmp.path(), "specs/001-demo");
        write(tmp.path(), "specs/001-demo/spec.md", "# Spec\n");
        write(tmp.path(), "specs/001-demo/plan.md", "# Plan\n");
        write(
            tmp.path(),
            "specs/001-demo/tasks.md",
            "# Tasks\n\n- [x] T001 Done thing\n- [X] T002 Also done\n",
        );
        let s = derive_state(tmp.path());
        assert_eq!(s.step, LifecycleStep::Acceptance);
    }

    // ---- 2. context_block exact shape ---------------------------------

    #[test]
    fn context_block_matches_contract_shape() {
        let state = LifecycleState {
            feature_directory: "specs/001-demo".to_string(),
            step: LifecycleStep::Specify,
            has_spec: true,
            has_plan: false,
            has_tasks: true,
            stale_pointer: false,
        };
        let block = context_block(&state);
        assert!(block.starts_with("## Spec-Kit Lifecycle Context\nFeature: specs/001-demo\n"));
        assert_eq!(
            block,
            "## Spec-Kit Lifecycle Context\n\
             Feature: specs/001-demo\n\
             Step: Specify (author specs/<feature>/spec.md via /speckit-specify)\n\
             Artifacts: spec.md present, plan.md absent, tasks.md present\n\
             Note: detected automatically; refresh by restarting the session or running /speckit-status.\n"
        );
    }

    #[test]
    fn context_block_none_feature_renders_placeholder() {
        let block = context_block(&LifecycleState::default());
        assert!(block.starts_with("## Spec-Kit Lifecycle Context\nFeature: (none)\n"));
        assert!(block.contains("Step: None (no active feature"));
        assert!(block.contains("Artifacts: spec.md absent, plan.md absent, tasks.md absent\n"));
    }

    // ---- 3. feature_scope ----------------------------------------------

    #[test]
    fn feature_scope_unions_targets_reads_artifacts_and_criteria() {
        let tmp = tempfile::tempdir().unwrap();
        activate(tmp.path(), "specs/001-demo");
        write(
            tmp.path(),
            "specs/001-demo/spec.md",
            "# Feature Specification: Demo\n\n\
             ### User Story 1 - Basic flow (Priority: P1)\n\
             - Given a fresh checkout\n\
             - Given a derived step\n\n\
             ## Success Criteria\n\
             - Given a fresh checkout\n\
             - CLI exits zero\n",
        );
        write(
            tmp.path(),
            "specs/001-demo/plan.md",
            "# Plan\n",
        );
        write(
            tmp.path(),
            "specs/001-demo/tasks.md",
            "# Tasks\n\n\
             ## Phase 1: Core\n\n\
             - [ ] T001 [P] Implement a in `src/a.rs`\n\
             - [ ] T002 Implement b in `src/a.rs` and `src/b.rs`\n",
        );

        let state = derive_state(tmp.path());
        assert_eq!(state.step, LifecycleStep::Implement);

        let scope = feature_scope(tmp.path(), &state);
        // write_files: deduped union of target files, order preserved.
        assert_eq!(scope.write_files, vec!["src/a.rs", "src/b.rs"]);
        // read_files: write_files + existing artifacts (spec.md, plan.md,
        // tasks.md; research.md absent → skipped).
        assert_eq!(
            scope.read_files,
            vec![
                "src/a.rs",
                "src/b.rs",
                "specs/001-demo/spec.md",
                "specs/001-demo/plan.md",
                "specs/001-demo/tasks.md",
            ]
        );
        // acceptance: scenarios + success criteria, deduped ("- Given a
        // fresh checkout" appears in both sources).
        assert_eq!(
            scope.acceptance_criteria,
            vec!["Given a fresh checkout", "Given a derived step", "CLI exits zero"]
        );
    }

    #[test]
    fn feature_scope_missing_dir_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        activate(tmp.path(), "specs/000-gone");
        let state = derive_state(tmp.path());
        let scope = feature_scope(tmp.path(), &state);
        assert_eq!(scope, FeatureScope::default());
    }

    // ---- 4/5/6. tasks_to_task_nodes ------------------------------------

    fn graph_of(nodes: Vec<TaskNode>) -> TaskGraph {
        TaskGraph {
            nodes: nodes.into_iter().map(|n| (n.id.clone(), n)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn phases_create_cross_deps_and_validate() {
        let md = "# Tasks\n\n\
                  ## Phase 1: Setup\n\n\
                  - [ ] T001 [P] Implement a in `src/a.rs`\n\n\
                  ## Phase 2: Polish\n\n\
                  - [ ] T002 Implement b in `src/b.rs`\n";
        let bundle = tasks_to_task_nodes(md);
        assert_eq!(bundle.nodes.len(), 2);
        assert!(bundle.collisions.is_empty());

        let t001 = &bundle.nodes[0];
        let t002 = &bundle.nodes[1];
        assert_eq!(t001.id.as_str(), "t001");
        assert_eq!(t002.id.as_str(), "t002");
        assert!(t001.dependencies.is_empty());
        assert!(t002.dependencies.contains(&t001.id));
        // read_set mirrors write_set.
        assert_eq!(t001.read_set, t001.write_set);

        assert!(graph_of(bundle.nodes.clone()).validate().is_ok());
    }

    #[test]
    fn same_phase_write_collision_is_surfaced() {
        let md = "# Tasks\n\n\
                  ## Phase 1: Core\n\n\
                  - [ ] T001 [P] Write `src/a.rs`\n\
                  - [ ] T002 [P] Also write `src/a.rs`\n";
        let bundle = tasks_to_task_nodes(md);
        assert_eq!(bundle.nodes.len(), 2);
        assert_eq!(bundle.collisions.len(), 1);
        let c = &bundle.collisions[0];
        assert_eq!(c.task_a, "T001");
        assert_eq!(c.task_b, "T002");
        assert_eq!(c.file, "src/a.rs");

        // No mutual deps were added, so validate() flags WRITE_OVERLAP.
        let errors = graph_of(bundle.nodes.clone()).validate().unwrap_err();
        let overlap: Vec<_> = errors
            .iter()
            .filter(|e| e.rule == rules::WRITE_OVERLAP)
            .collect();
        assert_eq!(overlap.len(), 1);
        assert!(overlap[0].task_ids.contains(&"t001".to_string()));
        assert!(overlap[0].task_ids.contains(&"t002".to_string()));
    }

    #[test]
    fn empty_tasks_md_yields_empty_bundle() {
        let bundle = tasks_to_task_nodes("");
        assert!(bundle.nodes.is_empty());
        assert!(bundle.collisions.is_empty());
    }

    #[test]
    fn risk_escalates_with_wide_write_set() {
        let md = "- [ ] T001 Touch `src/a.rs` `src/b.rs` `src/c.rs`\n";
        let bundle = tasks_to_task_nodes(md);
        assert_eq!(bundle.nodes[0].risk, RiskLevel::Medium);
        let narrow = tasks_to_task_nodes("- [ ] T002 Touch `src/a.rs`\n");
        assert_eq!(narrow.nodes[0].risk, RiskLevel::Low);
    }
}

// ---------------------------------------------------------------------
// T023/T026/T030: session injection + conductor snapshot + scope wiring
// ---------------------------------------------------------------------

/// Whether lifecycle context injection is allowed (feature 026, T023):
/// both `speckit.enabled` (default true) and `speckit.lifecycle_context`
/// (default true) must hold. False short-circuits every new injection
/// path (FR-013).
pub fn lifecycle_context_allowed(config: &joey_core::Config) -> bool {
    config.get_bool("speckit.enabled", true)
        && config.get_bool("speckit.lifecycle_context", true)
}

/// Map the derived on-disk state onto the conductor prompt's
/// [`LifecycleSnapshot`] (feature 026, T026).
pub fn lifecycle_snapshot(
    root: &std::path::Path,
) -> joey_omo::agents::prompts::conductor::LifecycleSnapshot {
    let state = derive_state(root);
    joey_omo::agents::prompts::conductor::LifecycleSnapshot {
        feature: state.feature_directory.clone(),
        step: state.step.as_str().to_string(),
        guidance: state.step.guidance().to_string(),
        spec_present: state.has_spec,
        plan_present: state.has_plan,
        tasks_present: state.has_tasks,
    }
}

/// The lifecycle snapshot for the CURRENT working directory, `None` when
/// lifecycle context is disabled in config or the cwd is not inside a
/// spec-kit repository (renderers must then stay byte-identical to the
/// pre-feature prompt).
pub fn lifecycle_snapshot_opt(
    config: &joey_core::Config,
) -> Option<joey_omo::agents::prompts::conductor::LifecycleSnapshot> {
    if !lifecycle_context_allowed(config) {
        return None;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let root = crate::speckit_slash::find_repo_root(&cwd)?;
    Some(lifecycle_snapshot(&root))
}

/// The session-start lifecycle context block (feature 026, T023): `None`
/// unless lifecycle context is allowed AND `cwd` is inside a spec-kit
/// repository; otherwise the rendered block for the derived state
/// (injected ONCE per session through the extra_instructions slot —
/// cache-friendly, never per-turn).
pub fn session_context_block(
    cwd: &std::path::Path,
    config: &joey_core::Config,
) -> Option<String> {
    if !lifecycle_context_allowed(config) {
        return None;
    }
    let root = crate::speckit_slash::find_repo_root(cwd)?;
    Some(context_block(&derive_state(&root)))
}

/// The engine feature-scope file set (feature 026, T030/US7): empty unless
/// the native surface is enabled (`speckit.enabled`) AND `cwd` is inside a
/// spec-kit repository; otherwise the active feature's deduped write set
/// (union of `tasks.md` target files). Empty scope = unchanged engine
/// behavior.
pub fn feature_scope_files(cwd: &Path, config: &joey_core::Config) -> Vec<String> {
    if !crate::speckit_slash::speckit_enabled(config) {
        return Vec::new();
    }
    match crate::speckit_slash::find_repo_root(cwd) {
        Some(root) => feature_scope(&root, &derive_state(&root)).write_files,
        None => Vec::new(),
    }
}

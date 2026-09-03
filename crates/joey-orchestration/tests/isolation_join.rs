//! Spec 023 T020 (US4) — isolation + joiner end-to-end at the
//! orchestration layer (Quickstart §5 scenario).
//!
//! Two isolated writers run through the REAL scheduler against REAL git
//! worktrees (via [`WorkspaceIsolation`]); their changes are collected by
//! the REAL [`Joiner`] and integrated into the shared checkout. Covers:
//!
//! - FR-015/FR-016: disjoint isolated writers integrate cleanly, bundles
//!   match their declared write sets, both edits land in the project root.
//! - FR-018: integration NEVER commits — exact commit-count equality plus
//!   a dirty `git status --porcelain`.
//! - SC-006: a conflicting patch is surfaced as [`IntegrationConflict`]
//!   and left unapplied — the first editor's value stands, no conflict
//!   markers ever reach the tree.
//! - Baseline parity: every isolated worktree shares the repo's HEAD sha.
//!
//! Tests skip gracefully (early return) when git or the scratch repo is
//! unavailable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use joey_orchestration::evaluator::{GateOutcome, VerificationGate, VerificationPlanView};
use joey_orchestration::evidence::RunHandle;
use joey_orchestration::joiner::{ChangeBundle, Joiner};
use joey_orchestration::scheduler::{Scheduler, SchedulerConfig, TaskDispatcher};
use joey_orchestration::task_graph::{
    AcceptanceCriterion, IsolationMode, ModelTier, RiskLevel, TaskGraph, TaskId, TaskNode,
    TaskStatus, WorkerRole,
};
use joey_orchestration::workspace::{baseline_revision, IsolatedWorkspace, WorkspaceIsolation};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Run git in `cwd`; `true` only when it exits zero.
fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run git in `cwd`, capturing trimmed stdout; `None` on failure.
fn git_out(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(cwd).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// `git log --oneline` line count (0 on failure).
fn count_commits(root: &Path) -> usize {
    git_out(root, &["log", "--oneline"])
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// Scratch git repo: `src/base.txt` (`value=orig`), empty `src/a.rs` and
/// `src/b.rs`, one commit. Returns the guard (keep alive!) and the repo
/// path; `None` ⇒ tests skip.
fn scratch_git_repo() -> Option<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::tempdir().ok()?;
    let root = dir.path().to_path_buf();
    if !git_ok(&root, &["init"]) {
        return None;
    }
    if !git_ok(&root, &["config", "user.email", "test@example.com"]) {
        return None;
    }
    if !git_ok(&root, &["config", "user.name", "Joey Test"]) {
        return None;
    }
    std::fs::create_dir_all(root.join("src")).ok()?;
    std::fs::write(root.join("src/base.txt"), "value=orig\n").ok()?;
    std::fs::write(root.join("src/a.rs"), "").ok()?;
    std::fs::write(root.join("src/b.rs"), "").ok()?;
    if !git_ok(&root, &["add", "-A"]) {
        return None;
    }
    if !git_ok(&root, &["commit", "-m", "init"]) {
        return None;
    }
    Some((dir, root))
}

/// Writer TaskNode: write_set `[path]`, IsolatedWorktree, no deps,
/// one manual acceptance criterion, default verification plan.
fn writer_node(id: &str, path: &str) -> TaskNode {
    TaskNode {
        id: TaskId::new(id).unwrap(),
        objective: format!("write {path}"),
        dependencies: vec![],
        read_set: vec![],
        write_set: vec![PathBuf::from(path)],
        artifact_ids: vec![],
        role: WorkerRole::Implementor,
        model_tier: ModelTier::Economical,
        risk: RiskLevel::Low,
        acceptance: vec![AcceptanceCriterion {
            criterion: "it works".to_string(),
            kind: "manual".to_string(),
        }],
        verification: VerificationPlanView::default(),
        isolation: IsolationMode::IsolatedWorktree,
        status: TaskStatus::Pending,
        attempts: 0,
    }
}

/// Graph over the given nodes.
fn make_graph(nodes: Vec<TaskNode>) -> TaskGraph {
    TaskGraph {
        nodes: nodes.into_iter().map(|n| (n.id.clone(), n)).collect(),
        baseline_revision: String::new(),
        run_id: String::new(),
    }
}

/// Local pass-through verification gate.
struct PassGate;

#[async_trait::async_trait]
impl VerificationGate for PassGate {
    async fn run(&self, _plan: &VerificationPlanView, _workdir: &Path) -> GateOutcome {
        GateOutcome::Passed
    }
}

/// Fake dispatcher mirroring the T020 CLI wiring: every task is prepared
/// an isolated workspace, its declared file is edited INSIDE that
/// workspace, and the (task id, workspace, file) triple is recorded for
/// post-run joiner collection.
struct IsolatingDispatcher {
    project_root: PathBuf,
    run_root: PathBuf,
    /// (task id, workspace, file edited)
    done: Mutex<Vec<(String, IsolatedWorkspace, String)>>,
}

impl IsolatingDispatcher {
    fn new(project_root: &Path, run_root: &Path) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            run_root: run_root.to_path_buf(),
            done: Mutex::new(Vec::new()),
        }
    }

    fn recorded(&self) -> Vec<(String, IsolatedWorkspace, String)> {
        self.done.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl TaskDispatcher for IsolatingDispatcher {
    async fn dispatch(&self, task: &TaskNode, _workdir: &Path) -> bool {
        let ws = WorkspaceIsolation::new(self.project_root.clone(), self.run_root.clone())
            .prepare(task)
            .expect("prepare isolated workspace");
        let declared = task
            .write_set
            .first()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("src/a.rs"));
        let target = ws.path().join(&declared);
        if declared == PathBuf::from("src/base.txt") {
            // Conflict scenario: replace the 'value=orig' line with the
            // task's own value (task-c1 → A, task-c2 → B).
            let tag = if task.id.as_str().ends_with("c1") { "A" } else { "B" };
            std::fs::write(&target, format!("value={tag}\n")).expect("write base.txt");
        } else {
            let mut content = std::fs::read_to_string(&target).unwrap_or_default();
            content.push_str(&format!("edited-by-{}\n", task.id.as_str()));
            std::fs::write(&target, content).expect("write declared file");
        }
        self.done
            .lock()
            .unwrap()
            .push((task.id.to_string(), ws, declared.display().to_string()));
        true
    }
}

/// The scheduler config the brief pins (max 4 workers, 1 repair attempt).
fn scheduler() -> Scheduler {
    Scheduler::new(SchedulerConfig {
        max_concurrent_workers: 4,
        max_repair_attempts: 1,
    })
}

// ---------------------------------------------------------------------------
// 1. Two isolated writers integrate cleanly (FR-015/FR-016/FR-018)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_isolated_writers_integrate_cleanly() {
    let Some((_guard, project_root)) = scratch_git_repo() else {
        eprintln!("skip: scratch repo unavailable");
        return;
    };
    let baseline = baseline_revision(&project_root).expect("baseline after commit");
    let run_dir = tempfile::tempdir().unwrap();
    let mut run = RunHandle::create_at(run_dir.path(), "run-iso", &baseline).unwrap();
    let mut graph = make_graph(vec![
        writer_node("task-a", "src/a.rs"),
        writer_node("task-b", "src/b.rs"),
    ]);
    let dispatcher = IsolatingDispatcher::new(&project_root, run_dir.path());
    let gate = PassGate;
    let commits_before = count_commits(&project_root);

    let stats = scheduler()
        .run_to_completion(&mut graph, &mut run, &dispatcher, &gate, &project_root)
        .await;
    assert_eq!(stats.completed, 2);
    for id in ["task-a", "task-b"] {
        assert_eq!(
            graph.nodes[&TaskId::new(id).unwrap()].status,
            TaskStatus::Completed,
            "{id} must complete"
        );
    }

    // Collect one bundle per recorded workspace (deterministic order via
    // BTreeMap so the integrate report order is pinned).
    let joiner = Joiner::new(&project_root);
    let recorded = dispatcher.recorded();
    assert_eq!(recorded.len(), 2, "one workspace per dispatched writer");
    let mut by_id: BTreeMap<String, ChangeBundle> = BTreeMap::new();
    for (id, ws, _file) in &recorded {
        let node = graph
            .nodes
            .get(&TaskId::new(id).unwrap())
            .expect("recorded task in graph");
        let b = joiner.collect(ws, node, &mut run).expect("collect");
        assert_eq!(
            b.actual_write_set, node.write_set,
            "no divergence: actual write set == declared"
        );
        assert_eq!(b.baseline_sha, baseline, "bundle baseline == repo baseline");
        assert!(
            std::fs::metadata(&b.patch_path).unwrap().len() > 0,
            "patch artifact non-empty"
        );
        by_id.insert(b.task_id.clone(), b);
    }
    let bundles: Vec<ChangeBundle> = by_id.into_values().collect();

    let report = joiner.integrate(&bundles, |_| Ok(())).expect("clean integrate");
    assert_eq!(report.applied_task_ids, vec!["task-a", "task-b"]);

    // Both edits present in the shared checkout.
    let a = std::fs::read_to_string(project_root.join("src/a.rs")).unwrap();
    assert!(a.contains("edited-by-task-a"), "src/a.rs: {a}");
    let b = std::fs::read_to_string(project_root.join("src/b.rs")).unwrap();
    assert!(b.contains("edited-by-task-b"), "src/b.rs: {b}");

    // FR-018: exact commit-count equality — integration never commits.
    assert_eq!(
        count_commits(&project_root),
        commits_before,
        "FR-018: git log unchanged"
    );
    // Changes live in the worktree/index only — status is dirty.
    let status = git_out(&project_root, &["status", "--porcelain"]).unwrap_or_default();
    assert!(
        !status.trim().is_empty(),
        "applied changes must leave the checkout dirty (never committed): {status}"
    );
}

// ---------------------------------------------------------------------------
// 2. Conflicting patch surfaced, tree untouched by it (SC-006/FR-017)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conflicting_patch_surfaced_untouched() {
    let Some((_guard, project_root)) = scratch_git_repo() else {
        eprintln!("skip: scratch repo unavailable");
        return;
    };
    let baseline = baseline_revision(&project_root).expect("baseline after commit");
    let run_dir = tempfile::tempdir().unwrap();
    let mut run = RunHandle::create_at(run_dir.path(), "run-iso-c", &baseline).unwrap();
    // Both write src/base.txt ⇒ overlap ⇒ the scheduler sequences them
    // (ascending id: task-c1 strictly before task-c2); each edits in ITS
    // OWN worktree from the same baseline, so both patches target
    // value=orig.
    let mut graph = make_graph(vec![
        writer_node("task-c1", "src/base.txt"),
        writer_node("task-c2", "src/base.txt"),
    ]);
    let dispatcher = IsolatingDispatcher::new(&project_root, run_dir.path());
    let gate = PassGate;
    let commits_before = count_commits(&project_root);

    let stats = scheduler()
        .run_to_completion(&mut graph, &mut run, &dispatcher, &gate, &project_root)
        .await;
    assert_eq!(stats.completed, 2);

    // Deterministic order: the conflict group serialized c1 before c2.
    let recorded = dispatcher.recorded();
    let order: Vec<&str> = recorded.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(
        order,
        vec!["task-c1", "task-c2"],
        "overlapping writers serialized in ascending id order"
    );

    let joiner = Joiner::new(&project_root);
    let mut bundles = Vec::new();
    for (id, ws, _file) in &recorded {
        let node = graph
            .nodes
            .get(&TaskId::new(id).unwrap())
            .expect("recorded task in graph");
        bundles.push(joiner.collect(ws, node, &mut run).expect("collect"));
    }

    // First applies, second surfaces as an untouched conflict.
    let err = joiner.integrate(&bundles, |_| Ok(())).unwrap_err();
    assert_eq!(err.task_id, "task-c2", "the SECOND bundle conflicts");
    assert!(!err.reason.is_empty(), "conflict carries a reason");
    assert!(
        err.patch_path.display().to_string().contains("task-c2"),
        "conflict names the unapplied patch: {}",
        err.patch_path.display()
    );

    // The tree holds the FIRST editor's value only; no conflict markers.
    let base = std::fs::read_to_string(project_root.join("src/base.txt")).unwrap();
    assert_eq!(base, "value=A\n", "first editor's value only, untouched by the conflicting patch");
    assert!(!base.contains("<<<<<<<"), "no conflict markers may reach the tree");
    assert_eq!(
        count_commits(&project_root),
        commits_before,
        "FR-018: git log unchanged even on conflict"
    );
}

// ---------------------------------------------------------------------------
// 3. Worktrees share the repo baseline
// ---------------------------------------------------------------------------

#[tokio::test]
async fn worktrees_share_baseline() {
    let Some((_guard, project_root)) = scratch_git_repo() else {
        eprintln!("skip: scratch repo unavailable");
        return;
    };
    let baseline = baseline_revision(&project_root).expect("baseline after commit");
    let run_dir = tempfile::tempdir().unwrap();
    let mut run = RunHandle::create_at(run_dir.path(), "run-iso-b", &baseline).unwrap();
    let mut graph = make_graph(vec![
        writer_node("task-a", "src/a.rs"),
        writer_node("task-b", "src/b.rs"),
    ]);
    let dispatcher = IsolatingDispatcher::new(&project_root, run_dir.path());
    scheduler()
        .run_to_completion(
            &mut graph,
            &mut run,
            &dispatcher,
            &PassGate,
            &project_root,
        )
        .await;

    let recorded = dispatcher.recorded();
    assert_eq!(recorded.len(), 2);
    for (id, ws, _file) in &recorded {
        assert_eq!(
            ws.baseline_revision, baseline,
            "worktree for {id} must share the repo HEAD sha"
        );
    }
}

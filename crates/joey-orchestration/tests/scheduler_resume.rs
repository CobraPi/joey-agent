//! Spec 023 T017 (US3) — scheduler resume integration tests.
//!
//! Covers SC-004 (resumed run never re-executes completed tasks), FR-030
//! (baseline mismatch refuses resume), the deterministic wave partition of
//! overlapping writers (conflict sequencing), and SC-007 (final task
//! statuses are reconstructible from the persisted decision log alone).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use joey_orchestration::evaluator::{GateOutcome, VerificationGate, VerificationPlanView};
use joey_orchestration::evidence::{
    is_valid_cause, RunHandle, ResumeError,
};
use joey_orchestration::scheduler::{
    RunStats, Scheduler, SchedulerConfig, TaskDispatcher,
};
use joey_orchestration::task_graph::{
    default_isolation, AcceptanceCriterion, IsolationMode, ModelTier, RiskLevel, TaskGraph,
    TaskId, TaskNode, TaskStatus, WorkerRole,
};

// ---------------------------------------------------------------------------
// Fakes and helpers
// ---------------------------------------------------------------------------

/// Local pass-through verification gate.
struct PassGate;

#[async_trait::async_trait]
impl VerificationGate for PassGate {
    async fn run(&self, _plan: &VerificationPlanView, _workdir: &Path) -> GateOutcome {
        GateOutcome::Passed
    }
}

/// Counting dispatcher: records `{id}:start` on entry and `{id}:end` on
/// exit, sleeps `delay_ms` in between, and always succeeds.
struct CountingDispatcher {
    calls: Mutex<Vec<String>>,
    delay_ms: u64,
}

impl CountingDispatcher {
    fn new(delay_ms: u64) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            delay_ms,
        }
    }

    fn log(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl TaskDispatcher for CountingDispatcher {
    async fn dispatch(&self, task: &TaskNode, _workdir: &Path) -> bool {
        {
            let mut calls = self.calls.lock().unwrap();
            calls.push(format!("{}:start", task.id.as_str()));
        }
        tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;
        self.calls
            .lock()
            .unwrap()
            .push(format!("{}:end", task.id.as_str()));
        true
    }
}

/// Minimal literal task node: Implementor role, Economical tier, Low risk,
/// one manual acceptance criterion, empty verification plan, default
/// isolation for its write set.
fn node(id: &str, writes: &[&str], deps: &[&str]) -> TaskNode {
    let write_set: Vec<PathBuf> = writes.iter().map(PathBuf::from).collect();
    TaskNode {
        id: TaskId::new(id).unwrap(),
        objective: format!("do {id}"),
        dependencies: deps.iter().map(|d| TaskId::new(d).unwrap()).collect(),
        read_set: vec![],
        artifact_ids: vec![],
        write_set: write_set.clone(),
        role: WorkerRole::Implementor,
        model_tier: ModelTier::Economical,
        risk: RiskLevel::Low,
        acceptance: vec![AcceptanceCriterion {
            criterion: format!("{id} done"),
            kind: "manual".to_string(),
        }],
        verification: VerificationPlanView::default(),
        isolation: default_isolation(&write_set),
        status: TaskStatus::Pending,
        attempts: 0,
    }
}

/// Graph over the given nodes with baseline "base-1".
fn make_graph(nodes: Vec<TaskNode>) -> TaskGraph {
    TaskGraph {
        nodes: nodes.into_iter().map(|n| (n.id.clone(), n)).collect(),
        baseline_revision: "base-1".to_string(),
        run_id: String::new(),
    }
}

fn scheduler() -> Scheduler {
    Scheduler::new(SchedulerConfig::default())
}

/// Status name as it appears in decision-log `to` fields (Debug-derived in
/// the scheduler: `format!("{:?}", status)`).
fn status_name(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "Pending",
        TaskStatus::Ready => "Ready",
        TaskStatus::Dispatched => "Dispatched",
        TaskStatus::Evaluating => "Evaluating",
        TaskStatus::Completed => "Completed",
        TaskStatus::Failed => "Failed",
        TaskStatus::Degraded => "Degraded",
        TaskStatus::Blocked => "Blocked",
        TaskStatus::Skipped => "Skipped",
    }
}

// ---------------------------------------------------------------------------
// 1. SC-004 — resumed run skips completed tasks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resumed_run_skips_completed_tasks() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let run_dir = project.join("run-1");

    // Phase 1: single task, run to completion.
    let mut graph1 = make_graph(vec![node("task-a", &["src/a.rs"], &[])]);
    let mut run1 = RunHandle::create_at(&run_dir, "run-1", "base-1").unwrap();
    let disp1 = CountingDispatcher::new(10);
    let gate = PassGate;
    scheduler()
        .run_to_completion(&mut graph1, &mut run1, &disp1, &gate, &project)
        .await;
    assert_eq!(
        graph1.nodes[&TaskId::new("task-a").unwrap()].status,
        TaskStatus::Completed
    );

    // Phase 2: the interrupted plan arrives fully — rebuild the graph from
    // the PERSISTED graph.json plus the new task-b.
    let persisted_raw = fs::read_to_string(run_dir.join("graph.json")).unwrap();
    let persisted: TaskGraph = serde_json::from_str(&persisted_raw).unwrap();
    let mut graph2 = persisted.clone();
    let task_b = node("task-b", &["src/b.rs"], &["task-a"]);
    graph2.nodes.insert(task_b.id.clone(), task_b);

    // Baseline unchanged ⇒ resume succeeds.
    let mut run2 = RunHandle::resume_at(&run_dir, "run-1", "base-1").unwrap();

    let disp2 = CountingDispatcher::new(10);
    let stats: RunStats = scheduler()
        .run_to_completion(&mut graph2, &mut run2, &disp2, &gate, &project)
        .await;

    // task-b completed, task-a still completed.
    assert_eq!(
        graph2.nodes[&TaskId::new("task-a").unwrap()].status,
        TaskStatus::Completed
    );
    assert_eq!(
        graph2.nodes[&TaskId::new("task-b").unwrap()].status,
        TaskStatus::Completed
    );

    // task-a was NOT re-dispatched: the call log contains only task-b.
    let calls = disp2.log();
    assert!(!calls.is_empty(), "task-b must have been dispatched");
    assert!(
        calls.iter().all(|c| c.starts_with("task-b:")),
        "unexpected re-dispatch of completed task-a: {calls:?}"
    );

    // Exactly one dispatch in the resumed run.
    assert_eq!(stats.dispatched, 1);
    assert_eq!(stats.completed, 1);
}

// ---------------------------------------------------------------------------
// 2. FR-030 — baseline mismatch refuses resume
// ---------------------------------------------------------------------------

#[tokio::test]
async fn baseline_mismatch_refuses_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let run_dir = tmp.path().join("run-1");
    RunHandle::create_at(&run_dir, "run-1", "base-1").unwrap();

    let err = RunHandle::resume_at(&run_dir, "run-1", "base-2").unwrap_err();
    match &err {
        ResumeError::BaselineMismatch { persisted, current } => {
            assert_eq!(persisted, "base-1");
            assert_eq!(current, "base-2");
        }
        other => panic!("expected BaselineMismatch, got {other:?}"),
    }

    // The report string mentions both revisions.
    let report = format!("{err}");
    assert!(report.contains("base-1"), "report missing persisted rev: {report}");
    assert!(report.contains("base-2"), "report missing current rev: {report}");

    // Refusal must not delete state.
    assert!(run_dir.is_dir());
    assert!(run_dir.join("graph.json").is_file());
    assert!(run_dir.join("decisions.jsonl").is_file());
}

// ---------------------------------------------------------------------------
// 3. Wave partition serializes overlapping writers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wave_partition_serializes_overlapping_writers() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let run_dir = project.join("run-1");

    // task-x and task-y both write src/shared.rs, no deps ⇒ same conflict
    // group ⇒ strictly serialized.
    let mut graph = make_graph(vec![
        node("task-x", &["src/shared.rs"], &[]),
        node("task-y", &["src/shared.rs"], &[]),
    ]);
    let mut run = RunHandle::create_at(&run_dir, "run-1", "base-1").unwrap();

    // 30ms inside each dispatch to force overlap attempts if the partition
    // failed to serialize them.
    let disp = CountingDispatcher::new(30);
    let gate = PassGate;
    let stats = scheduler()
        .run_to_completion(&mut graph, &mut run, &disp, &gate, &project)
        .await;

    assert_eq!(
        graph.nodes[&TaskId::new("task-x").unwrap()].status,
        TaskStatus::Completed
    );
    assert_eq!(
        graph.nodes[&TaskId::new("task-y").unwrap()].status,
        TaskStatus::Completed
    );
    assert_eq!(stats.completed, 2);

    // Deterministic sorted order: task-x fully before task-y, adjacent
    // start/end pairs (no interleaving).
    let calls = disp.log();
    assert_eq!(
        calls,
        vec!["task-x:start", "task-x:end", "task-y:start", "task-y:end"],
        "overlapping writers must be strictly serialized in id order"
    );

    // The decision log carries a conflict_sequenced entry naming both.
    let decisions = run.read_decisions().unwrap();
    let conflict = decisions
        .iter()
        .find(|d| d.cause == "conflict_sequenced")
        .expect("conflict_sequenced entry logged");
    assert!(conflict.detail.contains("task-x"), "detail: {}", conflict.detail);
    assert!(conflict.detail.contains("task-y"), "detail: {}", conflict.detail);
}

// ---------------------------------------------------------------------------
// 4. SC-007 — decision log reconstructs final statuses
// ---------------------------------------------------------------------------

#[tokio::test]
async fn decision_log_reconstructs_final_statuses() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let run_dir = project.join("run-1");

    // Diamond: a → {b, c} → d, disjoint writes.
    let mut graph = make_graph(vec![
        node("task-a", &["src/a.rs"], &[]),
        node("task-b", &["src/b.rs"], &["task-a"]),
        node("task-c", &["src/c.rs"], &["task-a"]),
        node("task-d", &["src/d.rs"], &["task-b", "task-c"]),
    ]);
    let mut run = RunHandle::create_at(&run_dir, "run-1", "base-1").unwrap();
    let disp = CountingDispatcher::new(10);
    let gate = PassGate;
    let stats = scheduler()
        .run_to_completion(&mut graph, &mut run, &disp, &gate, &project)
        .await;
    assert_eq!(stats.completed, 4);

    // --- Reconstruct from PERSISTED STATE ONLY ---
    let reader = RunHandle::resume_at(&run_dir, "run-1", "base-1").unwrap();
    let decisions = reader.read_decisions().unwrap();
    let persisted: TaskGraph =
        serde_json::from_str(&fs::read_to_string(run_dir.join("graph.json")).unwrap()).unwrap();

    // Every logged cause is in the pinned 13-cause vocabulary.
    assert!(!decisions.is_empty());
    for entry in &decisions {
        assert!(
            is_valid_cause(&entry.cause),
            "invalid cause in log: {}",
            entry.cause
        );
    }

    // Fold the log: last entry per task whose cause is terminal decides the
    // task's final status.
    let terminal_causes = ["gate_passed", "gate_failed", "degraded"];
    let mut reconstructed: std::collections::BTreeMap<String, TaskStatus> =
        std::collections::BTreeMap::new();
    for entry in &decisions {
        if terminal_causes.contains(&entry.cause.as_str()) {
            let status = match entry.to.as_str() {
                "Completed" => TaskStatus::Completed,
                "Failed" => TaskStatus::Failed,
                "Degraded" => TaskStatus::Degraded,
                other => panic!("terminal cause with unexpected target {other}"),
            };
            reconstructed.insert(entry.task_id.clone(), status);
        }
    }

    // All four tasks terminal-Completed, matching the persisted graph.
    for id in ["task-a", "task-b", "task-c", "task-d"] {
        assert_eq!(
            reconstructed.get(id),
            Some(&TaskStatus::Completed),
            "reconstructed status for {id}"
        );
        assert_eq!(
            persisted.nodes[&TaskId::new(id).unwrap()].status,
            TaskStatus::Completed,
            "persisted status for {id}"
        );
    }
    assert_eq!(reconstructed.len(), 4);

    // Every task that ran has logged transitions with a cause throughout:
    // each of the four tasks must have its full lifecycle entries logged
    // (Pending→Ready→Dispatched→Evaluating→Completed = 4 transitions).
    for id in ["task-a", "task-b", "task-c", "task-d"] {
        let entries: Vec<_> = decisions.iter().filter(|d| d.task_id == id).collect();
        assert!(entries.len() >= 4, "{id} has only {} logged transitions", entries.len());
        let causes: Vec<&str> = entries.iter().map(|d| d.cause.as_str()).collect();
        assert!(causes.contains(&"dependency_completed"), "{id} lifecycle causes: {causes:?}");
        assert!(causes.contains(&"worker_completed"), "{id} lifecycle causes: {causes:?}");
        assert!(causes.contains(&"gate_passed"), "{id} lifecycle causes: {causes:?}");
    }

    // Silence unused-code warning for status_name while pinning its mapping.
    assert_eq!(status_name(TaskStatus::Completed), "Completed");
    assert_eq!(status_name(TaskStatus::Degraded), "Degraded");
    // Isolation defaults: writers isolate, readers share.
    assert_eq!(
        default_isolation(&[PathBuf::from("src/a.rs")]),
        IsolationMode::IsolatedWorktree
    );
    assert_eq!(default_isolation(&[]), IsolationMode::SharedCheckout);
}

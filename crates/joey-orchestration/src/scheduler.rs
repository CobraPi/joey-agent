//! Spec 023 T015 (US3) — the deterministic wave scheduler.
//!
//! Drives a [`TaskGraph`] to completion in deterministic waves:
//!
//! - **FR-011 (deterministic wave scheduling):** each wave takes the
//!   current ready set ([`TaskGraph::ready_nodes`] — Pending tasks whose
//!   dependencies are all Completed), partitions it into conflict groups
//!   ([`ConflictAnalyzer`]), and runs the groups concurrently while the
//!   tasks inside a single group run strictly sequentially. Group order
//!   and intra-group order are both deterministic (ascending task id).
//! - **FR-012 (persist per transition):** every accepted lifecycle
//!   transition is persisted to the run directory — one decision line in
//!   `decisions.jsonl`, a rewritten `nodes/<task-id>.json` snapshot, and a
//!   rewritten full `graph.json` snapshot — before the loop moves on.
//! - **FR-013/SC-004 (completed is never re-executed):** the wave loop
//!   only ever dispatches tasks in the `Pending` state; terminal tasks
//!   (`Completed`/`Failed`/`Skipped`) are never handed to a dispatcher
//!   again, so re-running [`Scheduler::run_to_completion`] over a finished
//!   graph is a no-op.
//! - **FR-014 (every transition logged):** each transition appends a
//!   [`DecisionEntry`] whose cause comes from the pinned vocabulary in
//!   `crate::evidence::DECISION_CAUSES` (`dependency_completed`,
//!   `worker_completed`, `gate_passed`, `gate_failed`, `repair_scheduled`,
//!   `escalated`, `degraded`, `deferred_concurrency_cap`,
//!   `conflict_sequenced`).
//! - **FR-029 (concurrency cap + deterministic queue):** at most
//!   `SchedulerConfig::max_concurrent_workers` workers are in flight at
//!   once; a task that cannot immediately get a slot is logged
//!   `deferred_concurrency_cap` and then queued (awaited) in wave order.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::evaluator::{EvaluationDirective, Evaluator, RepairLedger, VerificationGate};
use crate::evidence::{DecisionEntry, RunHandle};
use crate::task_graph::{ModelTier, TaskGraph, TaskId, TaskNode, TaskStatus};

/// Scheduler tunables (FR-029).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SchedulerConfig {
    /// Maximum number of workers in flight at once (FR-029).
    pub max_concurrent_workers: usize,
    /// Per-tier repair budget handed to the evaluator (FR-021).
    pub max_repair_attempts: u32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_concurrent_workers: 16,
            max_repair_attempts: 3,
        }
    }
}

/// Counters collected over one `run_to_completion` call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunStats {
    /// Total worker dispatches (initial + repair/escalate re-dispatches).
    pub dispatched: usize,
    /// Tasks that reached `Completed`.
    pub completed: usize,
    /// Tasks that reached `Failed` (worker crash or gate failure).
    pub failed: usize,
    /// Tasks that reached `Degraded` (FR-031).
    pub degraded: usize,
    /// Tasks still `Pending` when the loop exited (permanently blocked).
    pub blocked_remaining: usize,
}

/// Partitions a ready set into conflict groups (deterministic, pure).
///
/// Two ready tasks *conflict* iff their write sets share a path (string
/// comparison) OR either write set is empty — an undeclared write set is
/// treated as "may write anything", so an undeclared task is never
/// concurrent with anyone, and two undeclared tasks also conflict with
/// each other (data-model line 53). Conflicts are transitive: union-find
/// merges conflicting pairs into groups.
///
/// Output: each group's ids sorted ascending, groups sorted by their
/// first id.
pub struct ConflictAnalyzer;

impl ConflictAnalyzer {
    pub fn partition(&self, ready: &[TaskId], graph: &TaskGraph) -> Vec<Vec<TaskId>> {
        // Deterministic working order (ready may arrive in any order).
        let mut sorted: Vec<TaskId> = ready.to_vec();
        sorted.sort();
        let n = sorted.len();

        // Union-find over indices into `sorted`.
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(parent: &mut [usize], mut i: usize) -> usize {
            while parent[i] != i {
                parent[i] = parent[parent[i]]; // path halving
                i = parent[i];
            }
            i
        }

        // Write sets as string sets; None = task unknown to the graph,
        // treated as undeclared (conflicts with everyone).
        let writes: Vec<Option<HashSet<String>>> = sorted
            .iter()
            .map(|id| {
                graph.nodes.get(id).map(|node| {
                    node.write_set
                        .iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect()
                })
            })
            .collect();

        for i in 0..n {
            for j in (i + 1)..n {
                let conflict = match (&writes[i], &writes[j]) {
                    (None, _) | (_, None) => true,
                    (Some(a), Some(b)) => {
                        a.is_empty() || b.is_empty() || a.iter().any(|p| b.contains(p))
                    }
                };
                if conflict {
                    let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                    if ri != rj {
                        parent[ri] = rj;
                    }
                }
            }
        }

        // Members are pushed in ascending index order ⇒ each group's ids
        // are sorted ascending; BTreeMap keyed by root, then a final sort
        // by first id makes group order deterministic.
        let mut by_root: BTreeMap<usize, Vec<TaskId>> = BTreeMap::new();
        for i in 0..n {
            by_root
                .entry(find(&mut parent, i))
                .or_default()
                .push(sorted[i].clone());
        }
        let mut groups: Vec<Vec<TaskId>> = by_root.into_values().collect();
        groups.sort_by(|a, b| a[0].cmp(&b[0]));
        groups
    }
}

/// Hands a task to a worker.
///
/// Returns `true` when the worker finished successfully (the scheduler
/// then proceeds to gate evaluation) and `false` when the worker crashed
/// (the scheduler treats the task as failed without invoking the gate).
///
/// The real implementation lives in joey-cli and wraps
/// `manager.dispatch_requests`; the choice of isolation (shared checkout
/// vs dedicated git worktree) is the dispatcher's concern, not the
/// scheduler's.
#[async_trait::async_trait]
pub trait TaskDispatcher: Send + Sync {
    async fn dispatch(&self, task: &TaskNode, workdir: &Path) -> bool;
}

/// Whether the graph needs a replan: any `Pending` node has a dependency
/// in `Failed` or `Degraded` — that dependency can never reach `Completed`,
/// so the dependent can never run (FR-011 / replan-on-blocked hook).
pub fn needs_replan(graph: &TaskGraph) -> bool {
    graph.nodes.values().any(|node| {
        node.status == TaskStatus::Pending
            && node.dependencies.iter().any(|dep| {
                matches!(
                    graph.nodes.get(dep).map(|d| d.status),
                    Some(TaskStatus::Failed) | Some(TaskStatus::Degraded)
                )
            })
    })
}

/// Optional observer for graph transitions: invoked with the full graph
/// snapshot JSON (`TaskGraph::snapshot()`) immediately after it is
/// persisted, so UIs can watch task-graph progress live.
pub type SnapshotSink = std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync>;

/// The deterministic wave scheduler (FR-011).
pub struct Scheduler {
    config: SchedulerConfig,
    snapshot_sink: Option<SnapshotSink>,
}

impl Scheduler {
    pub fn new(config: SchedulerConfig) -> Self {
        Self {
            config,
            snapshot_sink: None,
        }
    }

    /// Builder-style: attach a [`SnapshotSink`], fired with the graph
    /// snapshot JSON right after every persisted transition.
    pub fn with_snapshot_sink(mut self, sink: SnapshotSink) -> Self {
        self.snapshot_sink = Some(sink);
        self
    }

    /// Runs the graph to completion in deterministic waves.
    ///
    /// Each wave: take [`TaskGraph::ready_nodes`], partition into conflict
    /// groups, log `conflict_sequenced` once per multi-member group, then
    /// run groups concurrently (`join_all`) while each group's tasks run
    /// strictly sequentially. Per task: acquire a concurrency permit
    /// (FR-029; deferral is logged), transition `Pending → Ready →
    /// Dispatched`, then loop dispatch → evaluate → repair/escalate until
    /// the task reaches `Completed`, `Failed`, or `Degraded`. Every
    /// accepted transition is logged and persisted (FR-012/FR-014).
    /// Completed tasks are never re-dispatched (FR-013/SC-004).
    ///
    /// IO errors while persisting run state are reported on stderr and
    /// never abort the loop.
    pub async fn run_to_completion(
        &self,
        graph: &mut TaskGraph,
        run: &mut RunHandle,
        dispatcher: &dyn TaskDispatcher,
        gate: &dyn VerificationGate,
        workdir: &Path,
    ) -> RunStats {
        let evaluator = Evaluator::new(self.config.max_repair_attempts);
        // One ledger across the whole run: the evaluator mutates it keyed
        // by task id, so per-task repair history must survive across tasks.
        let mut ledger = RepairLedger::default();
        let mut stats = RunStats::default();

        let graph_m = Mutex::new(&mut *graph);
        let run_m = Mutex::new(&mut *run);
        let ledger_m = tokio::sync::Mutex::new(&mut ledger);
        let stats_m = Mutex::new(&mut stats);
        let semaphore = Arc::new(tokio::sync::Semaphore::new(
            self.config.max_concurrent_workers,
        ));

        loop {
            let ready = with_graph(&graph_m, |g| g.ready_nodes());
            if ready.is_empty() {
                // Nothing dispatchable: either everything terminal, or
                // permanently blocked tasks remain (FR-011 replan hook).
                let pending = with_graph(&graph_m, |g| {
                    g.nodes
                        .values()
                        .filter(|n| n.status == TaskStatus::Pending)
                        .count()
                });
                if pending > 0 {
                    with_stats(&stats_m, |s| s.blocked_remaining = pending);
                }
                break;
            }

            let groups = with_graph(&graph_m, |g| ConflictAnalyzer.partition(&ready, g));

            // Log conflict_sequenced ONCE per multi-member group, BEFORE
            // any transitions (FR-014).
            with_run(&run_m, |r| {
                for group in &groups {
                    if group.len() > 1 {
                        let ids: Vec<String> =
                            group.iter().map(|t| t.as_str().to_string()).collect();
                        let entry = DecisionEntry::new(
                            group[0].as_str(),
                            "Pending",
                            "Pending",
                            "conflict_sequenced",
                            vec![],
                            format!(
                                "serialized {} overlapping/undeclared writers: {:?}",
                                group.len(),
                                ids
                            ),
                        );
                        if let Err(e) = r.append_decision(&entry) {
                            tracing::warn!("joey-orchestration: failed to append decision: {e}");
                        }
                    }
                }
            });

            // Groups run concurrently; tasks within a group are strictly
            // sequential (process_group awaits each task fully).
            let wave = groups.into_iter().map(|group| {
                process_group(
                    group,
                    &graph_m,
                    &run_m,
                    &self.snapshot_sink,
                    &ledger_m,
                    &stats_m,
                    &semaphore,
                    &evaluator,
                    dispatcher,
                    gate,
                    workdir,
                )
            });
            futures::future::join_all(wave).await;
        }

        stats
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn with_graph<R>(m: &Mutex<&mut TaskGraph>, f: impl FnOnce(&mut TaskGraph) -> R) -> R {
    let mut g = m.lock().unwrap();
    f(&mut **g)
}

fn with_run<R>(m: &Mutex<&mut RunHandle>, f: impl FnOnce(&mut RunHandle) -> R) -> R {
    let mut r = m.lock().unwrap();
    f(&mut **r)
}

fn with_stats<R>(m: &Mutex<&mut RunStats>, f: impl FnOnce(&mut RunStats) -> R) -> R {
    let mut s = m.lock().unwrap();
    f(&mut **s)
}

/// Apply a lifecycle transition and, if accepted, persist it: decision
/// line + node snapshot + graph snapshot (FR-012/FR-014). IO errors are
/// warned about on stderr and never panic the loop.
fn transition_and_persist(
    graph_m: &Mutex<&mut TaskGraph>,
    run_m: &Mutex<&mut RunHandle>,
    sink: &Option<SnapshotSink>,
    id: &TaskId,
    to: TaskStatus,
    cause: &str,
    detail: &str,
) {
    let persisted: Option<(String, String, serde_json::Value, serde_json::Value)> = with_graph(
        graph_m,
        |g| {
            let from = match g.node(id) {
                Some(n) => n.status,
                None => {
                    tracing::warn!("joey-orchestration: scheduler referenced unknown task {id}");
                    return None;
                }
            };
            match g.transition(id, to) {
                Ok(_) => Some((
                    format!("{:?}", from),
                    format!("{:?}", to),
                    serde_json::to_value(g.node(id).expect("node just transitioned"))
                        .expect("TaskNode serialization is infallible"),
                    g.snapshot(),
                )),
                Err(e) => {
                    tracing::warn!("joey-orchestration: {e}");
                    None
                }
            }
        },
    );
    if let Some((from, to, node_json, graph_json)) = persisted {
        with_run(run_m, |r| {
            let entry =
                DecisionEntry::new(id.as_str(), from, to, cause, vec![], detail.to_string());
            if let Err(e) = r.append_decision(&entry) {
                tracing::warn!("joey-orchestration: failed to append decision: {e}");
            }
            if let Err(e) = r.write_node(id.as_str(), &node_json) {
                tracing::warn!("joey-orchestration: failed to write node snapshot: {e}");
            }
            if let Err(e) = r.write_graph(&graph_json) {
                tracing::warn!("joey-orchestration: failed to write graph snapshot: {e}");
            }
        });
        // Fire the snapshot sink (if attached) with the same JSON value
        // that was just persisted.
        if let Some(sink) = sink {
            sink(graph_json.clone());
        }
    }
}

/// One conflict group: tasks run strictly sequentially, each fully
/// dispatched/evaluated/repaired before the next begins.
#[allow(clippy::too_many_arguments)]
async fn process_group(
    group: Vec<TaskId>,
    graph_m: &Mutex<&mut TaskGraph>,
    run_m: &Mutex<&mut RunHandle>,
    sink: &Option<SnapshotSink>,
    ledger_m: &tokio::sync::Mutex<&mut RepairLedger>,
    stats_m: &Mutex<&mut RunStats>,
    semaphore: &Arc<tokio::sync::Semaphore>,
    evaluator: &Evaluator,
    dispatcher: &dyn TaskDispatcher,
    gate: &dyn VerificationGate,
    workdir: &Path,
) {
    for id in group {
        process_task(
            id,
            graph_m,
            run_m,
            sink,
            ledger_m,
            stats_m,
            semaphore,
            evaluator,
            dispatcher,
            gate,
            workdir,
        )
        .await;
    }
}

/// One task: permit acquisition, Pending→Ready→Dispatched, then the
/// dispatch → evaluate → repair/escalate loop until a terminal state.
#[allow(clippy::too_many_arguments)]
async fn process_task(
    id: TaskId,
    graph_m: &Mutex<&mut TaskGraph>,
    run_m: &Mutex<&mut RunHandle>,
    sink: &Option<SnapshotSink>,
    ledger_m: &tokio::sync::Mutex<&mut RepairLedger>,
    stats_m: &Mutex<&mut RunStats>,
    semaphore: &Arc<tokio::sync::Semaphore>,
    evaluator: &Evaluator,
    dispatcher: &dyn TaskDispatcher,
    gate: &dyn VerificationGate,
    workdir: &Path,
) {
    // FR-029: concurrency cap. If no permit is immediately available,
    // log the deferral first, then queue deterministically.
    let _permit = match semaphore.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            with_run(run_m, |r| {
                let entry = DecisionEntry::new(
                    id.as_str(),
                    "Ready",
                    "Ready",
                    "deferred_concurrency_cap",
                    vec![],
                    "queued beyond concurrency cap",
                );
                if let Err(e) = r.append_decision(&entry) {
                    tracing::warn!("joey-orchestration: failed to append decision: {e}");
                }
            });
            semaphore
                .clone()
                .acquire_owned()
                .await
                .expect("scheduler semaphore is never closed")
        }
    };

    // Pending → Ready → Dispatched, persisted per transition (FR-012).
    transition_and_persist(
        graph_m,
        run_m,
        sink,
        &id,
        TaskStatus::Ready,
        "dependency_completed",
        "dependencies satisfied",
    );
    transition_and_persist(
        graph_m,
        run_m,
        sink,
        &id,
        TaskStatus::Dispatched,
        "dependency_completed",
        "wave dispatch",
    );

    // Dispatch → evaluate → repair/escalate loop.
    loop {
        let snapshot = match with_graph(graph_m, |g| g.node(&id).map(|n| n.clone())) {
            Some(node) => node,
            None => {
                tracing::warn!("joey-orchestration: task {id} vanished mid-run");
                return;
            }
        };
        let ok = dispatcher.dispatch(&snapshot, workdir).await;
        with_stats(stats_m, |s| s.dispatched += 1);
        transition_and_persist(
            graph_m,
            run_m,
            sink,
            &id,
            TaskStatus::Evaluating,
            "worker_completed",
            "worker finished",
        );

        // Worker crashed ⇒ terminal failure, no gate evaluation.
        if !ok {
            transition_and_persist(
                graph_m,
                run_m,
                sink,
                &id,
                TaskStatus::Failed,
                "worker_completed",
                "dispatch reported failure",
            );
            with_stats(stats_m, |s| s.failed += 1);
            break;
        }

        let outcome = {
            let task = match with_graph(graph_m, |g| g.node(&id).map(|n| n.clone())) {
                Some(node) => node,
                None => {
                    tracing::warn!("joey-orchestration: task {id} vanished mid-run");
                    return;
                }
            };
            let mut ledger_guard = ledger_m.lock().await;
            evaluator
                .evaluate(&task, gate, workdir, &mut ledger_guard)
                .await
        };

        match outcome.directive {
            EvaluationDirective::Complete => {
                transition_and_persist(
                    graph_m,
                    run_m,
                    sink,
                    &id,
                    TaskStatus::Completed,
                    "gate_passed",
                    &outcome.detail,
                );
                with_stats(stats_m, |s| s.completed += 1);
                break;
            }
            EvaluationDirective::MarkDegraded => {
                transition_and_persist(
                    graph_m,
                    run_m,
                    sink,
                    &id,
                    TaskStatus::Degraded,
                    "degraded",
                    "verification unavailable (FR-031)",
                );
                with_stats(stats_m, |s| s.degraded += 1);
                break;
            }
            EvaluationDirective::Repair { .. } => {
                with_graph(graph_m, |g| {
                    if let Some(n) = g.node_mut(&id) {
                        n.attempts += 1;
                    }
                });
                transition_and_persist(
                    graph_m,
                    run_m,
                    sink,
                    &id,
                    TaskStatus::Dispatched,
                    "repair_scheduled",
                    &outcome.detail,
                );
                continue;
            }
            EvaluationDirective::Escalate { .. } => {
                with_graph(graph_m, |g| {
                    if let Some(n) = g.node_mut(&id) {
                        n.attempts += 1;
                        n.model_tier = ModelTier::Frontier;
                    }
                });
                transition_and_persist(
                    graph_m,
                    run_m,
                    sink,
                    &id,
                    TaskStatus::Dispatched,
                    "escalated",
                    &outcome.detail,
                );
                continue;
            }
            EvaluationDirective::Fail { .. } => {
                transition_and_persist(
                    graph_m,
                    run_m,
                    sink,
                    &id,
                    TaskStatus::Failed,
                    "gate_failed",
                    &outcome.detail,
                );
                with_stats(stats_m, |s| s.failed += 1);
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (T015) — fakes defined locally; evaluator's test fakes are NOT
// imported (they are private to that module anyway).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::{DefectBundle, GateOutcome, VerificationPlanView};
    use crate::task_graph::{
        AcceptanceCriterion, IsolationMode, RiskLevel, TaskGraph, WorkerRole,
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ----- fakes -------------------------------------------------------------

    /// Successful dispatcher with concurrency tracking and a call log.
    /// The log records `"{id}:start"` on entry and `"{id}:end"` on exit so
    /// tests can assert full-task serialization within a group.
    struct OkDispatcher {
        delay_ms: u64,
        current: AtomicUsize,
        max: AtomicUsize,
        calls: Mutex<Vec<String>>,
    }

    impl OkDispatcher {
        fn new() -> Self {
            Self {
                delay_ms: 0,
                current: AtomicUsize::new(0),
                max: AtomicUsize::new(0),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn with_delay(delay_ms: u64) -> Self {
            Self {
                delay_ms,
                ..Self::new()
            }
        }
    }

    #[async_trait::async_trait]
    impl TaskDispatcher for OkDispatcher {
        async fn dispatch(&self, task: &TaskNode, _workdir: &Path) -> bool {
            let cur = self.current.fetch_add(1, Ordering::SeqCst) + 1;
            self.max.fetch_max(cur, Ordering::SeqCst);
            self.calls
                .lock()
                .unwrap()
                .push(format!("{}:start", task.id.as_str()));
            if self.delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)).await;
            }
            self.calls
                .lock()
                .unwrap()
                .push(format!("{}:end", task.id.as_str()));
            self.current.fetch_sub(1, Ordering::SeqCst);
            true
        }
    }

    /// Dispatcher whose workers always crash.
    struct CrashDispatcher;

    #[async_trait::async_trait]
    impl TaskDispatcher for CrashDispatcher {
        async fn dispatch(&self, _task: &TaskNode, _workdir: &Path) -> bool {
            false
        }
    }

    /// Dispatcher that counts calls (used to prove nothing is re-executed).
    struct CountingDispatcher {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl TaskDispatcher for CountingDispatcher {
        async fn dispatch(&self, _task: &TaskNode, _workdir: &Path) -> bool {
            self.calls.fetch_add(1, Ordering::SeqCst);
            true
        }
    }

    struct PassGate;

    #[async_trait::async_trait]
    impl VerificationGate for PassGate {
        async fn run(&self, _plan: &VerificationPlanView, _workdir: &Path) -> GateOutcome {
            GateOutcome::Passed
        }
    }

    struct DegradeGate;

    #[async_trait::async_trait]
    impl VerificationGate for DegradeGate {
        async fn run(&self, _plan: &VerificationPlanView, _workdir: &Path) -> GateOutcome {
            GateOutcome::Degraded
        }
    }

    struct FailGate;

    #[async_trait::async_trait]
    impl VerificationGate for FailGate {
        async fn run(&self, _plan: &VerificationPlanView, _workdir: &Path) -> GateOutcome {
            GateOutcome::Failed(DefectBundle::default())
        }
    }

    /// Fails the first `n` evaluations, then passes.
    struct FailNTimesGate(AtomicUsize);

    #[async_trait::async_trait]
    impl VerificationGate for FailNTimesGate {
        async fn run(&self, _plan: &VerificationPlanView, _workdir: &Path) -> GateOutcome {
            if self.0.fetch_sub(1, Ordering::SeqCst) > 0 {
                GateOutcome::Failed(DefectBundle::default())
            } else {
                GateOutcome::Passed
            }
        }
    }

    // ----- helpers -----------------------------------------------------------

    fn node(id: &str, write: &[&str]) -> TaskNode {
        TaskNode {
            id: TaskId::new(id).unwrap(),
            objective: format!("do {id}"),
            dependencies: vec![],
            read_set: vec![],
            write_set: write.iter().map(PathBuf::from).collect(),
            artifact_ids: vec![],
            role: WorkerRole::Implementor,
            model_tier: ModelTier::Economical,
            risk: RiskLevel::Low,
            acceptance: vec![AcceptanceCriterion {
                criterion: "done".to_string(),
                kind: "command".to_string(),
            }],
            verification: VerificationPlanView::default(),
            isolation: IsolationMode::SharedCheckout,
            status: TaskStatus::Pending,
            attempts: 0,
        }
    }

    fn graph_of(nodes: Vec<TaskNode>) -> TaskGraph {
        TaskGraph {
            nodes: nodes.into_iter().map(|n| (n.id.clone(), n)).collect(),
            baseline_revision: String::new(),
            run_id: String::new(),
        }
    }

    fn run_handle(tmp: &Path) -> RunHandle {
        RunHandle::create_at(tmp, "run-1", "base").unwrap()
    }

    fn ids(names: &[&str]) -> Vec<TaskId> {
        names.iter().map(|s| TaskId::new(s).unwrap()).collect()
    }

    fn tid(name: &str) -> TaskId {
        TaskId::new(name).unwrap()
    }

    // ----- ConflictAnalyzer ---------------------------------------------------

    #[test]
    fn partition_groups_overlapping_writers() {
        let graph = graph_of(vec![
            node("a", &["src/x.rs"]),
            node("b", &["src/x.rs"]),
            node("c", &["src/other.rs"]),
        ]);
        let groups = ConflictAnalyzer.partition(&ids(&["a", "b", "c"]), &graph);
        assert_eq!(groups, vec![ids(&["a", "b"]), ids(&["c"])]);

        // Two undeclared (empty write set) tasks conflict with everyone —
        // including each other and any declared writer — so all three
        // merge into one group.
        let graph = graph_of(vec![
            node("x", &[]),
            node("y", &["src/y.rs"]),
            node("z", &["src/z.rs"]),
        ]);
        let groups = ConflictAnalyzer.partition(&ids(&["x", "y", "z"]), &graph);
        assert_eq!(groups, vec![ids(&["x", "y", "z"])]);
    }

    // ----- wave loop ----------------------------------------------------------

    #[tokio::test]
    async fn completes_diamond() {
        let tmp = tempfile::tempdir().unwrap();
        let a = node("a", &["src/a.rs"]);
        let mut b = node("b", &["src/b.rs"]);
        b.dependencies = ids(&["a"]);
        let mut c = node("c", &["src/c.rs"]);
        c.dependencies = ids(&["a"]);
        let mut d = node("d", &["src/d.rs"]);
        d.dependencies = ids(&["b", "c"]);
        let mut graph = graph_of(vec![a, b, c, d]);
        let mut run = run_handle(tmp.path());

        let scheduler = Scheduler::new(SchedulerConfig::default());
        let stats = scheduler
            .run_to_completion(&mut graph, &mut run, &OkDispatcher::new(), &PassGate, tmp.path())
            .await;

        assert_eq!(stats.completed, 4);
        assert_eq!(stats.dispatched, 4);
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.degraded, 0);
        assert_eq!(stats.blocked_remaining, 0);
        for id in ["a", "b", "c", "d"] {
            assert_eq!(graph.nodes[&tid(id)].status, TaskStatus::Completed);
        }

        let decisions = run.read_decisions().unwrap();
        for id in ["a", "b", "c", "d"] {
            for cause in ["dependency_completed", "worker_completed", "gate_passed"] {
                assert!(
                    decisions.iter().any(|d| d.task_id == id && d.cause == cause),
                    "missing {cause} decision for {id}"
                );
            }
        }
    }

    #[tokio::test]
    async fn snapshot_sink_observes_transitions() {
        let tmp = tempfile::tempdir().unwrap();
        let a = node("a", &["src/a.rs"]);
        let mut b = node("b", &["src/b.rs"]);
        b.dependencies = ids(&["a"]);
        let mut graph = graph_of(vec![a, b]);
        let mut run = run_handle(tmp.path());

        let captured: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_capture = captured.clone();
        let scheduler = Scheduler::new(SchedulerConfig::default()).with_snapshot_sink(Arc::new(
            move |v: serde_json::Value| {
                sink_capture.lock().unwrap().push(v);
            },
        ));
        let stats = scheduler
            .run_to_completion(&mut graph, &mut run, &OkDispatcher::new(), &PassGate, tmp.path())
            .await;

        assert_eq!(stats.completed, 2);

        let snapshots = captured.lock().unwrap();
        assert!(
            !snapshots.is_empty(),
            "sink must receive at least one snapshot"
        );
        let last = snapshots.last().unwrap().clone();
        let final_graph: TaskGraph = serde_json::from_value(last)
            .expect("last snapshot deserializes back into a TaskGraph");
        assert!(
            final_graph.nodes.values().all(|n| n.status.is_terminal()),
            "all nodes must be terminal (Completed/Failed/Skipped) in the last snapshot"
        );
        assert_eq!(final_graph.nodes.len(), 2);
    }

    #[tokio::test]
    async fn overlapping_writers_never_concurrent() {
        let tmp = tempfile::tempdir().unwrap();
        let mut graph = graph_of(vec![node("a", &["src/x.rs"]), node("b", &["src/x.rs"])]);
        let mut run = run_handle(tmp.path());
        let dispatcher = OkDispatcher::new();

        let scheduler = Scheduler::new(SchedulerConfig::default());
        let stats = scheduler
            .run_to_completion(&mut graph, &mut run, &dispatcher, &PassGate, tmp.path())
            .await;

        assert_eq!(stats.completed, 2);
        assert_eq!(dispatcher.max.load(Ordering::SeqCst), 1);
        // Same group ⇒ strictly sequential: a fully completes before b starts.
        assert_eq!(
            *dispatcher.calls.lock().unwrap(),
            vec!["a:start", "a:end", "b:start", "b:end"]
        );
    }

    #[tokio::test]
    async fn independent_tasks_run_concurrently() {
        let tmp = tempfile::tempdir().unwrap();
        let mut graph = graph_of(vec![
            node("a", &["src/a.rs"]),
            node("b", &["src/b.rs"]),
            node("c", &["src/c.rs"]),
        ]);
        let mut run = run_handle(tmp.path());
        let dispatcher = OkDispatcher::with_delay(50);

        let scheduler = Scheduler::new(SchedulerConfig {
            max_concurrent_workers: 4,
            max_repair_attempts: 3,
        });
        let stats = scheduler
            .run_to_completion(&mut graph, &mut run, &dispatcher, &PassGate, tmp.path())
            .await;

        assert_eq!(stats.completed, 3);
        assert!(
            dispatcher.max.load(Ordering::SeqCst) >= 2,
            "independent groups must overlap, saw max {}",
            dispatcher.max.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn concurrency_cap_defers_and_logs() {
        let tmp = tempfile::tempdir().unwrap();
        let mut graph = graph_of(vec![
            node("a", &["src/a.rs"]),
            node("b", &["src/b.rs"]),
            node("c", &["src/c.rs"]),
        ]);
        let mut run = run_handle(tmp.path());
        let dispatcher = OkDispatcher::with_delay(50);

        let scheduler = Scheduler::new(SchedulerConfig {
            max_concurrent_workers: 2,
            max_repair_attempts: 3,
        });
        let stats = scheduler
            .run_to_completion(&mut graph, &mut run, &dispatcher, &PassGate, tmp.path())
            .await;

        assert_eq!(stats.completed, 3);
        assert!(dispatcher.max.load(Ordering::SeqCst) <= 2);
        let decisions = run.read_decisions().unwrap();
        assert!(
            decisions
                .iter()
                .filter(|d| d.cause == "deferred_concurrency_cap")
                .count()
                >= 1
        );
    }

    #[tokio::test]
    async fn crash_fails_task() {
        let tmp = tempfile::tempdir().unwrap();
        let mut graph = graph_of(vec![node("t1", &["src/t1.rs"])]);
        let mut dependent = node("t2", &["src/t2.rs"]);
        dependent.dependencies = ids(&["t1"]);
        graph.nodes.insert(dependent.id.clone(), dependent);
        let mut run = run_handle(tmp.path());

        let scheduler = Scheduler::new(SchedulerConfig::default());
        let stats = scheduler
            .run_to_completion(&mut graph, &mut run, &CrashDispatcher, &PassGate, tmp.path())
            .await;

        assert_eq!(stats.failed, 1);
        assert_eq!(stats.completed, 0);
        assert_eq!(graph.nodes[&tid("t1")].status, TaskStatus::Failed);
        assert_eq!(graph.nodes[&tid("t2")].status, TaskStatus::Pending);
        assert_eq!(stats.blocked_remaining, 1);
    }

    #[tokio::test]
    async fn repair_then_complete() {
        let tmp = tempfile::tempdir().unwrap();
        let mut graph = graph_of(vec![node("t1", &["src/t1.rs"])]);
        let mut run = run_handle(tmp.path());
        let gate = FailNTimesGate(AtomicUsize::new(1));

        let scheduler = Scheduler::new(SchedulerConfig {
            max_concurrent_workers: 16,
            max_repair_attempts: 2,
        });
        let stats = scheduler
            .run_to_completion(&mut graph, &mut run, &OkDispatcher::new(), &gate, tmp.path())
            .await;

        assert_eq!(stats.completed, 1);
        assert_eq!(stats.failed, 0);
        assert_eq!(graph.nodes[&tid("t1")].status, TaskStatus::Completed);
        assert_eq!(graph.nodes[&tid("t1")].attempts, 1);
        assert!(
            run.read_decisions()
                .unwrap()
                .iter()
                .any(|d| d.task_id == "t1" && d.cause == "repair_scheduled")
        );
    }

    #[tokio::test]
    async fn escalate_then_fail() {
        let tmp = tempfile::tempdir().unwrap();
        let mut graph = graph_of(vec![node("t1", &["src/t1.rs"])]);
        let mut run = run_handle(tmp.path());

        let scheduler = Scheduler::new(SchedulerConfig {
            max_concurrent_workers: 16,
            max_repair_attempts: 1,
        });
        let stats = scheduler
            .run_to_completion(&mut graph, &mut run, &OkDispatcher::new(), &FailGate, tmp.path())
            .await;

        assert_eq!(stats.failed, 1);
        assert_eq!(stats.completed, 0);
        assert_eq!(graph.nodes[&tid("t1")].status, TaskStatus::Failed);
        assert_eq!(graph.nodes[&tid("t1")].model_tier, ModelTier::Frontier);
        // NOTE: the committed Evaluator resets the per-tier budget on
        // escalation, so after Escalate there is one more Repair at the
        // Frontier tier before terminal Fail: 1 repair + 1 escalate + 1
        // frontier repair = 3 attempts (the brief sketched 2).
        assert_eq!(graph.nodes[&tid("t1")].attempts, 3);
        let decisions = run.read_decisions().unwrap();
        assert!(
            decisions
                .iter()
                .any(|d| d.task_id == "t1" && d.cause == "escalated")
        );
        assert!(
            decisions
                .iter()
                .any(|d| d.task_id == "t1" && d.cause == "gate_failed")
        );
    }

    #[tokio::test]
    async fn degraded_blocks_dependents() {
        let tmp = tempfile::tempdir().unwrap();
        let mut graph = graph_of(vec![node("t1", &["src/t1.rs"])]);
        let mut dependent = node("t2", &["src/t2.rs"]);
        dependent.dependencies = ids(&["t1"]);
        graph.nodes.insert(dependent.id.clone(), dependent);
        let mut run = run_handle(tmp.path());

        let scheduler = Scheduler::new(SchedulerConfig::default());
        let stats = scheduler
            .run_to_completion(&mut graph, &mut run, &OkDispatcher::new(), &DegradeGate, tmp.path())
            .await;

        assert_eq!(stats.degraded, 1);
        assert_eq!(stats.completed, 0);
        assert_eq!(graph.nodes[&tid("t1")].status, TaskStatus::Degraded);
        assert_eq!(graph.nodes[&tid("t2")].status, TaskStatus::Pending);
        assert!(stats.blocked_remaining >= 1);
        assert!(needs_replan(&graph));
    }

    #[tokio::test]
    async fn completed_not_reexecuted_on_rerun() {
        let tmp = tempfile::tempdir().unwrap();
        let a = node("a", &["src/a.rs"]);
        let mut b = node("b", &["src/b.rs"]);
        b.dependencies = ids(&["a"]);
        let mut graph = graph_of(vec![a, b]);
        let mut run = run_handle(tmp.path());

        let scheduler = Scheduler::new(SchedulerConfig::default());
        let first = scheduler
            .run_to_completion(&mut graph, &mut run, &OkDispatcher::new(), &PassGate, tmp.path())
            .await;
        assert_eq!(first.completed, 2);

        // Re-run over the finished graph with a counting dispatcher:
        // nothing may be dispatched again (SC-004).
        let counting = CountingDispatcher {
            calls: AtomicUsize::new(0),
        };
        let second = scheduler
            .run_to_completion(&mut graph, &mut run, &counting, &PassGate, tmp.path())
            .await;
        assert_eq!(counting.calls.load(Ordering::SeqCst), 0);
        assert_eq!(second.dispatched, 0);
        assert_eq!(second.completed, 0);
        assert_eq!(graph.nodes[&tid("a")].status, TaskStatus::Completed);
        assert_eq!(graph.nodes[&tid("b")].status, TaskStatus::Completed);
    }

    #[tokio::test]
    async fn persist_smoke() {
        let tmp = tempfile::tempdir().unwrap();
        let mut graph = graph_of(vec![node("t1", &["src/t1.rs"])]);
        let mut run = run_handle(tmp.path());

        let scheduler = Scheduler::new(SchedulerConfig::default());
        scheduler
            .run_to_completion(&mut graph, &mut run, &OkDispatcher::new(), &PassGate, tmp.path())
            .await;

        let raw = std::fs::read_to_string(run.run_dir().join("graph.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["nodes"]["t1"]["status"], serde_json::json!("completed"));
    }
}

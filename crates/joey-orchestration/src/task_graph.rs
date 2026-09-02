//! Typed task graph core (spec 023, T011 / US2).
//!
//! This module defines the typed plan the orchestrator executes: a
//! [`TaskGraph`] of [`TaskNode`]s with dependencies, read/write sets,
//! acceptance criteria, and a verification plan view. It enforces the six
//! structural invariants from the spec's data model (see [`TaskGraph::validate`])
//! and the task lifecycle state machine (see [`TaskGraph::transition`]).
//!
//! No `joey-neurocode` imports are used here by design: the small enums
//! ([`WorkerRole`], [`ModelTier`], [`RiskLevel`]) intentionally mirror
//! neurocode's role/complexity/risk enums but are duplicated locally so the
//! orchestration plan types stand on their own. Do not replace them with
//! `joey_neurocode` re-exports.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::evaluator::VerificationPlanView;

/// Validated task identifier. Restricted to `[a-z0-9-]+` (non-empty) so ids
/// are safe to embed in branch names, log lines, and serialized map keys.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct TaskId(String);

impl TaskId {
    /// Creates a `TaskId` from `s`, or `None` if `s` is empty or contains any
    /// character outside `[a-z0-9-]`. Checked char-by-char (no regex crate).
    pub fn new(s: &str) -> Option<Self> {
        if s.is_empty() {
            return None;
        }
        if !s
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return None;
        }
        Some(TaskId(s.to_string()))
    }

    /// The identifier as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for TaskId {
    fn from(s: String) -> Self {
        TaskId(s)
    }
}

/// Lifecycle state of a task (FR-006 task lifecycle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Not yet dispatchable; dependencies unresolved.
    #[default]
    Pending,
    /// Dependencies satisfied; waiting for dispatch.
    Ready,
    /// Handed to a worker; running.
    Dispatched,
    /// Worker finished; verification gate evaluating.
    Evaluating,
    /// Verification passed cleanly.
    Completed,
    /// Verification failed irrecoverably.
    Failed,
    /// Passed with unverified-but-accepted caveats (FR-031).
    Degraded,
    /// Waiting on an external resolution (replan, human input).
    Blocked,
    /// Explicitly skipped (still terminal).
    Skipped,
}

impl TaskStatus {
    /// Terminal states are `Completed`, `Failed`, and `Skipped` — once a task
    /// reaches one of these it never transitions again.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Skipped
        )
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Ready => "ready",
            TaskStatus::Dispatched => "dispatched",
            TaskStatus::Evaluating => "evaluating",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Degraded => "degraded",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Skipped => "skipped",
        };
        f.write_str(s)
    }
}

/// Source-control isolation a task runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IsolationMode {
    /// Run in the shared checkout (read-only or trivially safe work).
    #[default]
    SharedCheckout,
    /// Run in a dedicated git worktree so writes never collide.
    IsolatedWorktree,
}

/// Worker archetype for a task. Mirrors neurocode's role enum locally;
/// serialized lowercase (`implementor`, …) to match planner JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkerRole {
    /// Exploration / research worker.
    Explorer,
    /// Heads-down implementation worker.
    Implementor,
    /// Coordination / planning worker.
    Orchestrator,
}

/// Model class a task requires. Mirrors neurocode's `ComplexityTier` minus
/// the ambiguous variant — planner JSON never carries "ambiguous", so the
/// typed plan does not model it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelTier {
    /// Cheap, fast models.
    Economical,
    /// Frontier-class models.
    Frontier,
}

impl ModelTier {
    /// Position on the FR-021 exhaustion ladder: when a higher tier is
    /// exhausted or budget-capped, selection falls back toward rank 0.
    /// `Economical` = 0, `Frontier` = 1.
    pub fn rank(self) -> u8 {
        match self {
            ModelTier::Economical => 0,
            ModelTier::Frontier => 1,
        }
    }
}

/// Risk classification driving verification strictness (FR-009/FR-010:
/// high risk requires a required verification step or a triggered review).
/// Mirrors neurocode's risk enum locally — the DAG constraint lives here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

/// One acceptance criterion a task must satisfy before its verification gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptanceCriterion {
    /// Human-readable statement of the criterion.
    pub criterion: String,
    /// Machine-checkable kind (e.g. `command`, `file_exists`).
    pub kind: String,
}

/// A single node of the typed task graph (FR-006).
///
/// `status` and `attempts` are *runtime* state: the planner JSON never
/// carries them, hence the serde defaults (pending / 0). `artifact_ids` are
/// neurocode `NodeId`s (u64) of the artifact-graph nodes this task touches;
/// they are opaque to orchestration. `verification` reuses the evaluator's
/// read-only [`VerificationPlanView`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskNode {
    /// This task's validated identifier.
    pub id: TaskId,
    /// What the task is meant to accomplish.
    pub objective: String,
    /// Tasks that must reach `Completed` before this one dispatches.
    pub dependencies: Vec<TaskId>,
    /// Repository-relative paths this task reads.
    pub read_set: Vec<PathBuf>,
    /// Repository-relative paths this task writes.
    pub write_set: Vec<PathBuf>,
    /// neurocode NodeIds (u64) of artifacts produced/consumed.
    pub artifact_ids: Vec<u64>,
    /// Worker archetype for dispatch.
    pub role: WorkerRole,
    /// Required model class (FR-021 ladder).
    pub model_tier: ModelTier,
    /// Risk classification (drives verification strictness).
    pub risk: RiskLevel,
    /// Criteria the verification gate checks.
    pub acceptance: Vec<AcceptanceCriterion>,
    /// Verification plan view (required for high-risk tasks).
    pub verification: VerificationPlanView,
    /// Source-control isolation mode.
    pub isolation: IsolationMode,
    /// Runtime lifecycle state (absent from planner JSON).
    #[serde(default)]
    pub status: TaskStatus,
    /// Runtime dispatch attempt counter (absent from planner JSON).
    #[serde(default)]
    pub attempts: u32,
}

/// A structural validation violation. FR-009/FR-010: every rejection names
/// the offending task ids plus a machine-readable rule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ValidationError {
    /// Ids of the tasks implicated (missing ids included for unknown deps).
    pub task_ids: Vec<String>,
    /// Machine-readable rule constant (see [`rules`]).
    pub rule: String,
    /// Human-readable explanation.
    pub detail: String,
}

/// Machine-readable validation rule identifiers.
pub mod rules {
    /// Dependency cycle detected.
    pub const CYCLE: &str = "dependency_cycle";
    /// A dependency references a nonexistent task id.
    pub const UNKNOWN_DEP: &str = "unknown_dependency";
    /// Two concurrently-dispatchable tasks share a write-set path.
    pub const WRITE_OVERLAP: &str = "concurrent_write_overlap";
    /// A read/write path escapes the project root.
    pub const PATH_ESCAPE: &str = "path_outside_project_root";
    /// A task has no acceptance criterion.
    pub const NO_ACCEPTANCE: &str = "missing_acceptance_criterion";
    /// High-risk task without required verification.
    pub const UNVERIFIED_HIGH_RISK: &str = "high_risk_without_verification";
    /// Strict planner JSON rejected: missing or unknown `format` tag
    /// (FR-008/FR-010 strict-format rejections).
    pub const INVALID_FORMAT: &str = "unsupported_format";
    /// Strict planner JSON carried a task id outside the `[a-z0-9-]+`
    /// charset (FR-008/FR-010).
    pub const INVALID_TASK_ID: &str = "invalid_task_id";
    /// Strict planner JSON violated the task schema; the detail string
    /// names the offending field where serde reports one (FR-008/FR-010).
    pub const SCHEMA: &str = "schema_violation";
}

/// The typed, executable plan: a map of tasks keyed by id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TaskGraph {
    /// All tasks, keyed by id (BTreeMap ⇒ deterministic serialization).
    pub nodes: BTreeMap<TaskId, TaskNode>,
    /// Git revision the plan was built against.
    #[serde(default)]
    pub baseline_revision: String,
    /// Identifier of the run executing this graph.
    #[serde(default)]
    pub run_id: String,
}

impl TaskGraph {
    /// Checks the six structural invariants (data-model.md):
    ///
    /// 1. no dependency cycles (`rules::CYCLE`),
    /// 2. every dependency references an existing id (`rules::UNKNOWN_DEP`),
    /// 3. no two concurrently-dispatchable tasks share a write-set path
    ///    (`rules::WRITE_OVERLAP`) — ancestor-related pairs are sequenced and
    ///    therefore legal,
    /// 4. all read/write paths are relative, normalized, and inside the
    ///    project root (`rules::PATH_ESCAPE`),
    /// 5. every task has at least one acceptance criterion
    ///    (`rules::NO_ACCEPTANCE`),
    /// 6. high-risk tasks have a required verification step or a
    ///    risk-triggered review (`rules::UNVERIFIED_HIGH_RISK`).
    ///
    /// All violations are collected (no short-circuit). An empty graph is
    /// valid. `Ok(())` iff no violations.
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();

        // (1) dependency cycles.
        if let Some(cycle) = find_cycle(&self.nodes) {
            errors.push(ValidationError {
                task_ids: cycle,
                rule: rules::CYCLE.to_string(),
                detail: "dependency cycle detected".to_string(),
            });
        }

        // (2) every dependency references an existing id.
        for (id, node) in &self.nodes {
            for dep in &node.dependencies {
                if !self.nodes.contains_key(dep) {
                    errors.push(ValidationError {
                        task_ids: vec![dep.to_string(), id.to_string()],
                        rule: rules::UNKNOWN_DEP.to_string(),
                        detail: format!("task {} depends on unknown task {}", id, dep),
                    });
                }
            }
        }

        // (3) no two concurrently-dispatchable tasks share a write-set path.
        let ids: Vec<&TaskId> = self.nodes.keys().collect();
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                let (a, b) = (ids[i], ids[j]);
                // Ancestor-related in either direction ⇒ sequenced ⇒ legal.
                if reaches(a, b, &self.nodes) || reaches(b, a, &self.nodes) {
                    continue;
                }
                let writes_a: HashSet<&Path> =
                    self.nodes[a].write_set.iter().map(|p| p.as_path()).collect();
                if let Some(shared) = self.nodes[b]
                    .write_set
                    .iter()
                    .find(|p| writes_a.contains(p.as_path()))
                {
                    errors.push(ValidationError {
                        task_ids: vec![a.to_string(), b.to_string()],
                        rule: rules::WRITE_OVERLAP.to_string(),
                        detail: format!(
                            "tasks {} and {} can be dispatched concurrently and both write {}",
                            a,
                            b,
                            shared.display()
                        ),
                    });
                }
            }
        }

        // (4) read/write paths stay inside the project root.
        for (id, node) in &self.nodes {
            for path in node.read_set.iter().chain(node.write_set.iter()) {
                if let Some(reason) = path_violation(path) {
                    errors.push(ValidationError {
                        task_ids: vec![id.to_string()],
                        rule: rules::PATH_ESCAPE.to_string(),
                        detail: format!("path {} rejected: {}", path.display(), reason),
                    });
                }
            }
        }

        // (5) at least one acceptance criterion per task.
        for (id, node) in &self.nodes {
            if node.acceptance.is_empty() {
                errors.push(ValidationError {
                    task_ids: vec![id.to_string()],
                    rule: rules::NO_ACCEPTANCE.to_string(),
                    detail: format!("task {} has no acceptance criterion", id),
                });
            }
        }

        // (6) high risk ⇒ required verification step or triggered review.
        for (id, node) in &self.nodes {
            if node.risk == RiskLevel::High
                && node.verification.required_steps().is_empty()
                && !node.verification.risk_triggered_review
            {
                errors.push(ValidationError {
                    task_ids: vec![id.to_string()],
                    rule: rules::UNVERIFIED_HIGH_RISK.to_string(),
                    detail: format!(
                        "task {} is high risk but has no required verification step and no risk-triggered review",
                        id
                    ),
                });
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Tasks with status `Pending` whose dependencies are all `Completed`.
    ///
    /// Deterministic order: topological (Kahn with alphabetical tie-break),
    /// then id ascending within the same depth; tasks with no dependencies
    /// are depth 0.
    pub fn ready_nodes(&self) -> Vec<TaskId> {
        // Kahn's algorithm over existing edges; a BTreeSet frontier gives the
        // alphabetical tie-break. depth(v) = 1 + max(depth(dependencies)).
        let mut indegree: BTreeMap<&TaskId, usize> = self
            .nodes
            .iter()
            .map(|(id, node)| {
                let known = node
                    .dependencies
                    .iter()
                    .filter(|d| self.nodes.contains_key(*d))
                    .count();
                (id, known)
            })
            .collect();
        let mut dependents: BTreeMap<&TaskId, Vec<&TaskId>> = BTreeMap::new();
        for (id, node) in &self.nodes {
            for dep in &node.dependencies {
                if self.nodes.contains_key(dep) {
                    dependents.entry(dep).or_default().push(id);
                }
            }
        }
        let mut depth: BTreeMap<&TaskId, usize> =
            self.nodes.keys().map(|k| (k, 0)).collect();
        let mut frontier: BTreeSet<&TaskId> = indegree
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(k, _)| *k)
            .collect();
        while let Some(u) = frontier.iter().next().copied() {
            frontier.remove(&u);
            let u_depth = *depth.get(u).unwrap_or(&0);
            if let Some(children) = dependents.get(u) {
                for v in children {
                    if let Some(d) = depth.get_mut(*v) {
                        *d = (*d).max(u_depth + 1);
                    }
                    if let Some(deg) = indegree.get_mut(*v) {
                        *deg -= 1;
                        if *deg == 0 {
                            frontier.insert(*v);
                        }
                    }
                }
            }
        }

        let mut ready: Vec<(usize, TaskId)> = self
            .nodes
            .iter()
            .filter(|(_, node)| node.status == TaskStatus::Pending)
            .filter(|(_, node)| {
                node.dependencies.iter().all(|d| {
                    self.nodes
                        .get(d)
                        .map(|dn| dn.status == TaskStatus::Completed)
                        .unwrap_or(false)
                })
            })
            .map(|(id, _)| (*depth.get(id).unwrap_or(&0), id.clone()))
            .collect();
        ready.sort();
        ready.into_iter().map(|(_, id)| id).collect()
    }

    /// Whether the task is in a terminal state. Unknown ids ⇒ `true`
    /// (treated as done so dependents never deadlock on a missing task).
    pub fn is_terminal(&self, id: &TaskId) -> bool {
        match self.nodes.get(id) {
            Some(node) => node.status.is_terminal(),
            None => true,
        }
    }

    /// Whether the task is Pending with at least one dependency that is
    /// neither `Completed` nor `Skipped` (an unknown dependency counts as
    /// unresolved). Unknown ids ⇒ `false`.
    pub fn is_blocked(&self, id: &TaskId) -> bool {
        let Some(node) = self.nodes.get(id) else {
            return false;
        };
        if node.status != TaskStatus::Pending {
            return false;
        }
        node.dependencies.iter().any(|d| {
            !matches!(
                self.nodes.get(d).map(|dn| dn.status),
                Some(TaskStatus::Completed) | Some(TaskStatus::Skipped)
            )
        })
    }

    /// Applies a lifecycle transition, enforcing the legal edge set:
    ///
    /// - `Pending → Ready`
    /// - `Ready → Dispatched`
    /// - `Dispatched → Evaluating`
    /// - `Evaluating → {Completed, Failed, Degraded, Blocked, Dispatched}`
    ///   (the `Evaluating → Dispatched` edge covers repair/escalate
    ///   re-dispatch)
    /// - `Degraded → Evaluating` (gate re-run after `override_acknowledged`
    ///   or a runnable command) and `Degraded → Blocked`
    /// - `Blocked → Ready` (replanned)
    /// - any non-terminal state → `Skipped`
    ///
    /// Terminal states are immutable. Incrementing `attempts` is the
    /// scheduler's job, not `transition`'s. Unknown ids ⇒ `Err`.
    pub fn transition(&mut self, id: &TaskId, to: TaskStatus) -> Result<TaskStatus, String> {
        let Some(node) = self.nodes.get_mut(id) else {
            return Err(format!("unknown task {}", id));
        };
        let from = node.status;
        let legal = match (from, to) {
            (TaskStatus::Pending, TaskStatus::Ready) => true,
            (TaskStatus::Ready, TaskStatus::Dispatched) => true,
            (TaskStatus::Dispatched, TaskStatus::Evaluating) => true,
            (TaskStatus::Evaluating, TaskStatus::Completed)
            | (TaskStatus::Evaluating, TaskStatus::Failed)
            | (TaskStatus::Evaluating, TaskStatus::Degraded)
            | (TaskStatus::Evaluating, TaskStatus::Blocked)
            | (TaskStatus::Evaluating, TaskStatus::Dispatched) => true,
            (TaskStatus::Degraded, TaskStatus::Evaluating)
            | (TaskStatus::Degraded, TaskStatus::Blocked) => true,
            (TaskStatus::Blocked, TaskStatus::Ready) => true,
            (f, TaskStatus::Skipped) if !f.is_terminal() => true,
            _ => false,
        };
        if !legal {
            return Err(format!(
                "illegal transition {} -> {} for task {}",
                from, to, id
            ));
        }
        node.status = to;
        Ok(to)
    }

    /// Full-graph JSON snapshot for events/persistence. Infallible for these
    /// types (all fields serialize unconditionally).
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("TaskGraph serialization is infallible")
    }

    /// Borrow a node by id.
    pub fn node(&self, id: &TaskId) -> Option<&TaskNode> {
        self.nodes.get(id)
    }

    /// Mutably borrow a node by id.
    pub fn node_mut(&mut self, id: &TaskId) -> Option<&mut TaskNode> {
        self.nodes.get_mut(id)
    }
}

impl TaskGraph {
    /// The strict planner JSON format tag this parser accepts.
    const FORMAT: &str = "joey-taskgraph/1";

    /// Parses strict planner JSON (`planner-json-format.md`, spec 023 US2)
    /// into a validated [`TaskGraph`].
    ///
    /// Strict means strict (FR-008/FR-010): the top level must be an
    /// object carrying `"format": "joey-taskgraph/1"` (anything else ⇒
    /// [`rules::INVALID_FORMAT`]), task ids are validated against the
    /// `[a-z0-9-]+` charset before typing ([`rules::INVALID_TASK_ID`]),
    /// and any serde mismatch is rejected as [`rules::SCHEMA`] with the
    /// serde error string as detail. Read/write paths must be relative,
    /// free of `..` components, and resolve inside `project_root`
    /// ([`rules::PATH_ESCAPE`]); backslashes are normalized to forward
    /// slashes before the check and the stored path is the normalized
    /// relative form. A task object without an `isolation` key gets
    /// [`default_isolation`] applied (isolated worktree iff it writes).
    ///
    /// `baseline_revision` may be missing (empty string) — the planner
    /// may omit it for uncommitted baselines; [`TaskGraph::validate`]
    /// remains the authority on plan content.
    ///
    /// All violations are collected: parse errors first, then any
    /// errors from [`TaskGraph::validate`] merged in. `Ok` iff the JSON
    /// is well-formed *and* the assembled graph validates.
    pub fn from_strict_json(
        json: &str,
        project_root: &Path,
    ) -> Result<TaskGraph, Vec<ValidationError>> {
        let _ = project_root; // containment is enforced by validate() + normalize paths
        let value: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(e) => {
                return Err(vec![ValidationError {
                    task_ids: vec![],
                    rule: rules::INVALID_FORMAT.to_string(),
                    detail: format!("expected joey-taskgraph/1, got {:?}", e.to_string()),
                }])
            }
        };
        let obj = match value.as_object() {
            Some(o) => o,
            None => {
                return Err(vec![ValidationError {
                    task_ids: vec![],
                    rule: rules::INVALID_FORMAT.to_string(),
                    detail: "expected joey-taskgraph/1, got non-object document".to_string(),
                }])
            }
        };
        let actual = obj.get("format").and_then(|f| f.as_str());
        if actual != Some(Self::FORMAT) {
            let got = match obj.get("format") {
                Some(v) => v.to_string(),
                None => "missing".to_string(),
            };
            return Err(vec![ValidationError {
                task_ids: vec![],
                rule: rules::INVALID_FORMAT.to_string(),
                detail: format!("expected joey-taskgraph/1, got {:?}", got),
            }]);
        }
        let baseline_revision = obj
            .get("baseline_revision")
            .and_then(|b| b.as_str())
            .unwrap_or("")
            .to_string();
        let empty_tasks: Vec<serde_json::Value> = Vec::new();
        let tasks = obj
            .get("tasks")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or(empty_tasks);

        let mut errors: Vec<ValidationError> = Vec::new();
        let mut nodes: BTreeMap<TaskId, TaskNode> = BTreeMap::new();
        for entry in tasks {
            let mut entry = entry;
            // isolation absent ⇒ default_isolation(&write_set). Injected
            // into the raw value BEFORE typing because `TaskNode`'s serde
            // shape requires the key (no serde default on the field).
            if entry.get("isolation").is_none() {
                let writes = entry
                    .get("write_set")
                    .and_then(|w| w.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let dummy: Vec<PathBuf> = (0..writes).map(|_| PathBuf::new()).collect();
                if let Some(obj) = entry.as_object_mut() {
                    obj.insert(
                        "isolation".to_string(),
                        serde_json::to_value(default_isolation(&dummy))
                            .expect("IsolationMode serialization is infallible"),
                    );
                }
            }
            let raw_id = entry.get("id").and_then(|i| i.as_str()).unwrap_or("");
            let Some(task_id) = TaskId::new(raw_id) else {
                errors.push(ValidationError {
                    task_ids: vec![raw_id.to_string()],
                    rule: rules::INVALID_TASK_ID.to_string(),
                    detail: format!("task id {:?} violates charset [a-z0-9-]+", raw_id),
                });
                continue;
            };
            let mut node: TaskNode = match serde_json::from_value(entry.clone()) {
                Ok(n) => n,
                Err(e) => {
                    errors.push(ValidationError {
                        task_ids: vec![task_id.to_string()],
                        rule: rules::SCHEMA.to_string(),
                        detail: e.to_string(),
                    });
                    continue;
                }
            };
            // Normalize paths (backslashes → forward slashes) and enforce
            // containment: relative, no `..`, resolves inside the project
            // root. Mirrors validate()'s PATH_ESCAPE check at parse time;
            // a violating node is dropped from the map so validate()
            // doesn't double-report it (the graph is rejected anyway).
            let mut path_ok = true;
            for field in [&mut node.read_set, &mut node.write_set] {
                for i in 0..field.len() {
                    let normalized = field[i].to_string_lossy().replace('\\', "/");
                    let path = PathBuf::from(normalized);
                    if let Some(reason) = path_violation(&path) {
                        path_ok = false;
                        errors.push(ValidationError {
                            task_ids: vec![task_id.to_string()],
                            rule: rules::PATH_ESCAPE.to_string(),
                            detail: format!("path {} rejected: {}", path.display(), reason),
                        });
                    }
                    field[i] = path;
                }
            }
            if !path_ok {
                continue;
            }
            nodes.insert(task_id, node);
        }

        let graph = TaskGraph {
            nodes,
            baseline_revision,
            run_id: String::new(),
        };
        if let Err(validate_errors) = graph.validate() {
            errors.extend(validate_errors);
        }
        if errors.is_empty() {
            Ok(graph)
        } else {
            Err(errors)
        }
    }

    /// Serializes to the strict planner JSON shape:
    /// `{"format":"joey-taskgraph/1","baseline_revision":…,"tasks":[…]}`.
    ///
    /// Each task object EXCLUDES `status` and `attempts` — planner JSON
    /// carries the plan, not runtime state. Round-trip guarantee:
    /// `from_strict_json(to_strict_json(g), root)` yields an equal graph
    /// (statuses/attempts restored to their `Pending`/`0` defaults).
    /// Infallible for these types.
    pub fn to_strict_json(&self) -> serde_json::Value {
        let tasks: Vec<serde_json::Value> = self
            .nodes
            .values()
            .map(|node| {
                let mut v = serde_json::to_value(node)
                    .expect("TaskNode serialization is infallible");
                let obj = v.as_object_mut()
                    .expect("TaskNode serializes to a JSON object");
                obj.remove("status");
                obj.remove("attempts");
                v
            })
            .collect();
        serde_json::json!({
            "format": Self::FORMAT,
            "baseline_revision": self.baseline_revision,
            "tasks": tasks,
        })
    }
}

/// Planner contract for isolation: a task with a non-empty write set runs in
/// an isolated worktree by default; read-only tasks share the checkout.
pub fn default_isolation(write_set: &[PathBuf]) -> IsolationMode {
    if write_set.is_empty() {
        IsolationMode::SharedCheckout
    } else {
        IsolationMode::IsolatedWorktree
    }
}

/// DFS cycle detection (white/gray/black). Returns the first cycle found as
/// task ids in cycle order, or `None`. Unknown dependencies are skipped
/// (rule 2 reports them).
fn find_cycle(nodes: &BTreeMap<TaskId, TaskNode>) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Gray,
        Black,
    }

    fn dfs<'a>(
        id: &'a TaskId,
        nodes: &'a BTreeMap<TaskId, TaskNode>,
        colors: &mut BTreeMap<&'a TaskId, Color>,
        stack: &mut Vec<&'a TaskId>,
    ) -> Option<Vec<String>> {
        colors.insert(id, Color::Gray);
        stack.push(id);
        for dep in &nodes[id].dependencies {
            if !nodes.contains_key(dep) {
                continue;
            }
            let color = *colors.entry(dep).or_insert(Color::White);
            match color {
                Color::Gray => {
                    let start = stack.iter().position(|s| *s == dep).unwrap();
                    return Some(stack[start..].iter().map(|s| s.to_string()).collect());
                }
                Color::White => {
                    if let Some(cycle) = dfs(dep, nodes, colors, stack) {
                        return Some(cycle);
                    }
                }
                Color::Black => {}
            }
        }
        stack.pop();
        colors.insert(id, Color::Black);
        None
    }

    let mut colors: BTreeMap<&TaskId, Color> =
        nodes.keys().map(|k| (k, Color::White)).collect();
    let mut stack: Vec<&TaskId> = Vec::new();
    for id in nodes.keys() {
        if colors.get(id) == Some(&Color::White) {
            if let Some(cycle) = dfs(id, nodes, &mut colors, &mut stack) {
                return Some(cycle);
            }
        }
    }
    None
}

/// Whether `from` reaches `to` by following dependency edges transitively
/// (i.e. `to` is an ancestor of `from`).
fn reaches(from: &TaskId, to: &TaskId, nodes: &BTreeMap<TaskId, TaskNode>) -> bool {
    let mut visited: HashSet<&TaskId> = HashSet::new();
    let mut queue: VecDeque<&TaskId> = VecDeque::new();
    queue.push_back(from);
    while let Some(cur) = queue.pop_front() {
        if !visited.insert(cur) {
            continue;
        }
        if cur == to {
            return true;
        }
        if let Some(node) = nodes.get(cur) {
            for dep in &node.dependencies {
                queue.push_back(dep);
            }
        }
    }
    false
}

/// Why a path violates the project-root constraint, if it does: absolute
/// paths and any path containing a `..` component are rejected.
fn path_violation(path: &Path) -> Option<&'static str> {
    if path.is_absolute() {
        return Some("absolute path");
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Some("parent-dir component");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::VerificationStepView;

    fn id(s: &str) -> TaskId {
        TaskId::new(s).expect("valid id")
    }

    fn node(id_str: &str, deps: &[&str]) -> TaskNode {
        TaskNode {
            id: id(id_str),
            objective: format!("do {}", id_str),
            dependencies: deps.iter().map(|d| id(d)).collect(),
            read_set: vec![],
            write_set: vec![],
            artifact_ids: vec![],
            role: WorkerRole::Implementor,
            model_tier: ModelTier::Economical,
            risk: RiskLevel::Low,
            acceptance: vec![AcceptanceCriterion {
                criterion: "tests pass".to_string(),
                kind: "command".to_string(),
            }],
            verification: VerificationPlanView::default(),
            isolation: IsolationMode::SharedCheckout,
            status: TaskStatus::Pending,
            attempts: 0,
        }
    }

    fn graph(nodes: Vec<TaskNode>) -> TaskGraph {
        TaskGraph {
            nodes: nodes.into_iter().map(|n| (n.id.clone(), n)).collect(),
            ..TaskGraph::default()
        }
    }

    fn rule_errors<'a>(errs: &'a [ValidationError], rule: &str) -> Vec<&'a ValidationError> {
        errs.iter().filter(|e| e.rule == rule).collect()
    }

    #[test]
    fn task_id_validation_and_display() {
        assert!(TaskId::new("task-auth").is_some());
        assert!(TaskId::new("a-1").is_some());
        assert!(TaskId::new("Task_Auth").is_none());
        assert!(TaskId::new("").is_none());
        assert!(TaskId::new("ta sk").is_none());
        let t = TaskId::new("task-auth").unwrap();
        assert_eq!(t.as_str(), "task-auth");
        assert_eq!(t.to_string(), "task-auth");
        let from: TaskId = String::from("x-y").into();
        assert_eq!(from.as_str(), "x-y");
        // serde-transparent: serializes as the bare string.
        assert_eq!(serde_json::to_value(&t).unwrap(), serde_json::json!("task-auth"));
        let back: TaskId = serde_json::from_value(serde_json::json!("task-auth")).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn task_status_terminal_and_model_tier_rank() {
        assert!(!TaskStatus::Pending.is_terminal());
        assert!(!TaskStatus::Ready.is_terminal());
        assert!(!TaskStatus::Dispatched.is_terminal());
        assert!(!TaskStatus::Evaluating.is_terminal());
        assert!(!TaskStatus::Degraded.is_terminal());
        assert!(!TaskStatus::Blocked.is_terminal());
        assert!(TaskStatus::Completed.is_terminal());
        assert!(TaskStatus::Failed.is_terminal());
        assert!(TaskStatus::Skipped.is_terminal());
        assert_eq!(ModelTier::Economical.rank(), 0);
        assert_eq!(ModelTier::Frontier.rank(), 1);
    }

    #[test]
    fn serde_round_trip_bare_keys_and_snake_case() {
        let mut a = node("task-a", &[]);
        a.read_set = vec![PathBuf::from("README.md")];
        a.write_set = vec![PathBuf::from("src/a.rs")];
        a.role = WorkerRole::Orchestrator;
        a.model_tier = ModelTier::Frontier;
        let b = node("task-b", &["task-a"]);
        let g = graph(vec![a, b]);

        let json = serde_json::to_value(&g).unwrap();
        let nodes = json["nodes"].as_object().unwrap();
        let keys: Vec<&str> = nodes.keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, vec!["task-a", "task-b"], "map keys must be bare ids");
        let na = nodes["task-a"].as_object().unwrap();
        for key in [
            "id",
            "objective",
            "dependencies",
            "read_set",
            "write_set",
            "artifact_ids",
            "role",
            "model_tier",
            "risk",
            "acceptance",
            "verification",
            "isolation",
            "status",
            "attempts",
        ] {
            assert!(na.contains_key(key), "missing snake_case field {}", key);
        }
        assert_eq!(na["model_tier"], "frontier");
        assert_eq!(na["role"], "orchestrator");
        assert_eq!(na["risk"], "low");
        assert_eq!(na["isolation"], "shared_checkout");
        assert_eq!(na["status"], "pending");
        // Default verification (no steps, risk_triggered_review absent from
        // the fixture ⇒ false) round-trips.
        assert_eq!(na["verification"]["risk_triggered_review"], false);
        assert_eq!(na["verification"]["steps"].as_array().unwrap().len(), 0);

        let back: TaskGraph = serde_json::from_value(json).unwrap();
        assert_eq!(back, g);

        // Planner JSON omits status/attempts ⇒ serde defaults kick in.
        let mut planner = serde_json::to_value(&g).unwrap();
        for n in planner["nodes"].as_object_mut().unwrap().values_mut() {
            let obj = n.as_object_mut().unwrap();
            obj.remove("status");
            obj.remove("attempts");
        }
        let from_planner: TaskGraph = serde_json::from_value(planner).unwrap();
        let pa = from_planner.node(&id("task-a")).unwrap();
        assert_eq!(pa.status, TaskStatus::Pending);
        assert_eq!(pa.attempts, 0);
    }

    #[test]
    fn validate_happy_path() {
        let mut a = node("task-a", &[]);
        a.write_set = vec![PathBuf::from("src/a.rs")];
        let mut b = node("task-b", &[]);
        b.write_set = vec![PathBuf::from("src/b.rs")];
        let g = graph(vec![a, b]);
        assert_eq!(g.validate(), Ok(()));
        // Empty graph is valid.
        assert_eq!(TaskGraph::default().validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_dependency_cycle() {
        let g = graph(vec![node("task-a", &["task-b"]), node("task-b", &["task-a"])]);
        let errs = g.validate().unwrap_err();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].rule, rules::CYCLE);
        assert_eq!(errs[0].task_ids, vec!["task-a", "task-b"]);
    }

    #[test]
    fn validate_rejects_unknown_dependency() {
        let g = graph(vec![node("task-a", &["task-missing"])]);
        let errs = g.validate().unwrap_err();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].rule, rules::UNKNOWN_DEP);
        assert_eq!(errs[0].task_ids, vec!["task-missing", "task-a"]);
    }

    #[test]
    fn validate_rejects_concurrent_write_overlap() {
        let mut a = node("task-a", &[]);
        a.write_set = vec![PathBuf::from("src/main.rs")];
        let mut b = node("task-b", &[]);
        b.write_set = vec![PathBuf::from("src/main.rs")];
        let g = graph(vec![a, b]);
        let errs = g.validate().unwrap_err();
        let overlap = rule_errors(&errs, rules::WRITE_OVERLAP);
        assert_eq!(overlap.len(), 1, "exactly one WRITE_OVERLAP error");
        assert_eq!(overlap[0].task_ids, vec!["task-a", "task-b"]);

        // Ancestor-related writers on the same path are sequenced ⇒ legal.
        let mut a = node("task-a", &[]);
        a.write_set = vec![PathBuf::from("src/main.rs")];
        let mut b = node("task-b", &["task-a"]);
        b.write_set = vec![PathBuf::from("src/main.rs")];
        let g = graph(vec![a, b]);
        assert_eq!(g.validate(), Ok(()));
    }

    #[test]
    fn validate_rejects_path_escape() {
        // ParentDir component in write_set.
        let mut a = node("task-a", &[]);
        a.write_set = vec![PathBuf::from("../escape.rs")];
        let g = graph(vec![a]);
        let errs = g.validate().unwrap_err();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].rule, rules::PATH_ESCAPE);
        assert_eq!(errs[0].task_ids, vec!["task-a"]);

        // Absolute path in read_set.
        let mut a = node("task-a", &[]);
        a.read_set = vec![PathBuf::from("/abs/path.rs")];
        let g = graph(vec![a]);
        let errs = g.validate().unwrap_err();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].rule, rules::PATH_ESCAPE);
        assert_eq!(errs[0].task_ids, vec!["task-a"]);
    }

    #[test]
    fn validate_requires_acceptance_criterion() {
        let mut a = node("task-a", &[]);
        a.acceptance = vec![];
        let g = graph(vec![a]);
        let errs = g.validate().unwrap_err();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].rule, rules::NO_ACCEPTANCE);
        assert_eq!(errs[0].task_ids, vec!["task-a"]);
    }

    #[test]
    fn validate_requires_verification_for_high_risk() {
        let mut a = node("task-a", &[]);
        a.risk = RiskLevel::High;
        let g = graph(vec![a]);
        let errs = g.validate().unwrap_err();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].rule, rules::UNVERIFIED_HIGH_RISK);
        assert_eq!(errs[0].task_ids, vec!["task-a"]);

        // A required verification step satisfies the invariant.
        let mut a = node("task-a", &[]);
        a.risk = RiskLevel::High;
        a.verification = VerificationPlanView {
            steps: vec![VerificationStepView {
                name: "build".to_string(),
                command: "cargo build".to_string(),
                parse: "plain".to_string(),
                timeout_sec: 600,
                required: true,
            }],
            risk_triggered_review: false,
        };
        assert_eq!(graph(vec![a]).validate(), Ok(()));

        // So does a risk-triggered review alone.
        let mut a = node("task-a", &[]);
        a.risk = RiskLevel::High;
        a.verification = VerificationPlanView {
            steps: vec![],
            risk_triggered_review: true,
        };
        assert_eq!(graph(vec![a]).validate(), Ok(()));
    }

    #[test]
    fn ready_nodes_diamond() {
        let mut g = graph(vec![
            node("a", &[]),
            node("b", &["a"]),
            node("c", &["a"]),
            node("d", &["b", "c"]),
        ]);
        // Nothing completed yet ⇒ only the root is ready.
        assert_eq!(g.ready_nodes(), vec![id("a")]);
        // Complete a ⇒ b and c ready, in id order (b before c).
        g.node_mut(&id("a")).unwrap().status = TaskStatus::Completed;
        assert_eq!(g.ready_nodes(), vec![id("b"), id("c")]);
        // Complete b and c ⇒ d ready; a/b/c are terminal, never re-listed.
        g.node_mut(&id("b")).unwrap().status = TaskStatus::Completed;
        g.node_mut(&id("c")).unwrap().status = TaskStatus::Completed;
        assert_eq!(g.ready_nodes(), vec![id("d")]);
    }

    #[test]
    fn ready_nodes_topological_depth_order() {
        // Independent z (depth 0) must precede depth-1 diamond children even
        // though "z" sorts after "b"/"c" alphabetically.
        let mut g = graph(vec![
            node("z", &[]),
            node("a", &[]),
            node("b", &["a"]),
            node("c", &["a"]),
            node("d", &["b", "c"]),
        ]);
        g.node_mut(&id("a")).unwrap().status = TaskStatus::Completed;
        assert_eq!(g.ready_nodes(), vec![id("z"), id("b"), id("c")]);
    }

    #[test]
    fn is_terminal_and_is_blocked_truth_table() {
        let mut g = graph(vec![
            node("dep-done", &[]),
            node("dep-wait", &[]),
            node("dep-skip", &[]),
            node("dep-fail", &[]),
            node("c1", &["dep-done"]),
            node("c2", &["dep-wait"]),
            node("c3", &["dep-skip"]),
            node("c4", &["dep-fail"]),
        ]);
        g.node_mut(&id("dep-done")).unwrap().status = TaskStatus::Completed;
        g.node_mut(&id("dep-skip")).unwrap().status = TaskStatus::Skipped;
        g.node_mut(&id("dep-fail")).unwrap().status = TaskStatus::Failed;

        // is_terminal: unknown ⇒ true; terminal statuses ⇒ true; else false.
        assert!(g.is_terminal(&id("nope")));
        assert!(g.is_terminal(&id("dep-done")));
        assert!(g.is_terminal(&id("dep-fail")));
        assert!(g.is_terminal(&id("dep-skip")));
        assert!(!g.is_terminal(&id("dep-wait")));
        assert!(!g.is_terminal(&id("c1")));

        // is_blocked: unknown ⇒ false.
        assert!(!g.is_blocked(&id("nope")));
        // Pending + all deps Completed ⇒ not blocked.
        assert!(!g.is_blocked(&id("c1")));
        // Pending + a Pending dep ⇒ blocked.
        assert!(g.is_blocked(&id("c2")));
        // Pending + a Skipped dep ⇒ resolved, not blocked.
        assert!(!g.is_blocked(&id("c3")));
        // Pending + a Failed (terminal but unresolved) dep ⇒ blocked.
        assert!(g.is_blocked(&id("c4")));
        // Non-Pending tasks are never "blocked" by this predicate.
        g.node_mut(&id("dep-wait")).unwrap().status = TaskStatus::Ready;
        assert!(!g.is_blocked(&id("dep-wait")));
    }

    #[test]
    fn transition_full_legal_chain() {
        let mut g = graph(vec![node("a", &[])]);
        let a = id("a");
        assert_eq!(g.transition(&a, TaskStatus::Ready), Ok(TaskStatus::Ready));
        assert_eq!(
            g.transition(&a, TaskStatus::Dispatched),
            Ok(TaskStatus::Dispatched)
        );
        assert_eq!(
            g.transition(&a, TaskStatus::Evaluating),
            Ok(TaskStatus::Evaluating)
        );
        assert_eq!(
            g.transition(&a, TaskStatus::Completed),
            Ok(TaskStatus::Completed)
        );
        assert_eq!(g.node(&a).unwrap().status, TaskStatus::Completed);

        // Degraded paths: Degraded → Evaluating (gate re-run) and
        // Degraded → Blocked.
        let mut g = graph(vec![node("b", &[])]);
        let b = id("b");
        g.node_mut(&b).unwrap().status = TaskStatus::Evaluating;
        assert_eq!(g.transition(&b, TaskStatus::Degraded), Ok(TaskStatus::Degraded));
        assert_eq!(
            g.transition(&b, TaskStatus::Evaluating),
            Ok(TaskStatus::Evaluating)
        );
        assert_eq!(g.transition(&b, TaskStatus::Degraded), Ok(TaskStatus::Degraded));
        assert_eq!(g.transition(&b, TaskStatus::Blocked), Ok(TaskStatus::Blocked));
        // Blocked → Ready (replanned).
        assert_eq!(g.transition(&b, TaskStatus::Ready), Ok(TaskStatus::Ready));
    }

    #[test]
    fn transition_illegal_edges_and_terminals() {
        let mut g = graph(vec![node("a", &[])]);
        let a = id("a");
        // Pending → Dispatched is not an edge.
        assert_eq!(
            g.transition(&a, TaskStatus::Dispatched),
            Err("illegal transition pending -> dispatched for task a".to_string())
        );
        // Legal climb to Completed, then immutable.
        g.transition(&a, TaskStatus::Ready).unwrap();
        g.transition(&a, TaskStatus::Dispatched).unwrap();
        g.transition(&a, TaskStatus::Evaluating).unwrap();
        g.transition(&a, TaskStatus::Completed).unwrap();
        assert_eq!(
            g.transition(&a, TaskStatus::Ready),
            Err("illegal transition completed -> ready for task a".to_string())
        );
        assert_eq!(
            g.transition(&a, TaskStatus::Skipped),
            Err("illegal transition completed -> skipped for task a".to_string())
        );
        // Evaluating → Dispatched (repair re-dispatch) is legal.
        let mut g = graph(vec![node("b", &[])]);
        let b = id("b");
        g.node_mut(&b).unwrap().status = TaskStatus::Evaluating;
        assert_eq!(
            g.transition(&b, TaskStatus::Dispatched),
            Ok(TaskStatus::Dispatched)
        );
        // Unknown task id ⇒ Err.
        assert!(g.transition(&id("nope"), TaskStatus::Ready).is_err());
    }

    #[test]
    fn transition_skip_from_dispatched() {
        let mut g = graph(vec![node("a", &[])]);
        let a = id("a");
        g.node_mut(&a).unwrap().status = TaskStatus::Dispatched;
        assert_eq!(g.transition(&a, TaskStatus::Skipped), Ok(TaskStatus::Skipped));
        // Skipped is terminal.
        assert!(g.transition(&a, TaskStatus::Ready).is_err());
        assert_eq!(g.node(&a).unwrap().status, TaskStatus::Skipped);
    }

    #[test]
    fn default_isolation_branches() {
        assert_eq!(default_isolation(&[]), IsolationMode::SharedCheckout);
        assert_eq!(
            default_isolation(&[PathBuf::from("src/main.rs")]),
            IsolationMode::IsolatedWorktree
        );
    }

    #[test]
    fn snapshot_round_trips() {
        let g = graph(vec![node("a", &[]), node("b", &["a"])]);
        let snap = g.snapshot();
        assert!(snap["nodes"]["a"].is_object());
        let back: TaskGraph = serde_json::from_value(snap).unwrap();
        assert_eq!(back, g);
    }

    // ---- T012: strict planner JSON (from_strict_json / to_strict_json) ----

    const PLAN: &str = r#"{"format":"joey-taskgraph/1","baseline_revision":"abc123","tasks":[{"id":"task-auth","objective":"Implement token refresh","dependencies":[],"read_set":[],"write_set":["src/auth.rs"],"artifact_ids":[42,1337],"role":"implementor","model_tier":"economical","risk":"medium","acceptance":[{"criterion":"cargo test -p joey-core auth","kind":"command"}],"verification":{"steps":[{"name":"scoped-tests","command":"cargo test -p joey-core auth","parse":"plain","timeout_sec":300,"required":true}],"risk_triggered_review":false},"isolation":"isolated_worktree"}]}"#;

    fn plan_root() -> PathBuf {
        PathBuf::from("/tmp/project")
    }

    fn reformat_plan(fmt: Option<&str>) -> String {
        let v: serde_json::Value = serde_json::from_str(PLAN).unwrap();
        let mut v = v;
        let obj = v.as_object_mut().unwrap();
        match fmt {
            Some(f) => obj.insert("format".to_string(), serde_json::json!(f)),
            None => obj.remove("format"),
        };
        serde_json::to_string(&v).unwrap()
    }

    fn mutate_task<F: FnOnce(&mut serde_json::Value)>(f: F) -> String {
        let mut v: serde_json::Value = serde_json::from_str(PLAN).unwrap();
        f(&mut v["tasks"][0]);
        serde_json::to_string(&v).unwrap()
    }

    #[test]
    fn strict_json_accepts_contract_example() {
        let g = TaskGraph::from_strict_json(PLAN, &plan_root()).expect("plan must parse");
        assert_eq!(g.baseline_revision, "abc123");
        assert_eq!(g.run_id, "");
        let n = g.node(&id("task-auth")).unwrap();
        assert_eq!(n.objective, "Implement token refresh");
        assert_eq!(n.isolation, IsolationMode::IsolatedWorktree);
        assert_eq!(n.model_tier, ModelTier::Economical);
        assert_eq!(n.risk, RiskLevel::Medium);
        assert_eq!(n.artifact_ids, vec![42, 1337]);
        assert_eq!(n.write_set, vec![PathBuf::from("src/auth.rs")]);
        assert!(n.verification.steps[0].required);
        assert_eq!(n.status, TaskStatus::Pending);
        assert_eq!(n.attempts, 0);
    }

    #[test]
    fn strict_json_rejects_unknown_format() {
        for json in [
            reformat_plan(Some("joey-taskgraph/2")),
            reformat_plan(None),
        ] {
            let errs = TaskGraph::from_strict_json(&json, &plan_root()).unwrap_err();
            assert_eq!(errs.len(), 1, "one error for {:?}", json);
            assert_eq!(errs[0].rule, rules::INVALID_FORMAT);
            assert_eq!(errs[0].task_ids, Vec::<String>::new());
            assert!(errs[0].detail.contains("expected joey-taskgraph/1, got"),
                "detail must echo the bad value: {}", errs[0].detail);
        }
    }

    #[test]
    fn strict_json_rejects_bad_id_charset() {
        let json = mutate_task(|t| t["id"] = serde_json::json!("Task_Auth"));
        let errs = TaskGraph::from_strict_json(&json, &plan_root()).unwrap_err();
        let id_errs = rule_errors(&errs, rules::INVALID_TASK_ID);
        assert_eq!(id_errs.len(), 1);
        assert_eq!(id_errs[0].task_ids, vec!["Task_Auth".to_string()]);
    }

    #[test]
    fn strict_json_rejects_path_escape() {
        // Parent traversal in write_set.
        let json = mutate_task(|t| t["write_set"] = serde_json::json!(["../outside.rs"]));
        let errs = TaskGraph::from_strict_json(&json, &plan_root()).unwrap_err();
        let escapes = rule_errors(&errs, rules::PATH_ESCAPE);
        assert_eq!(escapes.len(), 1);
        assert_eq!(escapes[0].task_ids, vec!["task-auth".to_string()]);

        // Absolute path in read_set.
        let json = mutate_task(|t| t["read_set"] = serde_json::json!(["/abs/x"]));
        let errs = TaskGraph::from_strict_json(&json, &plan_root()).unwrap_err();
        let escapes = rule_errors(&errs, rules::PATH_ESCAPE);
        assert_eq!(escapes.len(), 1);
        assert_eq!(escapes[0].task_ids, vec!["task-auth".to_string()]);
    }

    #[test]
    fn strict_json_rejects_missing_dependency() {
        let json = mutate_task(|t| t["dependencies"] = serde_json::json!(["task-zzz"]));
        let errs = TaskGraph::from_strict_json(&json, &plan_root()).unwrap_err();
        let unknown = rule_errors(&errs, rules::UNKNOWN_DEP);
        assert_eq!(unknown.len(), 1);
        assert_eq!(
            unknown[0].task_ids,
            vec!["task-zzz".to_string(), "task-auth".to_string()],
            "must name BOTH the missing target and the referencing task"
        );
    }

    #[test]
    fn strict_json_rejects_enum_typo() {
        let json = mutate_task(|t| t["model_tier"] = serde_json::json!("cheap"));
        let errs = TaskGraph::from_strict_json(&json, &plan_root()).unwrap_err();
        let schema = rule_errors(&errs, rules::SCHEMA);
        assert_eq!(schema.len(), 1);
        assert_eq!(schema[0].task_ids, vec!["task-auth".to_string()]);
        assert!(schema[0].detail.contains("cheap"),
            "schema detail must carry serde's message: {}", schema[0].detail);
    }

    #[test]
    fn strict_json_isolation_default_applied() {
        // Non-empty write_set, isolation key removed ⇒ IsolatedWorktree.
        let json = mutate_task(|t| {
            let obj = t.as_object_mut().unwrap();
            obj.remove("isolation");
        });
        let g = TaskGraph::from_strict_json(&json, &plan_root()).unwrap();
        assert_eq!(
            g.node(&id("task-auth")).unwrap().isolation,
            IsolationMode::IsolatedWorktree
        );

        // Empty write_set and no isolation ⇒ SharedCheckout.
        let json = mutate_task(|t| {
            let obj = t.as_object_mut().unwrap();
            obj.remove("isolation");
            obj.insert("write_set".to_string(), serde_json::json!([]));
        });
        let g = TaskGraph::from_strict_json(&json, &plan_root()).unwrap();
        assert_eq!(
            g.node(&id("task-auth")).unwrap().isolation,
            IsolationMode::SharedCheckout
        );
    }

    #[test]
    fn strict_json_validation_merged_with_parse_errors() {
        // One entry with a bad id, two valid-id entries forming a cycle:
        // parse errors first, then validate()'s CYCLE, both present.
        let v: serde_json::Value = serde_json::from_str(PLAN).unwrap();
        let mut v = v;
        let task = v["tasks"][0].clone();
        let bad = {
            let mut t = task.clone();
            t["id"] = serde_json::json!("Task_Auth");
            t
        };
        let mut a = task.clone();
        a["id"] = serde_json::json!("task-a");
        a["dependencies"] = serde_json::json!(["task-b"]);
        a["write_set"] = serde_json::json!(["src/a.rs"]);
        let mut b = task;
        b["id"] = serde_json::json!("task-b");
        b["dependencies"] = serde_json::json!(["task-a"]);
        b["write_set"] = serde_json::json!(["src/b.rs"]);
        v["tasks"] = serde_json::json!([bad, a, b]);
        let json = serde_json::to_string(&v).unwrap();

        let errs = TaskGraph::from_strict_json(&json, &plan_root()).unwrap_err();
        assert!(
            errs.iter().any(|e| e.rule == rules::INVALID_TASK_ID),
            "parse error must be present: {:?}", errs
        );
        assert!(
            errs.iter().any(|e| e.rule == rules::CYCLE),
            "validation CYCLE must be merged in: {:?}", errs
        );
        // Parse errors come first.
        assert_eq!(errs[0].rule, rules::INVALID_TASK_ID);
    }

    #[test]
    fn strict_json_round_trip_strips_runtime_state() {
        // Build a 2-node graph from planner JSON.
        let v: serde_json::Value = serde_json::from_str(PLAN).unwrap();
        let mut v = v;
        let mut second = v["tasks"][0].clone();
        second["id"] = serde_json::json!("task-auth-2");
        second["dependencies"] = serde_json::json!(["task-auth"]);
        second["write_set"] = serde_json::json!(["src/auth2.rs"]);
        v["tasks"].as_array_mut().unwrap().push(second);
        let json = serde_json::to_string(&v).unwrap();
        let mut g = TaskGraph::from_strict_json(&json, &plan_root()).unwrap();

        // Mutate runtime state so stripping is observable.
        g.node_mut(&id("task-auth")).unwrap().status = TaskStatus::Completed;
        g.node_mut(&id("task-auth")).unwrap().attempts = 3;
        g.node_mut(&id("task-auth-2")).unwrap().status = TaskStatus::Failed;

        let strict = g.to_strict_json();
        // Serialized tasks carry no status/attempts keys.
        for t in strict["tasks"].as_array().unwrap() {
            let obj = t.as_object().unwrap();
            assert!(!obj.contains_key("status"), "status leaked: {:?}", t);
            assert!(!obj.contains_key("attempts"), "attempts leaked: {:?}", t);
        }
        assert_eq!(strict["format"], "joey-taskgraph/1");
        assert_eq!(strict["baseline_revision"], "abc123");

        // Round trip restores status/attempts defaults and equal nodes.
        let out = serde_json::to_string(&strict).unwrap();
        let g2 = TaskGraph::from_strict_json(&out, &plan_root()).unwrap();
        for (kid, knode) in &g.nodes {
            let mut expected = knode.clone();
            expected.status = TaskStatus::Pending;
            expected.attempts = 0;
            assert_eq!(g2.node(kid), Some(&expected), "node {} must round-trip", kid);
        }
        assert_eq!(g2.nodes.len(), 2);
    }
}

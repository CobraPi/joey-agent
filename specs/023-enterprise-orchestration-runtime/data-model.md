# Data Model: Enterprise Orchestration Runtime

Entities, fields, relationships, validation rules and state transitions. Field-level types here are logical; exact Rust types are pinned in contracts/public-api.md.

## Analysis plane (joey-neurocode)

### TaskAnalysis
Unified per-request analysis produced by `EnterpriseTaskAnalyzer::analyze`.
- `revision: String` — repository revision the analysis is bound to.
- `target_artifacts: Vec<NodeId>` — directly requested artifacts.
- `impacted_artifacts: Vec<NodeId>` — transitively impacted (dependency closure).
- `effective_policies: Vec<PolicyBinding>` — combined effective conventions.
- `complexity: ComplexityRoute` — existing route type (tier, reasoning, signals).
- `risk: RiskAssessment` — see below.
- `model_tier: ComplexityTier` — recommended capability tier.
- `execution_hint: ExecutionHint` — graph-shape facts for the router: `write_overlap: bool`, `strict_dependency_depth: u32`, `independent_components: u32`, `cross_component_coordination: bool`.
- `verification: VerificationPlan` — see below.
Relationships: 1 TaskAnalysis → N PolicyBinding, 1 RiskAssessment, 1 VerificationPlan; references NodeIds in the DependencyGraph.

### PolicyBinding
- `layer: PolicyLayer` — Organization | Repository | Module | ScopedRule | TaskContract.
- `source_path: PathBuf` — file the rule came from.
- `applies_to: Vec<String>` — path globs ("**" = unrestricted); scoped rules apply only to matching task paths.
- `directive: String` — the instruction content.
- `conflicts_with: Vec<String>` — ids of bindings it contradicts (surfaced, never dropped).
Validation: a binding with an empty `applies_to` is invalid; unrestricted bindings must come from unrestricted layers only.

### RiskAssessment
- `level: RiskLevel` — Low | Medium | High.
- `factors: Vec<RiskFactor>` — each: kind (PublicApiExposure | SecuritySensitive | Concurrency | FanOut | OwnershipBoundary), evidence (artifact ids / paths), weight.
Rule (FR-022 trigger): any PublicApiExposure, SecuritySensitive or Concurrency factor, or fan-out/ownership above configured thresholds ⇒ High.

### VerificationPlan
- `steps: Vec<VerificationStep>` — name, command, parse format, timeout, `required: bool`.
- `risk_triggered_review: bool` — specialist review required before approval.
Derivation (verification_plan.rs): scoped to impacted modules; high risk adds the review flag; steps never broader than project-scoped builds/tests.

### OutcomeMemory
Fields per spec story 7 (task_signature, repository_revision, artifact_ids, policy_ids, failure_signature?, resolution?, evidence_ids, confidence, hit_count, last_confirmed_at).
Lifecycle: written only from verified outcomes (pass or terminal fail); consulted by task signature/artifact match; each consultation bumps hit_count and re-checks artifact hashes — changed ⇒ expired or down-ranked (confidence scaled toward 0).

## Execution plane (joey-orchestration)

### TaskNode
Fields per FR-006: `id: TaskId`, `objective`, `dependencies: Vec<TaskId>`, `read_set: Vec<PathBuf>`, `write_set: Vec<PathBuf>`, `artifact_ids: Vec<NodeId>`, `role: WorkerRole`, `model_tier`, `risk: RiskLevel`, `acceptance: Vec<AcceptanceCriterion>`, `verification: VerificationPlan`, `isolation: IsolationMode` (SharedCheckout | IsolatedWorktree), `status: TaskStatus`, `attempts: u32`.
Validation invariants (FR-009/FR-010, enforced by TaskGraph::validate):
1. No dependency cycles (reject with the cycle's task ids).
2. Every dependency references an existing task id.
3. No two concurrently-dispatchable tasks share a write-set path (scheduler also enforces at wave partition).
4. All declared paths resolve inside the project root (path traversal rejected).
5. At least one acceptance criterion per task.
6. High-risk tasks must carry a verification plan with ≥1 required step or risk-triggered review.
Empty write sets (legacy conversion) are legal but mark the node "undeclared" — never dispatched concurrently with any other writer.

### TaskStatus lifecycle
Pending → Ready (all dependencies Completed) → Dispatched → Evaluating →
- gate passed (and review clean) → Completed
- gate failed → Repair (attempts+1) → Dispatched | tier-exhausted → Escalated (next tier) → Dispatched | top-tier-exhausted → Failed
- command unavailable → Degraded (blocks completion; clears on runnable or explicit user acknowledgment → Dispatched or Completed)
- planner blocked → Blocked → (re-plan) → Ready
Any state → Skipped (only via explicit re-plan decision, logged).
Terminal: Completed, Failed, Skipped. Every transition appends one decisions.jsonl entry with cause.

### TaskGraph
- `nodes: Map<TaskId, TaskNode>`, `baseline_revision: String`, `run_id`.
- Operations: validate, ready_nodes (deterministic order: topo then id), is_terminal, is_blocked, transition(task_id, verdict), snapshot.
- Conversion: `from_workstreams(workstreams, …)` (legacy, conservative) and `from_strict_json(...)` (new format, contracts/planner-json-format.md).

### ChangeBundle (writer result)
- `baseline_sha`, `declared_write_set`, `actual_write_set`, `patch_path` (runs/<run-id>/patches/<task-id>.patch), `evidence: Vec<EvidenceRecord>`.
Rule: actual ⊄ declared ⇒ divergence report; undeclared writes are defects.

### DefectBundle (gate failure)
- `task_id`, `failed_commands: Vec<CommandFailure>` (command, exit, structured errors), `policy_violations`, `reviewer_findings`, `changed_paths`.
Feeds: repair worker dispatch, escalation-ladder accounting.

### EvidenceRecord
- `id`, `task_id`, `kind` (CommandOutput | ReviewOutcome | Inspection | DivergenceReport), `payload`, `recorded_at`.
Immutable once written; referenced by decisions.jsonl entries and OutcomeMemory.

### RunState (on-disk, contracts/run-state-format.md)
`~/.joey/hypercode/projects/<project-hash>/runs/<run-id>/` — graph.json, nodes/<task-id>.json, evidence/<task-id>.json, patches/<task-id>.patch, decisions.jsonl.
Resume contract: baseline revision must match; completed nodes never re-executed; in-flight nodes re-dispatched from persisted state.

## Relationship map

TaskAnalysis ← produced per request (analysis plane)
TaskAnalysis → feeds → TaskNode fields (tier, risk, verification, artifacts)
TaskGraph → schedules → workers → ChangeBundle → joiner → integrated baseline
ChangeBundle + VerificationPlan → evaluator → DefectBundle → repair → (loop) → EvidenceRecord
Verified outcome → OutcomeMemory → consulted by future TaskAnalysis

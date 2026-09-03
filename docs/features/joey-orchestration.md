# joey-orchestration — subagent manager, task graphs, evaluation & join

`joey-orchestration` is the agentic orchestration engine of joey-agent: it
dispatches subagents (single-task and parallel batch) into isolated execution
contexts, shares concurrency limits between parent and children, and exposes
structured lifecycle events. On top of that runtime core it carries the
spec-023 "enterprise orchestration" pipeline — a typed, strictly-validated
`TaskGraph`, a deterministic wave `Scheduler` with verification gates and a
repair/escalation ladder, git-worktree workspace isolation, a three-way-apply
`Joiner`, and an append-only evidence/decision run store — plus feature-022
agent teams (shared task list + per-member mailboxes). Higher layers
(`joey-omo`, `joey-cli`) build on it; it never depends on them.

> See also: [../orchestration.md](../orchestration.md)

## Overview

The crate has three strata:

1. **Delegation runtime** — `SubagentManager` + `Subagent`. A child is a full
   `joey_agent_core::Agent` with its own history, `SessionState`, and filtered
   tool registry, run as a tokio task. Blocking and background (`background:
   true`) dispatch share the same plumbing: child registry, two-pool permit
   semaphores with grant-back, event taps, one-way terminal history.
2. **Plan → isolate → evaluate → join** (spec 023) — `TaskGraph` (strict JSON
   in/out), `Scheduler` + `ConflictAnalyzer`, `Evaluator` with
   `VerificationGate`/`RepairLedger`, `WorkspaceIsolation` (git worktrees),
   `Joiner` (`ChangeBundle` collect/integrate), `RunHandle` evidence store.
3. **Surface tools** — `delegate_task`, `subagent_control`, `call_omo_agent`,
   and the team tools `team_status`/`team_message`/`team_tasks`.

## Module map (39 files)

16 source modules, 22 integration test files, `Cargo.toml` (39 total):

| File | Role |
|---|---|
| `src/lib.rs` | Crate root; re-exports, `CategoryResolver` trait, `register_orchestration*` |
| `src/manager.rs` | `SubagentManager`, `ManagerConfig`, `ChildRegistry`, grant-back state |
| `src/subagent.rs` | Child `Agent` construction, model resolution, run loop + recovery |
| `src/types.rs` | `DelegationRequest`, `TaskSpec`, `DelegationResult`, `StopReason`, `Budgets`, `WorkHandle`, `DelegationOverview` |
| `src/delegation_tool.rs` | `delegate_task` (`DelegateTask`) + `call_omo_agent` (`CallOmoAgent`), HyperCode role routing |
| `src/control_tool.rs` | `subagent_control` (`SubagentControl`): steer/stop/list/status/log/wait |
| `src/background.rs` | Background dispatch, `JoinSet` watcher, budget watcher, completion notices |
| `src/capacity.rs` | `SystemCapacity::detect`, `capacity_children`/`capacity_requests` |
| `src/tap.rs` | Process-global delegation event tap |
| `src/task_graph.rs` | `TaskGraph`, `TaskId`, `TaskNode`, `TaskStatus`, validation rules |
| `src/scheduler.rs` | Wave `Scheduler`, `SchedulerConfig`, `ConflictAnalyzer`, `needs_replan`, `TaskDispatcher` |
| `src/evaluator.rs` | `VerificationGate`, `GateOutcome`, `Evaluator`, `RepairLedger`, `EvaluationDirective` |
| `src/workspace.rs` | `WorkspaceIsolation`, `WorktreeMode`, baseline revision |
| `src/joiner.rs` | `ChangeBundle`, `Joiner::collect`/`integrate` |
| `src/evidence.rs` | `RunHandle`, `DecisionEntry`, `DECISION_CAUSES`, run-directory layout |
| `src/team.rs` | Team registry, team tools, lead/teammate directives |
| `tests/*.rs` | 22 integration suites (see Testing) |

## SubagentManager & ManagerConfig

`ManagerConfig` defaults (feature-020 tuning in brackets):

| Field | Default | Meaning |
|---|---|---|
| `max_concurrent_children` | `3` | Max parallel subagents per batch wave |
| `max_concurrent_requests` | `5` | Provider-request permits across parent + children |
| `max_spawn_depth` | `1` | Max nesting depth (1 = flat, leaf only) |
| `default_max_turns` | `50` | Default iteration budget per child |
| `default_persist` | `false` | Default trace persistence |
| `default_model` | `None` | Default subagent model (falls back to parent model) |
| `omo_default_concurrency` | `5` | OMO background-task concurrency (FR-031) |
| `omo_provider_concurrency` | `{}` | Per-provider concurrency overrides |
| `omo_model_concurrency` | `{}` | Per-model concurrency overrides |
| `parent_reserved_permits` | `1` | Parent's guaranteed permit share (FR-018); 0 disables |
| `wind_down_timeout_secs` | `10` | Bounded wait at session wind-down (FR-015) |
| `subagent_recovery_attempts` | `1` | Extra attempts after a fatal PROVIDER error |

`ManagerConfig::from_config(cfg)` reads `delegation.*` and
`omo.background_task.*` keys:

- `delegation.max_concurrent_children` — `0` (or negative/`auto`) selects
  capacity-driven sizing from the detected host, clamped to
  `[FLOOR_CHILDREN, HARD_CHILD_CEILING]`; a positive value is a hard cap.
- `delegation.max_concurrent_requests` — `0` selects
  `capacity_requests(children)`; otherwise the literal value.
- `delegation.auto_mem_reserve_mb_per_child` (default 256) and
  `delegation.auto_mem_max_fraction` (default 0.6, clamped 0.05–0.95) tune the
  memory limiter.
- `delegation.default_model`, `delegation.max_spawn_depth`,
  `delegation.default_max_turns`, `delegation.default_persist`,
  `delegation.parent_reserved_permits`, `delegation.wind_down_timeout_secs`,
  `delegation.subagent_recovery_attempts`.
- `omo.background_task.defaultConcurrency`,
  `omo.background_task.providerConcurrency`,
  `omo.background_task.modelConcurrency` (YAML tables of name → limit).

## Child lifecycle

- A **child** is built by `Subagent::new`: a fresh `AgentConfig` (own model,
  `enabled_tools`, pinned `model_pinned: true`), a fresh `ToolContext` with
  session id `subagent-<uuid>`, and a filtered `ToolRegistry` containing only
  the requested toolsets' tools. Each child runs as its own tokio task with
  its own history — the parent's history is untouched.
- **Model resolution chain**: per-`TaskSpec.model` > `DelegationRequest.model`
  > config `delegation.default_model` > parent `AgentConfig.model`.
- **ChildRegistry** (shared by the top-level manager and the transient
  per-child managers a batch creates): `running` map keyed by global child id
  plus a session-lifetime `history` of `DelegationOverview` records.
  - `insert(id, handle)` at spawn; `pending_stop(id)` -> `Some(None)` while
    running clean, `Some(Some(reason))` once a stop is pending;
    `running_ids()`; `history_contains(id)`.
  - `complete(id, &DelegationResult) -> Option<DelegationOverview>` — moves a
    finished child from `running` into a terminal history record. **One-way**:
    if a record already exists for the id (e.g. `shutdown` finalized a
    straggler and the task completed late) the append is skipped and `None`
    returned. State mapping: `pending_stop` (or `result.stop_reason`) →
    `Stopped { reason }`; else `success` → `Completed { result }`; else
    `Failed { error }`.
  - `finalize_stopped(id, reason)` — force-finalize a still-running child as
    `Stopped` (used by `shutdown` timeout); one-way like `complete`.
- **Child ids** come from a process-global counter
  `NEXT_CHILD_ID: AtomicU64` (starts at 1). Every `SubagentManager` in the
  process draws from it, so two concurrently-alive managers can never mint the
  same id — hosts route wrapped `AgentEvent::SubagentEvent`s to panes by
  first-match on child id, so a collision would cross-contaminate surfaces.

## Concurrency & grant-back

- **Two pools**: the parent pool is `Semaphore(max_concurrent_requests)`; the
  child pool is `Semaphore(max(1, requests − parent_reserved_permits))`
  (`reserve = parent_reserved_permits.min(requests − 1)`). Children acquire
  provider permits from the child pool only, so the parent can never starve
  under total child saturation. `parent_reserved_permits == 0` sizes the child
  pool equal to the parent pool (pre-feature behavior).
- **Grant-back**: a lazily spawned watcher task polls every **150 ms** and
  runs one lend/reclaim step (`GrantBackState::step`, serialized by a lock;
  `lent: AtomicUsize`). While children run and the parent is idle, it lends
  the still-unlent portion of `reserve` to the child pool; when the parent
  shows activity (or no children remain) it reclaims whatever lent permits
  the child pool has **spare** — in-flight child calls keep theirs; reclaiming
  never cancels or blocks running children. The idle test is
  `available + lent >= total` (not `available >= total`) so the loan itself is
  not misread as parent activity. Deadlock-free by construction:
  `lent ≤ reserve`, so the parent pool always retains ≥ `total − reserve`.
- Exactly **one** watcher per shared pool pair (a `watcher_spawned` flag lives
  on the shared `GrantBackState`); it holds only `Weak` handles and exits when
  the manager is dropped.
- Batch admission: every child spawns immediately, but a
  `max_concurrent_children` slot semaphore admits that many into their turn
  loops at once — a finishing child hands its slot straight to the next waiter
  (no fixed chunk barriers).

## Dispatch API

| Method | Purpose |
|---|---|
| `dispatch_single(req, …)` | One child, blocking until the `DelegationResult` |
| `dispatch_batch(tasks, batch_model, batch_toolsets, …)` | Parallel wave from `TaskSpec`s (stable result order by index) |
| `dispatch_batch_with_roles(…)` | `dispatch_batch` + HyperCode per-task role routing (`explorer`/`implementor`) |
| `dispatch_requests(requests, …)` | Wave of pre-built heterogeneous requests (different models/toolsets/budgets/prompts in one wave) |

`DelegationRequest` fields: `goal`, `context`, `tasks` (batch mode when
non-empty), `model`, `toolsets`, `max_turns`, `reasoning`
(`ReasoningEffort`), `max_tokens`, `persist`, `role` (`Leaf` default /
`Orchestrator`), `workdir`, `category` (OMO; mutually exclusive with
`subagent_type`), `subagent_type`, `load_skills`, `prompt_append`, `team`,
`name`. `DelegationRequest::single(goal)` is the single-task constructor.

- **Background path**: `background: true` returns immediately (in practice
  <100 ms) with a `WorkHandle { child_id, goal, started_at }` and the tool
  line `[BACKGROUND] id=<child_id> goal=<goal> started`. The caller never
  awaits child tasks: each wave is loaded into a `tokio::task::JoinSet` owned
  by ONE dedicated watcher task — the only place the JoinSet lives — which
  drains it, fires the completion tap (notices are capped at
  `NOTICE_SUMMARY_MAX_CHARS = 2000` chars ≈ 500 tokens), and exits. Budgeted
  entry points live in `background.rs`
  (`dispatch_background_with_notices_and_budgets`).
- **`Budgets`**: `max_turns`, `max_tokens`, `max_wall_clock_secs` — all
  optional; `validate()` requires every present value `> 0` (errors name the
  offending field); deserialization rejects zeros at parse time. On the
  blocking path only `max_turns` is enforced (as the child's turn cap);
  tokens/wall-clock are enforced by the background watcher (50 ms tick),
  which stops the child with `BudgetExceeded` on breach.
- **`StopReason`** (serde snake_case): `orchestrator_requested`,
  `operator_requested`, `budget_exceeded`, `session_end`.
- **`DelegationState`**: `Running`, `Completed { result }`, `Failed { error }`,
  `Stopped { reason }`; `is_terminal()` — terminal states are one-way
  (FR-019).
- **`DelegationOverview`**: `{ child_id, goal, state, elapsed, tokens }` —
  one record per child in the session overview; in-memory only.

## Control & recovery

- `stop_child(id, reason)` — records `pending_stop` FIRST (the terminal record
  keeps the reason), then sets the per-child interrupt flag; the child's
  bridge loop forwards it into its Agent, which winds down at the next check
  point. Idempotent while winding down (first reason wins); `Err` for unknown
  ids ("No subagent with id …") or terminal children ("already finished").
- `steer_child(id, message)` — appends to the child's steer slot; delivered
  before the child's next action. `Err` on unknown/terminal ids or an empty
  message.
- `child_status(id)` — snapshot `DelegationOverview` (running with live
  elapsed/tokens, or the terminal record from history); `overview()` —
  running children + terminal history, oldest first.
- `shutdown(timeout)` — signals every child `SessionEnd`, waits bounded by
  `timeout` (50 ms poll) for the registry to drain, then force-finalizes
  stragglers as `Stopped { SessionEnd }` and returns the final overview.
  Never acquires a permit; control stays live under child saturation.
- **Cooperative interrupt**: a manager-level `Arc<AtomicBool>`
  (`signal_interrupt` / `interrupt_handle`) plus per-child flags; a forwarder
  task polls at 50 ms.
- **Self-recovery**: a child turn that dies with a fatal PROVIDER error
  (`TurnResult::fatal_provider_error` — not behavioral fatal tool errors)
  gets its poisoned history cleared and the turn re-run from the initial
  prompt on the SAME `Agent` (interrupt/steer bridges stay wired), up to
  `subagent_recovery_attempts` extra attempts. Usage and iteration counts
  accumulate across attempts; each retry surfaces
  `AgentEvent::RetryAttempt`; interrupts always win over retry. 0 disables
  (single-shot pre-feature behavior).

## Events & taps

- Orchestration events: `AgentEvent::SubagentSpawn { id, goal, model,
  toolset_summary, depth }`, `SubagentComplete`, `SubagentFailed`,
  `SubagentStopped { id, goal, reason }` (reason as snake_case string), plus
  per-batch `DelegationBatchComplete`. Child-internal events are wrapped as
  `AgentEvent::SubagentEvent { id, event }` to taps while the per-dispatch
  channel keeps receiving raw events.
- Emission fan-out at every site: per-dispatch `event_tx` → manager tap →
  recorder tap. The manager tap resolves local-first:
  `SubagentManager::set_event_tap`, else the process-global
  `tap::set_global_tap` (process-scoped; manager-local takes precedence).
- The **recorder tap** (`set_recorder_tap`) is a SECONDARY channel fed
  alongside the external tap; it never participates in tap resolution, so it
  can never shadow a host tap (the T029 bug). `subagent_control` installs one
  to fill per-child log rings.
- Startup wiring order (manager → `SubagentControl` → `set_global_tap`) is
  pinned by the T031 regression test `tests/tap_wiring_order.rs`.

## Capacity

`SystemCapacity::detect()` (cached in a `OnceLock` for the process lifetime):
logical CPUs via `available_parallelism`, total/available RAM via `sysinfo`.

| Constant | Value |
|---|---|
| `HARD_CHILD_CEILING` | `32` |
| `FLOOR_CHILDREN` | `4` |
| `DEFAULT_MEM_RESERVE_MB_PER_CHILD` | `256` MB |
| `DEFAULT_MEM_MAX_FRACTION` | `0.6` |

`capacity_children(cap, reserve_mb, fraction)` = `min(mem_limit, cpu_limit)`
clamped to `[4, 32]`, where `mem_limit = (available × fraction) / reserve`.
`capacity_requests(children) = (children + 2).min(64)` — children are
network-bound; the request semaphore, not CPU count, is the real throttle.

## Plan→isolate→evaluate→join pipeline (spec 023)

- **`TaskGraph`**: a `BTreeMap<TaskId, TaskNode>` (deterministic
  serialization) plus `baseline_revision` and `run_id`.
  `TaskGraph::from_strict_json(json, project_root)` parses the planner's
  strict JSON (rejecting bad `format` tags, invalid task ids, schema
  violations) and validates; `to_strict_json()` round-trips (statuses/attempts
  reset to `Pending`/`0`); `from_workstreams(&[LegacyWorkstream], baseline)`
  converts the legacy workstream shape. `TaskId` is restricted to
  `[a-z0-9-]+`. `TaskNode` carries `objective`, `dependencies`, `read_set`/
  `write_set` (repo-relative paths), `artifact_ids`, `role`
  (`WorkerRole`: explorer/implementor/orchestrator), `model_tier`
  (`ModelTier`: economical rank 0 / frontier rank 1), `risk` (low/medium/
  high), `acceptance` criteria, `verification` (`VerificationPlanView`),
  `isolation` (`SharedCheckout` default / `IsolatedWorktree`), and runtime
  `status` + `attempts`.
- **Six structural invariants** (`TaskGraph::validate`, rule constants in
  `task_graph::rules`): no dependency cycles (`dependency_cycle`); every
  dependency exists (`unknown_dependency`); no two
  concurrently-dispatchable tasks share a write-set path
  (`concurrent_write_overlap`; ancestor-related pairs are sequenced and
  legal); read/write paths stay inside the project root
  (`path_outside_project_root`); ≥1 acceptance criterion per task
  (`missing_acceptance_criterion`); high-risk tasks need a required
  verification step or risk-triggered review (`high_risk_without_verification`).
  `TaskStatus` lifecycle: `Pending → Ready → Dispatched → Evaluating →
  Completed | Failed | Degraded | Blocked | Skipped`; `Completed`/`Failed`/
  `Skipped` are terminal (`is_terminal`).
- **`Scheduler`** (`SchedulerConfig { max_concurrent_workers: 16,
  max_repair_attempts: 3 }`): `run_to_completion(graph, run, dispatcher,
  gate, workdir)` drives deterministic waves — take `ready_nodes()` (Pending
  tasks whose deps are all Completed; Kahn order with alphabetical
  tie-break), partition into conflict groups, log `conflict_sequenced` per
  multi-member group, run groups concurrently while each group's tasks run
  strictly sequentially. Per task: acquire a concurrency permit (deferral
  logged `deferred_concurrency_cap`), transition `Pending → Ready →
  Dispatched`, then dispatch → evaluate → repair/escalate until terminal.
  Every accepted transition is persisted before the loop moves on. Only
  `Pending` tasks are ever dispatched — **completed tasks are never
  re-executed** (FR-013/SC-004; pinned by `tests/scheduler_resume.rs`, T017).
- **`ConflictAnalyzer`**: two ready tasks conflict iff their write sets share
  a path OR either write set is empty (undeclared = may write anything,
  conflicts with everyone); transitive union-find merge; output groups sorted
  by first id. `needs_replan(graph)`: any `Pending` node depends on a
  `Failed`/`Degraded` node.
- **`TaskDispatcher`** trait: `dispatch(&task, workdir) -> bool` (true =
  worker finished; false = crash). The real implementation lives in joey-cli
  wrapping `manager.dispatch_requests`.
- **Evaluation**: `VerificationGate::run(&plan, workdir) -> GateOutcome`
  (`Passed` / `Failed(DefectBundle)` / `Degraded` when a verification command
  is unavailable). `Evaluator::evaluate` converts outcomes into an
  `EvaluationDirective` — `Complete`, `Repair { defect }` (budget remains),
  `Escalate { defect }` (per-tier budget exhausted below the ladder top),
  `Fail { defect }` (exhausted at the top tier), or `MarkDegraded` —
  consulting the `RepairLedger`. The FR-021 exhaustion ladder counts attempts
  PER TIER (a tier bump resets the counter); terminal failure only at the
  highest tier. High-risk tasks force `risk_triggered_review` on the plan
  view (FR-022).
- **`Joiner`**: `collect(ws, task, run) -> ChangeBundle` snapshots what the
  isolated workspace actually wrote (FR-017 divergence check: paths outside
  the declared write set become `DivergenceReport` evidence);
  `integrate(bundles, on_integrated)` applies bundles into the shared root
  via three-way apply, returning `IntegrationReport` or
  `IntegrationConflict`. No `git commit` is ever run — integration leaves no
  user-visible history entries (FR-018).
- **Workspace isolation**: `WorkspaceIsolation::new(project_root, run_dir)`;
  worktrees live at `<run_dir>/worktree/<task-id>`. `prepare(task)` is
  idempotent (reuses a resumed workspace; mode recovered from the
  `.joey-worktree-copy` marker) and prefers `git worktree add --detach`
  (`WorktreeMode::GitWorktree`), falling back to a recursive copy excluding
  `.git`, `target`, `node_modules`, `.venv`, `dist`, `build`
  (`WorktreeMode::FullCopy`). `cleanup(ws)` prunes/removes.
- **Evidence**: run directory
  `~/.joey/hypercode/projects/<project-hash>/runs/<run-id>/` (project hash =
  FNV-1a-64 hex of the canonicalized path) holding `graph.json` (rewritten
  atomically per transition), `nodes/<task-id>.json`,
  `evidence/<task-id>.json` (immutable `EvidenceRecord`s, ids `ev-N`),
  `patches/<task-id>.patch`, and the append-only `decisions.jsonl`.
  `DecisionEntry.cause` must come from the pinned 13-entry `DECISION_CAUSES`
  vocabulary (`dependency_completed`, `worker_completed`, `gate_passed`,
  `gate_failed`, `repair_scheduled`, `escalated`, `degraded`,
  `override_acknowledged`, `replanned`, `deferred_concurrency_cap`,
  `conflict_sequenced`, `baseline_mismatch_abort`, `run_resumed`).
  `RunHandle::create_at` builds the tree; `RunHandle::resume_at` refuses on
  baseline mismatch (FR-030) and rebuilds the evidence counter.

## Teams (feature 022)

Off by default (`hypercode.team.enabled`). The in-process registry
(`team::global_teams()`, a `OnceLock`) is the claiming authority; JSON state
is written synchronously under `<joey-home>/teams/<team>/` on every change.
`register_spawn` lazily creates the team — the first child is the **lead**
(`SubagentRole::Orchestrator`, gets `delegate_task` on a shared child manager
plus the team toolset); later spawns are teammates (bound team tools +
`team` toolset, identity = member name). Team states: `Active` /
`WindingDown` / `Closed`; member status `Idle`/`Working`/`Stopped`; task
status `Pending`/`Running`/`Done`/`Failed`.

Three tools (registered unconditionally; exposure scoped by the `team`
toolset): `TeamStatusTool` (`team_status`), `TeamMessageTool`
(`team_message`, per-poll message limit from `hypercode.team.message_limit`,
default 10), `TeamTasksTool` (`team_tasks` — actions add/claim/complete/
release/list). `bound_team_tools(team, member, limit)` builds member-bound
instances injected into a team child's registry. The contract-pinned
directives `TEAM_LEAD_DIRECTIVE` and `TEAMMATE_DIRECTIVE` (do not reword) get
configured preamble lines: a concurrency cap line for the lead
(`hypercode.team.max_members` default 8, `max_parallel_members` default 4)
and a poll-cadence line for teammates (`poll_interval_ms` default 500). A
finishing team child releases its claimed tasks on failure and pushes a
`[TEAM] <team>/<member> …` notice either way (`manager::team_child_finished`).
A `team` spawn with the feature off errors `team mode is disabled`.

## Tools exposed

| Tool | Struct | Toolset | Notes |
|---|---|---|---|
| `delegate_task` | `DelegateTask` | `delegation` | Single/batch, `role` explorer/implementor routing, `category`/`subagent_type` OMO routing (mutually exclusive, BC-011), `background`, `budgets`, `persist`, `load_skills`, `team`/`name`. Batch `tasks[]` supports per-task `subagent_type` (any OMO agent): resolved model + identity prompt, resolved model wins over per-task/batch `model`; composes with per-task `role`. Role mapping defaults to direct 1:1 specialists (explore/hephaestus/atlas) — toggle `hypercode.omo_specialists.enabled=false` for legacy chains. Role directives: `EXPLORER_DIRECTIVE` (read-only facts, `file-read`+`terminal`+`web`), `IMPLEMENTOR_DIRECTIVE` (execution only, `file`+`terminal`+`web`); role config tables `hypercode.<explorer|implementor>.<provider>` fill model/max_tokens/max_turns/reasoning_level gaps-only |
| `subagent_control` | `SubagentControl` | `delegation` | Actions `steer`/`stop`/`list`/`status`/`log`/`wait`; per-child log ring capped at 256 lines; `log` default last=10; `wait` default timeout 60 s, 50 ms poll; read-only actions never acquire provider permits |
| `call_omo_agent` | `CallOmoAgent` | `delegation` | Junior's research path: forces `subagent_type` (`explore`/`librarian`/`oracle`), then delegates to the inner `DelegateTask` |
| `team_status` / `team_message` / `team_tasks` | `team.rs` tools | `team` | See Teams above |

Registration entry points in `lib.rs`: `register_orchestration`,
`register_orchestration_with_allocator` (dynamic model allocator; `auto`
model resolution via `ModuleId::Subagent`),
`register_orchestration_with_resolver` (OMO `CategoryResolver`),
`register_orchestration_with_resolver_and_allocator` (both). The
`CategoryResolver` trait (`resolve_category` / `resolve_subagent_type` →
`ResolvedDelegation { model, prompt_append }`) is implemented by the CLI
layer to avoid a circular dependency. Toolset mechanics are described in
[joey-tools.md](joey-tools.md); the consumer wiring in
[joey-cli.md](joey-cli.md).

## Testing (22 files)

- `tests/background.rs` — feature-020 background mode; blocking-path byte parity (T007/T008).
- `tests/batch_resilience.rs` — one failed child never aborts the others (SC-003).
- `tests/budgets.rs` — per-child resource budgets: parse-time rejection, breach stops (T019).
- `tests/category_delegation.rs` — `delegate_task` category routing contract (T063).
- `tests/concurrency_limiter.rs` — the parent semaphore is shared across batch children (SC-008).
- `tests/control_tool.rs` — `subagent_control` against real manager plumbing (T014/T015).
- `tests/evaluator_loop.rs` — evaluator API end-to-end over the four directive paths (T024).
- `tests/events.rs` — spawn/complete/fail/batch event envelopes (SC-009).
- `tests/interrupt.rs` — cooperative interrupt propagation (FR-015).
- `tests/isolation.rs` — child context isolation; parent history untouched (US2/AC1).
- `tests/isolation_join.rs` — isolation + joiner end-to-end (spec-023 T020).
- `tests/model_selection.rs` — per-subagent model selection in a mixed batch (SC-004).
- `tests/neurocode_cascade.rs` — parent NeuroCode engine flows into children; same graph.db (FR-021).
- `tests/notices.rs` — background completion notices: one per failure, bounded size (T011/T012).
- `tests/parallel_batch.rs` — parallel batch wall-clock vs slowest child (SC-001).
- `tests/parallel_tap.rs` — event tap receives lifecycle + wrapped child events.
- `tests/recovery.rs` — self-recovery from fatal provider errors; accounting accumulates.
- `tests/run_state.rs` — spec-023 run-state store: layout, atomic writes, evidence ids.
- `tests/scheduler_resume.rs` — resumed runs never re-execute completed tasks (T017/SC-004/FR-030).
- `tests/tap_wiring_order.rs` — real startup tap wiring order regression (T031).
- `tests/task_graph_validation.rs` — public task-graph surface incl. all six invariants (T014).
- `tests/team_tools.rs` — team registry lifecycle via public API only (T015).

## See also

- [../orchestration.md](../orchestration.md) — orchestration subsystem doc
- [joey-omo.md](joey-omo.md) — the OMO layer built on this crate
- [joey-agent-core.md](joey-agent-core.md) — the `Agent` turn loop children run
- [joey-tools.md](joey-tools.md) — tool registry, toolsets, filtering
- [joey-cli.md](joey-cli.md) — registration wiring, HyperCode engine
- [joey-neurocode.md](joey-neurocode.md) — NeuroCode engine cascade (feature 015)

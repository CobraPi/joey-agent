# Feature Specification: Enterprise Orchestration Runtime

**Feature Branch**: `023-enterprise-orchestration-runtime`

**Created**: 2026-09-02

**Status**: Draft

**Input**: User description: "please apply these new features to hypercode and neurocode: Implement this as one shared enterprise execution protocol: NeuroCode supplies repository intelligence; HyperCode owns orchestration. Do not duplicate routing or task state across both commands." (Full responsibility split, typed task-node schema, deterministic scheduler loop, worktree isolation, evaluator-repair loop, graph-based routing rules, structured Reflexion memory, and the eight-step delivery sequence were provided in the original request.)

## Clarifications

### Session 2026-09-02

- **Q1**: Is flipping the two feature flags to enabled-by-default part of this feature's delivery scope? → A: Yes — included in this feature; the flip executes once the SC-001 parity check passes in CI.
- **Q2**: Is there a bound on how many isolated workers may run concurrently in a conflict-free wave? → A: Yes — a configurable maximum, defaulting to 16, enforced by the scheduler.
- **Q3**: What happens on resume when the baseline repository revision has changed since the run was persisted? → A: The run refuses to resume and stops with a clear report directing the user to relaunch; relaunching re-plans against the current revision.
- **Q4**: Can a task complete when its required verification command is unavailable or cannot run? → A: No — a degraded gate never completes the task by itself, is not treated as a code defect, and clears only when the command becomes runnable or the user explicitly acknowledges an override.
- **Q5**: What rule decides escalation to a higher capability tier ("when economical")? → A: Exhaustion ladder — escalate when repair attempts are exhausted at the current tier; the run fails only when the highest available tier is also exhausted.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Repository-Aware Task Analysis (Priority: P1)

When a user asks for a code change, the repository intelligence command analyzes the request against the current codebase and returns one unified analysis: which artifacts the change targets, which are indirectly impacted, which conventions actually apply at those locations, how complex and risky the change is, which worker capability tier it deserves, and exactly which checks verify it. Every later stage of execution consumes this single analysis instead of re-deriving facts.

**Why this priority**: All downstream behavior — routing, isolation, verification gates, memory — consumes this analysis. Without it nothing else in the feature can function, and it delivers standalone value even in single-worker runs (better context, right-sized capability tier, scoped verification).

**Independent Test**: Can be fully tested by requesting an analysis for a change in a sample repository and confirming the report lists direct and indirect artifacts, combined conventions, complexity/risk, tier, and a scoped verification recipe — with no execution pipeline enabled.

**Acceptance Scenarios**:

1. **Given** a change request touching one module that others depend on, **When** the analysis runs, **Then** the report lists both the directly targeted artifacts and the transitively impacted ones.
2. **Given** instruction files exist at organization, repository and module level plus scoped rules that apply only to certain paths, **When** conventions are resolved for a task, **Then** the effective policy combines every applicable layer and applies scoped rules only to matching task paths — never first-found-wins.
3. **Given** a request touching a widely depended-upon component, **When** complexity is classified, **Then** the classification reflects dependency fan-in/fan-out, affected module count, public interface exposure, ownership boundaries and prior anti-pattern matches — not keyword matches alone.
4. **Given** the existing analysis interface in use today, **When** the new capability is introduced, **Then** existing consumers continue to work unchanged (the capability is additive).

---

### User Story 2 - Validated Typed Task Plans (Priority: P2)

Instead of loose "workstreams" carrying only an identifier and a focus sentence, plans become typed task nodes — each carrying its objective, dependencies, read set, write set, related artifacts, worker role, capability tier, risk level, acceptance criteria, verification plan, isolation mode, status and attempt count. A validator enforces structural safety before anything runs, and legacy plan output keeps working by being converted immediately into this typed form.

**Why this priority**: Typed nodes with declared read/write sets are the precondition for safe parallelism (Story 4), deterministic scheduling (Story 3) and graph-based routing (Story 6). Shipping the representation and validator first de-risks every later story.

**Independent Test**: Can be fully tested by feeding legacy and strict-format plans into the validator: valid plans produce a typed graph; cyclic, overlapping-write, out-of-project, acceptance-free and unverified-high-risk plans are rejected with rule-specific errors.

**Acceptance Scenarios**:

1. **Given** a plan produced in the legacy two-workstream format, **When** it is parsed, **Then** it is immediately converted into a validated typed graph and execution proceeds.
2. **Given** a plan in the strict structured format, **When** it is parsed, **Then** each node's declared fields are preserved exactly (including read/write sets, acceptance criteria and verification plan).
3. **Given** a plan containing a dependency cycle or a reference to a missing task, **When** validated, **Then** it is rejected and the offending tasks and violated rule are named.
4. **Given** a plan whose high-risk task lacks required verification, **When** validated, **Then** it is rejected and the reason stated.

---

### User Story 3 - Deterministic, Persistent Execution Runs (Priority: P3)

Scheduling moves out of model improvisation into a deterministic runtime: the runtime repeatedly dispatches the tasks whose dependencies are satisfied, partitions each wave so that only non-conflicting tasks run concurrently, evaluates results, transitions task state, and re-plans only when genuinely blocked. Every run persists its graph, per-task state, evidence, patches and an append-only decision log under a per-project, per-run location, so an interrupted run resumes from its last committed task instead of starting over.

**Why this priority**: Determinism and persistence convert orchestration from best-effort prompting into an auditable protocol. Resumability and auditability matter most once multiple tasks run (after Stories 1-2) and before writers run in parallel (Story 4).

**Independent Test**: Can be fully tested by starting a multi-task run in a sample repository, killing it mid-run, and resuming: completed tasks are not re-executed, and the decision log explains every recorded transition.

**Acceptance Scenarios**:

1. **Given** a validated graph with three independent tasks, **When** the run executes, **Then** all three are dispatched in the same wave and the persisted graph records their transitions.
2. **Given** a run interrupted after some tasks completed and an unchanged baseline revision, **When** the run resumes, **Then** previously completed tasks are not re-executed and in-flight tasks are re-attempted from their persisted state.
3. **Given** any task state transition, **When** the run finishes or is inspected, **Then** the decision log contains an entry attributing the transition to its cause (evaluation verdict, dependency completion, re-plan, escalation).
4. **Given** a wave where two ready tasks declare overlapping write sets, **When** dispatching, **Then** they are sequenced rather than run concurrently.

---

### User Story 4 - Safe Parallel Writers with Isolation and Clean Integration (Priority: P4)

Read-only analysis workers share the original checkout and index. Every parallel writing worker receives an isolated ephemeral copy of the repository and returns a change bundle — baseline revision, declared write set, actual observed write set, patch and evidence. A joiner verifies actual write sets, applies patches with three-way conflict detection, and refreshes repository intelligence incrementally. No user-visible repository history entries are created unless the user asks.

**Why this priority**: This is where parallelism becomes safe rather than lucky. It depends on typed write sets (Story 2) and the persisted run (Story 3), and it is the prerequisite for trustworthy team-scale execution.

**Independent Test**: Can be fully tested by running a two-writer plan against a sample repository: each writer operates in its own copy, the joiner integrates both patches, an intentionally conflicting third patch is detected and reported, and the baseline repository's history is untouched.

**Acceptance Scenarios**:

1. **Given** a plan with two independent writing tasks, **When** the wave dispatches, **Then** each writer operates in its own isolated copy branched from the same baseline revision.
2. **Given** a completed writer, **When** its result is collected, **Then** the change bundle records the baseline revision, declared write set, actual write set, patch artifact and evidence.
3. **Given** a worker that edited files outside its declared write set, **When** the bundle is inspected, **Then** the divergence between declared and actual write sets is reported.
4. **Given** two sibling patches touching the same lines, **When** integration runs, **Then** the conflict is detected by three-way comparison before any application and surfaced rather than silently overwritten.
5. **Given** a fully integrated run, **When** the user inspects their repository, **Then** no commits or history entries were created unless explicitly requested.

---

### User Story 5 - Verified Completion Gates with Repair and Escalation (Priority: P5)

A task is not complete until its required verification gate finishes and passes. On failure, the system assembles a structured defect record — failed commands, policy violations, reviewer findings, changed paths — routes it to a repair worker, and re-evaluates. If repairs are exhausted at the assigned capability tier, the task escalates to the next higher capability tier; only when the highest available tier is also exhausted does the run fail with a clear report. Background verification may continue for information, but never substitutes for the gate.

**Why this priority**: Completion gates are the difference between "looks done" and "is done". They build on the analysis plane's verification recipes (Story 1) and the persisted task state (Story 3), and they protect every parallel integration (Story 4).

**Independent Test**: Can be fully tested with a deliberately broken worker result: the gate fails, a defect record is produced naming the failing command, a repair worker fixes it, the gate passes, and the task completes; a permanently failing task exhausts the escalation ladder and fails the run with a report.

**Acceptance Scenarios**:

1. **Given** a worker result whose verification command fails, **When** evaluated, **Then** the task remains incomplete and a defect record lists the failed command, policy violations and changed paths.
2. **Given** a defect record, **When** repair is scheduled, **Then** a repair worker receives the defect record and the task's attempt count increases.
3. **Given** repeated failed repairs at the assigned tier, **When** attempts are exhausted, **Then** the task escalates to the next higher capability tier; the run stops with a failure report naming the task and its defects only when the highest available tier is also exhausted.
4. **Given** a high-risk task, **When** its verification passes, **Then** an independent specialist review runs before approval, and review findings enter the same defect loop.

---

### User Story 6 - Graph-Based Execution Mode Routing (Priority: P6)

The system keeps three concerns separate: the intelligence command decides economical versus frontier capability; the orchestration command decides single worker, parallel subagents or coordinated team; the persona layer only supplies roles and personas. Mode selection derives from graph properties — write-set overlap forces a single worker, deep strict dependencies force sequential subagent execution, genuinely independent components allow parallel subagents, and independent components needing coordination select team mode. Team mode is seeded from the validated graph so the team lead coordinates existing nodes rather than inventing a new decomposition.

**Why this priority**: Routing correctness depends on the preceding stories; replacing the "two workstreams implies a team" heuristic removes a safety hole that only matters once typed graphs exist.

**Independent Test**: Can be fully tested by preparing four plans with known shapes (overlapping writes; deep chain; independent components; independent components requiring coordination) and asserting the selected mode for each.

**Acceptance Scenarios**:

1. **Given** tasks whose write sets overlap, **When** routing decides the mode, **Then** single-worker execution is selected regardless of task count.
2. **Given** a strict dependency chain deeper than two levels, **When** routing decides, **Then** sequential subagent execution is selected.
3. **Given** two or more independent components that require coordination, **When** routing decides, **Then** team mode is selected and the team's task list is pre-seeded from the validated graph.
4. **Given** two or more independent components with no coordination needed, **When** routing decides, **Then** parallel subagents are selected.

---

### User Story 7 - Structured Outcome Memory (Priority: P7)

The system records lessons only after verified outcomes, each with provenance: task signature, repository revision, related artifacts, applied policy identifiers, failure signature, resolution, evidence links, confidence and confirmation history. Lessons expire or are down-ranked when the code they reference changes materially, so stale persona notes never harden into permanent guidance. Free-text wisdom scraping is replaced by these structured records.

**Why this priority**: Memory quality compounds over time and depends on verified outcomes (Story 5) as the recording trigger; it is valuable but not blocking for the execution protocol itself.

**Independent Test**: Can be fully tested by completing one failing-then-repaired task and one permanently failing task, then confirming exactly the appropriate lessons exist with correct provenance; after rewriting the referenced code, the lessons are down-ranked or expired.

**Acceptance Scenarios**:

1. **Given** a task that failed verification and was repaired, **When** the outcome is recorded, **Then** a lesson exists linking the failure signature, resolution, revision and evidence.
2. **Given** an unverified or in-progress task, **When** the run ends, **Then** no lesson is recorded for it.
3. **Given** a recorded lesson whose referenced code has since changed materially, **When** lessons are consulted, **Then** the lesson is expired or down-ranked rather than offered at full strength.

---

### User Story 8 - Risk-Triggered Specialist Review (Priority: P8)

Changes classified as risky — public interface exposure, security-sensitive areas, concurrency, broad fan-out, ownership boundary crossings — trigger an independent reviewer persona that evaluates the change before approval. Findings feed the defect loop; review is skipped gracefully when no reviewer is configured, and low-risk changes are never slowed by it.

**Why this priority**: An optional quality multiplier that composes on top of the risk signals (Story 1) and the defect loop (Story 5); it can land last without blocking the protocol.

**Independent Test**: Can be fully tested by submitting one high-risk and one low-risk worker result: the high-risk one is reviewed (findings affect approval), the low-risk one is not.

**Acceptance Scenarios**:

1. **Given** a task flagged high-risk for public interface exposure, **When** its verification passes, **Then** a specialist review runs before approval.
2. **Given** a reviewer finding, **When** the review completes, **Then** the finding enters the defect loop and blocks approval until addressed or explicitly accepted.
3. **Given** no reviewer persona configured, **When** a high-risk task completes, **Then** the run proceeds with a recorded notice instead of failing.
4. **Given** a low-risk task, **When** evaluated, **Then** no review is requested.

### Edge Cases

- What happens when a worker edits files outside its declared write set? (Detected at bundle inspection; divergence reported and treated as a defect or plan violation.)
- What happens when the baseline repository revision changes while a run is in flight or between interruption and resume? (Detected at integration and on resume; the run stops with a clear report rather than silently applying or rebasing stale patches; relaunching re-plans against the new revision.)
- What happens when two sibling patches conflict? (Detected by three-way comparison before application; surfaced for re-planning — never partially applied.)
- What happens when a required verification command is unavailable or cannot run in the environment? (Recorded as a degraded — not passed — gate; it never completes the task by itself and is not routed to code-defect repair; it clears only when the command becomes runnable or the user explicitly acknowledges the override.)
- What happens when the planner output is ambiguous, empty, or describes a trivial single-task change? (Converted to a single-node graph; routing falls back to single worker.)
- What happens when instruction layers contradict each other? (Combined with explicit general-to-specific precedence; the conflict is surfaced in the effective policy rather than silently dropped.)
- What happens when a run is interrupted mid-wave with some task state uncommitted? (Resume re-executes only in-flight tasks; committed results are never repeated.)
- What happens when both feature flags are off? (All pre-existing behavior is preserved exactly — legacy planning, routing, execution and memory.)
- What happens when a conflict-free wave contains more ready tasks than the concurrency limit allows? (Excess tasks queue deterministically and dispatch as slots free; the decision log records each deferral.)
- What happens when outcome memory references code that has been rewritten? (Lesson down-ranked or expired on next consultation.)
- What happens when a team-mode lead proposes tasks outside the validated graph? (Proposal rejected or routed through re-planning; the graph remains authoritative.)

## Requirements *(mandatory)*

### Functional Requirements

*Note: FR identifiers are stable; FR-030 and FR-031 are late additions placed in their semantic sections rather than renumbered.*

*Analysis plane*

- **FR-001**: The system MUST produce, for every accepted change request, a unified task analysis identifying directly targeted artifacts, transitively impacted artifacts, effective conventions, complexity, risk, recommended capability tier, an execution suggestion, and a scoped verification recipe.
- **FR-002**: Convention resolution MUST combine all applicable layers — organization policy, repository instructions, module/directory instructions, matching scoped rules, and task-specific contracts — and MUST apply scoped rules only to task paths they match; first-found-wins resolution is prohibited.
- **FR-003**: The system MUST parse instruction sources from other assistant ecosystems (repository-level assistant and copilot instruction files) as policy inputs alongside its own instruction files.
- **FR-004**: Complexity classification MUST account for dependency fan-in and fan-out, number of affected modules, public interface exposure, ownership boundaries, and previously recorded anti-pattern matches — in addition to keyword signals.
- **FR-005**: The analysis capability MUST be introduced additively without altering or breaking the existing analysis interface or its consumers.

*Typed planning*

- **FR-006**: Task plans MUST be represented as typed task nodes carrying, at minimum: identifier, objective, dependencies, read set, write set, related artifact identifiers, worker role, capability tier, risk level, acceptance criteria, verification plan, isolation mode, status, and attempt count.
- **FR-007**: The system MUST continue to parse existing legacy plan output (workstream-style) and MUST convert it immediately into the validated typed graph representation before any execution.
- **FR-008**: The system MUST accept an additional strict, structured plan format for new runs that unambiguously expresses all task-node fields.
- **FR-009**: Plan validation MUST reject: dependency cycles; references to missing dependencies; concurrent dispatch of tasks with overlapping write sets; declared paths outside the project; tasks without acceptance criteria; and high-risk tasks lacking required verification.
- **FR-010**: Every validation rejection MUST identify the offending task(s) and the specific rule violated, in a form the planning stage can act on.

*Runtime*

- **FR-011**: Execution scheduling MUST be computed by a deterministic runtime component from the validated graph — not inferred by the planning model at prompt time.
- **FR-012**: The runtime MUST persist, per project and per run: the current graph, per-task state, per-task evidence, produced patches, and an append-only decision log.
- **FR-013**: A persisted run MUST be resumable after interruption without re-executing completed tasks.
- **FR-014**: Every task state transition MUST be recorded in the decision log with its cause.
- **FR-030**: A persisted run MUST resume only when the current baseline repository revision matches the run's persisted baseline revision; when it differs, the runtime MUST refuse to resume and MUST stop with a clear report directing the user to relaunch (a fresh run re-plans against the current revision).

*Isolation and integration*

- **FR-015**: Parallel writing workers MUST execute in isolated working copies of the repository; read-only analysis workers MAY share the baseline checkout and shared repository index.
- **FR-016**: Each writing worker MUST return a change bundle recording the baseline revision, declared write set, actual observed write set, patch artifact, and evidence records.
- **FR-017**: Integration MUST verify actual write sets, detect patch conflicts before application using three-way comparison, and refresh repository intelligence incrementally after integrating each patch.
- **FR-018**: Integration MUST NOT create user-visible repository history entries unless the user explicitly requested them.

*Evaluation and repair*

- **FR-019**: A task MUST NOT be marked complete until all of its required verification checks have finished and passed; detached or background verification MUST NOT be treated as a passed gate.
- **FR-020**: Failed evaluation MUST produce a structured defect record containing failed commands, policy violations, reviewer findings, and changed paths, and MUST route it to a repair worker.
- **FR-021**: When repair attempts are exhausted at the assigned capability tier, the system MUST escalate the task to the next higher capability tier; the affected work stops with a clear failure report only when the highest available capability tier's repair attempts are also exhausted.
- **FR-022**: High-risk tasks MUST undergo independent specialist review before approval, and review findings MUST enter the defect loop.
- **FR-031**: A verification gate whose required command is unavailable or cannot run MUST be recorded as degraded — distinct from both a passed gate and a code defect; a degraded gate MUST NOT complete its task, MUST NOT trigger code-defect repair, and MUST clear only when the command becomes runnable or the user explicitly acknowledges an override.

*Routing*

- **FR-023**: Execution mode selection MUST derive from task-graph properties — write-set overlap, strict dependency depth, number of independent components, and cross-component coordination need — and MUST NOT use workstream count as a proxy for safe independence.
- **FR-024**: In team mode, the team task list MUST be pre-seeded from the validated graph, and the team lead MUST coordinate existing nodes rather than inventing a separate decomposition.

*Memory*

- **FR-025**: Lessons MUST be recorded only after verified outcomes, each carrying provenance including task signature, repository revision, related artifact identifiers, applied policy identifiers, failure signature and resolution where applicable, evidence links, and a confidence measure.
- **FR-026**: Lessons MUST be down-ranked or expired when the code they reference changes materially.

*Rollout*

- **FR-027**: The new analysis and execution behaviors MUST each ship behind an independent feature flag, disabled by default, and when disabled the system MUST behave exactly as before.
- **FR-028**: The feature MUST conclude by flipping both feature-flag defaults to enabled once the backward-parity check (SC-001) passes in the continuous integration suite.
- **FR-029**: The scheduler MUST enforce a configurable maximum number of concurrently executing workers (default 16); excess ready tasks MUST queue deterministically as worker slots free up.

### Key Entities

- **TaskAnalysis**: The unified per-request analysis — target and impacted artifacts, effective policies, complexity route, risk assessment, capability tier, execution hint, verification plan. Produced by the analysis plane; consumed by planning, routing, evaluation and memory.
- **PolicyBinding**: One resolved convention rule with its source layer and the paths it applies to; the effective policy is the combination of all applicable bindings.
- **TaskNode**: One unit of planned work with objective, dependencies, read/write sets, artifact references, role, capability tier, risk, acceptance criteria, verification plan, isolation mode, status, and attempt count.
- **TaskGraph**: The validated set of task nodes and dependency edges; the single authoritative execution state. Legacy plans are converted into it.
- **ChangeBundle**: A writer's result — baseline revision, declared and actual write sets, patch artifact, evidence records.
- **DefectBundle**: A structured failure report — failed commands, policy violations, reviewer findings, changed paths — driving repair.
- **EvidenceRecord**: Verifiable proof attached to a task (command output, review outcome, inspection result).
- **OutcomeMemory**: A post-verification lesson with provenance and confidence, subject to expiry or down-ranking.
- **RunState**: The persisted per-run artifacts — graph, node states, evidence, patches, decision log — enabling resume and audit.
- **ExecutionMode**: The selected execution shape (single worker, parallel subagents, coordinated team), chosen from graph properties.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: With both feature flags disabled, 100% of existing orchestration behaviors produce identical outcomes to the current release (full backward parity).
- **SC-002**: 100% of structurally invalid plans (cycles, missing dependencies, concurrently overlapping write sets, out-of-project paths, missing acceptance criteria, unverified high-risk tasks) are rejected before execution with a rule-specific error.
- **SC-003**: Across all parallel runs, zero pairs of concurrently executing writers have overlapping declared write sets, and zero edits occur outside an isolated working copy.
- **SC-004**: After an interruption, resumed runs re-execute zero already-completed tasks.
- **SC-005**: Zero tasks reach "complete" status without a recorded passing verification gate.
- **SC-006**: Zero partially applied integrations: 100% of sibling patch conflicts are detected before any patch in the conflicting set is applied.
- **SC-007**: 100% of decision-log entries for a completed run are sufficient to reconstruct why each task reached its final status, using only persisted run artifacts.
- **SC-008**: 100% of lessons surfaced as guidance were recorded against verified outcomes and carry provenance; every lesson whose referenced code changed materially is expired or down-ranked within the next run that consults it.
- **SC-009**: For tasks whose failure signature matches a previously recorded lesson, the recurrence rate of the same failure is at least 50% lower than for unmatched baselines — demonstrating that memory improves outcomes.

## Assumptions

- The spec directory for this feature is `specs/023-enterprise-orchestration-runtime/` as named in the delivery sequence; the git branch (`023-enterprise-orchestration-runtime`) matches the directory name.
- Both feature flags ship disabled by default (provisionally named `hypercode.execution_graph.enabled` and `neurocode.enterprise_context.enabled`; final key names are a planning-phase decision); flipping both defaults to enabled is the final delivery step of this feature and executes once the SC-001 parity check passes in CI.
- Specialist reviewers ("Momus"/"Oracle") are assumed to be available through the existing persona layer; risk-triggered review uses them when configured and skips gracefully (with a recorded notice) when not.
- Run state lives under the product's per-user home directory, scoped per project and run; no automatic pruning or retention policy is required for the first release.
- Patches are stored as artifacts and applied to the working tree; version-control history entries are created only on explicit user request.
- Legacy workstream-style plan parsing remains supported at least until the flags default to enabled; removal is a future, separately versioned decision per the project's backward-compatibility principle.
- Workers execute on local checkouts on the same machine; remote or distributed execution is out of scope.
- Where lightweight working-copy branching is unavailable on a supported platform, isolation falls back to a full copy; correctness is unaffected, only cost.
- Conflicting instruction layers resolve by explicit general-to-specific precedence (organization → repository → module → scoped rules → task contract), with conflicts surfaced, not silently discarded.
- The delivery sequence (analysis plane → typed graph → runtime scheduling → write-set enforcement and isolation → verification/repair/escalation → outcome memory → risk-triggered review) is the intended implementation order, each step independently testable.

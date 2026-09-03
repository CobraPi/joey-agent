# Tasks: Enterprise Orchestration Runtime

**Input**: Design documents from `/specs/023-enterprise-orchestration-runtime/`

**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/ (config-keys.md, planner-json-format.md, run-state-format.md, public-api.md), quickstart.md

**Tests**: Included — the project constitution (`.specify/memory/constitution.md` Principle IV) mandates tests written alongside implementation, not deferred.

**Organization**: Tasks grouped by user story (spec.md P1–P8) so each story is independently implementable and testable.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies on incomplete tasks)
- **[Story]**: User story this task belongs to (US1–US8)
- Exact file paths in every description

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Configuration surface for both feature flags before any plane ships.

- [X] T001 Add config keys `hypercode.execution_graph.enabled` (false), `hypercode.execution_graph.max_concurrent_workers` (16), `hypercode.execution_graph.max_repair_attempts` (3), `neurocode.enterprise_context.enabled` (false) to the default config text in crates/joey-core/src/config.rs, bump `_config_version` to 34, and add unit tests asserting the defaults round-trip via get_bool/get_i64 (per contracts/config-keys.md)

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Cross-story persistence and gate contracts multiple user stories depend on.

- [X] T002 [P] Implement crates/joey-orchestration/src/evidence.rs: EvidenceRecord (CommandOutput | ReviewOutcome | Inspection | DivergenceReport), the pinned decision-cause vocabulary from contracts/run-state-format.md, run-state directory creation under `joey_home()/hypercode/projects/<project-hash>/runs/<run-id>/`, atomic graph.json and nodes/<task-id>.json writes, append-only decisions.jsonl, and RunHandle::resume with the baseline-revision match check (FR-030); unit tests in the same file plus crates/joey-orchestration/tests/run_state.rs
- [X] T003 [P] Create crates/joey-orchestration/src/evaluator.rs type layer: `VerificationGate` trait (`async fn run(&self, plan: &VerificationPlanView, workdir: &Path) -> GateOutcome`), `GateOutcome::{Passed, Failed(DefectBundle), Degraded}`, `DefectBundle`/`CommandFailure`/`VerificationPlanView` types per contracts/public-api.md, with serde derives and unit tests (the evaluation loop itself lands in US5); register `pub mod evaluator;` in crates/joey-orchestration/src/lib.rs

**Checkpoint**: Persistence + gate contract ready; user story implementation can begin.

---

## Phase 3: User Story 1 - Repository-Aware Task Analysis (Priority: P1) 🎯 MVP

**Goal**: The enterprise analysis plane: unified TaskAnalysis with combined policies, graph-aware complexity, risk, tier, execution hint, scoped verification — all behind `neurocode.enterprise_context.enabled`.

**Independent Test**: quickstart.md §2 — request an analysis for a change in a fixture repo and get target+impacted artifacts, combined policies with surfaced conflicts, fan-in/out signals, risk factors, tier, hint, and scoped verification steps, with the flag off producing legacy behavior.

### Implementation for User Story 1

- [X] T004 [P] [US1] Implement crates/joey-neurocode/src/policy/sources.rs: parsers for JOEY.md, AGENTS.md, CLAUDE.md, .cursorrules, .github/copilot-instructions.md and nested applyTo-scoped files into PolicyBinding sources (FR-003), with unit tests including applyTo glob matching
- [X] T005 [P] [US1] Implement crates/joey-neurocode/src/policy/resolver.rs (plus policy/mod.rs): hierarchical combine organization → repository → module → scoped rules → task contract, globs applied only to matching task paths, conflicts surfaced in `conflicts_with` never dropped (FR-002), unit tests covering layering, glob scoping and conflict surfacing
- [X] T006 [P] [US1] Implement crates/joey-neurocode/src/risk.rs: RiskAssessment, RiskLevel (Low|Medium|High), RiskFactorKind (PublicApiExposure | SecuritySensitive | Concurrency | FanOut | OwnershipBoundary) with the FR-022 high-risk rule, unit tests for factor-to-level mapping
- [X] T007 [P] [US1] Implement crates/joey-neurocode/src/verification_plan.rs: VerificationPlan/VerificationStep derivation scoped to impacted modules with required flags and risk_triggered_review (FR-001 verification half), unit tests for scoping
- [X] T008 [US1] Extend classify() in crates/joey-neurocode/src/classifier.rs: score dependency fan-in/fan-out (store.dependents_count, traverse_edges/traverse_to), affected-module count (nodes_by_source_path grouping), public-API exposure, ownership boundaries, and prior anti-pattern hits (anti_patterns_for_artifacts); activate the currently-unused SignalKind::GraphHub (FR-004); extend the existing classifier unit tests with these signals
- [X] T009 [US1] Implement crates/joey-neurocode/src/analysis.rs: `EnterpriseTaskAnalyzer` trait (analyze/context_for/verification_for/record_outcome), `TaskAnalysis` (revision, target/impacted artifacts, effective_policies, complexity, risk, model_tier, ExecutionHint, verification) per data-model.md, DefaultEngine implementation, `VerifiedOutcome`/`TaskContext` types, and register the new modules (analysis, policy, risk module, verification_plan, memory::outcomes stub with OutcomeMemory types) in crates/joey-neurocode/src/lib.rs re-exports (FR-005: additive, NeuroCodeEngine untouched) — depends on T004–T008
- [X] T010 [US1] End-to-end integration test crates/joey-neurocode/tests/enterprise_analysis.rs: fixture repo with layered instruction files and a hub-type change; asserts combined policies, impacted closure, GraphHub signal, risk factors, tier, execution hint and scoped verification steps in one analyze() call

**Checkpoint**: Analysis plane complete and independently testable — MVP delivered.

---

## Phase 4: User Story 2 - Validated Typed Task Plans (Priority: P2)

**Goal**: TaskNode/TaskGraph with all six validation invariants, strict JSON planning format, and immediate legacy workstream conversion.

**Independent Test**: quickstart.md §3 — cyclic/overlap/out-of-project/no-acceptance/unverified-high-risk plans rejected with rule-specific errors; legacy `<workstreams>` converts to a validated graph.

### Implementation for User Story 2

- [X] T011 [P] [US2] Implement crates/joey-orchestration/src/task_graph.rs: TaskId, TaskNode (all FR-006 fields), TaskStatus, IsolationMode, AcceptanceCriterion, TaskGraph with validate (six invariants from data-model.md, errors naming task ids + violated rule per FR-009/FR-010, undeclared-empty-write-set semantics), ready_nodes (deterministic topo-then-id order), is_terminal, is_blocked, transition, snapshot; register in lib.rs; unit tests for every invariant and transition
- [X] T012 [US2] Implement `TaskGraph::from_strict_json` in crates/joey-orchestration/src/task_graph.rs per contracts/planner-json-format.md (format string `joey-taskgraph/1`, id charset, tier/risk/role/isolation enums, path normalization + project-root containment, unknown-format rejection) with unit tests per schema rule — depends on T011 types (can share the file: implement after T011 lands)
- [X] T013 [US2] Implement `TaskGraph::from_workstreams` legacy converter in crates/joey-orchestration/src/task_graph.rs (objective = focus, empty read/write sets → undeclared → SingleWorker routing, never concurrent) with unit tests, and wire it into crates/joey-cli/src/hypercode.rs so parse_workstreams output converts immediately when `hypercode.execution_graph.enabled` is true (FR-007) — parse_workstreams itself must remain unchanged
- [X] T014 [US2] Integration test crates/joey-orchestration/tests/task_graph_validation.rs: strict-JSON acceptance, each of the six rejections, legacy conversion equivalence

**Checkpoint**: Typed planning + validation complete; scheduler work unblocked.

---

## Phase 5: User Story 3 - Deterministic, Persistent Execution Runs (Priority: P3)

**Goal**: Runtime-owned scheduler loop with conflict partitioning, concurrency cap, replan-on-blocked, and resumable persisted run state.

**Independent Test**: quickstart.md §6 — kill mid-run and resume with no re-executed completed tasks; changed baseline refuses resume with a report.

### Implementation for User Story 3

- [X] T015 [P] [US3] Implement crates/joey-orchestration/src/scheduler.rs: Scheduler + SchedulerConfig, ready-wave selection, ConflictAnalyzer write-set partitioning (overlapping writers sequenced, decision logged `conflict_sequenced`), semaphore concurrency cap (`deferred_concurrency_cap` logged, FR-029), deterministic dispatch order, blocked→replan hook, transition recording via evidence.rs (FR-011/FR-012/FR-014); unit tests with a fake dispatcher and fake gate
- [X] T016 [US3] Wire the scheduler into crates/joey-cli/src/hypercode.rs behind `hypercode.execution_graph.enabled`: waves dispatched via ctx.manager.dispatch_requests, graph/nodes/evidence persisted per contracts/run-state-format.md, resume entry point honoring the baseline check, decisions appended on every transition
- [X] T017 [US3] Integration test crates/joey-orchestration/tests/scheduler_resume.rs: multi-node run interrupted mid-wave resumes without re-executing completed nodes; baseline-mismatch run refuses and reports; wave partitioning serializes overlapping writers; plus a decision-log reconstruction case (SC-007): replay decisions.jsonl and persisted node states to independently reconstruct each task's final status

**Checkpoint**: Runs are deterministic, auditable and resumable.

---

## Phase 6: User Story 4 - Safe Parallel Writers (Priority: P4)

**Goal**: Worktree-isolated writers returning ChangeBundles; three-way joiner with no silent conflicts and no user-visible commits.

**Independent Test**: quickstart.md §5 — two isolated writers from one baseline sha, bundles with declared==actual sets, patches applied, conflicting patch surfaced untouched, `git log` unchanged.

### Implementation for User Story 4

- [X] T018 [P] [US4] Implement crates/joey-orchestration/src/workspace.rs: WorkspaceIsolation::prepare creating `worktree/<task-id>` via `git worktree add --detach` (precedent: joey-speckit-ui/src/staging_impl.rs:48-69) with full-copy fallback and cleanup (FR-015); unit tests with a scratch git repo (skip gracefully where git is unavailable)
- [X] T019 [P] [US4] Implement crates/joey-orchestration/src/joiner.rs: ChangeBundle collection, actual-vs-declared write-set verification with DivergenceReport evidence, `git apply --check --3way` before any application, conflicts surfaced never partially applied (FR-017, SC-006), incremental index-refresh callback after each integration, no user-visible commits unless requested (FR-018); unit tests incl. a deliberate same-line conflict
- [X] T020 [US4] Wire isolation+joiner into the scheduler path in crates/joey-cli/src/hypercode.rs (writers get IsolatedWorkspace, readers SharedCheckout) and add integration test crates/joey-orchestration/tests/isolation_join.rs running the quickstart §5 scenario end-to-end

**Checkpoint**: Parallel writes are isolated, verified and cleanly integrated.

---

## Phase 7: User Story 5 - Verified Completion Gates (Priority: P5)

**Goal**: Awaited verification gates, DefectBundle repair routing, exhaustion-ladder escalation, degraded-gate semantics.

**Independent Test**: quickstart.md §7 — broken worker fails gate, DefectBundle names the command, repair fixes it; permanent failure escalates economical→frontier then fails with a report; unavailable command records Degraded and never completes.

### Implementation for User Story 5

- [X] T021 [P] [US5] Implement the evaluation loop in crates/joey-orchestration/src/evaluator.rs: awaited gate per task (detached verification stays informational only, FR-019), Degraded handling per FR-031 (never completes, never code-defect repair, clears on runnable or explicit acknowledgment), DefectBundle construction from failed commands, repair-dispatch accounting (attempts per tier), exhaustion-ladder escalation (FR-021 as clarified Q5); unit tests with fake gates covering pass/fail/degrade/escalate/terminal-fail paths
- [X] T022 [P] [US5] Add the additive tier-ranking helper in crates/joey-neurocode/src/tier_resolver.rs (`ComplexityTier::rank() -> u8`, Economical < Frontier; AmbiguousDefault resolves before ranking) with unit tests — no new trait impls on existing types
- [X] T023 [US5] Implement the VerifyLoop→VerificationGate adapter in crates/joey-cli/src/hypercode.rs (or a new adjacent module crates/joey-cli/src/hypercode_gate.rs): maps VerificationPlan steps to VerifyConfig, replaces the no-op `|_| false` repair callback (call sites crates/joey-neurocode/src/verify/mod.rs:224 and :262 — the only joey-neurocode file touched by US5) with DefectBundle-driven repair dispatch, and awaits the gate before any task transitions to Completed
- [X] T024 [US5] Integration test crates/joey-orchestration/tests/evaluator_loop.rs: repaired-success scenario (fail→DefectBundle→repair→pass), escalation scenario (attempts exhausted→tier bump→eventual pass), terminal-failure scenario (top tier exhausted→run fails with report), degraded-command scenario (blocked until acknowledgment)

**Checkpoint**: "Complete" now means verified — repair and escalation are real.

---

## Phase 8: User Story 6 - Graph-Based Routing (Priority: P6)

**Goal**: Routing derived from graph properties; team mode pre-seeded from the validated graph.

**Independent Test**: quickstart.md §4 — four plan shapes select SingleWorker / DagSubagents / Team (pre-seeded) / ParallelSubagents.

### Implementation for User Story 6

- [ ] T025 [P] [US6] Add `ModeRoute::{SingleWorker, DagSubagents, ParallelSubagents}` variants and `route_mode_from_graph(hint: &ExecutionHint) -> ModeRoute` in crates/joey-cli/src/hypercode.rs implementing the FR-023 decision table (overlap→SingleWorker; strict depth>2→DagSubagents; independent≥2+coordination→Team; independent≥2→ParallelSubagents; else SingleWorker); route_mode and its existing variants remain unchanged; unit tests cover all five branches
- [ ] T026 [US6] Switch flag-on call sites in crates/joey-cli/src/hypercode.rs (replacing the workstream_count>=2 heuristic at the route decision) and pre-seed team_tasks from the validated graph in team runs (FR-024); extend crates/joey-cli/src/tests/hypercode_team.rs with graph-seeded team assertions

**Checkpoint**: Team mode is graph-driven, not count-driven.

---

## Phase 9: User Story 7 - Structured Outcome Memory (Priority: P7)

**Goal**: Verified-outcome-only lessons with provenance and hash-based expiry/down-ranking, replacing wisdom scraping as guidance when enabled.

**Independent Test**: quickstart.md §8 — repaired task yields exactly one provenance-complete record; rewritten code down-ranks it; unverified tasks yield none.

### Implementation for User Story 7

- [X] T027 [P] [US7] Implement crates/joey-neurocode/src/memory/outcomes.rs: additive `CREATE TABLE IF NOT EXISTS outcome_memory` store (NEUROCODE_SCHEMA_VERSION stays 3), OutcomeMemory per data-model.md, record only from VerifiedOutcome, consult-by-signature/artifact with artifact-hash re-check → expire/down-rank (FR-025/FR-026, SC-008); unit tests with an in-memory DB
- [ ] T028 [US7] Wire record_outcome from evaluator completions and consult in the analysis plane when `neurocode.enterprise_context.enabled` is on, in crates/joey-cli/src/hypercode.rs plus the analyzer call path; OMO notepad text (extract_wisdom/accumulate_wisdom) is no longer used as execution guidance on the flag-on path; unit test asserting zero records from unverified tasks
- [X] T034 [P] [US7] Add the SC-009 failure-recurrence A/B integration test crates/joey-neurocode/tests/outcome_memory_recurrence.rs: deterministic fake-repair harness where a worker with a known failure signature fails unless its consulted guidance contains a verified lesson matching that signature; run the fixture cycle 8 times with consultation off (unmatched baseline — count recurrences) and 8 times with one recorded verified lesson and consultation on; assert the lesson-matched recurrence count is at least 50% lower than the baseline count (SC-009)

**Checkpoint**: Memory is structured, provenance-aware and self-expiring.

---

## Phase 10: User Story 8 - Risk-Triggered Specialist Review (Priority: P8)

**Goal**: High-risk changes reviewed by oracle/momus before approval; findings enter the defect loop.

**Independent Test**: quickstart §7/§8 review clauses — high-risk task reviewed with findings blocking approval; low-risk task unreviewed; missing reviewer records a notice.

- [ ] T029 [US8] Implement risk-triggered review in the evaluation path (crates/joey-cli/src/hypercode.rs + evaluator wiring): invoke the existing oracle/momus personas as reviewers (not team members — team.rs:220 rejects them there) when RiskAssessment is High per FR-022, feed Findings into DefectBundle, record a notice-and-proceed when no reviewer is configured; unit tests for trigger/no-trigger/notice paths

**Checkpoint**: Risky changes get independent eyes before approval.

---

## Phase 11: Polish & Cross-Cutting Concerns

- [ ] T030 [P] Update docs/ (architecture/orchestration sections) and PORTING.md with the new subsystem parity status (dates, Complete/Partial/Deliberate-deviation entries per repo convention)
- [ ] T031 [P] Execute quickstart.md scenarios §1–§9 end-to-end on a scratch repository with JOEY_HOME isolation; record results (pass/fail per scenario) in specs/023-enterprise-orchestration-runtime/quickstart.md as an appended validation log
- [ ] T032 Verify SC-001 flag-off parity: full `cargo build --workspace` + `cargo test --workspace` green with both flags defaulted false, no `~/.joey/hypercode/` tree created on legacy runs; collect the parity evidence summary in the feature directory
- [ ] T033 Execute FR-028: flip both flag defaults to true in crates/joey-core/src/config.rs once T032's parity evidence is recorded, update the default-config unit tests, and re-run the full workspace suite

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — start immediately.
- **Foundational (Phase 2)**: After Phase 1; T002 ∥ T003 (different files).
- **US1 (Phase 3)**: After Phase 1 only (analysis plane is independent of the runtime); T004–T008 parallel, then T009, then T010.
- **US2 (Phase 4)**: After Phase 2 (T003 types); T011 → T012 → T013/T014.
- **US3 (Phase 5)**: After US2; T015 ∥ T016 prep, T017 after.
- **US4 (Phase 6)**: After US3; T018 ∥ T019, then T020.
- **US5 (Phase 7)**: After US3 + US1 (verification plans); T021 ∥ T022, then T023, T024.
- **US6 (Phase 8)**: After US2 (+US1's ExecutionHint for full function).
- **US7 (Phase 9)**: After US5 (verified outcomes are the write trigger); T027 → T028 → T034.
- **US8 (Phase 10)**: After US5 + US1 (risk signal).
- **Polish (Phase 11)**: After all stories; T033 strictly last (requires T032 evidence).

### Within Each User Story

- Tests are written alongside each module in the same task (constitution Principle IV).
- Types/contracts before wiring; wiring before end-to-end integration tests.
- crates/joey-cli/src/hypercode.rs is edited by T013, T016, T020, T023, T025–T029 — those tasks MUST run sequentially (single file); all other [P] tasks touch disjoint files.

### Parallel Opportunities

- Phase 2: T002 ∥ T003.
- US1: T004–T008 all parallel (five disjoint new files in joey-neurocode).
- US2: T011 first, then T012 ∥ T014-test-scaffold; T013 sequential (touches hypercode.rs).
- US4: T018 ∥ T019 (workspace.rs ∥ joiner.rs).
- US5: T021 ∥ T022 (evaluator.rs ∥ tier_resolver.rs).
- Polish: T030 ∥ T031 ∥ T032.

---

## Parallel Example: User Story 1

```bash
# After Phase 1, launch five disjoint implementation tasks together:
Task: "policy sources parser in crates/joey-neurocode/src/policy/sources.rs"        # T004
Task: "policy resolver combine in crates/joey-neurocode/src/policy/resolver.rs"     # T005
Task: "risk model in crates/joey-neurocode/src/risk.rs"                             # T006
Task: "verification plan derivation in crates/joey-neurocode/src/verification_plan.rs"  # T007
Task: "classifier signal extension in crates/joey-neurocode/src/classifier.rs"      # T008
# Then: analysis.rs trait+impl (T009), then the e2e test (T010).
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Phase 1 (config keys) → Phase 2 (evidence + gate types).
2. Phase 3 = US1 complete: analysis plane independently valuable (better context, tiers, verification recipes) with the runtime still off.
3. STOP and VALIDATE: quickstart §1 + §2 green; `cargo test -p joey-neurocode -p joey-orchestration -p joey-core` green.

### Incremental Delivery

Each subsequent story adds one independently testable capability: typed plans (US2) → deterministic runs (US3) → safe parallel writers (US4) → verified gates (US5) → graph routing (US6) → outcome memory (US7) → risk review (US8) → polish + flag flip (Phase 11). Every story leaves both flags default-off; behavior changes only for opted-in runs until T033.

---

## Notes

- [P] = disjoint files, no dependency on incomplete tasks; hypercode.rs-touching tasks are never [P] relative to each other.
- Story labels map tasks to spec.md user stories for traceability.
- Every task's verification is `cargo build -p <crate>` + `cargo test -p <crate>` [filter] (targeted), with the full workspace suite run at phase checkpoints and before T033.
- Commit after each task or logical group; keep `cargo build --workspace` and `cargo test --workspace` green on every increment.

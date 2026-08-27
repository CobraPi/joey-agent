# Tasks: Async Delegation & Subagent Control

**Input**: Design documents from `/specs/020-async-delegation-control/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/, quickstart.md

**Tests**: Tests ARE included — the repo constitution mandates tests alongside implementation and regression coverage for public surfaces (blocking-parity regression T007 is non-negotiable).

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

- **Workspace root**: `/Users/jo110366/Development/joey-agent` (Cargo workspace, crates under `crates/`)
- **Rust sources**: `crates/<crate>/src/`, integration tests: `crates/<crate>/tests/`, inline tests: `#[cfg(test)]` modules in source files
- **Docs**: `docs/` at repository root; parity audit in `PORTING.md`
- Key crates: `joey-orchestration` (control plane), `joey-agent-core` (events/guidance/guardrails), `joey-tools` (toolsets), `joey-cli` (repl.rs, engine.rs), `joey-tui` (app.rs, tui.rs)

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Verify a clean baseline before any changes

- [x] T001 Verify baseline before any changes: run cargo build --workspace and cargo test --workspace from repo root (/Users/jo110366/Development/joey-agent) and record green baseline

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core infrastructure that MUST be complete before ANY user story can be implemented

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [x] T002 [P] Add config keys delegation.parent_reserved_permits (default 1, FR-018) and delegation.wind_down_timeout_secs (default 10, FR-015) alongside existing delegation.* definitions (in crates/joey-orchestration/src/manager.rs, alongside delegation.default_max_turns) with defaults/loading unit tests
- [x] T003 Extend crates/joey-orchestration/src/types.rs: StopReason enum (OrchestratorRequested, OperatorRequested, BudgetExceeded, SessionEnd), Budgets (max_turns/max_tokens/max_wall_clock_secs, all validated >0), TaskSpec.background (default false) + TaskSpec.budgets, DelegationResult.stop_reason, WorkHandle, RunningUsage, ChildHandle, DelegationOverview (terminal states one-way per FR-019); add unit tests for Budgets validation and defaults
- [x] T004 Implement child-handle registry in crates/joey-orchestration/src/manager.rs: HashMap<child_id, ChildHandle> insert/remove/get with one-way terminal-state transitions (FR-019), stop_child (records StopReason before setting interrupt AtomicBool) and steer_child (via existing Agent::steer_handle) and status; already-finished and unknown-id errors; registry lifecycle unit tests
- [x] T005 Implement two-pool reserved-capacity semaphore in crates/joey-orchestration/src/manager.rs: children receive (concurrency_limit − delegation.parent_reserved_permits) permits, grant-back while parent idle, reclaim on parent activity; add parent-starvation unit test (SC-007: control actions <5s under saturation)
- [x] T006 [P] Add AgentEvent::SubagentStopped{id, goal, reason, summary_preview} variant to crates/joey-agent-core/src/events.rs (enum is #[non_exhaustive], strictly additive) and verify all event consumers (wildcard arms) still compile

**Checkpoint**: workspace builds green; registry + types + events + config in place.

---

## Phase 3: User Story 1 - Background Delegation (Priority: P1) 🎯 MVP

**Goal**: background=true delegation returns a handle (id+goal) in <2s while other work proceeds; excess tasks queue; background=false remains byte-for-byte blocking parity

**Independent Test**: delegate with background=true and observe "[BACKGROUND] id=<child_id> goal=<goal> started" returned in <2s (SC-001); re-run blocking-path regression suite unchanged (FR-002)

### Tests for User Story 1

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation** (T007 must PASS before implementation — it pins pre-feature behavior)

- [x] T007 [P] [US1] Write blocking-parity regression tests in crates/joey-orchestration/tests/background.rs asserting background=false (default) result bytes unchanged vs pre-feature behavior (FR-002, SC-005, constitution VII); tests must pass before implementation
- [x] T008 [US1] Write fail-first tests in crates/joey-orchestration/tests/background.rs: background=true returns [BACKGROUND] handle (id+goal) in <2s (FR-001, SC-001) and excess tasks queue under existing concurrency limits (FR-013)

### Implementation for User Story 1

- [x] T009 [US1] Create crates/joey-orchestration/src/background.rs: background dispatch path spawning via manager registry + JoinSet watcher (no bare tokio::spawn), returning WorkHandle and formatted "[BACKGROUND] id=<child_id> goal=<goal> started" result without blocking the caller
- [x] T010 [US1] Add background parameter (boolean, default false) to delegate_task schema and dispatch routing in crates/joey-orchestration/src/delegation_tool.rs; update tool description text

**Checkpoint**: At this point, User Story 1 should be fully functional and testable independently (MVP complete — STOP and VALIDATE)

---

## Phase 4: User Story 2 - Completion Notifications (Priority: P1)

**Goal**: Distilled completion notices ("[SUBAGENT COMPLETE|FAILED|STOPPED] id= goal= outcome= tokens= duration=s" + summary ≤500 tokens) delivered at the next turn boundary; failures never dropped; idle wake

**Independent Test**: run a background child to completion while idle and observe the distilled notice arrive via idle wake (TUI) or next interaction (line REPL)

### Tests for User Story 2

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [x] T011 [P] [US2] Write notice tests in crates/joey-orchestration/tests/notices.rs: distilled "[SUBAGENT COMPLETE|FAILED|STOPPED] id= goal= outcome= tokens= duration=s" format with summary ≤500 tokens (FR-003/004/016), failure completions never dropped (SC-002), and notice size stays bounded regardless of child transcript length — context grows with subagent count, not activity volume (SC-006)

### Implementation for User Story 2

- [x] T012 [US2] Implement completion watcher in crates/joey-orchestration/src/background.rs: on child finish, distill CompletionNotice and push via existing ToolContext::push_background_completion (cap 64, drop-oldest), drained at next run_turn start
- [x] T013 [P] [US2] Implement idle wake in crates/joey-cli/src/engine.rs and crates/joey-tui/src/tui.rs: new EngineCommand::DelegationNoticePending emitted when notices pending while idle, handled in pump_one select loop to wake the agent (FR-004; line REPL degrades to next-interaction delivery — documented deviation); add wake-path tests in crates/joey-cli/tests/ asserting an idle engine emits DelegationNoticePending and starts a turn when notices are pending

**Checkpoint**: At this point, User Stories 1 AND 2 should both work independently

---

## Phase 5: User Story 3 - Steer & Stop (Priority: P1)

**Goal**: Per-subagent steer delivered before the child's next action; stop yields partial result with stop reason; graceful already-finished handling; subagent_control tool registered and guided

**Independent Test**: start ≥3 children, steer one and stop another via subagent_control, verify selective delivery and partial DelegationResult with stop_reason (SC-003)

### Tests for User Story 3

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [x] T014 [P] [US3] Write fail-first tests in crates/joey-orchestration/tests/control_tool.rs: steer delivered before child's next action, stop yields partial result with stop_reason, selective stop of one of ≥3 children (SC-003), already-finished and unknown-id errors (FR-008/009/010)

### Implementation for User Story 3

- [x] T015 [US3] Create crates/joey-orchestration/src/control_tool.rs: subagent_control tool skeleton with steer and stop actions wired to manager registry; stop path records StopReason, emits AgentEvent::SubagentStopped, returns partial DelegationResult; register in register_orchestration_inner in crates/joey-orchestration/src/lib.rs
- [x] T016 [P] [US3] Register subagent_control in the delegation toolset in crates/joey-tools/src/toolsets.rs and add it to sibling-tool name-lists in crates/joey-agent-core/src/guardrails.rs, crates/joey-agent-core/src/compressor.rs, crates/joey-agent-core/src/breakdown.rs; update guidance text in crates/joey-agent-core/src/guidance.rs

**Checkpoint**: User Story 3 functional; US1 and US3 both independently testable

---

## Phase 6: User Story 4 - Progress Inspection (Priority: P2)

**Goal**: list/status/log/wait actions exposing id, goal, elapsed, resources, status with bounded recent-activity slice and wait-for-one timeout

**Independent Test**: delegate background tasks, then subagent_control list → status <id> → log <id> → wait <id> and verify bounded output and timeout behavior

### Tests for User Story 4

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [x] T017 [US4] Write fail-first tests in crates/joey-orchestration/tests/control_tool.rs for list/status (id, goal, elapsed, resources, status; bounded recent-activity slice, last defaults 10), wait with timeout_secs default 60, unknown-id error (FR-005/006/007)

### Implementation for User Story 4

- [x] T018 [US4] Implement list/status/log/wait actions in crates/joey-orchestration/src/control_tool.rs returning DelegationOverview records with one-way terminal states (FR-019)

**Checkpoint**: User Stories 1–4 all independently functional

---

## Phase 7: User Story 5 - Resource Budgets (Priority: P2)

**Goal**: Optional turns/tokens/wall-clock budgets per child; breach stops after ≤1 more action with report; cumulative usage visible in status

**Independent Test**: delegate a background child with max_turns=2 and verify it stops with BudgetExceeded report; submit budgets with 0 values and verify tool-layer rejection

### Tests for User Story 5

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [x] T019 [P] [US5] Write fail-first tests in crates/joey-orchestration/tests/budgets.rs: budget breach stops child after ≤1 more action with report (SC-004), invalid budgets (≤0) rejected at tool layer (FR-011), cumulative usage visible in status (FR-012)

### Implementation for User Story 5

- [x] T020 [US5] Implement parent-side budget watcher in crates/joey-orchestration/src/background.rs over existing tap events: on breach call stop_child(BudgetExceeded); track RunningUsage in ChildHandle
- [x] T021 [US5] Add budgets object parameter (max_turns, max_tokens, max_wall_clock_secs; batch form: top-level applies to all children) with >0 validation to delegate_task in crates/joey-orchestration/src/delegation_tool.rs

**Checkpoint**: User Stories 1–5 all independently functional

---

## Phase 8: User Story 6 - Operator Subagent Control (Priority: P2)

**Goal**: TUI focused-pane keybindings x=stop / s=steer per subagent, routed to the manager with stop reason OperatorRequested

**Independent Test**: focus a subagent pane in the TUI, press x then s, verify stop (OperatorRequested) and steer reach the targeted child only

### Tests for User Story 6

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [x] T022 [P] [US6] Write inline tests in crates/joey-tui/src/app.rs for focused-pane keybindings x=stop and s=steer mapping to TuiAction::StopSubagent/SteerSubagent (FR-017); assert background children appear as live subagent panes in the operator interface, exactly as blocking children do today (FR-014)

### Implementation for User Story 6

- [x] T023 [US6] Implement focused-pane keybindings x/s producing TuiAction::StopSubagent/SteerSubagent in crates/joey-tui/src/app.rs
- [x] T024 [US6] Route TuiAction::StopSubagent/SteerSubagent through crates/joey-tui/src/tui.rs to EngineCommand and crates/joey-cli/src/engine.rs into manager stop_child(OperatorRequested)/steer_child honoring ordering (FR-017)

**Checkpoint**: All user stories (US1–US6) independently functional

---

## Phase 9: Polish & Cross-Cutting Concerns

**Purpose**: Improvements that affect multiple user stories

- [x] T025 Implement session wind-down: SubagentManager::shutdown(timeout) bounded by delegation.wind_down_timeout_secs in crates/joey-orchestration/src/manager.rs, hooked into crates/joey-cli/src/repl.rs end_session and TUI exit, children stopped with SessionEnd reason; add wind-down tests (FR-015, quickstart #8)
- [x] T026 [P] Document the feature in docs/orchestration.md (new: async delegation overview, subagent_control usage) and update docs/state-and-config.md with the two new delegation.* config keys
- [x] T027 [P] Add async-delegation parity/status entry to PORTING.md per repo convention (living audit document)
- [x] T028 Final validation: run cargo build --workspace and cargo test --workspace (must be fully green) and walk through all quickstart.md scenarios in specs/020-async-delegation-control/quickstart.md as smoke validation

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies - can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion - BLOCKS all user stories (CRITICAL checkpoint: workspace builds green; registry + types + events + config in place)
- **User Stories (Phases 3–8)**: All depend on Foundational phase completion
  - User stories can then proceed in parallel (if staffed)
  - Or sequentially in priority order (P1 → P2)
- **Polish (Phase 9)**: Depends on all desired user stories being complete

### User Story Dependencies

- **User Story 1 (P1, Background Delegation)**: Can start after Foundational (Phase 2) - no dependencies on other stories
- **User Story 2 (P1, Completion Notifications)**: Depends on US1 (completion watcher observes background tasks)
- **User Story 3 (P1, Steer & Stop)**: Can start after Foundational - US1 and US3 are mutually independent after Phase 2 (can proceed in parallel)
- **User Story 4 (P2, Progress Inspection)**: Depends on US3 (list/status/log/wait extend the subagent_control tool)
- **User Story 5 (P2, Resource Budgets)**: Depends on US1 (budget watcher hooks background dispatch)
- **User Story 6 (P2, Operator Subagent Control)**: Depends on US3 (stop/steer targets must exist)

### Within Each User Story

- Tests MUST be written first and FAIL before implementation (T007 blocking-parity regression must PASS before implementation - it pins pre-feature behavior)
- Types/manager work before tools (Phase 2 before any story)
- Core implementation before integration (e.g., T009 before T012; T015 before T024)
- Same-file tasks are sequential within and across stories: manager.rs T004→T005→T025; delegation_tool.rs T010→T021; control_tool.rs T015→T018; background.rs T009→T012→T020; tests/control_tool.rs T014→T017
- Story complete before moving to next priority

### Parallel Opportunities

- Phase 2 tasks marked [P] can run in parallel (T002, T003, T006 touch different files)
- After the Phase 2 checkpoint, US1 and US3 streams proceed in parallel
- Test tasks across stories can run in parallel (different files): T007 (tests/background.rs) vs T011 (tests/notices.rs) vs T014 (tests/control_tool.rs) vs T019 (tests/budgets.rs) vs T022 (joey-tui inline)
- T016 name-list sweep is parallel to US4/US5 test-writing
- Docs tasks T026/T027 run in parallel during Polish

---

## Parallel Example: User Story 1

```bash
# After the Phase 2 checkpoint, launch cross-story test tasks in parallel (different files):
Task: "T007 [US1] Blocking-parity regression tests in crates/joey-orchestration/tests/background.rs"
Task: "T014 [US3] Fail-first steer/stop tests in crates/joey-orchestration/tests/control_tool.rs"
Task: "T019 [US5] Fail-first budget tests in crates/joey-orchestration/tests/budgets.rs"

# Then launch US1 implementation together (different files):
Task: "T009 [US1] Create background dispatch + JoinSet watcher in crates/joey-orchestration/src/background.rs"
Task: "T010 [US1] Add background param to delegate_task in crates/joey-orchestration/src/delegation_tool.rs"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (green baseline)
2. Complete Phase 2: Foundational (CRITICAL - blocks all stories)
3. Complete Phase 3: User Story 1
4. **STOP and VALIDATE**: blocking-parity regression green (T007) + background handle <2s (T008)
5. Deploy/demo if ready

### Incremental Delivery

1. Complete Setup + Foundational → Foundation ready
2. Add User Story 1 → Test independently → Deploy/Demo (MVP!)
3. Add User Story 2 → Test independently → Deploy/Demo
4. Add User Story 3 → Test independently → Deploy/Demo
5. Add User Stories 4/5/6 → Test each independently → Deploy/Demo
6. Each phase leaves the workspace green (constitution V: cargo build --workspace && cargo test --workspace)

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational together (T004→T005 sequential on manager.rs)
2. Once Foundational is done:
   - Stream A: US1 → US2 → US5 (background.rs pipeline: T007–T010, T011–T013, T019–T021)
   - Stream B: US3 → US4 → US6 (control surfaces: T014–T016, T017–T018, T022–T024)
3. Polish last (T025–T028), respecting same-file rule (manager.rs T025 after Stream A/B merge)

---

## Notes

- [P] tasks = different files, no dependencies on incomplete tasks
- [Story] label maps task to specific user story for traceability
- Every task is traceable to FR IDs (FR-001–FR-019) and SC IDs where applicable
- Verify tests fail before implementing (exception: T007 parity regression must pass first)
- Commit after each task or logical story group
- Stop at any checkpoint to validate story independently
- Constitution VII regression coverage included (T007 blocking-parity is NON-NEGOTIABLE)
- All state is in-memory/session-lifetime: NO SQLite or on-disk format changes
- Avoid: vague tasks, same file conflicts, cross-story dependencies that break independence

## Phase 10: Convergence

- [ ] T029 Fix delegation event tap wiring so the TUI global tap receives subagent events per FR-014 / US4/AC1 (partial) — `SubagentControl::new` (crates/joey-orchestration/src/control_tool.rs:76-89) captures `forward = manager.event_tap()` as `None` before the TUI installs the global tap (crates/joey-cli/src/tui.rs:239-241), then installs its recorder as the manager-local tap; `SubagentManager::event_tap` (crates/joey-orchestration/src/manager.rs:479-486) prefers local over global, so SubagentSpawn/SubagentEvent/SubagentComplete are recorded then dropped and background panes never appear. Resolve the shadowing (e.g. resolve/chain the forward target at drain time or let the global tap win) and verify panes render live child activity.
- [ ] T030 Consume `AgentEvent::SubagentStopped` in the TUI so stopped children show their final state per FR-010 / FR-016 / contracts/config-and-events.md (missing) — the event is emitted (crates/joey-orchestration/src/manager.rs:1028-1039) and wakes the engine (crates/joey-cli/src/tui.rs:928-937) but `App::apply` drops it via `_ => {}` (crates/joey-tui/src/state.rs:2262), leaving panes/job-board entries stuck on Running; render reason + summary_preview from the `SubagentStopped{id,goal,reason,summary_preview}` contract.
- [ ] T031 Add an end-to-end regression test for the tap wiring per Constitution IV (test-first) and VII (regression coverage) (missing) — existing pane tests inject events directly into `App` state and orchestration tests call `set_event_tap` only after `SubagentControl::new` (crates/joey-orchestration/src/control_tool.rs:400), which is why the T029 ordering bug escaped; add a test that installs taps in real TUI startup order (build_agent_parts -> SubagentControl registered -> set_global_tap) and asserts the global tap receives spawn/event/complete/stopped events.

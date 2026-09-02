---
description: "Task list for feature implementation"
---

# Tasks: HyperCode Agent Teams

**Input**: Design documents from `/specs/022-you-fully-implement/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/team-tools.md, quickstart.md

**Tests**: Included. The constitution (Principle VII) mandates regression coverage for any feature touching public surfaces — this feature adds config keys, tool parameters, a toolset, and a report field — so test tasks are constitutional requirements, not optional.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies on incomplete tasks)
- **[Story]**: Which user story this task belongs to (US1–US4 per spec.md)
- Include exact file paths in descriptions

## Path Conventions

Cargo workspace layout (see plan.md Project Structure): implementation lands in `crates/joey-orchestration/src/team.rs` (new module), `crates/joey-orchestration/src/delegation_tool.rs`, `crates/joey-orchestration/src/background.rs`, `crates/joey-orchestration/src/lib.rs`, `crates/joey-tools/src/toolsets.rs`, `crates/joey-core/src/config.rs`, `crates/joey-cli/src/hypercode.rs`; integration tests in `crates/joey-orchestration/tests/` and `crates/joey-cli/src/tests/`.

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Configuration foundation consumed by every later phase

- [X] T001 Add `hypercode.team.*` defaults block (enabled: false, lead_model: "", max_members: 8, max_parallel_members: 4, message_limit: 10, poll_interval_ms: 500, cleanup_days: 7) to DEFAULT_CONFIG_YAML in crates/joey-core/src/config.rs and extend the existing config-defaults test (~line 1550) to assert the team block

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Team state, tools, and delegation wiring in joey-orchestration — MUST be complete before any user story

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [X] T002 Create crates/joey-orchestration/src/team.rs: entities (Team, TeamMember{name,role,model,status}, TeamTask{id,title,status,claimed_by,dependencies}, TeamMessage{from,to,content,timestamp}), a process-wide TeamRegistry (Mutex<HashMap<team-name, TeamRecord>>), and synchronous JSON persistence to `<joey-home>/teams/<team>/<config.json|tasks.json|inboxes/<member>.json>` honoring the JOEY_HOME override, with inline unit tests for file round-trip and member-name uniqueness
- [X] T003 Implement TeamTaskList operations in crates/joey-orchestration/src/team.rs: add (returns `task_{uuid}`), claim (only when status=Pending AND all dependencies Done; exactly one winner under concurrency), complete(success), release (Running → Pending), list — with inline unit tests for dependency blocking, claim race single-winner, and release semantics
- [X] T004 Implement TeamMailbox in crates/joey-orchestration/src/team.rs: send (validates recipient is a current member; drop-oldest at message_limit=10), receive (drains recipient inbox), poll (non-destructive) — with inline unit tests for delivery, cap/drop-oldest, and unknown-recipient error
- [X] T005 Implement the three team tools in crates/joey-orchestration/src/team.rs — team_status (members+tasks snapshot), team_message (to, content), team_tasks (action add|list|claim|complete|release) — plus TEAM_LEAD_DIRECTIVE and TEAMMATE_DIRECTIVE constants per contracts/team-tools.md; TEAM_LEAD_DIRECTIVE instructs the lead to keep at most max_parallel_members teammates running concurrently (advisory cap; max_members is the hard spawn cap), with inline unit tests for each action and error strings
- [X] T006 [P] Register the team tools: add them in register_orchestration_inner in crates/joey-orchestration/src/lib.rs (threading a team-enabled flag the way register_orchestration_with_resolver_and_allocator threads its resolver, per research.md D4) and define the `team` toolset in crates/joey-tools/src/toolsets.rs (~lines 194-198, beside `delegation`) so orchestrator-capable children receive it
- [X] T007 [P] Add optional `team` and `name` parameters to delegate_task in crates/joey-orchestration/src/delegation_tool.rs: lazy team creation on first reference, member registration under the chosen name, error `team mode is disabled` when the enabled flag is off, name-uniqueness enforcement, hard-cap enforcement of max_members, and refusal of spawns for a new team name while another team is active in the session (error names the one-team-per-session constraint and suggests subagent mode, FR-010) — with inline unit tests for gating, registration, the members cap, and the second-team refusal
- [X] T008 [P] Extend completion notices in crates/joey-orchestration/src/background.rs (format_completion_notice) to identify the team member name for team children, with an inline unit test

**Checkpoint**: Foundation ready — team state, tools, and delegation wiring exist behind the disabled-by-default flag; user story implementation can begin

---

## Phase 3: User Story 1 - Orchestrator Chooses the Optimum Execution Mode (Priority: P1) 🎯 MVP

**Goal**: Per-task routing between subagent mode and team mode with stated rationale, recorded in the run report; byte-identical behavior while disabled

**Independent Test**: quickstart.md Scenario 1 (disabled parity) and Scenario 2 (routing decision) — observable from the report's mode statement with no team directory created when disabled

### Implementation for User Story 1

- [X] T009 [US1] Parse `hypercode.team.*` keys in crates/joey-cli/src/hypercode.rs into a TeamConfig struct (enabled, lead_model, max_members, max_parallel_members, message_limit, poll_interval_ms, cleanup_days), mirroring the existing from_config pattern (~lines 148-179)
- [X] T010 [US1] Add the documented mode-selection guidance to orchestrator_overlay in crates/joey-cli/src/hypercode.rs: teams for independent, parallelizable work; subagents/single session for sequential, same-file, or interdependent steps; prefer the cheaper mode when ambiguous; state chosen mode + 1-2 sentence rationale; when disabled, route to subagents and inform the user; the guidance MUST additionally cover mixed-mode routing while a team is active (FR-018 — unrelated tasks continue via subagents in parallel) and confirmation-free team starts (FR-009)
- [X] T011 [US1] Add `mode_decisions: Vec<String>` to HypercodeReport in crates/joey-cli/src/hypercode.rs (~lines 392-403) and record `mode=<subagent|team> task=<summary> rationale=<text>` entries during runs (FR-016)
- [X] T012 [P] [US1] Create regression tests in crates/joey-cli/src/tests/hypercode_team.rs: TeamConfig parsing defaults and overrides, mode_decisions formatting, and SC-005 disabled-parity (delegation path unchanged when hypercode.team.enabled=false)

**Checkpoint**: MVP — routing decisions are made, stated, and recorded; existing behavior untouched while disabled

---

## Phase 4: User Story 2 - Lead Coordinates a Team with a Shared Task List and Direct Messaging (Priority: P2)

**Goal**: A lead with a configurable model decomposes the objective, teammates claim dependency-safe tasks and message each other directly

**Independent Test**: quickstart.md Scenario 3 — tasks.json shows Pending → Running → Done exactly once with dependency ordering; inboxes show lead-to-teammate AND teammate-to-teammate messages; synthesized final answer

### Implementation for User Story 2

- [X] T013 [US2] Implement lead dispatch in crates/joey-cli/src/hypercode.rs: spawn the lead as an Orchestrator-role child via the existing manager path, applying hypercode.team.lead_model through the model-resolution chain (TaskSpec.model > request > delegation.default_model > parent) with empty = inherit (FR-019), and inject TEAM_LEAD_DIRECTIVE (decompose → team_tasks add with dependencies → assign/self-claim → synthesize); append the `team` toolset to the lead's effective toolsets (ORCHESTRATOR_TOOLSET does not include it) so the lead can call team_status/team_message/team_tasks
- [X] T014 [P] [US2] Wire teammate spawning in crates/joey-orchestration/src/delegation_tool.rs: apply role profiles (explorer/implementor toolsets per FR-017, with the `team` toolset appended — role toolsets alone lack team_status/team_message/team_tasks) and TEAMMATE_DIRECTIVE (claim next unassigned unblocked task; poll mailbox via team_message/team_status; notify lead when idle) for children spawned with team+name
- [X] T015 [P] [US2] Create integration tests in crates/joey-orchestration/tests/team_tools.rs: registry lifecycle + on-disk round-trip, concurrent claim single-winner, dependency blocking, teammate-to-teammate message delivered without lead relay (FR-002), mailbox drop-oldest at cap, and a toolset-resolution test asserting team-spawned children (lead and teammates) resolve the three team tools while teammates still lack delegate_task

**Checkpoint**: A team can execute a parallelizable objective end-to-end with correct collaboration semantics

---

## Phase 5: User Story 3 - Teams Start, Run, and Shut Down as Documented (Priority: P3)

**Goal**: Natural-language start without confirmation, graceful wind-down, automatic session-exit cleanup, task-list persistence for resumption

**Independent Test**: quickstart.md Scenario 4 — session exit leaves zero active children, removes config.json and inboxes/, retains tasks.json readable by a new session

### Implementation for User Story 3

- [X] T016 [US3] Implement wind-down/close in crates/joey-orchestration/src/team.rs: TeamRegistry close semantics — stop members via manager stop_child (StopReason::OrchestratorRequested), release Running tasks to Pending, delete config.json and inboxes/, retain tasks.json, state transition Active → WindingDown → Closed — plus lead-side failure and deadlock handling: on teammate failure the affected task returns to Pending and the lead re-plans (reassign or split it); when every incomplete task is blocked the lead detects the deadlock, informs the user, and re-plans dependencies or winds down — with inline unit tests for close semantics, failure re-plan, and deadlock detection
- [X] T017 [US3] Implement session-end cleanup and retention in crates/joey-orchestration/src/team.rs + the joey-cli session shutdown path: automatic cleanup of active teams at session exit, startup purge of `<joey-home>/teams/*` older than cleanup_days (default 7), and a resumption read API for persisted tasks.json (FR-012) — with inline unit tests using a temp JOEY_HOME

**Checkpoint**: Full lifecycle — start, run, shut down, resume — works as documented

---

## Phase 6: User Story 4 - Users Stay Informed and in Control of Team Activity (Priority: P4)

**Goal**: Team status visibility, teammate finish/idle notifications to the user, stopping one teammate or the whole team early

**Independent Test**: quickstart.md Scenario 5 — team_status shows members and task statuses; stopping one teammate returns its Running task to Pending and it stays claimable; stopping the team halts everyone

### Implementation for User Story 4

- [X] T018 [US4] Link the stop path to task release in crates/joey-orchestration/src/control_tool.rs + team.rs: when a team child is stopped (subagent_control stop), its claimed Running tasks return to Pending and become claimable (FR-015) — with inline unit tests
- [X] T019 [US4] Implement whole-team stop and user-facing notifications in crates/joey-orchestration/src/team.rs + background.rs: stop-all-members request path, and teammate finish/fail/idle notices surfaced to the user session through the existing completion-notice queue (FR-014, SC-003) — with inline unit tests

**Checkpoint**: All four stories independently functional

---

## Phase 7: Polish & Cross-Cutting Concerns

**Purpose**: Documentation, parity audit, end-to-end validation, final gate

- [X] T020 [P] Update docs: team mode section in docs/orchestration.md, `team` toolset rows in docs/tools.md (~line 135 area), HyperCode team bullet in docs/features.md (~line 63 area), `hypercode.team.*` keys + `~/.joey/teams/` layout in docs/state-and-config.md
- [X] T021 [P] Add the agent-teams parity section to PORTING.md: reference behavior (Claude Code v2.1.178), decisions D1–D7 from specs/022-you-fully-implement/research.md, and deliberate deviations (no tmux panes, no TeamCreate/TeamDelete, single-process claiming)
- [X] T022 Run the quickstart.md validation scenarios 1-5 against a sandbox home (export JOEY_HOME=$(mktemp -d), enable hypercode.team.enabled for scenarios 2-5) and record actual outcomes; score mode selection against SC-001 using its fixed 10-task evaluation set (5 team-suited, 5 subagent-suited) and record the score
- [X] T023 Final gate: run `cargo build --workspace` and `cargo test --workspace` from repo root; both MUST be green (constitution acceptance bar)

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — start immediately
- **Foundational (Phase 2)**: Depends on T001; T002 → T003 → T004 → T005 sequential (same file team.rs); T006, T007, T008 parallel to each other after T005 — BLOCKS all user stories
- **User Stories (Phases 3-6)**: All depend on Phase 2 completion; proceed in parallel if staffed, else sequentially P1 → P2 → P3 → P4
- **Polish (Phase 7)**: T020/T021 parallel after Phase 3; T022 after all stories; T023 last

### User Story Dependencies

- **US1 (P1)**: After Phase 2 — no story dependencies (works with team mode disabled)
- **US2 (P2)**: After Phase 2 — uses T006/T007 wiring; independently testable via integration tests
- **US3 (P3)**: After Phase 2 — builds on team.rs state from T002-T005; no US1/US2 dependency
- **US4 (P4)**: After Phase 2 — builds on stop/notice paths (T008) and task release (T003)

### Within Each User Story

- State/tasks primitives before dispatch wiring; dispatch wiring before integration tests
- Core implementation before integration tasks; story complete before next priority

### Parallel Opportunities

- T006 ∥ T007 ∥ T008 (lib.rs+toolsets.rs ∥ delegation_tool.rs ∥ background.rs)
- T012 ∥ T013-phase work (test file vs hypercode.rs) — more precisely T012 ∥ T014 ∥ T015 after T011
- T020 ∥ T021 (different docs files)
- Different user stories can be worked in parallel by different implementors once Phase 2 lands

---

## Parallel Example: User Story 2

```bash
# After Phase 2 and T011, launch together (different files, no mutual deps):
Task: "Wire teammate spawning in crates/joey-orchestration/src/delegation_tool.rs"      # T014
Task: "Create integration tests in crates/joey-orchestration/tests/team_tools.rs"      # T015
Task: "Create regression tests in crates/joey-cli/src/tests/hypercode_team.rs"         # T012 (US1 tail)
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)
1. Complete Phase 1 (T001) and Phase 2 (T002-T008)
2. Complete Phase 3 (T009-T012)
3. STOP and VALIDATE: quickstart Scenario 1 + 2 — routing stated/recorded, disabled parity holds
4. Ship if only routing value is needed
Note: strictly, US1 depends only on T001 and T009-T012 (it operates with team mode disabled); Phase 2 remains first per the foundational-first convention.

### Incremental Delivery
1. Setup + Foundational → team machinery exists, feature dark
2. + US1 → MVP: routing with rationale (demo)
3. + US2 → teams collaborate on shared task list (demo)
4. + US3 → full lifecycle with persistence (demo)
5. + US4 → visibility and control (demo); then Polish (T020-T023)

### Parallel Team Strategy
1. Implementors complete Phases 1-2 together
2. Then: Implementor A → US1; Implementor B → US2; Implementor C → US3; Implementor D → US4 (disjoint files per story: US1/US3 touch hypercode.rs+team.rs, US2/US4 touch delegation_tool.rs+team.rs+background.rs — sequence same-file tasks across stories)
3. T022-T023 as the shared final gate

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] labels map tasks to spec.md user stories for traceability
- Every task touching a public surface carries its regression test inline (constitution VII)
- Never edit crates/joey-omo/src/team.rs — it stays untouched (research.md D7)
- Commit after each task or logical group; stop at any checkpoint to validate a story independently

## Phase 8: Convergence

- [X] T024 Add a regression test proving the team lead request inherits the orchestrator's effective model when `hypercode.team.lead_model` is unset/empty per FR-019 (partial)
- [X] T025 Add a regression test for the team-member completion path in `crates/joey-orchestration/src/manager.rs` (~L1187-1207): failure releases the member's claimed tasks to Pending and a `[TEAM] ... finished|failed` notice is posted on both outcomes per FR-006, FR-015, SC-003 (partial)
- [X] T026 Add a regression test that a team-mode delegation attempted while another team is already active records the team-start refusal in `mode_decisions` and dispatches the work via subagents per FR-018 (partial)
- [X] T027 Append to the 'Agent teams (feature 022, 2026-09-02)' section of PORTING.md the two undocumented deltas: team regression tests live at `crates/joey-cli/src/tests/hypercode_team.rs` (plan.md named `crates/joey-cli/tests/hypercode_team.rs`), and the plan-named `TeamTaskList`/`TeamMailbox` surfaces are implemented as `TeamRecord` methods in `crates/joey-orchestration/src/team.rs` per plan: team module shape (partial)
- [X] T028 Document the SC-001 scoring procedure (10-task eval set, correct-mode rubric, >=9/10 threshold) in `specs/022-you-fully-implement/quickstart.md` per SC-001 (partial)

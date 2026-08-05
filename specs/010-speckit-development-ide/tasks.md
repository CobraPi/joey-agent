---
description: "Task list for feature 010-speckit-development-ide"
---

# Tasks: Spec-Kit Development IDE

**Input**: Design documents from `/specs/010-speckit-development-ide/`

**Prerequisites**: plan.md (required), spec.md (required for user stories),
research.md, data-model.md, contracts/ (speckit-ui-api.md,
workflow-runner.md, staging-api.md, history-jsonl.md), quickstart.md,
`.specify/memory/constitution.md` v1.1.0.

**Tests**: The spec does not request a separate TDD track, but Constitution
Principle IV ("tests alongside implementation, not deferred") and Principle
VII ("any feature touching a public surface MUST ship with regression
coverage") make tests mandatory for this feature: it introduces a new
versioned on-disk format (JSONL history), extends the public REST/WS API,
and extends shared parsers. Tests are therefore folded into each
implementation task ("with unit tests") and cross-cutting regression tasks
are explicit in Phase 2 and the Polish phase.

**Organization**: Tasks are grouped by user story (spec.md US1–US5) so each
story is independently implementable and testable. This feature **extends**
the existing `joey-speckit-ui` crate + `web/speckit-ui` frontend from
`specs/001` (no new crate); every task is additive (Constitution VII).

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies on incomplete tasks in the same phase)
- **[Story]**: Which user story this task belongs to (US1–US5). Setup/Foundational/Polish tasks carry NO story label.
- All backend paths are under `crates/joey-speckit-ui/`; all frontend paths are under `web/speckit-ui/`.

## Path Conventions

- **Backend**: `crates/joey-speckit-ui/src/<module>.rs` and `crates/joey-speckit-ui/tests/<name>.rs`
- **Frontend**: `web/speckit-ui/src/<area>/<file>.ts`
- This is a web application (existing `backend` = Rust crate, `frontend` = Vite/TS app).

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Add dependencies and extend the data/parsing layer that every
user story builds on. No behavioral change yet — purely additive types and
tolerant parsers (Constitution VII: existing types preserved).

- [X] T001 Add `gix` dependency (pin latest 0.6x) to `crates/joey-speckit-ui/Cargo.toml` `[dependencies]`; record compile-time + binary-size delta vs `specs/001` baseline in `research.md` §3 (Constitution VIII)
- [X] T002 [P] Add frontend deps `diff` (jsdiff) and `split.js` to `web/speckit-ui/package.json` `devDependencies`; record `vite build` bundle-size delta vs baseline in `research.md` §4
- [X] T003 [P] Extend `crates/joey-speckit-ui/src/model.rs` with additive entity types from `data-model.md`: `Artifact`/`ArtifactKind`, `SaveState`, `ValidationFinding`/`ArtifactLocation`, `WorkflowStep`/`StepState`, `RunConfiguration`/`Scope`/`AgentOptions`/`ChangeMode`, `WorkflowAttempt`/`AttemptStatus`, `AgentInteraction`/`InteractionKind`, `ChangeSet`/`ChangedFile`/`Hunk`/`AcceptState`, `DependencyLink`/`DependencyKind`, `WorkspacePreference`, `Checkpoint` — all `#[derive(Serialize,Deserialize)]`, existing types untouched
- [X] T004 [P] Extend `crates/joey-speckit-ui/src/parser/mod.rs` (and sibling files) with tolerant discovery + parsing for `checklist`, `research`, `data_model`, `contract`, `quickstart`, `constitution`, and `supporting` artifact kinds — reuse the `Status::Unparsed`-on-malformed pattern; do not regress `parser/spec.rs`, `parser/plan.rs`, `parser/tasks.rs`

**Checkpoint**: Data layer extended; `cargo build -p joey-speckit-ui` passes; existing `tests/parser_roundtrip.rs` still green.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core infrastructure that MUST be complete before ANY user story
can be implemented — the shared write/validation/history/graph primitives
and the trait boundaries behind which US2 (runner) and US3 (staging) plug
in (Constitution VI: depend only on abstractions).

**⚠️ CRITICAL**: No user story work can begin until this phase is complete.

- [X] T005 Create `crates/joey-speckit-ui/src/editor.rs`: multi-artifact conflict-safe writes composing `writer.rs::replace_line_if_unchanged`; support `whole` and `section` scopes; carry `based_on_hash` → 409 on external change (FR-020/SC-005); preserve unrelated content
- [X] T006 [P] Create `crates/joey-speckit-ui/src/validation.rs`: required-structure + unresolved-marker checks per `ArtifactKind` (spec/plan/tasks/checklist/constitution structure; `NEEDS CLARIFICATION`/`TBD`/`[REMOVE IF UNUSED]` markers) → `ValidationFinding` anchored to `ArtifactLocation` (FR-007); with unit tests
- [X] T007 [P] Create `crates/joey-speckit-ui/src/workflow.rs` core: build the `DependencyLink` graph from artifacts on feature load; implement `derive_step_state(step, artifacts, prerequisites, active_runs) -> StepState` as a pure function (FR-022); cycle detection for task deps (Edge Cases); with unit tests
- [X] T008 Create `crates/joey-speckit-ui/src/history.rs`: append-only JSONL store at `~/.joey/speckit-ui/history/<feature-id>.jsonl`; mandatory `schema_version:1`; O(1) append, streamed lazy read (`serde_json::Deserializer::from_reader`), 90-day expiry sweep via file-mtime (FR-018/031); tolerant skip of partial last line
- [X] T009 [P] Define `trait StagingArea` (open/checkpoint/diff/apply/discard) in `crates/joey-speckit-ui/src/staging.rs` per `contracts/staging-api.md` — the contract boundary US2 mocks and US3 implements; include `StagingError` (ConflictingRun, etc.)
- [X] T010 [P] Define `trait WorkflowRunner` (prepare_and_start/respond/cancel) + `RunnerEvent` envelope in `crates/joey-speckit-ui/src/runner.rs` per `contracts/workflow-runner.md`; exit-code → terminal-status mapping table
- [X] T011 Regression coverage: create `crates/joey-speckit-ui/tests/history_jsonl_roundtrip.rs` — round-trip `WorkflowAttempt` → JSONL line → record (schema_version preserved); assert partial-line tolerance; stub a v1→v2 migration path test (Constitution VII public-format gate)
- [X] T012 [P] Regression coverage: extend `crates/joey-speckit-ui/tests/contract_*.rs` to assert every `specs/001` REST/WS endpoint (`GET /api/features`, `GET /api/features/{id}`, `PATCH .../spec`, `PATCH .../tasks/{taskId}`, `POST .../clarify*`, `POST .../analyze`, `POST .../tasks/{taskId}/execute`, `POST /api/init`, `WS .../watch`, `WS .../session/{id}`, `WS /api/runs/{run_id}`) is preserved unchanged (Constitution VII)

**Checkpoint**: Foundation ready — editor, validation, dependency graph, history store, and the runner/staging trait boundaries exist and are tested. User story implementation can now begin.

---

## Phase 3: User Story 1 — Author Every Spec-Kit Artifact (Priority: P1) 🎯 MVP

**Goal**: A developer can discover, open, edit, and validate every
feature artifact (spec, plan, tasks, checklists, research, data-model,
contracts, quickstart) without leaving the UI, with synchronized dirty/
saving/saved/invalid/externally-changed states (spec US1; FR-003/004/005/
006/007).

**Independent Test**: Open an existing feature with populated artifacts;
select and edit each supported artifact kind; verify changes persist to
disk, validation findings are surfaced and navigable, and unsaved edits
are preserved or prompted on navigation (quickstart.md scenario 1).

### Implementation for User Story 1

- [X] T013 [US1] Backend: `GET /api/features/{id}/artifacts` in `crates/joey-speckit-ui/src/api/rest.rs` — discover artifacts by workflow phase without assuming all exist (FR-003); include `exists`, `content_hash`, `save_state`, `validity`, `stale`
- [X] T014 [US1] Backend: `GET /api/features/{id}/artifacts/{path}` in `crates/joey-speckit-ui/src/api/rest.rs` — raw text + rendered outline (sections with line ranges via `pulldown-cmark`) + validity (FR-006); 404 for non-existent
- [X] T015 [US1] Backend: `PATCH /api/features/{id}/artifacts/{path}` in `crates/joey-speckit-ui/src/api/rest.rs` — route to `editor.rs`; whole/section scope; 200 with new hash, 409 on external change, 422 `invalid_request` with `ValidationFinding[]` on structural failure (FR-004/005/020)
- [X] T016 [P] [US1] Frontend: `web/speckit-ui/src/views/explorer.ts` — feature/artifact navigator grouped by workflow phase; lists `exists:false` artifacts so the user can create them (FR-003); consumes `GET .../artifacts`
- [X] T017 [P] [US1] Frontend: `web/speckit-ui/src/views/editor.ts` — source + rendered reading views with outline navigation between document headings and referenced locations (FR-006); save-state transitions `dirty→saving→saved→invalid→externally_changed→read_only` (FR-005)
- [X] T018 [US1] Frontend: dirty/discard prompts on switch feature/view and external-change reload/compare choices in `web/speckit-ui/src/views/editor.ts` (FR-005 acceptance 3, FR-020 acceptance 3)

**Checkpoint**: User Story 1 is fully functional and independently testable — every artifact kind is authorable and validation findings are navigable. This is the suggested MVP.

---

## Phase 4: User Story 2 — Control the Complete Workflow with Joey Agent (Priority: P1)

**Goal**: A developer sees the full Spec-Kit lifecycle as controllable
steps, can inspect/modify each step's instructions/scope/options, run it
through the **native Joey Agent out-of-process**, answer questions and
approvals, monitor progress, cancel safely, and review the result (spec
US2; FR-008/009/010/011/012/013/014/015/024/034).

**Independent Test**: Starting from a draft spec, run each applicable
workflow step through the IDE including an interactive (question/
approval) step; verify the agent spawns in feature context, streams
progress, writes expected artifacts, leaves an auditable attempt, and
cancels safely (quickstart.md scenarios 2 & 3).

### Implementation for User Story 2

- [X] T019 [US2] Backend: `GET /api/features/{id}/workflow` in `crates/joey-speckit-ui/src/api/rest.rs` — step catalog in lifecycle order with derived `StepState`, `available`, `blocking_reason`, inputs/outputs, prerequisites, `installed_definition_ref` (FR-008/009/022); route reads `workflow.rs`
- [X] T020 [P] [US2] Backend: `GET /api/options` in `crates/joey-speckit-ui/src/api/rest.rs` — server-advertised model/reasoning/max-iterations catalog with content-hash `revision` (FR-010); backend validates selected targets/options against active workflow + provider catalog + safety bounds
- [X] T021 [US2] Backend: run-config + override endpoints in `crates/joey-speckit-ui/src/api/rest.rs` — `GET .../workflow/{step}/config` (effective merged instructions), `PUT .../workflow/{step}/override`, `DELETE .../workflow/{step}/override` (FR-034); installed definitions read-only; overrides stored under `~/.joey/speckit-ui/overrides/`
- [X] T022 [US2] Backend: implement `WorkflowRunner` for the out-of-process Joey Agent in `crates/joey-speckit-ui/src/runner.rs` — spawn `joey <skill>` (or `.specify/scripts/bash/<step>.sh` fallback) via `tokio::process::Command` in the staging root; set feature context (`.specify/feature.json`/`SPECIFY_FEATURE`); stream stdout/stderr line-by-line → `RunnerEvent` classification; write `InteractionPayload` to stdin on respond; SIGTERM/kill on cancel (FR-011/012/013/014); with hermetic subprocess-harness unit tests
- [X] T023 [US2] Backend: `POST .../workflow/{step}/run` in `crates/joey-speckit-ui/src/api/rest.rs` — validate `RunConfiguration` (targets, options, mandatory `change_mode`, `option_catalog_rev`); FR-015 conflict guard (overlapping in-flight attempt scope → 409 `conflicting_run`); 409 `stale_option_catalog`; freeze config on prepare; append attempt to JSONL history
- [X] T024 [US2] Backend: interaction endpoints + WS stream in `crates/joey-speckit-ui/src/api/{rest,ws}.rs` — `POST .../attempts/{id}/answer`, `POST .../attempts/{id}/approve`, `POST .../attempts/{id}/cancel` (FR-013/014); `WS /api/attempts/{id}/stream` reusing `AppState::channel_for` broadcast; confirm interaction advances checkpoint (FR-033)
- [X] T025 [P] [US2] Frontend: `web/speckit-ui/src/views/workflow.ts` — step list with states, run-configuration panel (instructions/scope/options/change_mode), project-override management (save/remove, effective-merged display) (FR-008/009/010/034)
- [X] T026 [P] [US2] Frontend: `web/speckit-ui/src/views/run-panel.ts` — streamed progress/tool/question/approval/output events over WS; answer/approve/cancel controls; terminal status display (FR-012/013/014)
- [X] T027 [US2] Frontend: wire task-board run-one and run-selection controls to `POST .../workflow/{step}/run` with task-scoped `scope.task_ids` in `web/speckit-ui/src/views/workflow.ts` (FR-024); depends on T023 (run endpoint) + T025 (workflow view)

**Checkpoint**: User Stories 1 and 2 both work independently — artifacts are authorable and every workflow step is controllable through the native agent.

---

## Phase 5: User Story 3 — Review, Refine, and Re-run Agent Changes Safely (Priority: P1)

**Goal**: A developer remains in control of agent-authored changes — review
diffs hunk-by-hunk, selectively accept/reject with dependency warnings,
edit and re-run, and recover safely from failed/cancelled/unwanted runs
(spec US3; FR-016/017/019/020/033).

**Independent Test**: Run a workflow that modifies multiple files; retain
one change while reverting another via supported recovery controls; edit
the plan and re-run the step; verify no unrelated work is overwritten and
the earlier attempt remains for comparison (quickstart.md scenarios 4 & 6).

### Implementation for User Story 3

- [X] T028 [US3] Backend: implement Git-backed `StagingArea` in `crates/joey-speckit-ui/src/staging.rs` — `gix` for read/object side (HEAD/tree/index/diff/blobs/refs); `git` CLI subprocess (via `commands.rs` helper) for `worktree add/remove` and `git apply --reject`; staged mode = temp worktree on `joey/staging/<feature>/<attempt>`, direct mode = primary worktree (FR-016); with temp-bare-repo test fixtures in `crates/joey-speckit-ui/tests/staging_git.rs`
- [X] T029 [US3] Backend: `GET .../attempts/{id}/changes` in `crates/joey-speckit-ui/src/api/rest.rs` — change set (files + hunks + `accept_state` + `depends_on` edges + `why` summaries) from `StagingArea::diff` (FR-016)
- [X] T030 [US3] Backend: `POST .../attempts/{id}/changes/apply` in `crates/joey-speckit-ui/src/api/rest.rs` — apply accepted hunks/files via `StagingArea::apply`; emit `depends_on` warnings BEFORE application for unsafe partial selections (SC-016); 409 if external change since load
- [X] T031 [US3] Backend: recovery + restart resume in `crates/joey-speckit-ui/src/recovery.rs` + `crates/joey-speckit-ui/src/api/rest.rs` — `POST .../attempts/{id}/recover` (FR-017); on backend startup, scan in-progress attempts, resume from latest valid checkpoint WITHOUT replaying unconfirmed actions or mark `recovery_failed` with preserved-effects summary (FR-033); warn when recovery affects unrelated user changes; with tests in `crates/joey-speckit-ui/tests/recovery_resume.rs`
- [X] T032 [US3] Backend: re-run linking in `crates/joey-speckit-ui/src/api/rest.rs` + `history.rs` — `POST .../workflow/{step}/run` with `prior_attempt_id` creates a distinct attempt linked to the prior for comparison (FR-019); prior attempt retained
- [X] T033 [P] [US3] Frontend: `web/speckit-ui/src/views/review.ts` + `web/speckit-ui/src/components/diff-view.ts` — additions/removals per file, individual hunk/file accept-reject selection, dependency markers, "why" summaries, apply/recover controls (FR-016/017)
- [X] T034 [P] [US3] Frontend: recovery controls + dependency-warning confirmation flow + re-run-from-edit entry point in `web/speckit-ui/src/views/review.ts` (FR-017/019)
- [X] T035 [US3] Backend: post-run scope verification in `crates/joey-speckit-ui/src/staging.rs` — after run completion, `git diff --name-only` check warns if the change set exceeds declared scope targets (Edge Cases: run changes files outside feature directory); surface out-of-scope files in the change review (FR-016)

**Checkpoint**: All three P1 stories are independently functional — author, run, and review/recover work end-to-end.

---

## Phase 6: User Story 4 — Navigate Work as an Integrated Development Project (Priority: P2)

**Goal**: A developer moves fluidly between workflow, documents, tasks,
execution output, and repository changes in a coherent, desktop-class,
keyboard-accessible workspace that remembers the working layout (spec US4;
FR-002/023/025/026/027/028).

**Independent Test**: Work through a multi-document feature using only
keyboard and pointer navigation; resize/collapse/reorder panes; filter
tasks; move between a running workflow and its changed files; reload the
page; verify the working context is restored (quickstart.md scenario 7).

### Implementation for User Story 4

- [X] T036 [P] [US4] Frontend: `web/speckit-ui/src/components/pane-layout.ts` — resizable/collapsible/reorderable workspace panes via `split.js` (FR-002); unified shell in `web/speckit-ui/src/app.ts` composing explorer + editor + workflow + run panel
- [X] T037 [P] [US4] Backend: `GET`/`PUT /api/features/{id}/preferences` in `crates/joey-speckit-ui/src/api/rest.rs` — `WorkspacePreference` (last feature, open artifacts, active view, pane layout, filters); reject embedded artifact content with 422 (Constitution III); stored at `~/.joey/speckit-ui/preferences.json` (FR-026)
- [X] T038 [P] [US4] Frontend: `web/speckit-ui/src/views/search.ts` — search/filter across feature artifacts, requirement ids, task ids, workflow states, and run history (FR-025); virtualized lists for scale (FR-031)
- [X] T039 [US4] Frontend: `web/speckit-ui/src/a11y/` — keyboard navigation + visible focus + descriptive ARIA labels across explorer/editor/workflow/run-panel/review; `role="alert"` for blocking prompts; verify all primary journeys keyboard-reachable (FR-027/SC-011)
- [X] T040 [US4] Frontend: implement reference-navigation in `web/speckit-ui/src/views/` — activate a validation finding, task, requirement reference, graph node, or run event to open the target artifact at the relevant `line_or_section` (FR-023); depends on T014 (artifact GET) + T044 (traceability endpoint)

**Checkpoint**: The workspace is coherent, state survives reload, and core flows are keyboard accessible.

---

## Phase 7: User Story 5 — Understand Readiness and Delivery Progress (Priority: P2)

**Goal**: A developer or reviewer can determine what is complete, stale,
blocked, and next across the feature, derived from actual artifacts and
workflow history, with traceability from requirements through convergence
findings (spec US5; FR-021/022/023/032).

**Independent Test**: Complete several workflow steps; modify an upstream
requirement; verify dependent plan/analysis/task outputs are marked stale
within 3 s with a clear next action; trace a requirement → plan → task →
attempt → finding (quickstart.md scenario 5).

### Implementation for User Story 5

- [X] T041 [US5] Backend: stale propagation trigger in `crates/joey-speckit-ui/src/workflow.rs` + `api/ws.rs` — on `watcher.rs` upstream-artifact change event, walk the `DependencyLink` graph downstream and mark affected `Artifact.stale` + `WorkflowStep.state=stale` WITHOUT deleting content (FR-021/SC-007 < 3 s); push event over existing `WS .../watch`
- [X] T042 [US5] Backend: `GET .../features/{id}/history` (streamed/paginated, newest first) + traceability summary endpoint in `crates/joey-speckit-ui/src/api/rest.rs` — requirement → plan section → task → attempt → finding trace (FR-019/023/032); lazy JSONL decode for scale (SC-010)
- [X] T043 [P] [US5] Frontend: `web/speckit-ui/src/views/readiness.ts` — lifecycle/readiness summary, stale propagation display with next-step explanations, end-to-end progress trace (FR-021/022/032)
- [X] T044 [P] [US5] Frontend: `web/speckit-ui/src/components/status-badges.ts` — `ready|blocked|running|attention_needed|succeeded|failed|stale|unavailable` badges with descriptive `aria-label` from derived state text; `attention_needed` exposes underlying state + remediation (spec US2 note)

**Checkpoint**: All user stories are independently functional; readiness is trustworthy and traceable.

---

## Phase 8: Polish & Cross-Cutting Concerns

**Purpose**: Connectivity states, scale/restart validation, documentation,
and the workspace-wide acceptance bar.

- [X] T045 [P] Backend: `GET /api/health` in `crates/joey-speckit-ui/src/api/rest.rs` — advertises backend reachable, `joey` agent binary discovered, credentials present, repo writable (FR-028); frontend maps absence to `disconnected|unavailable_agent|missing_credential|read_only|failed_write` with recovery actions; never presents unknown as success
- [X] T046 [P] Scale validation: add perf tests asserting FR-031 ceilings (≥500 tasks, ≥100 attempts, ≥1000 changed files) stay interactive — open-artifact/filter-tasks/inspect-run < 2 s for ≥95 % of interactions (SC-010); virtualized frontend lists + streamed JSONL backend reads
- [X] T047 [P] Restart-recovery integration tests: assert every active attempt with a valid checkpoint resumes without repeating confirmed actions, and every attempt without one stops with a truthful recovery status + preserved effects (FR-033/SC-015); 90-day expiry sweep verified (SC-014)
- [X] T048 [P] Update `PORTING.md` with the upstream-parity status of this feature (Complete/Partial/Deliberate-deviation per subsystem, dated) — `joey-speckit-ui` is a Joey-original crate, note the deliberate out-of-process divergence from any upstream in-process UI
- [X] T049 [P] Run `specs/010-speckit-development-ide/quickstart.md` validation scenarios 1–7 end-to-end (manual + Playwright `web/speckit-ui/tests/`) and confirm every spec acceptance criterion it maps to passes
- [X] T050 [P] Final acceptance: `cargo build --workspace && cargo test --workspace` green (constitution mandate); `cd web/speckit-ui && npm run build && npm run test:e2e` green

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately.
- **Foundational (Phase 2)**: Depends on Phase 1 (model/parser types) — BLOCKS all user stories.
- **US1 (Phase 3)**: Depends on Foundational (editor, validation). No dependency on other stories. **Suggested MVP.**
- **US2 (Phase 4)**: Depends on Foundational (workflow graph, history, runner trait) + US1 (edit step inputs = edit artifacts). Reads readiness from Foundational.
- **US3 (Phase 5)**: Depends on Foundational (`StagingArea` trait, history) + US2 (attempts exist to review/recover). Implements the concrete Git-backed staging US2's runner runs inside.
- **US4 (Phase 6)**: Depends on US1–US3 surfaces existing (composes their views). Mostly frontend.
- **US5 (Phase 7)**: Depends on Foundational graph + US2/US3 history (attempts to trace). Adds stale propagation + traceability surfacing.
- **Polish (Phase 8)**: Depends on all user stories being complete.

### User Story Dependencies

- **US1 (P1)**: Can start after Foundational — no dependencies on other stories. Deliver first as MVP.
- **US2 (P1)**: Can start after Foundational; integrates with US1's editor for modifying step inputs.
- **US3 (P1)**: Can start after Foundational; consumes US2's attempts. The concrete `StagingArea` (T028) is what US2's runner (T022) ultimately runs inside — US2 is testable with a mock staging until T028 lands.
- **US4 (P2)**: Can start after US1–US3; independently testable (workspace ergonomics).
- **US5 (P2)**: Can start after Foundational; integrates with US2/US3 history. Independently testable (readiness derivation is in Foundational).

### Within Each User Story

- Backend endpoints/routes before frontend views that consume them.
- Trait/contract before implementation.
- Models/services before endpoints.
- Implementation tasks carry their own unit tests (Constitution IV).
- Regression tasks (T011, T012) are in Foundational so every later phase inherits a green public-surface baseline.

### Parallel Opportunities

- **Phase 1**: T002, T003, T004 are independent (`[P]`) — frontend deps, backend model, backend parsers touch disjoint files.
- **Phase 2**: T006, T007, T009, T010, T012 are independent (`[P]`) — validation, workflow graph, staging trait, runner trait, API regression touch disjoint files. T008 (history) and T011 (its regression test) are sequential.
- **Per story**: frontend views marked `[P]` (T016/T017, T025/T026/T027, T033/T034, T036/T037/T038, T043/T044) run in parallel with each other once their backend endpoint exists.
- **Cross-story**: US4 and US5 (both P2) can proceed in parallel once their Foundational + P1 prerequisites land.
- **Polish**: T045–T049 are all `[P]` (disjoint concerns) once stories are complete.

---

## Implementation Strategy

**MVP first (Constitution V — incremental, reviewable delivery):**

1. **MVP = US1 (Phase 3)** alone. Once Phase 1 + Phase 2 + Phase 3 land,
   a user can author every artifact with validation and conflict-safe
   saves — a useful, independently shippable increment that builds and
   tests green on its own.
2. **Then US2** — the workflow controls + out-of-process agent run. This
   is the defining enhancement; it depends on US1's editor (modify
   inputs) and Foundational's runner trait + history.
3. **Then US3** — change review + Git-backed staging + recovery. Completes
   the P1 trio (author → run → review/recover).
4. **US4 and US5 (P2)** can then proceed in parallel — integrated
   workspace ergonomics and readiness/traceability surfacing.
5. **Polish** — connectivity states, scale/restart validation, docs,
   workspace-wide acceptance bar.

Each increment MUST leave `cargo build --workspace && cargo test
--workspace` green (constitution mandate) and preserve all `specs/001`
behavior (regression tasks T011/T012 guard this throughout).

---

## Parallel Example: User Story 1

```text
Phase 1 (Setup)         Phase 2 (Foundational)      Phase 3 (US1)
───────────────         ──────────────────────      ─────────────
T001 (gix dep) ───────▶ T005 (editor) ────────────▶ T013 (GET artifacts) ─▶ T016 [P] (explorer)
T002 [P] (fe deps) ──┐  T006 [P] (validation) ────▶ T014 (GET artifact) ─▶ T017 [P] (editor view)
T003 [P] (model) ────┼─▶ T007 [P] (graph)              │                  └▶ T018 (dirty/discard)
T004 [P] (parsers) ──┘  T008 (history)                 T015 (PATCH) ─┐
                        T009 [P] (staging trait)                      │
                        T010 [P] (runner trait)                      ├─▶ US1 checkpoint
                        T011 (jsonl regression) ─────────────────────┘
                        T012 [P] (api regression)
```

Once US1's backend endpoints (T013–T015) exist, its frontend views
(T016–T018) can be built in parallel, with T018 (dirty/discard) depending
on T017 (editor view).

---

## Phase 9: Convergence

**Purpose**: Close the gap between the implemented code and the feature's
specification. The initial implementation pass created all modules,
endpoints, types, and frontend views, but several critical integration
points were left as stubs — the pieces exist but are not wired together
end-to-end. These tasks complete the wiring.

- [X] T051 Wire `JoeyCliRunner` into `POST .../workflow/{step}/run` in `crates/joey-speckit-ui/src/api/rest.rs` — after creating the attempt record, call `runner.prepare_and_start()` with the staging area, spawn a task to forward `RunnerEvent`s from the runner's event channel into the attempt's WS broadcast channel, and update the attempt status from `preparing` → `running` → terminal per FR-011/012 (partial)
- [X] T052 Wire interaction endpoints to the runner handle in `crates/joey-speckit-ui/src/api/rest.rs` — `POST .../attempts/{id}/answer` must call `runner.respond()` with an `InteractionPayload::Answer`; `POST .../attempts/{id}/approve` must call `runner.respond()` with `InteractionPayload::Approval`; `POST .../attempts/{id}/cancel` must call `runner.cancel()` and record a truthful partial-effects status per FR-013/014 (partial)
- [X] T053 Wire `GitStagingArea::diff()` into `GET .../attempts/{id}/changes` and `GitStagingArea::apply()` into `POST .../attempts/{id}/changes/apply` in `crates/joey-speckit-ui/src/api/rest.rs` — replace the hardcoded empty `files: []` stub with a real call to the staging area, resolving the staging root from the attempt's run_config; emit `depends_on` warnings before application per FR-016/SC-016 (partial)
- [X] T054 Wire `recovery.rs` into `POST .../attempts/{id}/recover` and backend startup in `crates/joey-speckit-ui/src/api/rest.rs` + `src/main.rs` — the recover endpoint must call `recovery::evaluate_recovery()` and either resume (re-spawn agent from checkpoint) or mark `recovery_failed`; `main.rs` must call `recovery::scan_all_for_recovery()` on startup and process each recoverable attempt per FR-017/033 (partial)
- [X] T055 Wire stale propagation to watcher events in `crates/joey-speckit-ui/src/api/{rest,ws}.rs` + `src/workflow.rs` — on `watcher.rs` file-change event for an upstream artifact, call `build_dependency_graph()` then `propagate_stale()` to mark affected `Artifact.stale` + `WorkflowStep.state=stale`, and push the stale notification over the existing `WS .../watch` channel per FR-021/SC-007 (missing)
- [X] T056 Add periodic JSONL history expiry sweep to `crates/joey-speckit-ui/src/main.rs` — spawn a `tokio::spawn` task on startup that calls `history::sweep_expired()` once immediately, then on an hourly interval, to remove records older than 90 days per FR-018/SC-014 (missing)
- [X] T057 Wire `WorkspaceApp` from `web/speckit-ui/src/app.ts` into `web/speckit-ui/src/main.ts` — replace the old `Workspace` instantiation with `WorkspaceApp` so the new explorer/editor/workflow/run-panel/review/readiness views are actually rendered; preserve backward compatibility with existing specs/001 canvas/board views per FR-002 (partial)
- [X] T058 Implement FR-015 conflict guard in `crates/joey-speckit-ui/src/api/rest.rs` `post_workflow_run` — before creating a staging area, compute the candidate scope's affected paths and check for overlap with any in-flight attempt's change set; overlap → 409 `conflicting_run` per FR-015 (missing)
- [X] T059 Add scale/perf validation tests in `crates/joey-speckit-ui/tests/scale_validation.rs` — generate fixtures with 500 tasks, 100 JSONL attempt records, and 1000-file change sets; assert open-artifact / filter-tasks / inspect-run stay under 2s for ≥95% of interactions per FR-031/SC-010 (missing)
- [X] T060 Add Playwright e2e tests in `web/speckit-ui/tests/ide-journeys.spec.ts` covering quickstart scenarios 1–7 (author artifact, run workflow step, answer question, review changes, recover, search, keyboard nav) per T049/SC-001 (missing)

---

## Phase 10: Convergence

**Purpose**: Close gaps found by `/speckit-converge` after the Phase 9 wiring
pass. The backend and all frontend view modules are fully implemented and
wired, but the new IDE is not actually reachable in the browser, and no
automated test exercises it.

- [X] T061 [CRITICAL] Wire the new IDE `WorkspaceApp` into `web/speckit-ui/index.html` — add an "IDE" nav button and a `<div id="view-ide" class="view"></div>` container so the `if (ideContainer)` guard in `web/speckit-ui/src/main.ts` succeeds and `WorkspaceApp` mounts, rendering the explorer/editor/workflow/run-panel/review/readiness views built in T016–T044. Today `index.html` only defines the old specs/001 nav (Canvas/Workspace/Board/Init) with no `#view-ide`, so `WorkspaceApp` is never instantiated and the entire P1/P2 frontend is invisible. Preserve the existing canvas/board/workspace views and their nav buttons for backward compatibility per FR-002 / US4-AC1. Verify by loading the page, selecting the IDE view, and confirming the explorer and editor render. (FR-002, US1/AC1, US4/AC1, T057) (partial)
- [X] T062 [HIGH] Move `web/speckit-ui/tests/ide-journeys.spec.ts` into the configured Playwright `testDir` (`web/speckit-ui/tests/e2e/`) and make it assertively drive the new IDE views (explorer/editor/workflow/run-panel/review/readiness/search) covering quickstart scenarios 1–7 (author artifact, run workflow step, answer question, review changes, recover, search, keyboard nav). Today the file lives at `tests/` root, outside `playwright.config.ts`'s `testDir: './tests/e2e'`, so `playwright test` never executes it; all 7 configured e2e specs target only the old canvas/board/workspace views, so no automated test covers any new IDE surface. Remove the `catch(() => false)` skip-guards so failures surface. Confirm `npm run test:e2e` runs and passes the new specs after T061 makes the IDE render. (T060, T049, SC-001, SC-011) (partial)

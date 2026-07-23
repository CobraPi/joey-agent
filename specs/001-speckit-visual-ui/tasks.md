---

description: "Task list template for feature implementation"
---

# Tasks: SpecKit Visual UI

**Input**: Design documents from `/specs/001-speckit-visual-ui/`

**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/speckit-ui-api.md, quickstart.md

**Tests**: Included. The project constitution (Principle IV, Test-First for New Crates) mandates tests alongside implementation for the new `joey-speckit-ui` crate, so contract/round-trip and integration tests are treated as required, not optional, for this feature.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

Web application structure per plan.md:
- Backend crate: `crates/joey-speckit-ui/src/`, tests in `crates/joey-speckit-ui/tests/`
- Frontend: `web/speckit-ui/src/`, tests in `web/speckit-ui/tests/e2e/`

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Project initialization and basic structure

- [X] T001 Create `crates/joey-speckit-ui/` crate skeleton (`Cargo.toml`, `src/lib.rs`, `src/main.rs`) and add `joey-speckit-ui` to the `[workspace] members` list in `Cargo.toml`
- [X] T002 Add new dependencies to `crates/joey-speckit-ui/Cargo.toml`: `axum`, `notify`, `pulldown-cmark` (new), plus workspace deps `tokio`, `serde`, `serde_json`, `sha2`, `anyhow`, `tracing`, `async-trait` (per research.md decisions)
- [X] T003 [P] Scaffold `web/speckit-ui/` frontend project (`package.json`, `tsconfig.json`, minimal `src/index.ts` + `index.html`) per plan.md Project Structure
- [X] T004 [P] Configure linting/formatting for the new crate (`cargo fmt`/`clippy` already workspace-wide; add `.eslintrc`/`prettier` config for `web/speckit-ui`)

**Checkpoint**: `cargo build -p joey-speckit-ui` and `npm install && npm run build` (in `web/speckit-ui`) both succeed with empty/stub code.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core infrastructure that MUST be complete before ANY user story can be implemented — data model, parsers, conflict-safe writer, and the API server skeleton all three pillars depend on.

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [X] T005 [P] Define core model types (Feature, Specification, UserStory, Requirement, Plan, ConstitutionGate, Task, ClarificationEntry, AnalysisFinding, Status enum) in `crates/joey-speckit-ui/src/model.rs` per data-model.md
- [X] T006 [P] Implement `spec.md` parser in `crates/joey-speckit-ui/src/parser/spec.rs` (Specification, UserStory, Requirement, ClarificationEntry extraction) using `pulldown-cmark`
- [X] T007 [P] Implement `plan.md` parser in `crates/joey-speckit-ui/src/parser/plan.rs` (Plan, ConstitutionGate extraction, Constitution Check table parsing)
- [X] T008 [P] Implement `tasks.md` parser in `crates/joey-speckit-ui/src/parser/tasks.rs` (Task extraction: id, status, `[P]` marker, target files, user_story_ref), mapping unparseable lines to `Status::Unparsed` per data-model.md
- [X] T009 Implement conflict-safe writer in `crates/joey-speckit-ui/src/writer.rs` and `crates/joey-speckit-ui/src/conflict.rs`: SHA-256 content hashing, hash-check-then-write, reject with conflict on mismatch (depends on T005)
- [X] T010 [P] Implement filesystem watcher in `crates/joey-speckit-ui/src/watcher.rs` using `notify`, debounced (~500ms), emitting change events per feature directory (depends on T001)
- [X] T011 Implement `commands.rs` in `crates/joey-speckit-ui/src/commands.rs`: wrappers that invoke the existing `/speckit-clarify`, `/speckit-analyze`, `/speckit-implement` skill scripts/CLI equivalents as subprocesses (depends on T001)
- [X] T012 Implement API server skeleton in `crates/joey-speckit-ui/src/api/mod.rs` and `src/main.rs`: axum app bound to `127.0.0.1`, `GET /api/features` and `GET /api/features/{id}` routes wired to the parsers (depends on T005, T006, T007, T008)
- [X] T013 [P] Contract round-trip test in `crates/joey-speckit-ui/tests/parser_roundtrip.rs`: parse → model → re-serialize preserves byte-for-byte-equivalent content for a sample spec.md/plan.md/tasks.md fixture (depends on T006, T007, T008)
- [X] T014 [P] Conflict detection test in `crates/joey-speckit-ui/tests/conflict_detection.rs`: verify a write with a stale hash is rejected (409-equivalent) and the file is left unmodified (depends on T009)
- [X] T015 [P] API client scaffolding in `web/speckit-ui/src/api-client.ts`: typed fetch/WebSocket wrapper matching contracts/speckit-ui-api.md (depends on T003)

**Checkpoint**: Foundation ready — `GET /api/features/{id}` returns a correctly parsed model for a real feature directory (e.g. this one), round-trip and conflict tests pass, and the frontend can fetch it via `api-client.ts`. User story implementation can now begin.

---

## Phase 3: User Story 1 - Visualize the Spec-to-Task Hierarchy (Priority: P1) 🎯 MVP

**Goal**: Render an interactive canvas showing Specification → Plan → Task nodes with status color-coding, live-updating from external file changes, and supporting inline edit-and-persist.

**Independent Test**: Open the canvas for `specs/001-speckit-visual-ui/` itself (once it has a `tasks.md`) and verify per quickstart.md scenarios V1–V3: correct node/connection counts with zero drops/duplicates, external edits reflected within 5s, and inline edits persisted back to disk.

### Tests for User Story 1 ⚠️

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [X] T016 [P] [US1] Contract test for `PATCH /api/features/{id}/tasks/{taskId}` (success + 409 conflict cases) in `crates/joey-speckit-ui/tests/contract_patch_task.rs`
- [X] T017 [P] [US1] Contract test for `PATCH /api/features/{id}/spec` (success + 409 conflict cases) in `crates/joey-speckit-ui/tests/contract_patch_spec.rs`
- [X] T018 [P] [US1] E2E test "canvas renders full hierarchy with zero drops/duplicates" in `web/speckit-ui/tests/e2e/canvas_hierarchy.spec.ts` (quickstart V1)
- [X] T019 [P] [US1] E2E test "external file change updates node status within 5s" in `web/speckit-ui/tests/e2e/canvas_live_update.spec.ts` (quickstart V2)
- [X] T020 [P] [US1] E2E test "inline edit persists to disk" in `web/speckit-ui/tests/e2e/canvas_inline_edit.spec.ts` (quickstart V3)

### Implementation for User Story 1

- [X] T021 [US1] Implement `PATCH /api/features/{id}/spec` handler in `crates/joey-speckit-ui/src/api/rest.rs` using the writer/conflict module (depends on T009, T012, T017)
- [X] T022 [US1] Implement `PATCH /api/features/{id}/tasks/{taskId}` handler in `crates/joey-speckit-ui/src/api/rest.rs` (depends on T009, T012, T016)
- [X] T023 [US1] Implement `WebSocket /api/features/{id}/watch` handler in `crates/joey-speckit-ui/src/api/ws.rs`, bridging `watcher.rs` events to connected clients (depends on T010, T012)
- [X] T024 [P] [US1] Build canvas rendering module in `web/speckit-ui/src/canvas/` (nodes for Specification/Plan/Task, parent-child connections, status color-coding per data-model.md Status enum) (depends on T015)
- [X] T025 [US1] Wire canvas to `GET /api/features/{id}` on load and to the `watch` WebSocket for live updates (depends on T023, T024)
- [X] T026 [US1] Implement double-click inline editor in `web/speckit-ui/src/canvas/inline-editor.ts`, calling the `PATCH` endpoints and handling 409 conflicts with a visible message + reload prompt (depends on T021, T022, T024)
- [X] T027 [US1] Implement "not yet created" empty state for missing `plan.md`/`tasks.md` with a button to trigger the corresponding step, in `web/speckit-ui/src/canvas/` (Edge Cases in spec.md) (depends on T024)
- [X] T028 [US1] Surface `Status::Unparsed` nodes distinctly on the canvas (malformed/hand-edited entries) instead of dropping them (Edge Cases) (depends on T024)

**Checkpoint**: At this point, User Story 1 should be fully functional and testable independently — this is the MVP.

---

## Phase 4: User Story 2 - Draft and Clarify Specs in a Split-Screen Workspace (Priority: P2)

**Goal**: Split-screen workspace with a document pane and an assistant panel that drives `/speckit-clarify` and `/speckit-analyze`, highlighting the exact document line each question/finding concerns.

**Independent Test**: Open a spec with an unresolved `[NEEDS CLARIFICATION]` marker, run clarify from the panel, answer it, and confirm both UI and on-disk file update (quickstart V4); run analyze and confirm findings anchor to specific lines (quickstart V5).

### Tests for User Story 2 ⚠️

- [X] T029 [P] [US2] Contract test for `POST /api/features/{id}/clarify` + `POST .../clarify/{session_id}/answer` in `crates/joey-speckit-ui/tests/contract_clarify.rs`
- [X] T030 [P] [US2] Contract test for `POST /api/features/{id}/analyze` (finding anchoring + constitution_compliance field) in `crates/joey-speckit-ui/tests/contract_analyze.rs`
- [X] T031 [P] [US2] E2E test "clarify question highlights correct document line and resolves on disk" in `web/speckit-ui/tests/e2e/clarify_flow.spec.ts` (quickstart V4)
- [X] T032 [P] [US2] E2E test "analyze findings anchor to specific lines" in `web/speckit-ui/tests/e2e/analyze_findings.spec.ts` (quickstart V5)

### Implementation for User Story 2

- [X] T033 [US2] Implement `POST /api/features/{id}/clarify` + session-scoped `POST .../answer` handlers in `crates/joey-speckit-ui/src/api/rest.rs`, invoking `commands.rs` clarify wrapper and streaming questions over a session WebSocket (depends on T011, T012, T029)
- [X] T034 [US2] Implement `POST /api/features/{id}/analyze` handler in `crates/joey-speckit-ui/src/api/rest.rs`, invoking `commands.rs` analyze wrapper and producing `AnalysisFinding` list + `constitution_compliance` (depends on T011, T012, T030)
- [X] T035 [P] [US2] Build document editor pane in `web/speckit-ui/src/workspace/document-pane.ts` (renders active spec/plan Markdown, supports line-anchored scroll/highlight) (depends on T015)
- [X] T036 [P] [US2] Build assistant panel with QA chat box in `web/speckit-ui/src/workspace/assistant-panel.ts`, invoking clarify/analyze endpoints (depends on T015)
- [X] T037 [US2] Wire clarify question/answer flow: chat box question → highlight corresponding line in document pane → submit answer → replace highlighted text and reflect new content hash (depends on T033, T035, T036)
- [X] T038 [US2] Wire analyze findings to document-pane annotations anchored by `target_line_or_section` (depends on T034, T035, T036)
- [X] T039 [US2] Implement Constitution Compliance gauge component in `web/speckit-ui/src/workspace/constitution-gauge.ts`, driven by `plan.constitution_gates` and/or the latest analyze result (FR-016) (depends on T034)

**Checkpoint**: At this point, User Stories 1 AND 2 should both work independently.

---

## Phase 5: User Story 3 - Track and Launch Execution from a Kanban Board (Priority: P3)

**Goal**: Kanban board parsed from `tasks.md` with Todo/In Progress/Done columns, per-card metadata (user story, parallel eligibility, target files), single-task-per-click execution with live output, and a toggleable dependency/timeline view.

**Independent Test**: Open the board for a feature with `[P]`-marked tasks, confirm badges/metadata, execute a single task via its card and confirm only that card runs and moves to Done with `tasks.md` updated (quickstart V6); toggle dependency view and confirm shared-file tasks are linked (quickstart V7).

### Tests for User Story 3 ⚠️

- [X] T040 [P] [US3] Contract test for `POST /api/features/{id}/tasks/{taskId}/execute` (single-task-only semantics, run status transitions) in `crates/joey-speckit-ui/tests/contract_execute.rs`
- [X] T041 [P] [US3] E2E test "board shows correct metadata badges and only executes the clicked card" in `web/speckit-ui/tests/e2e/board_execute.spec.ts` (quickstart V6)
- [X] T042 [P] [US3] E2E test "dependency/timeline view links tasks sharing target files" in `web/speckit-ui/tests/e2e/board_dependency_view.spec.ts` (quickstart V7)

### Implementation for User Story 3

- [X] T043 [US3] Implement `POST /api/features/{id}/tasks/{taskId}/execute` handler in `crates/joey-speckit-ui/src/api/rest.rs`: invokes `commands.rs` implement-wrapper scoped to one task, streams output over `ws://.../api/runs/{run_id}`, and on completion updates `tasks.md` via the writer (depends on T009, T011, T012, T040)
- [X] T044 [P] [US3] Build Kanban board DOM/column layout in `web/speckit-ui/src/board/board.ts` (Todo/In Progress/Done columns from Task.status) (depends on T015)
- [X] T045 [P] [US3] Build task card component in `web/speckit-ui/src/board/task-card.ts` showing user story, Parallel badge (`parallel_eligible`), and target files (depends on T044)
- [X] T046 [US3] Wire "Execute Tasks" click on a card to `POST .../execute` and the run's WebSocket, rendering live output and moving the card to Done on success (depends on T043, T045)
- [X] T047 [P] [US3] Implement dependency/timeline toggle view in `web/speckit-ui/src/board/dependency-view.ts`, linking tasks that share `target_files` (depends on T044)

**Checkpoint**: All user stories should now be independently functional.

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Improvements that affect multiple user stories

- [X] T048 [P] Implement guided init wizard (`POST /api/init` handler in `crates/joey-speckit-ui/src/api/rest.rs` + `web/speckit-ui/src/init-wizard.ts`) per FR-015
- [X] T049 [P] Documentation: add `web/speckit-ui/README.md` and `crates/joey-speckit-ui/README.md` covering setup/run steps from quickstart.md
- [X] T050 Error handling audit: ensure every API error path returns the shared `{ "error", "message" }` shape from contracts/speckit-ui-api.md
- [X] T051 [P] Add `tracing` spans/logs across `crates/joey-speckit-ui` request handlers for observability
- [X] T052 Run full `quickstart.md` validation pass (V1–V9) against `specs/001-speckit-visual-ui/` itself and record results — V1/V2/V3 (canvas), V4/V5 (clarify/analyze), V6/V7 (board execute/dependency), V8 (conflict rejection) verified via the 8 passing Playwright E2E specs against the mocked API contract; backend contract behavior (conflict, patch, execute, clarify, analyze) independently verified via 34 passing `cargo test -p joey-speckit-ui` cases. V9 (gauge) covered by `analyze_findings.spec.ts`. Full manual browser walkthrough against a live-running backend + frontend pair was not additionally performed in this pass (see Completion Report note on remaining risk).
- [X] T053 Run `cargo test --workspace` and `cargo clippy --workspace` to confirm no regressions introduced in existing crates

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies - can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion - BLOCKS all user stories
- **User Stories (Phase 3+)**: All depend on Foundational phase completion
  - User Story 1 has no dependency on US2/US3
  - User Story 2 depends on the document/parser foundation (Phase 2) but not on US1's canvas UI; it may reuse US1's `api-client.ts` wiring conventions
  - User Story 3 depends on Phase 2's writer/conflict module (shared with US1) but not on US1's canvas or US2's workspace
- **Polish (Phase 6)**: Depends on all desired user stories being complete

### User Story Dependencies

- **User Story 1 (P1)**: Can start after Foundational (Phase 2) — No dependencies on other stories
- **User Story 2 (P2)**: Can start after Foundational (Phase 2) — Independently testable; does not require US1's canvas to function
- **User Story 3 (P3)**: Can start after Foundational (Phase 2) — Independently testable; shares the writer/conflict module with US1 but not US1's UI code

### Within Each User Story

- Tests MUST be written and FAIL before implementation (constitution Principle IV)
- Backend handlers before frontend wiring that calls them
- Core canvas/board/workspace components before the interactions that wire them together
- Story complete before moving to next priority

### Parallel Opportunities

- T003, T004 (Setup) can run in parallel with T001/T002
- T005–T008, T010 (Foundational, different files) can run in parallel; T009 depends on T005; T011 is independent; T012 depends on T005–T008
- T013, T014, T015 (Foundational tests/scaffolding) can run in parallel once their dependencies land
- All Tests-first tasks within a story (T016–T020, T029–T032, T040–T042) marked [P] can run in parallel
- Frontend-only implementation tasks marked [P] within a story (e.g. T024, T035/T036, T044/T045, T047) can run in parallel with each other
- Once Foundational completes, US1, US2, and US3 backend+test tracks can proceed in parallel if staffed, though US1 remains the recommended MVP-first sequence

---

## Parallel Example: User Story 1

```bash
# Launch all tests for User Story 1 together:
Task: "Contract test for PATCH /api/features/{id}/tasks/{taskId} in crates/joey-speckit-ui/tests/contract_patch_task.rs"
Task: "Contract test for PATCH /api/features/{id}/spec in crates/joey-speckit-ui/tests/contract_patch_spec.rs"
Task: "E2E test canvas renders full hierarchy in web/speckit-ui/tests/e2e/canvas_hierarchy.spec.ts"
Task: "E2E test external file change updates node status in web/speckit-ui/tests/e2e/canvas_live_update.spec.ts"
Task: "E2E test inline edit persists to disk in web/speckit-ui/tests/e2e/canvas_inline_edit.spec.ts"

# Launch canvas rendering work in parallel with backend handlers:
Task: "Build canvas rendering module in web/speckit-ui/src/canvas/"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (CRITICAL - blocks all stories)
3. Complete Phase 3: User Story 1
4. **STOP and VALIDATE**: Run quickstart.md scenarios V1–V3 independently
5. Deploy/demo if ready — the canvas alone already delivers the core value proposition from the spec

### Incremental Delivery

1. Complete Setup + Foundational → Foundation ready
2. Add User Story 1 → Validate (V1–V3) → Deploy/Demo (MVP!)
3. Add User Story 2 → Validate (V4–V5) → Deploy/Demo
4. Add User Story 3 → Validate (V6–V7) → Deploy/Demo
5. Polish phase → Validate (V8–V9, full quickstart pass, workspace-wide tests) → Final release

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational together
2. Once Foundational is done:
   - Developer A: User Story 1 (canvas)
   - Developer B: User Story 2 (co-pilot workspace)
   - Developer C: User Story 3 (Kanban board)
3. Stories complete and integrate independently against the shared API contract

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Verify tests fail before implementing (constitution Principle IV)
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- Avoid: vague tasks, same file conflicts, cross-story dependencies that break independence
- Reject-on-conflict write semantics (FR-018, Clarifications Q2) and single-task-per-click execution (Clarifications Q3) are load-bearing constraints — do not relax them during implementation without a spec amendment

---

## Phase 7: Convergence

**Purpose**: Close gaps found by `/speckit-converge` between the implemented code and the spec/plan/tasks intent. Generated from the Convergence Findings table — see that report for full evidence per item.

- [X] T054 Implement `WebSocket /api/features/{id}/session/{session_id}` in `crates/joey-speckit-ui/src/api/ws.rs`, streaming live clarify question/answer events for a session started by `post_clarify`, per FR-007/FR-008 (missing)
- [X] T055 Implement `WebSocket /api/runs/{run_id}` in `crates/joey-speckit-ui/src/api/ws.rs`, streaming live task-execution output and a terminal succeeded/failed status for a run started by `post_execute`, per FR-012/FR-013 (missing)
- [X] T056 Wire `commands::run_execute` output into the `/api/runs/{run_id}` channel from T055, and on terminal success write the task's completion (checkbox + status) back to `tasks.md` via `writer.rs`, per FR-012/FR-013/FR-017 (partial)
- [X] T057 Perform one live manual smoke run of `cargo run -p joey-speckit-ui` + `npm run dev` (in `web/speckit-ui`) together, exercising quickstart.md scenarios V1–V9 against the real (non-mocked) backend, and record actual pass/fail results in quickstart.md or a follow-up note (partial)
- [X] T058 [P] Add a short root-level dev-workflow note (e.g. in the repo's top-level README or a `web/speckit-ui`/`crates/joey-speckit-ui` cross-link) documenting how to start both the backend and frontend together for local development, per plan.md Project Structure (missing)

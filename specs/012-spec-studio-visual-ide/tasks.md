---
description: "Task list for feature 012-spec-studio-visual-ide"
---

# Tasks: Spec Studio — Visual IDE for Spec Kit

**Input**: Design documents from `/specs/012-spec-studio-visual-ide/`

**Prerequisites**: plan.md (required), spec.md (required for user stories),
research.md, data-model.md, contracts/ (cst-parser.md, patch-engine.md,
semantic-graph.md, meaning-widgets.md, overlay-store.md), quickstart.md,
`.specify/memory/constitution.md` v1.1.0.

**Tests**: The spec does not request a separate TDD track, but Constitution
Principle IV ("tests alongside implementation, not deferred"), Principle VII
("any feature touching a public surface MUST ship with regression coverage"),
and Principle VIII ("performance-sensitive paths MUST carry an explicit
budget or benchmark note") make tests mandatory for this feature: it
introduces a new lossless CST format, extends the public REST/WS API,
extends shared parsers, and adds the new UI-state JSON on-disk format.
Tests are therefore folded into each implementation task ("with … tests")
and cross-cutting regression + performance tasks are explicit in Phase 2
and the Polish phase.

**Organization**: Tasks are grouped by user story (spec.md US1–US5) so each
story is independently implementable and testable. This feature **extends**
the existing `joey-speckit-ui` crate + `web/speckit-ui` frontend from
`specs/001`/`010` (no new crate); every task is additive (Constitution VII).
The concept's P0–P6 build sequence maps to phases as follows: P0 → Phase 2
(Foundational); P1 → US1; P2 → US2; P3 → US3; P4 → US4; P5 → US5; P6 →
Polish. The concept's framing — "if P0 is shaky, everything collapses" —
makes Phase 2 the hard gate.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies on incomplete tasks in the same phase)
- **[Story]**: Which user story this task belongs to (US1–US5). Setup/Foundational/Polish tasks carry NO story label.
- All backend paths are under `crates/joey-speckit-ui/`; all frontend paths are under `web/speckit-ui/`.

## Path Conventions

- **Backend**: `crates/joey-speckit-ui/src/<module>.rs` and `crates/joey-speckit-ui/tests/<name>.rs`
- **Frontend**: `web/speckit-ui/src/<area>/<file>.ts`
- This is a web application (existing `backend` = Rust crate, `frontend` = Vite/TS app, both from `specs/001`/`010`).

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Add the one new frontend dependency (CodeMirror 6, scoped —
research.md §2) and extend the data layer with the additive CST + meaning +
overlay types (data-model.md §1–§4). No behavioral change yet — purely
additive types (Constitution VII: existing types preserved).

- [X] T001 Add `codemirror` and `@codemirror/lang-markdown` to `web/speckit-ui/package.json` `dependencies`; record `vite build` bundle-size delta vs `specs/010` baseline in `research.md` §2 (Constitution VIII). Do NOT add `@uiw/react-codemirror` (research.md §1 — no React).
- [X] T002 [P] Add `cst/`, `meaning/`, `patch/`, and `ui_state.rs` module declarations to `crates/joey-speckit-ui/src/lib.rs` (stubs only — implementations land in Phase 2). Confirm `cargo build -p joey-speckit-ui` still passes.
- [X] T003 [P] Extend `crates/joey-speckit-ui/src/model.rs` with additive CST + meaning + overlay types from `data-model.md`: `CstNode`/`CstKind`/`CstProps`/`CstDocument`/`NodeId` (§1), `SemanticNode`/`SemanticKind`/`SemanticProps`/`SemanticId`/`OriginTag`/`Edge`/`EdgeKind`/`SemanticGraph` (§2), `Defect`/`DefectClass`/`Scaffold`/`PatchOp`/`PatchResult`/`ThreeWayMerge`/`MergeConflict`/`Resolution` (§3), `OverlayRecord` (extended `AcceptedClarify`/`CommentThread` variants) + `UiState`/`PaneLayout`/`BoardFilters` (§4) — all `#[derive(Serialize,Deserialize)]`, existing types untouched (Constitution VII).
- [X] T004 [P] Add CST test fixtures under `crates/joey-speckit-ui/tests/fixtures/cst/` covering: clean spec.md/plan.md/tasks.md, malformed lists, unknown extensions, code fences with spec-kit project trees, GWT blocks, tables, comment-style prose, and the byte-preservation edge cases (FR-012 Edge Cases). These fixtures back T011 (cst_roundtrip) and are referenced by quickstart.md scenario 1.

**Checkpoint**: Data layer extended; `cargo build -p joey-speckit-ui` passes; existing `tests/parser_roundtrip.rs` and `tests/contract_api_regression.rs` still green.

---

## Phase 2: Foundational — P0 Lossless CST + Patch Engine (Blocking Prerequisites)

**Purpose**: The P0 critical foundation. A lossless CST parser built on the
already-present `pulldown-cmark` (research.md §3 — no new backend dep), a
byte-anchor patch engine with guard/transaction/undo (FR-012/013/014), a
three-way semantic-block merge (FR-016), the derived semantic graph +
cache (FR-040, research.md §4), and the overlay store extension (FR-032,
clarification Q2). Every later widget depends on this. **The concept calls
this "the one thing to get right first."**

**⚠️ CRITICAL**: No user story work can begin until this phase is complete.

### CST parser (FR-012/013)

- [X] T005 Implement `crates/joey-speckit-ui/src/cst/parser.rs`: `CstParser` trait per `contracts/cst-parser.md`. Walk `pulldown-cmark`'s `OffsetIter` event stream (offsets already exposed by the dep — research.md §3) and build a `CstDocument` whose nodes partition `[0, file_len)` with no gaps (gaps become `CstKind::Raw` preserving bytes verbatim). Always total — never panics, never drops bytes, never returns `Err` for odd markdown (only I/O errors). Performance: ≤400 ms p95 for a 200-task file (FR-040).
- [X] T006 [P] Implement `crates/joey-speckit-ui/src/cst/anchors.rs`: per-node `byte_start`/`byte_end` (UTF-8), `expected_bytes`, `revision_hash` (SHA-256 of file via existing `sha2` dep), and `fingerprint` (structural id like `"requirement/FR-016"`) per data-model.md §1. Deterministic `NodeId` allocation stable across reparses of byte-identical content.
- [X] T007 [P] Implement `crates/joey-speckit-ui/src/cst/fingerprint.rs`: structural fingerprint derivation per `CstKind` + extracted semantic id (e.g. a `ListItem` whose text matches `^\s*-\s*\*\*FR-\d+\*\*` gets `"requirement/FR-016"`). Used by three-way merge pairing and UI re-binding across edits.
- [X] T008 Implement `CstMaterialize::materialize()` for `CstDocument` — reconstruct exact source bytes by concatenating node ranges in order. The identity `parse(p,b)?.materialize() == b` is the round-trip invariant.

### Patch engine (FR-014/016)

- [X] T009 Implement `crates/joey-speckit-ui/src/patch/guard.rs`: before-write verification of `revision_hash` + `expected_bytes` for every targeted node; 100% external-change detection (SC-006). Returns `Ok` or routes to `PatchResult::Conflict`.
- [X] T010 Implement `crates/joey-speckit-ui/src/patch/surgical.rs`: apply `PatchOp::{Replace, InsertAfter, Delete}` to a temp buffer so only the edited node's range changes; every byte outside it stays identical (FR-041). Range-shift accounting for siblings.
- [X] T011 Implement `crates/joey-speckit-ui/src/patch/transaction.rs`: temp buffer → CST re-parse → validation → atomic file replace (write-temp + rename) → return verified inverse `undo: Vec<PatchOp>` (FR-014). On validation failure return `PatchResult::ValidationFailed` with diagnostics, replacing no file.
- [X] T012 Implement `crates/joey-speckit-ui/src/patch/merge.rs`: three-way merge at semantic-block (CST node) level per `contracts/patch-engine.md` + research.md §6. Pair nodes by `fingerprint` across base/current/proposed; auto-merge non-conflicting nodes; surface `MergeConflict` for both-sides-changed nodes with `TakeBase|TakeCurrent|TakeProposed|Edit(bytes)` resolution. <500 ms budget for a 200-task file (plan.md performance table).
- [X] T013 Implement `crates/joey-speckit-ui/src/patch/mod.rs`: `PatchEngine` trait + default impl composing guard/surgical/transaction/merge per `contracts/patch-engine.md`. Wire `PatchResult::{Applied, Conflict, AnchorUnresolved, ValidationFailed}`. Per-node locking when a run touches the same file (FR-016 concurrency).

### Semantic graph + cache (FR-009/040)

- [X] T014 Implement `crates/joey-speckit-ui/src/meaning/mapping.rs`: classify each CST node into at most one `SemanticKind` per the exhaustive FR-009 catalog (`contracts/semantic-graph.md` mapping table). A node matching no pattern produces no semantic node. Pure function, no I/O.
- [X] T015 Implement `crates/joey-speckit-ui/src/meaning/graph.rs`: `SemanticGraphBuilder::build(feature_id, documents)` deriving the `SemanticGraph` (nodes + edges + `revision_hashes`) per data-model.md §2. Edges: traceability spine + coverage + containment + dependency + proposed-entity-relationship (FR-011).
- [X] T016 [P] Implement `crates/joey-speckit-ui/src/meaning/coverage.rs`: defect detection — `OrphanRequirement`, `RogueTask`, `Unverified`, `ConstitutionBreach` — each with its `Scaffold` (deterministic stub insertion bytes + anchor + `InsertionMode`) and optional `GenerativeFollowon` (clarification Q3 hybrid). 100% recall on fixtures (SC-009).
- [X] T017 Implement `crates/joey-speckit-ui/src/meaning/cache.rs`: in-memory `SemanticGraph` per open feature, invalidated by the existing `watcher.rs` events (research.md §4). Lazy recompute on next read (≤400 ms budget). Never persisted (Constitution III).
- [X] T018 Implement `crates/joey-speckit-ui/src/meaning/mod.rs`: re-exports + `OriginTag` propagation (Source/Derived/Overlay per FR-010); success-criterion current values appear only with a named evidence source, else "not measured".

### Overlay store extension (FR-032)

- [X] T019 Extend `crates/joey-speckit-ui/src/history.rs`: add `OverlayRecord::AcceptedClarify` and `OverlayRecord::CommentThread` variants per data-model.md §4, preserving the existing `WorkflowAttempt` variant and `schema_version: 1` gate (Constitution VII). `CommentThread` carries `anchor_node` + `anchor_fingerprint` for detach detection.
- [X] T020 Implement `crates/joey-speckit-ui/src/ui_state.rs`: `OverlayStore::load_ui_state`/`save_ui_state` for the per-repo+branch JSON file at `~/.joey/speckit-ui/ui-state/<repo-hash>-<branch>.json`, atomic save (write-temp + rename), `schema_version: 1`, write-tree isolation verified (never inside a feature dir).

### Regression + round-trip coverage (Constitution IV/VII — mandatory)

- [X] T021 Create `crates/joey-speckit-ui/tests/cst_roundtrip.rs`: assert `parse(p,b)?.materialize() == b` (the identity) across every fixture in T004 — clean and malformed/unknown-syntax. Document any construct that breaks the identity as a failing test (FR-012).
- [X] T022 [P] Create `crates/joey-speckit-ui/tests/byte_anchor_patch.rs`: for each `PatchOp` and each node kind, assert only the edited node's range changed and every other byte is identical (FR-014/041); assert the guard returns `Conflict` on every external-change scenario (SC-006).
- [X] T023 [P] Create `crates/joey-speckit-ui/tests/three_way_merge.rs`: assert semantic-block merge produces `MergeConflict` labelled by `fingerprint` (not line number); auto-mergeable nodes resolve silently; resolutions apply cleanly (FR-016).
- [X] T024 [P] Create `crates/joey-speckit-ui/tests/ui_state_roundtrip.rs`: assert the UI-state JSON round-trips, `schema_version` preserved, and the store never writes inside any `specs/` directory (FR-032 write-tree isolation).
- [X] T025 [P] Regression: extend `crates/joey-speckit-ui/tests/history_jsonl_roundtrip.rs` to cover the two new `OverlayRecord` variants and assert the existing `WorkflowAttempt` record still round-trips unchanged (Constitution VII public-format gate).
- [X] T026 Regression: run `cargo test -p joey-speckit-ui` and confirm `tests/contract_api_regression.rs`, `tests/parser_roundtrip.rs`, `tests/conflict_detection.rs`, `tests/contract_patch_spec.rs`, `tests/contract_patch_task.rs`, `tests/scale_validation.rs` all pass unchanged (Constitution VII — the feature is strictly additive over `specs/001`/`010`).

**Checkpoint**: P0 foundation ready — lossless CST, byte-anchor patch engine, three-way merge, semantic graph + cache, and the overlay store extension exist and are tested. The identity round-trip is green. User story implementation can now begin.

---

## Phase 3: User Story 1 — Start a Feature and Orient (Priority: P1) 🎯 MVP

**Goal**: A developer who has never run a Spec Kit slash command opens the
IDE, is guided through setup into a single landing view that answers where
the feature is, what's healthy, what's blocked, and the one deterministic
next action — with every empty/failed/disconnected state offering exactly
one recovery action (spec US1; FR-001/002/003/004/005/006/007/008).

**Independent Test**: Point the IDE at a repo with partial Spec Kit setup,
complete the guided flow, and confirm the landing view renders a
deterministic next action, health, progress, branch binding, artifact list,
and recent-activity timeline — all from on-disk state, no LLM
recommendation (quickstart.md scenario 8, partial).

### Backend for US1

- [X] T027 [P] [US1] Implement readiness derivation in `crates/joey-speckit-ui/src/workflow.rs`: extend the `specs/010` step-state derivation so "Done" now requires the output artifact's CST to parse cleanly and be newer than its inputs (FR-007). Pure function over CST + run history; deterministic, no LLM (FR-005).
- [X] T028 [P] [US1] Implement the first-run setup endpoint sequence in `crates/joey-speckit-ui/src/api/rest.rs`: `GET /api/setup/scan-repo` (validate read/write + detect Spec Kit setup gaps), `POST /api/setup/preview` (propose slug/branch/paths/permissions, nothing written), `POST /api/setup/commit` (create feature dir + initial artifact in staged mode). All additive over the existing `specs/001`/`010` routes (Constitution VII).
- [X] T029 [US1] Implement the Atlas landing endpoint in `crates/joey-speckit-ui/src/api/rest.rs`: `GET /api/features/{id}/atlas` returning next-action (deterministic), progress, health (parsing status + open unknowns + orphan count from the semantic graph), branch binding + drift, artifact list with staleness, and recent-activity timeline (from JSONL history) per FR-004/005.
- [X] T030 [US1] Implement `GET /api/features/{id}/stage-bar` returning the five-stage indicator (Define→Design→Break down→Build→Review) + per-step state + gate reasons per FR-006/007/008.
- [X] T031 [US1] Implement the recovery-state endpoint `GET /api/features/{id}/recovery-states` returning each empty/failed/disconnected state with exactly one primary recovery action and which repo files the action touches per FR-002.

### Frontend for US1

- [X] T032 [P] [US1] Create `web/speckit-ui/src/firstrun/wizard.ts`: the five-step setup wizard (repo → Spec Kit check → branch → brief → preview) per FR-001, with staged-mode confirmation before any write. Vanilla-TS web component (research.md §1).
- [X] T033 [P] [US1] Create `web/speckit-ui/src/atlas/landing.ts`: the bento landing view (next action, progress, health, binding, artifacts, recent activity) per FR-004, each tile opening the relevant stage without losing context.
- [X] T034 [P] [US1] Create `web/speckit-ui/src/atlas/stage-bar.ts`: the compact five-stage header + expandable Spec Kit command detail + gate cards per FR-006/008. States computed from the backend; never guessed.
- [X] T035 [US1] Create `web/speckit-ui/src/atlas/recovery.ts`: render each empty/failed/disconnected state with exactly one primary recovery action per FR-002 (no stack traces, no bare "command failed").
- [X] T036 [US1] Extend `web/speckit-ui/src/app.ts` with intent-based navigation (Overview → Define → Design → Break down → Build → Review) per FR-003; `spec.md`/`plan.md`/`tasks.md` appear inside stages as source indicators and escape hatches, not as upfront knowledge.
- [X] T037 [US1] Extend `web/speckit-ui/src/api-client.ts` with typed clients for the new `/setup/*`, `/atlas`, `/stage-bar`, and `/recovery-states` routes.

**Checkpoint**: US1 functional — a developer can enter a feature, see a deterministic landing view, and recover from any empty/failed state. Each tile opens the right stage.

---

## Phase 4: User Story 2 — Read and Author Meaning, Not Markdown (Priority: P1)

**Goal**: Each markdown construct renders with the visual primitive
matching its semantics, and the developer can edit through structured
forms, inline markdown (⌥M), or the raw whole file (⌥⇧M) — with every
edit writing back through verified byte anchors and preserving every
untouched byte (spec US2; FR-009/010/011/012/013/014/015/016).

**Independent Test**: Open a populated `spec.md`/`plan.md`, confirm every
FR-009 construct renders as its matching widget, edit one node through the
structured form, and confirm only that node's bytes changed on disk
(quickstart.md scenarios 1 + 8, partial).

### Meaning widgets — spec constructs (FR-009 catalog)

- [X] T038 [P] [US2] Create `web/speckit-ui/src/meaning/story-card.ts`: render `UserStory` + nested `AcceptanceScenario` (Given/When/Then flow) per FR-009, with priority color and move controls. Edits compile to `PatchOp` and POST to `/patch`.
- [X] T039 [P] [US2] Create `web/speckit-ui/src/meaning/requirement-chip.ts`: render `Requirement` with modality-driven color + derived coverage chip from the graph per FR-009/022.
- [X] T040 [P] [US2] Create `web/speckit-ui/src/meaning/metric-card.ts`: render `SuccessCriterion` with target/unit/direction + `OriginTag`-distinguished evidence per FR-010. "Not measured" when no evidence source; no decorative bars implying absent data.
- [X] T041 [P] [US2] Create `web/speckit-ui/src/meaning/entity-graph.ts`: render `KeyEntity` + `EntityRelationship` as a vanilla-SVG graph (no @xyflow — research.md §1); proposed edges dashed and requiring confirmation per FR-011. Relationship-table view as the keyboard/mobile alternative (FR-037/039).
- [X] T042 [P] [US2] Create `web/speckit-ui/src/meaning/spec-sheet.ts`: render `TechnicalContextField` as labelled tiles; unresolved values render as directly-clickable controls, not color-only text per FR-009.

### Meaning widgets — plan constructs (FR-009 catalog, continued)

- [X] T043 [P] [US2] Create `web/speckit-ui/src/meaning/gate-row.ts`: render `ConstitutionGate` pass/fail rows with evidence + an aggregate gauge, and `ComplexityViolation` as a side-by-side rule/need/rejected-alternative card per FR-009.
- [X] T044 [P] [US2] Create `web/speckit-ui/src/meaning/tree-diff.ts`: render `ProjectStructureNode` as a tree diff with exists/planned-missing/not-in-plan status; each missing node offers "scaffold this" per FR-009.

### The three editing depths (FR-015)

- [X] T045 [US2] Create `web/speckit-ui/src/editor/structured-form.ts`: typed form fields per node kind (priority=select, GWT=three inputs, modality=toggle) — the default depth, impossible to produce malformed markdown per FR-015. Compiles to a single `Replace` op.
- [X] T046 [US2] Create `web/speckit-ui/src/editor/inline-markdown.ts`: CodeMirror 6 (`codemirror` + `@codemirror/lang-markdown`, framework-free — research.md §2) on just the selected node's byte range (⌥M). Maps CodeMirror line offsets back to CST byte anchors.
- [X] T047 [US2] Create `web/speckit-ui/src/editor/raw-file.ts`: CodeMirror 6 on the whole document (⌥⇧M) — the escape hatch per FR-015.
- [X] T048 [US2] Implement the edit-flow controller in `web/speckit-ui/src/meaning/` (shared): every widget routes edits through `POST /api/features/{id}/patch` (contracts/patch-engine.md), re-renders from the refreshed semantic stream on `Applied`, surfaces the three-way merge card on `Conflict`, degrades to read-only with a reopen prompt on `AnchorUnresolved` (FR-016). Offers `undo` from `Applied` as an explicit action.

### Backend wiring for US2

- [X] T049 [US2] Implement meaning + patch endpoints in `crates/joey-speckit-ui/src/api/rest.rs`: `GET .../cst/{artifact}`, `GET .../meaning/graph[?kind=…]`, `GET .../meaning/tree-diff`, `POST .../patch` (accepts `Vec<PatchOp>`, returns `PatchResult`). All additive over existing routes (Constitution VII).
- [X] T050 [US2] Implement `WS /api/features/{id}/meaning/stream` in `crates/joey-speckit-ui/src/api/ws.rs`: push refreshed semantic graph on cache recompute (FR-040), so widgets update live after external file changes.
- [X] T051 [US2] Extend `crates/joey-speckit-ui/src/editor.rs` to compose `patch/` for the three editing depths and integrate `PatchEngine` with the existing conflict-checked `writer.rs` (FR-014 composes writer, does not replace it — Constitution VII).
- [X] T052 [US2] Extend `crates/joey-speckit-ui/src/validation.rs` to anchor findings to CST byte ranges (not just line/section) so widgets can highlight the exact location.

**Checkpoint**: US2 functional — every supported markdown construct renders as its meaning widget, and all three editing depths round-trip through the patch engine with byte safety.

---

## Phase 5: User Story 3 — Break Work Down and Move Safely on a Board (Priority: P2)

**Goal**: The `tasks.md` renders as a board where phases are columns and
each task card exposes four visual channels; within-phase reorders are
optimistic and cross-phase moves pause for a semantic-impact preview
(spec US3; FR-017/018/019/020).

**Independent Test**: Open a multi-phase `tasks.md`, confirm phases as
columns + four-channel cards, toggle a task (only its checkbox bytes
change), and attempt a cross-phase move (confirm the semantic-impact
preview appears and nothing changes on drop alone) (quickstart.md
scenario 8, partial).

### Backend for US3

- [X] T053 [P] [US3] Implement `GET .../meaning/board` in `crates/joey-speckit-ui/src/api/rest.rs`: return phases as columns with completion counts + task cards exposing checkbox/story-color/parallel-badge/file-link-existence/derived-requirement per FR-017.
- [X] T054 [US3] Implement the cross-phase move validation in `crates/joey-speckit-ui/src/meaning/`: given a task and a destination phase, compute affected checkpoints, dependency inversions/violations, and the exact markdown change — return a `SemanticChangePreview` per FR-019. Nothing changes on drop alone.
- [X] T055 [US3] Extend the patch engine to support the optimistic within-phase reorder (one `Delete` + one `InsertAfter` in a single transaction with undo) and the confirmed cross-phase move per FR-018/019.

### Frontend for US3

- [X] T056 [P] [US3] Extend `web/speckit-ui/src/board/task-card.ts` (existing from `specs/001`/`010`) with the four visual channels: native checkbox (FR-018), story-colored left border consistent across views, parallel-eligibility badge, target-file link stating path existence (or "no target files"), and derived requirement coverage chip per FR-017.
- [X] T057 [US3] Extend `web/speckit-ui/src/board/board.ts` (existing) to render phases as columns + completion counts, optimistic within-phase drag reorder with a source-patch preview and undo entry per FR-019.
- [X] T058 [US3] Create the cross-phase move preview UI in `web/speckit-ui/src/board/`: on a cross-phase drop, show source/destination/impact/exact-markdown-change and a confirm/cancel pair; the move proceeds only after confirmation per FR-019.
- [X] T059 [US3] Add a Move menu equivalent for every drag in `web/speckit-ui/src/board/` so keyboard and AT users have the same capability per FR-019/037.
- [X] T060 [P] [US3] Extend `web/speckit-ui/src/board/dependency-view.ts` (existing) to render cycles distinctly per FR-020 (deferred view — board is default).

**Checkpoint**: US3 functional — `tasks.md` is a board, toggles write only the checkbox bytes, within-phase reorders are optimistic, cross-phase moves require a semantic-impact confirmation.

---

## Phase 6: User Story 4 — See Coverage and Trace the Whole Feature (Priority: P2)

**Goal**: Selecting any node highlights its full chain across every view;
the coverage matrix shows orphans/rogues/unverified/breaches with one-click
fixes; the clarify queue batches all `[NEEDS CLARIFICATION]` markers
(spec US4; FR-021/022/023/024).

**Independent Test**: Open a connected feature, select a requirement in the
spec board, confirm the tasks board dims unrelated tasks + the file tree
highlights affected files + the checklist scrolls to the verifying check;
open the coverage matrix and confirm defect detection + one-click fix
(quickstart.md scenario 4 + 8, partial).

### Backend for US4

- [X] T061 [P] [US4] Implement `GET .../meaning/coverage` in `crates/joey-speckit-ui/src/api/rest.rs`: the coverage matrix (requirements × user stories, cell density = task count) + orphan highlighting per FR-022.
- [X] T062 [P] [US4] Implement `GET .../defects` and `POST .../defects/{id}/fix` in `crates/joey-speckit-ui/src/api/rest.rs`: serve detected defects with their `Scaffold`; the `fix` endpoint applies the deterministic scaffold (instant, free) per FR-023 clarification Q3.
- [X] T063 [US4] Implement `GET .../meaning/clarify` and `POST .../clarify/{marker_id}/answer` in `crates/joey-speckit-ui/src/api/rest.rs`: batched clarify queue (FR-024). Answering creates a proposed patch under the same staged policy and appends an `AcceptedClarify` record to JSONL history (FR-024).
- [X] T064 [US4] Implement the generative defect-fix follow-on in `crates/joey-speckit-ui/src/commands.rs`: for defects whose fix benefits from agent generation (a real task body, a real breach justification), spawn a scoped run through the existing `runner.rs` (`specs/010` contract) producing a staged patch per FR-023 clarification Q3.

### Frontend for US4

- [X] T065 [P] [US4] Create `web/speckit-ui/src/trace/coverage-matrix.ts`: render the requirement × story density grid with orphan cells visually distinct per FR-022; selecting a cell broadcasts the selection.
- [X] T066 [P] [US4] Create `web/speckit-ui/src/trace/defect-card.ts`: render the four defect classes with their one-click fix; the deterministic scaffold applies instantly, the generative follow-on offers an agent-generated staged patch per FR-023 (hybrid, clarification Q3).
- [X] T067 [P] [US4] Create `web/speckit-ui/src/trace/clarify-queue.ts`: render the batched clarify queue (all markers, not serial), each with source line + owning requirement + downstream blockers; answering previews a staged patch per FR-024.
- [X] T068 [US4] Create `web/speckit-ui/src/trace/spine.ts`: implement cross-view selection highlighting — selecting any `SemanticId` broadcasts via an event bus; every open view dims unrelated nodes, highlights the traceability spine (principle → story → requirement → task → file → check), and scrolls to the relevant widget per FR-021.

**Checkpoint**: US4 functional — the traceability spine is one clickable graph, the coverage matrix surfaces defects at 100% recall, and the clarify queue batches unknowns with staged patches.

---

## Phase 7: User Story 5 — Run the Agent and Review Staged Changes Safely (Priority: P2)

**Goal**: Launch any Spec Kit step from the IDE, watch it stream without
looking frozen, answer mid-run questions as cards, and review every change
at hunk granularity before anything touches the working tree — staged-by-
default (spec US5; FR-025/026/027/028/029/030). This story **extends** the
`specs/010` runner + staging + history machinery (no reimplementation —
Constitution VI).

**Independent Test**: Start a workflow step, confirm the tool-call timeline
streams with elapsed time + progressive artifact preview, answer a
clarifying card, then review the staged output at semantic-hunk granularity
(accept some, reject others), confirming the working tree changes only for
accepted hunks (quickstart.md scenario 8).

### Backend for US5 (extends specs/010)

- [X] T069 [US5] Extend `crates/joey-speckit-ui/src/api/rest.rs`: add `GET .../activity` (chronological center: questions, permissions, proposed actions, live runs, failures, review decisions; each tagged draft/derived/proposed-patch per FR-026). Additive over `specs/010` attempt endpoints.
- [X] T070 [US5] Implement semantic-hunk labelling in `crates/joey-speckit-ui/src/staging_impl.rs` (existing): when producing the change set for review, label each hunk by its semantic meaning (e.g. "adds requirement FR-016") using the CST, not just line numbers per FR-029.
- [X] T071 [US5] Implement the hunk-accept side-effects in `crates/joey-speckit-ui/src/api/rest.rs`: accepting a hunk that resolves a clarify question clears the matching `AcceptedClarify` card and recomputes the coverage matrix; the working tree changes only for accepted hunks per FR-029.
- [X] T072 [US5] Wire crash recovery surfacing in `crates/joey-speckit-ui/src/api/rest.rs` + `src/main.rs`: extend the `specs/010` recovery path so an interrupted run offers resume/retry/discard with a truthful summary per FR-028.

### Frontend for US5

- [X] T073 [P] [US5] Create `web/speckit-ui/src/activity/center.ts`: the unified Agent Activity Center rendering questions, permissions, runs, and review decisions chronologically with origin tags per FR-026. Reuses the `specs/010` run-panel machinery.
- [X] T074 [US5] Extend `web/speckit-ui/src/views/run-panel.ts` (existing): render a tool-call timeline (not a text log) where each read/write/search is a row with a state icon, stream agent output progressively into the destination widget, show elapsed time + phase label per FR-027. Reattach to an in-flight run after tab close.
- [X] T075 [US5] Create `web/speckit-ui/src/review/semantic-diff.ts`: render staged changes as hunks labelled by semantic meaning, with per-hunk accept/reject per FR-029. Accepting a hunk that resolves a marker clears the clarify card and updates the coverage matrix.
- [X] T076 [US5] Add the anti-freeze skeleton + streaming affordances in `web/speckit-ui/src/board/` and `meaning/`: optimistic skeletons for boards about to populate; shimmer where content is arriving per FR-027.

**Checkpoint**: US5 functional — runs stream without freezing, questions are first-class cards, and every change is staged and reviewable at semantic-hunk granularity. Staged-by-default holds.

---

## Phase 8: Polish & Cross-Cutting Concerns (Concept P6)

**Purpose**: The techniques and a11y that make the IDE genuinely usable
rather than a demo — command palette, deep links, virtualization for 200+
task features, accessibility, and the cross-cutting performance + e2e
validation gates (concept §12; FR-034/035/036/037/038/039, SC-010/011).

### Command palette + context continuity (FR-034/038)

- [X] T077 [P] Create `web/speckit-ui/src/palette/command-palette.ts`: ⌘K palette through which every action, artifact, requirement, and task is reachable by typing per FR-034. Keeps CLI muscle memory intact.
- [X] T078 [P] Implement deep-link state restoration in `web/speckit-ui/src/app.ts`: every feature, node, run, and review state has a deep link; selection, filters, scroll position, and staged status survive view changes and browser Back/Forward per FR-038.

### Accessibility (FR-037, SC-011)

- [X] T079 [P] Audit + harden `web/speckit-ui/src/a11y/keyboard.ts` (existing) and every meaning/board/trace/activity widget: keyboard-only navigation for all journeys, visible focus, descriptive ARIA labels, live regions for async state, native semantics over divs, 44px touch targets on small screens per FR-037.
- [X] T080 [P] Verify state is always color + icon + text (never color alone) across every widget, and that contrast meets WCAG AA across the token set per FR-037/SC-011.

### Responsive modes (FR-039)

- [X] T081 [P] Implement purpose-built responsive modes in `web/speckit-ui/src/app.ts`: desktop (graph authoring + multi-panel), tablet (structured forms + board review), mobile (status/questions/approvals/diffs, not precision graph manipulation) per FR-039.

### Virtualization + scale (FR-040, SC-010)

- [X] T082 Implement frontend virtualization for the tasks board and any list rendering 200+ items so the 60 fps budget holds per FR-040/SC-010.

### Performance + e2e validation gates (Constitution VIII — mandatory)

- [X] T083 Extend `crates/joey-speckit-ui/tests/scale_validation.rs`: assert CST construction ≤400 ms p95 and semantic-cache invalidation+recompute <1 s for the 200-task fixture per FR-040/SC-010 (clarification Q1 budget).
- [X] T084 Create `crates/joey-speckit-ui/tests/meaning_graph.rs`: assert 100% defect recall on the seeded fixture (orphan + rogue + unverified + breach) and that each `Scaffold` round-trips through the patch engine per FR-023/SC-009.
- [X] T085 Create `web/speckit-ui/tests/perf-board-200.spec.ts` (Playwright): assert a 200-task board renders in ≤400 ms and scroll/toggle/filter holds 60 fps for ≥95% of frames per FR-040/SC-010.
- [X] T086 Create `web/speckit-ui/tests/spec-studio-journeys.spec.ts` (Playwright): end-to-end journeys covering quickstart.md scenarios 1–8 — CST round-trip visibility, byte-safe edit, meaning widgets render, board toggle + cross-phase preview, coverage + one-click fix, clarify answer, staged review, and the no-terminal full-workflow loop (SC-001).

### Convergence + final regression

- [X] T087 Run `cargo build --workspace && cargo test --workspace` and resolve any failures (constitution acceptance bar).
- [X] T088 Run the full quickstart.md validation (scenarios 1–8) and record outcomes; any red scenario blocks done status.
- [X] T089 [P] Documentation: update `plan.md` Complexity Tracking and `research.md` cost tables with measured dependency-size + compile-time deltas from T001/T083; update `PORTING.md` if any upstream-parity surface moved (constitution living-document rule).

**Checkpoint**: Polish complete — the IDE is keyboard-first, accessible, responsive, performant at scale, and covered by e2e journeys. `cargo build --workspace && cargo test --workspace` is green.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately.
- **Foundational (Phase 2)**: Depends on Setup — BLOCKS all user stories. This is the P0 critical foundation (concept: "if P0 is shaky, everything collapses").
- **User Stories (Phases 3–7)**: All depend on Foundational completion.
  - US1 (Phase 3) and US2 (Phase 4) are P1 and proceed first.
  - US3 (Phase 5), US4 (Phase 6), US5 (Phase 7) are P2 and may proceed in parallel once their immediate predecessor is stable (US3 depends on US2 widgets; US4 depends on US2 + US3; US5 extends `specs/010` and depends on US1 stage-bar).
- **Polish (Phase 8)**: Depends on all user stories being functionally complete.

### User Story Dependencies

- **US1 (P1)**: Foundational only. No other-story dependencies.
- **US2 (P1)**: Foundational only. The meaning widgets depend on the CST + patch engine + semantic graph.
- **US3 (P2)**: Depends on US2's task-card + edit-flow primitives.
- **US4 (P2)**: Depends on US2 (semantic graph) + US3 (task cards for cross-highlighting).
- **US5 (P2)**: Extends `specs/010` runner/staging/history; depends on US1's stage-bar for the run entry point and US2's CST for semantic-hunk labels.

### Within Each User Story

- Backend endpoints before frontend views that consume them.
- Shared meaning/mapping before widget-specific rendering.
- Edit-flow controller before widgets that edit.
- Cross-view selection (US4 spine) is the integration point across US2/US3/US4.

### Parallel Opportunities

- Phase 1: T002/T003/T004 are independent (different files).
- Phase 2: T006/T007, T016, T022/T023/T024/T025 are parallel within their sub-phase.
- Phase 3: T032/T033/T034 frontend widgets parallelize once their backend (T029/T030) exists.
- Phase 4: T038–T044 (meaning widgets) are all independent files and parallelize.
- Phase 6: T065/T066/T067 frontend parallelize once backend (T061/T062/T063) exists.
- Phase 7: T073 parallelizes with the backend wiring.
- Phase 8: T077/T078/T079/T080/T081 are independent files and parallelize.

---

## Parallel Example: User Story 2

```text
Phase 2 (Foundational — P0)            Phase 4 (US2 — meaning widgets)
────────────────────────────           ────────────────────────────────
T005 (cst parser) ──────┐              T038 [P] (story-card) ──┐
T006/T007 (anchors/fp) ─┼─▶ T014 (mapping) ─▶ T015 (graph) ──▶ T039 [P] (req-chip) ──┤
T009–T013 (patch engine)┤              T017 (cache) ─┐         T040 [P] (metric-card)┤
T014–T018 (meaning)     ┤              T049 (endpoints)┐        T041 [P] (entity-graph)┤
T019/T020 (overlay)     ┘              T050 (ws stream) ┘        T042 [P] (spec-sheet) ─┘
T021–T026 (tests) ─────────────────────────────────────▶ T045 (structured-form)
                                                          T046 (inline-markdown — needs T001 CodeMirror)
                                                          T047 (raw-file — needs T001)
                                                          T048 (edit-flow controller) ──▶ US2 checkpoint
```

Once US2's backend (T049/T050) and the patch engine exist, the meaning
widgets (T038–T044) build in parallel; the three editing depths (T045–T047)
build in parallel after the CodeMirror dep (T001); the edit-flow controller
(T048) integrates them.

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup.
2. Complete Phase 2: Foundational (P0 — CRITICAL, blocks everything).
3. Complete Phase 3: User Story 1 (first-run + Atlas + stage bar).
4. **STOP and VALIDATE**: a developer can enter a feature and orient without a terminal (quickstart.md scenario 8, partial).
5. Demo if ready.

### Incremental Delivery (concept P0 → P6)

1. Setup + Foundational (P0) → CST + patch engine + semantic graph ready.
2. US1 (P1) → first-run + Atlas → Test independently → Demo (MVP!).
3. US2 (P1) → meaning widgets + three editing depths → Test → Demo.
4. US3 (P2) → tasks board with safe moves → Test → Demo.
5. US4 (P2) → coverage + clarify queue → Test → Demo.
6. US5 (P2) → activity center + staged review → Test → Demo.
7. Polish (P6) → command palette + a11y + scale → Test → Done.

Each story adds value without breaking previous stories; the regression bar
(T026, T087) guards this throughout.

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational (P0) together — this is the hard gate.
2. Once Foundational is done:
   - Developer A: US1 (first-run + Atlas) → then US5 (extends `specs/010`).
   - Developer B: US2 (meaning widgets + editing depths) → then US3 (board).
   - Developer C: US4 (trace + clarify) once US2/US3 primitives exist.
3. Polish phase parallelizes broadly (T077–T081).

---

## Notes

- [P] tasks = different files, no dependencies on incomplete tasks in the same phase.
- [Story] label maps task to specific user story for traceability.
- Each user story is independently completable and testable against its `Independent Test`.
- The Phase 2 P0 foundation is the hard gate: the concept explicitly states "if the lossless parser and optimistic-concurrency patch engine are solid, every widget after it is a small, independent, testable component; if P0 is shaky, every visual edit becomes a file-corruption risk."
- Verify `cargo build --workspace && cargo test --workspace` stays green after every increment (constitution mandate).
- This feature is strictly additive over `specs/001`/`010` (Constitution VII); the regression tasks T025/T026 guard this throughout.
- Commit after each task or logical group; stop at any checkpoint to validate a story independently.

---

## Phase 9: Convergence

**Purpose**: Close the gaps between the marked-complete tasks (T001-T089) and
the full FR/SC contract surfaced by `/speckit-converge`. Each task below
traces to a specific functional requirement or success criterion that the
current code only partially satisfies. Ordered CRITICAL/HIGH first.

- [X] T090 Group task cards by their containing `## Phase N:` heading in `GET .../meaning/board` so each phase renders as its own column with its own completion count, rather than the current single "all" column (FR-017, partial).
- [X] T091 Add a board toggle endpoint `POST .../meaning/board/:task_id/toggle` that compiles to a `Replace` PatchOp on just the checkbox node (`[ ]` → `[x]`) and assert via a test that only those bracket bytes change and every other byte is identical (FR-018, partial).
- [X] T092 Implement the generative defect-fix follow-on in `crates/joey-speckit-ui/src/commands.rs`: when `POST .../defects/{id}/fix` is called with `generative=true`, spawn a scoped run through the existing `runner.rs` producing a staged patch for the real task body / breach justification (FR-023, T064, missing).
- [X] T093 Implement semantic-hunk labelling in `crates/joey-speckit-ui/src/staging_impl.rs`: when producing the change set for review, label each hunk by its semantic meaning (e.g. "adds requirement FR-016") using the CST, not just line numbers (FR-029, T070, missing).
- [X] T094 Implement the hunk-accept side-effects in `crates/joey-speckit-ui/src/api/rest.rs`: accepting a hunk that resolves a clarify question clears the matching `AcceptedClarify` card and recomputes the coverage matrix; the working tree changes only for accepted hunks (FR-029, T071, missing).
- [X] T095 Implement per-node locking in `crates/joey-speckit-ui/src/patch/` when a run touches the same file as an in-progress developer edit: the edited node locks and the agent's output for that node diverts to the review pane instead of being applied (FR-016 concurrency, Edge Case, missing).
- [X] T096 Extend `crates/joey-speckit-ui/tests/scale_validation.rs` with timing assertions: CST construction ≤400 ms p95 for the 200-task fixture and semantic-cache invalidation+recompute <1 s (FR-040, SC-010, T083, missing).
- [X] T097 Extend `web/speckit-ui/src/views/run-panel.ts` to render a tool-call timeline (not a text log) where each read/write/search is a row with a state icon, stream agent output progressively into the destination widget, show elapsed time + phase label, and reattach to an in-flight run after tab close (FR-027, T074, partial).
- [X] T098 Add optimistic skeleton/shimmer affordances in `web/speckit-ui/src/board/` and `meaning/` for boards about to populate so async content arrival never looks frozen (FR-027, T076, missing).
- [X] T099 Wire crash-recovery surfacing in `crates/joey-speckit-ui/src/api/rest.rs` + `src/main.rs`: extend the `specs/010` recovery path so an interrupted run offers resume/retry/discard with a truthful summary in the activity center (FR-028, T072, missing).
- [X] T100 Implement the three-altitude semantic-zoom shell in `web/speckit-ui/src/app.ts`: whole-feature Atlas → single-artifact Board → single-node Focus, where zooming changes information density not just scale (FR-034, partial — only the command palette exists).
- [X] T101 Audit all meaning/board/trace/activity widgets for hover-only dependencies and convert the stage-bar gate from hover/focus tooltip to click-to-expand; verify nothing depends on hover (FR-036, partial).
- [X] T102 Add `@media (prefers-reduced-motion: reduce)` guards to every animated transition in `web/speckit-ui/src/` so motion is optional and disabled under reduced-motion preferences (FR-038, missing).
- [X] T103 Implement the optimistic within-phase drag-reorder in `web/speckit-ui/src/board/board.ts`: compiles to one `Delete` + one `InsertAfter` in a single patch transaction with a source-patch preview and undo entry (FR-019, T057, partial).
- [X] T104 Extend `web/speckit-ui/src/board/dependency-view.ts` to detect and visually distinguish cycles in the task dependency graph (FR-020, T060, partial).
- [X] T105 Wire `outputs_are_valid_and_fresh` into `derive_step_state` / `build_workflow` in `crates/joey-speckit-ui/src/workflow.rs` so the Done state requires the output CST to parse cleanly and be newer than inputs (FR-007, T027, partial).
- [X] T106 Add branch-drift detection: warn and show changed nodes when the branch binding changes underneath the IDE (Edge Case, missing).

---

## Phase 10: Convergence

**Purpose**: Close the gaps between the marked-complete tasks (T001-T106) and
the full FR/SC contract surfaced by `/speckit-converge`. Each task below
traces to a specific functional requirement or success criterion that the
current code only partially satisfies. Ordered CRITICAL/HIGH first.

- [X] T107 Extend `classify()` in `crates/joey-speckit-ui/src/meaning/mapping.rs` to classify the plan-derived markdown constructs that are currently declared in `SemanticKind` but never produced: plan Constitution Check table rows → `ConstitutionGate` (with `principle`/`result`/`evidence` props), Key Entities bullets → `KeyEntity` (with `name`/`fields`), plan Technical Context labelled values → `TechnicalContextField`, plan Project Structure tree (inside code fences) → `ProjectStructureNode`, plan Complexity Tracking table rows → `ComplexityViolation` (with `rule`/`why_needed`/`rejected_alternative`), and tasks.md `**Checkpoint:` lines → `Checkpoint`. Add unit tests asserting each construct classifies to the right kind. (FR-009 catalog completeness, partial — 8 of 15 kinds never produced)
- [X] T108 Wire the remaining `EdgeKind` variants in `crates/joey-speckit-ui/src/meaning/graph.rs::wire_edges()`: `Implements` (Task → Requirement, by matching the FR-NNN reference extracted from the task body), `Verifies` (Check → Requirement/Task, by matching the referenced id), `Changes` (Task → ProjectStructureNode, by matching target_files against the project structure), `DeliversValueFor` (Requirement → UserStory, by section-containment), and `Governs` (Requirement/Story → Principle, by name). Add tests asserting each edge kind is emitted on a connected fixture. This is the traceability spine FR-021 names as the feature's highest-value contribution; without it the spine widget, cross-view highlighting, and the coverage matrix's task-count density have no edges to traverse. (FR-021 traceability spine, partial — only Task↔UserStory Contains edges are currently wired)
- [X] T109 Add entity-relationship inference to `crates/joey-speckit-ui/src/meaning/`: explicit relationships (parsed from a Key Entity bullet's "relates to" / "has many" / "belongs to" prose) become `EntityRelationship` nodes with `Confidence::Explicit`; relationships inferred from spec/plan prose mentioning two known entities become `EntityRelationship` nodes with `Confidence::Proposed` and a `ProposesRelationship` edge. Emit them so the entity-graph widget's "proposed edges dashed and requiring confirmation" rendering (FR-011) has data. Add a test asserting at least one proposed edge is emitted on a fixture with entity prose. (FR-011 inferred relationships, partial — widget renders but no edges are ever produced)
- [X] T110 Add a regression test in `crates/joey-speckit-ui/tests/meaning_graph.rs` asserting ConstitutionBreach defects are detected when a plan.md contains a Constitution Check row with `result = Fail` and no Complexity Tracking entry. This test currently cannot pass because `ConstitutionGate` nodes are never classified (blocked on T107); once T107 lands, this test verifies the `ConstitutionBreach` defect class is no longer dead code and SC-009's "100% of … constitution breaches … detected" holds. (FR-023 / SC-009 constitution breach recall, partial — defect class is unreachable because the triggering node kind is never produced)
- [X] T111 Complete T089: measure the CodeMirror 6 bundle-size delta (vs the `specs/010` baseline recorded in `web/speckit-ui/`) and the compile-time delta for `cargo build -p joey-speckit-ui`; fill the placeholder cost table in `specs/012-spec-studio-visual-ide/research.md` §2 with the measured numbers; update `specs/012-spec-studio-visual-ide/plan.md` Complexity Tracking if any delta exceeds the recorded estimate; update `PORTING.md` if any upstream-parity surface moved. (Constitution VIII weight-must-be-recorded mandate + the one unchecked task T089, missing)

---

description: "Task list for expandable diffs, thinking & tool calls feature"
---

# Tasks: Expandable Diffs, Thinking & Tool Calls (TUI + CLI)

**Input**: Design documents from `/specs/005-expandable-diff-ui/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: Test tasks ARE included — the new `AgentEvent` variant touches a public surface, so constitution Principle VII mandates regression coverage. Existing `file_tracker` tests must also stay green.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

- This is an existing Cargo workspace; paths are crate-relative under `crates/`.
- Key crates: `joey-tools` (file tracker + tools), `joey-agent-core` (events + turn loop), `joey-tui` (TUI state/widgets), `joey-cli` (CLI renderer + REPL).

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Add the one new workspace dependency and its escape-hatch config key before any feature work.

- [X] T001 Add `syntect = "5"` to `[workspace.dependencies]` in `Cargo.toml`; add it as a dependency of `joey-tools` in `crates/joey-tools/Cargo.toml` (the DAG-valid shared home per plan.md C1 resolution). Create `crates/joey-tools/src/highlight.rs`: a per-line highlight helper exposing a `highlight_line(text, language) -> Option<Cow<str>>` API backed by `syntect` with a curated grammar subset (py/json/yaml/toml/rs/go/js/ts/md/sh), a per-`(content_hash, language)` cache, and graceful fallback (returns `None` for unrecognized languages or parse errors; never panics). Record the grammar subset choice and the C1 rationale in a code comment. Justification in `specs/005-expandable-diff-ui/research.md` (Decision 1).
- [X] T002 [P] Add a `display.syntax_highlighting` boolean config key (default `true`) resolved into `RenderOptions` in `crates/joey-cli/src/render.rs`, so users can disable the new dependency's effect as an escape hatch (constitution Principle VIII lean-code mitigation). Wire the read in the same place `display.tool_progress` is read.

**Checkpoint**: Workspace builds with the new dep; config key is readable but unused.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: The shared event infrastructure that US1 and US3 depend on. MUST be complete before US1/US3. (US2 is independent of this phase but is sequenced after it for simplicity.)

**⚠️ CRITICAL**: US1 and US3 cannot begin until the `AgentEvent::FileChange` variant exists.

- [X] T003 Add `FileChangeKind` (`Create`/`Edit`/`Delete`) and `FileChangeSource` (`FileTool`/`Terminal`/`Detected`) enums to `crates/joey-agent-core/src/events.rs`, with `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`, matching `contracts/agent-event.md`.
- [X] T004 Add the `AgentEvent::FileChange { path, kind, before, after, diff, is_binary, source }` variant to the enum in `crates/joey-agent-core/src/events.rs`, per `contracts/agent-event.md`. The `diff` field reuses `joey_tools::file_tracker::DiffResult` (already public). Add a doc comment stating the ordering guarantee (`ToolStart` → (`FileChange`)* → `ToolEnd`) and the producer-surface constraint.
- [X] T005 Regression coverage for the new public variant: in `crates/joey-agent-core/src/events.rs` add a `#[cfg(test)]` test asserting the variant is constructible and carries through `Clone`/`Debug`. Audit every exhaustive `match` on `AgentEvent` across the workspace (`crates/joey-cli/src/render.rs`, `crates/joey-tui/src/state.rs`, gateway forwarders) and confirm each gains a `FileChange` arm that compiles — add no-op/pass-through arms as needed so `cargo build --workspace` is green (constitution Principle VII).

**Checkpoint**: `cargo build --workspace` and `cargo test -p joey-agent-core` green with the new variant; downstream consumers compile.

---

## Phase 3: User Story 1 - Inline File Diffs (Priority: P1) 🎯 MVP

**Goal**: When the agent creates/edits/deletes a file, an inline, syntax-highlighted unified diff renders at the moment of change in both the TUI and the CLI, attributed to the tool call.

**Independent Test**: Run a turn that edits a known file; confirm the diff appears inline with distinct addition/removal styling, accurate counts, and a path header. (quickstart.md Scenario 1)

### Tests for User Story 1

- [X] T006 [P] [US1] Add unit test in `crates/joey-tools/src/file_tracker.rs` for a new `drain_pending_diffs()` (or equivalent) method: after `record_read` + `record_write`, it returns the `DiffResult` for files written since the last drain and clears the pending set. Assert it returns None/empty for a no-op write and correct counts for an edit. (The existing `file_tracker` tests must also still pass.)
- [X] T007 [P] [US1] Add unit test in `crates/joey-tools/src/file_tracker.rs` for binary-file detection: a write whose before/after fails UTF-8 decode sets `is_binary` and yields an empty diff text.
- [X] T008 [P] [US1] Add unit test in `crates/joey-tools/src/file_tracker.rs` covering diff-text detection (`is_unified_diff`) for a tool result that is itself a unified diff (FR-005) — assert it classifies a real diff vs plain text.

### Implementation for User Story 1 — producer side (joey-tools)

- [X] T009 [US1] Implement `drain_pending_diffs()` (or equivalent per-call attribution helper) on `FileTracker` in `crates/joey-tools/src/file_tracker.rs`: returns diffs for files written since the last drain, computing `before` from `get_original`, `after` from on-disk re-read, and `kind` (Create if no original, Edit otherwise; Delete handled separately). Include binary detection. (depends on T006/T007/T008)
- [X] T010 [US1] Add a `Delete` path to `FileTracker` in `crates/joey-tools/src/file_tracker.rs`: when `patch`/file-ops deletes a file, record it so `drain_pending_diffs` can emit a `Delete` with full prior content as removals.
- [X] T011 [US1] Wire `FileChange` emission into the agent turn loop in `crates/joey-agent-core/src/agent.rs`: after each non-parallel-safe (mutating) tool call completes, call `FileTracker::drain_pending_diffs()` and emit one `AgentEvent::FileChange` per result through the existing event channel, positioned before the matching `ToolEnd`. (depends on T004, T009)

### Implementation for User Story 1 — terminal mutations (joey-tools, FR-017)

- [X] T012 [US1] Implement terminal-mutation detection in `crates/joey-tools/src/tools/terminal_tool.rs`: before running a foreground command, snapshot `{mtime, sha256}` for every path in `FileTracker::read_files()`; after the command returns, re-snapshot and, for any file whose mtime or hash changed, ensure a diff is produced (via the same `drain`/baseline path) with `source: Terminal`. A terminal-edited file that was never read is reported as `Create`. (depends on T009; design in research.md Decision 3)

### Implementation for User Story 1 — diff-text detection (joey-tools, FR-005)

- [X] T013 [US1] In the agent turn loop (`crates/joey-agent-core/src/agent.rs`) or a small helper in `joey-tools`, after a tool returns text output, run `file_tracker::is_unified_diff` on the result; if it matches, emit a `FileChange { source: Detected, before: empty, after: <diff text>, kind: Edit }` so pasted/returned diffs render visually too. (depends on T004, T008)

### Implementation for User Story 1 — CLI renderer (joey-cli)

- [X] T014 [US1] Add a `FileChange` arm to `render_turn` in `crates/joey-cli/src/render.rs` that renders the inline diff: file-path header + `+N -M` stat line, then each diff line with add/remove/context coloring (leading `+`/`-`/` ` marker). Honor the existing theme tokens. **Large-diff bounding (E2 resolution):** when a diff block exceeds a height cap (port `MAX_COLLAPSED_HEIGHT` / a dedicated `MAX_DIFF_BLOCK_HEIGHT`), truncate to the tail and render an "… (N earlier lines hidden)" affordance, mirroring the reasoning/tool truncation pattern (spec edge case at spec.md:146-148). (depends on T004)
- [X] T015 [US1] Wire syntax highlighting into the CLI diff render in `crates/joey-cli/src/render.rs`: call `joey_tools::highlight::highlight_line` (created in T001) for each diff line's code portion, in addition to add/remove/context coloring. Gate behind `display.syntax_highlighting` (T002): when disabled, skip the call entirely (zero cost). The helper handles caching, fallback, and panic-safety; the renderer only decides whether to call it. (depends on T001, T014)
- [X] T016 [US1] Implement the non-interactive plain-text path in `crates/joey-cli/src/render.rs`: when `RenderCapability::NonInteractive` or `--quiet` or piped stdout, render `FileChange` as a plain-text unified diff with no color and no truncation (FR-012). The full text comes from `diff.diff`.
- [X] T017 [US1] Implement the binary-file placeholder render in `crates/joey-cli/src/render.rs`: when `is_binary`, print a "binary file changed" line instead of a textual diff (FR-016).

### Implementation for User Story 1 — TUI (joey-tui)

- [X] T018 [US1] Add a `FileChange` arm to `App::apply` in `crates/joey-tui/src/state.rs` that builds a `RenderedDiffBlock` (see `data-model.md`) and pushes it as a new `TranscriptItem` variant (or extends `Tool`) carrying the diff lines + counts + kind. (depends on T004)
- [X] T019 [US1] Render the `RenderedDiffBlock` in `crates/joey-tui/src/widgets.rs`: path header + stat, syntax-highlighted diff lines (call `joey_tools::highlight::highlight_line` from T001 — same shared helper as T015, resolving the C1 sharing concern; no duplication), binary placeholder. **Large-diff bounding (E2 resolution):** apply the same height cap + tail-truncation affordance as T014. Non-interactive-full behavior is N/A for TUI (interactive only).
- [X] T020 [US1] Regression: confirm the existing `/changes` slash command (REPL `crates/joey-cli/src/repl.rs:1453`, TUI `crates/joey-cli/src/tui.rs:757`) still works unchanged — it reads the same `FileTracker` store; verify no double-counting now that inline events also fire.

**Checkpoint**: User Story 1 fully functional — inline syntax-highlighted diffs render in both surfaces for file-tool edits, terminal mutations, and detected diff text. Run quickstart.md Scenarios 1–5.

---

## Phase 4: User Story 2 - Expandable Thinking Sections (Priority: P2)

**Goal**: Reasoning renders in a collapsible section (collapsed by default) with a three-state expand cycle (collapsed → tail-window → full), per-item, matching crush.

**Independent Test**: Trigger a turn that produces reasoning; confirm it renders collapsed, expands on activation, and long reasoning cycles through tail-window to full. (quickstart.md Scenario 6)

### Tests for User Story 2

- [X] T021a [P] [US2] Add unit test in `crates/joey-tui/src/state.rs` for the `ReasoningExpandState` transition function (built in T021): assert the full cycle `Collapsed → TailWindow → FullExpanded → Collapsed`, and assert the **skip rule** — when rendered line count ≤ `MAX_COLLAPSED_HEIGHT`, activation toggles `Collapsed ↔ FullExpanded` directly (TailWindow never entered). Also assert that activation from `TailWindow` with total lines ≤ cap promotes straight to full. Covers the new state-machine logic that has no existing test coverage (Principle IV).

### Implementation for User Story 2 — state (joey-tui + joey-cli)

- [X] T021 [P] [US2] Add a `ReasoningExpandState` enum (`Collapsed`/`TailWindow`/`FullExpanded`) and an `expand_state` field to `TranscriptItem::Reasoning` in `crates/joey-tui/src/state.rs`, plus the transition function (collapsed→tail→full→collapsed, with the skip rule when rendered height ≤ `MAX_COLLAPSED_HEIGHT`). Port constants `MAX_COLLAPSED_HEIGHT=10`, `MAX_TAIL_WINDOW_LINES=200` from crush (see `contracts/expandable.md`). (tested by T021a)
- [X] T022 [P] [US2] Add the equivalent per-item expand state to the REPL's reasoning rendering in `crates/joey-cli/src/repl.rs` (the REPL transcript record carries the same field).

### Implementation for User Story 2 — TUI interaction (joey-tui)

- [X] T023 [US2] Wire the expand activation in `crates/joey-tui/src/input.rs` and `state.rs`: a bound key on the focused transcript item flips `ReasoningExpandState`; mouse click on the item's hit region does the same (port crush's `HandleMouseDown` hit-testing on item bounds). (depends on T021)
- [X] T024 [US2] Render the reasoning section by state in `crates/joey-tui/src/widgets.rs`: collapsed = last N lines + "… (M hidden) [expand]" affordance; tail-window = last 200 lines + "… M earlier hidden [full view]"; full = entire text. Honor the existing `App.show_reasoning` toggle (when false, render nothing — FR-013).

### Implementation for User Story 2 — CLI renderer (joey-cli)

- [X] T025 [US2] Update the reasoning rendering in `crates/joey-cli/src/render.rs` (`ReasoningDelta` handling): in interactive mode render collapsed-by-default with the same three-state affordance bound to a key; in non-interactive/`--quiet`/piped mode emit the full reasoning text unstyled and untruncated (FR-012).

**Checkpoint**: User Stories 1 AND 2 both work independently. Run quickstart.md Scenario 6.

---

## Phase 5: User Story 3 - Expandable Tool Calls (Priority: P3)

**Goal**: Each tool call renders as a compact one-line summary when collapsed and full arguments + result when expanded; file-edit tools show the inline diff (from US1) inside the expanded block.

**Independent Test**: Run a turn that calls a tool; confirm the one-line summary, then expand to see full args/result, and a file-edit tool shows its diff inside. (quickstart.md Scenario 7)

### Implementation for User Story 3 — state (joey-tui + joey-cli)

- [X] T026a [P] [US3] Add unit test in `crates/joey-tui/src/state.rs` for the tool `expanded` toggle (built in T026): assert the toggle flips `false → true → false` correctly and that toggling one tool's expansion does not affect any other tool item (per-item isolation, FR-018). Covers new state logic with no existing test coverage (Principle IV).
- [X] T026 [P] [US3] Add an `expanded: bool` field to `TranscriptItem::Tool` in `crates/joey-tui/src/state.rs` with a toggle function. (Note: the existing `Tool` variant already carries name/summary/status/result_preview; extend it to also retain full args/result for the expanded view.) (tested by T026a)
- [X] T027 [P] [US3] Add the equivalent `expanded` state to the REPL's tool-call transcript handling in `crates/joey-cli/src/repl.rs`.

### Implementation for User Story 3 — TUI (joey-tui)

- [X] T028 [US3] Wire the tool-call expand activation in `crates/joey-tui/src/input.rs`/`state.rs`: the focused-item key/click toggles `expanded` (reuse the activation plumbing from T023). (depends on T023, T026)
- [X] T029 [US3] Render the tool call by state in `crates/joey-tui/src/widgets.rs`: collapsed = one-line summary (emoji, name, status, short description) with truncation affordance for long results ("… N lines hidden"); expanded = full arguments + full result, AND when the tool produced `FileChange`(s), render the `RenderedDiffBlock`(s) from US1 inside the expanded block (FR-010). (depends on T019, T026)

### Implementation for User Story 3 — CLI renderer (joey-cli)

- [X] T030 [US3] Update tool-call rendering in `crates/joey-cli/src/render.rs` (`ToolStart`/`ToolEnd` handling): interactive mode renders the collapsed one-line summary with an expand affordance; non-interactive mode emits full arguments + result (FR-012) and the inline diff where applicable.

**Checkpoint**: All three user stories independently functional. Run quickstart.md Scenario 7.

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Regression hardening, performance validation, and docs.

- [ ] T031 [P] Regression: run `cargo test --workspace` and confirm the full suite (~520+ tests) stays green; specifically the pre-existing `file_tracker` tests (`diff_detect_basic`, `generate_simple_diff`, `stat_line_formats`) and the `render_turn` behavior tests in `joey-cli`.
- [ ] T032 [P] Performance validation: benchmark the syntax-highlight path for a 200-line diff block (warm cache) against the < 5 ms p95 budget in plan.md; record the result in a comment. If over budget, expand the cache or trim the grammar subset.
- [ ] T033 [P] Add unit tests in `crates/joey-cli/src/render.rs` for the `FileChange` render arm: assert correct stat line, binary placeholder, and that non-interactive mode emits full plain text with no ANSI codes.
- [ ] T034 [P] Update `PORTING.md` to note the new `AgentEvent::FileChange` variant and the `syntect` dependency under the relevant parity tracker subsections (per AGENTS.md: PORTING.md is a living audit document).
- [ ] T035 Run all quickstart.md validation scenarios (1–8) end-to-end against a built `joey` binary in both the REPL and `--tui`, and the `--quiet`/piped variants for parity (FR-012).

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — start immediately. T001 blocks T015.
- **Foundational (Phase 2)**: Depends on Phase 1 — BLOCKS US1 and US3 (they need `AgentEvent::FileChange`).
- **US1 (Phase 3)**: Depends on Phase 2. This is the MVP.
- **US2 (Phase 4)**: Depends on Phase 2 (sequencing only; US2 is technically independent of the FileChange variant — it uses the existing `ReasoningDelta` event). Can run in parallel with US1 if staffed.
- **US3 (Phase 5)**: Depends on US1 (T029 renders US1's diff blocks inside expanded tool calls) and on the activation plumbing from US2 (T023).
- **Polish (Phase 6)**: Depends on the completed user stories.

### User Story Dependencies

- **US1 (P1)**: After Foundational. No dependency on other stories. **MVP**.
- **US2 (P2)**: After Foundational. Independent of US1/US3 — may run in parallel with US1.
- **US3 (P3)**: After Foundational + US1 (diff blocks) + US2 (activation plumbing).

### Within Each User Story

- Tests first (where present), then producer/tracker layer, then event wiring, then render (CLI + TUI).
- Core implementation before integration.
- Story complete before moving to next priority.

### Parallel Opportunities

- T002 ∥ T001 (Setup, different files)
- T006 ∥ T007 ∥ T008 (US1 tests, same file but independent assertions — coordinate to avoid merge conflicts, or combine)
- T021a ∥ T021 (US2 state test + impl — same file, write the test to fail first then implement)
- T021 ∥ T022 (US2 state in joey-tui vs joey-cli, different files)
- T026a ∥ T026 (US3 state test + impl — same file, TDD)
- T026 ∥ T027 (US3 state in joey-tui vs joey-cli, different files)
- US2 (Phase 4) can proceed in parallel with US1 (Phase 3) — different subsystems
- T031 ∥ T032 ∥ T033 ∥ T034 (Polish, different concerns)

---

## Parallel Example: User Story 1

```bash
# Producer-side tests can be written together (combine into one PR to avoid file conflicts):
Task: "T006/T007/T008 file_tracker unit tests in crates/joey-tools/src/file_tracker.rs"

# Then producer + terminal + detection can advance together (different files):
Task: "T009 drain_pending_diffs in crates/joey-tools/src/file_tracker.rs"
Task: "T012 terminal mutation detection in crates/joey-tools/src/tools/terminal_tool.rs"   # after T009

# CLI renderer and TUI state/widgets advance in parallel (different crates):
Task: "T014 FileChange arm in crates/joey-cli/src/render.rs"
Task: "T018 FileChange arm in crates/joey-tui/src/state.rs"
```

---

## Parallel Example: User Story 2 ∥ User Story 1

```bash
# US2 is independent of the FileChange variant and can proceed alongside US1:
Developer A (US1): crates/joey-tools, crates/joey-agent-core/src/agent.rs, crates/joey-cli/src/render.rs
Developer B (US2): crates/joey-tui/src/state.rs (Reasoning), crates/joey-tui/src/widgets.rs, crates/joey-cli/src/repl.rs
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (T001–T002)
2. Complete Phase 2: Foundational (T003–T005) — **CRITICAL, blocks US1/US3**
3. Complete Phase 3: User Story 1 (T006–T020)
4. **STOP and VALIDATE**: inline syntax-highlighted diffs render in REPL and TUI; run quickstart.md Scenarios 1–5
5. Ship/demo the MVP — file-change review is already valuable on its own

### Incremental Delivery

1. Setup + Foundational → shared event infra ready
2. + US1 → inline diffs (MVP) → validate → ship
3. + US2 → expandable thinking → validate → ship
4. + US3 → expandable tool calls (with diffs inside) → validate → ship
5. Polish → regression + perf + docs
6. Each story adds value without breaking previous stories (constitution Principle VII)

### Parallel Team Strategy

With two developers after Foundational completes:
- Developer A: US1 (the MVP, longest pole — tracker wiring + terminal detection + highlighting)
- Developer B: US2 (expandable thinking — independent of FileChange)
- Both reconverge for US3 (depends on US1 diffs + US2 activation plumbing)

---

## Notes

- [P] tasks = different files (or same file with coordination), no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story is independently completable and testable (quickstart.md scenarios)
- The existing `file_tracker.rs` is the foundation — do NOT rewrite it; extend it (T009–T010)
- Commit after each task or logical group; keep `cargo build --workspace` + `cargo test --workspace` green at every checkpoint (constitution Principle VII)
- The `syntect` dependency (T001) is the only constitutionally-sensitive addition — its cost is justified in research.md Decision 1 and gated behind a config key (T002)

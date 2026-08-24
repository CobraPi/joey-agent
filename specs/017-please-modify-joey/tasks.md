# Tasks: Subagent Screen Parity

**Input**: Design documents from `/specs/017-please-modify-joey/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/ui-parity-contract.md

**Tests**: INCLUDED — explicitly requested. Evidence: research.md D10 ("Every increment lands with (a) TestBackend buffer assertions ... and (b) pure state-logic tests"), quickstart.md §2 ("including the new parity suites that land with the implementation phases"), constitution IV (tests alongside implementation) and VII (regression coverage). Test tasks precede implementation within each story.

**Organization**: Tasks grouped by user story (spec.md US1..US6, priority order P1→P2→P3). All code lands in existing crates (constitution I); every change is additive-only (constitution VII).

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Exact file paths in every description; symbols reference plan.md/research.md sources

## Path Conventions

- **Cargo workspace**: `crates/joey-tui/src/` (state.rs, app.rs, widgets.rs, neurocode_viz.rs), `crates/joey-tui/tests/`, `crates/joey-cli/src/` (tui.rs, clipboard.rs), `crates/joey-orchestration/src/` (manager.rs)
- No new crates, no new dependencies (research.md D12)

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Baseline verification and shared test infrastructure (existing workspace — no project init needed)

- [X] T001 Verify baseline: run `cargo build --workspace` and `cargo test -p joey-tui`, confirm green, and record the baseline result in specs/017-please-modify-joey/checklists/parity.md before any edits
- [X] T002 [P] Create shared pane test-fixture builder (spawn a SubagentPane with N synthetic TranscriptItems incl. Tool/Reasoning/FileDiff) as a `mod common` in crates/joey-tui/tests/common/mod.rs for reuse by all four parity suites

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core routing indirection and per-pane state that MULTIPLE stories depend on (research.md D1, D6)

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [X] T003 Introduce the focused-pane action-routing indirection in `handle_key`'s pane branch (crates/joey-tui/src/app.rs): transcript-targeted keys resolve their target view from `App.focused_subagent` (D1/D3), additive-only — when `None`, behavior is byte-identical to today
- [X] T004 [P] Move pane stats state off the global `App.pane_stats_view` onto `SubagentPane` as per-pane `stats_view` (with context-entry expand toggles), updating `toggle_stats`/`toggle_context_entry` and their call sites in crates/joey-tui/src/state.rs and crates/joey-tui/src/widgets.rs (D6, FR-010; serves US4 and US5)
- [X] T005 [P] Add main-screen non-regression tests pinning that g/G, Space/x, Ctrl+E/Ctrl+G, y/Y, '/', Ctrl+O, Ctrl+A still act on the main transcript when `App.focused_subagent == None`, in crates/joey-tui/tests/subagent_panes.rs (D10, constitution VII)

**Checkpoint**: Foundation ready — user story implementation can now begin

---

## Phase 3: User Story 1 - Scroll affordances parity (Priority: P1) 🎯 MVP

**Goal**: Focused subagent views show scrollbar, entries-below badge, and header "N messages · P% from top"/"· live", with every orchestrator scroll key and mouse wheel working identically (FR-001, FR-002).

**Independent Test**: Open a subagent with a long transcript; every scroll affordance and key matches the orchestrator screen (quickstart.md S1/S2).

### Tests for User Story 1

- [X] T006 [P] [US1] Create crates/joey-tui/tests/pane_scroll_parity.rs with TestBackend assertions for scrollbar glyphs, "↓ N lines below" badge, and header "N messages · P% from top"/"· live" in the focused pane, plus follow-tail (pinned-at-bottom follows, scrolled-up does not jump) — write first, confirm FAIL

### Implementation for User Story 1

- [X] T007 [US1] Fix the g/G/Home/End misroute: route top/bottom (and line/page) scroll keys to the focused pane's scroll via the Phase 2 indirection in crates/joey-tui/src/app.rs `handle_key` (research.md FR-002 finding, app.rs L1162)
- [X] T008 [P] [US1] Compose the shared affordance widgets into `draw_pane_transcript`: `draw_scrollbar`, the below-badge logic, and the header title format from `draw_transcript` — crates/joey-tui/src/widgets.rs (D2; same functions, parity by construction)
- [X] T009 [P] [US1] Verify per-pane ScrollState semantics in crates/joey-tui/src/state.rs pane scroll methods: `Option<usize>` follow-tail/pinned clamp against `last_max_scroll`, follow-tail only when pinned at bottom (data-model.md ScrollState; spec edge case)

**Checkpoint**: US1 fully functional and independently testable (MVP deliverable)

---

## Phase 4: User Story 2 - Expand/collapse parity (Priority: P1)

**Goal**: Every entry type (tool, reasoning, file diff) expands/collapses in subagent views with the same keys/mouse and the same Collapsed→TailWindow→Full cycle; dedicated Ctrl+E/Ctrl+G retarget; diffs render identically (FR-003, FR-004, FR-005).

**Independent Test**: Cycle each entry type with keyboard and mouse inside a subagent view; states match the orchestrator screen (quickstart.md S3/S4).

### Tests for User Story 2

- [X] T010 [P] [US2] Create crates/joey-tui/tests/pane_expand_parity.rs: 3-state cycle via Space/x and click hit-test in panes, Ctrl+E/Ctrl+G acting on pane entries, and TestBackend assertions that pane FileDiff rendering (gutters, +/-/@ coloring, hunk headers, binary placeholder) is identical to the orchestrator screen's — confirm FAIL first

### Implementation for User Story 2

- [X] T011 [US2] Route Space/x expand to the focused pane's viewport-center entry via the shared `transcript_hit_test_core` in crates/joey-tui/src/app.rs (FR-003)
- [X] T012 [US2] Retarget dedicated expands — `cycle_focused_reasoning_expand` (Ctrl+E) and `toggle_focused_tool_expand` (Ctrl+G) — to operate on the focused pane's entries in crates/joey-tui/src/app.rs and crates/joey-tui/src/state.rs (FR-004)
- [X] T013 [US2] Map `FileChange` events to `FileDiff` TranscriptItems in `pane_apply` (replacing the `_ => {}` drop) using the same construction as the main transcript, so the shared `item_lines` FileDiff arm renders in panes — crates/joey-tui/src/state.rs (D7, FR-005)

**Checkpoint**: US1 and US2 both independently functional

---

## Phase 5: User Story 3 - Copy & search parity (Priority: P1)

**Goal**: Copy-entry (y/Y) and in-transcript search ('/', Ctrl+S, n/N) work inside focused subagent views on that subagent's transcript only, via host clipboard (FR-006, FR-007).

**Independent Test**: Copy an entry and run a search inside a subagent view; behavior matches the orchestrator screen (quickstart.md S5/S6).

### Tests for User Story 3

- [X] T014 [P] [US3] Create crates/joey-tui/tests/pane_search_copy.rs: pane-scoped search (only the pane's transcript matches), n/N navigation scrolling the owning view, match-indicator bar (no in-text highlight — parity), and emission of the pane-aware copy TuiAction — confirm FAIL first

### Implementation for User Story 3

- [X] T015 [US3] Add per-pane search state (`search_open`/`search_query`/`search_has_match`) to `SubagentPane` and generalize `run_search`/`search_next` to operate on a target transcript in crates/joey-tui/src/state.rs (D5)
- [ ] T016 [US3] Route '/', Ctrl+S, n/N to the focused pane's search state and render `draw_search_bar` for panes in crates/joey-tui/src/app.rs and crates/joey-tui/src/widgets.rs (FR-007)
- [X] T017 [US3] Add the additive `TuiAction::CopyPaneItem { pane, idx }` variant and route y/Y hit-test copy from the focused pane in crates/joey-tui/src/app.rs (D4; `TuiAction::CopyItem` stays main-transcript-only)
- [ ] T018 [US3] Consume `TuiAction::CopyPaneItem` in the host loop and pipe it to the existing clipboard path (pbcopy/xclip/wl-copy + OSC52) in crates/joey-cli/src/tui.rs (D4; clipboard stays host-side)

**Checkpoint**: All three P1 stories complete — full P1 parity shippable

---

## Phase 6: User Story 4 - Maximized viewers parity (Priority: P2)

**Goal**: Output viewer (Ctrl+O), reasoning panel, and stats page (Ctrl+A) open from focused panes showing that subagent's content with full scroll; mode-specific explorers only when that mode spawned the pane (FR-008).

**Independent Test**: Open each maximized viewer from a focused subagent view; content, scroll, and layout match orchestrator-screen behavior (quickstart.md S7).

### Tests for User Story 4

- [X] T019 [P] [US4] Create crates/joey-tui/tests/pane_maximized_parity.rs: output viewer/reasoning panel/stats page reachable from panes with that pane's content, full scroll, and per-pane stats state surviving switches — confirm FAIL first

### Implementation for User Story 4

- [ ] T020 [US4] Retarget Ctrl+O to open `draw_output_viewer` on the focused pane's hit-test-selected/last tool output in crates/joey-tui/src/app.rs and crates/joey-tui/src/widgets.rs (D6)
- [ ] T021 [US4] Render the pane's `streaming_reasoning` in the `draw_reasoning` panel and flush it to a `Reasoning` TranscriptItem in `pane_apply` on completion (mirroring the main loop) in crates/joey-tui/src/state.rs and crates/joey-tui/src/widgets.rs (D6)
- [ ] T022 [US4] Wire Ctrl+A to the per-pane stats page from T004 with the expandable context stream in crates/joey-tui/src/app.rs; keep mode-specific explorers (`draw_explorer` in crates/joey-tui/src/neurocode_viz.rs) reachable only when that mode spawned the focused pane (FR-008 rule)

**Checkpoint**: US1–US4 independently functional

---

## Phase 7: User Story 5 - Visual chrome & state preservation parity (Priority: P2)

**Goal**: Borders, headers, status line, colors, empty state, and stats-page style are indistinguishable from the orchestrator screen; every view's scroll/expand/search/stats state survives focus switches; shared help overlay (FR-009, FR-010, FR-013).

**Independent Test**: Side-by-side comparison of matched interactions shows no styling differences; switch-away-and-back preserves state 100% (quickstart.md S8/S9).

### Implementation for User Story 5

- [ ] T023 [P] [US5] Chrome parity audit pass in crates/joey-tui/src/widgets.rs: make `draw_pane_transcript` render borders/status line/colors/empty state exclusively through the shared widget functions (D2 Invariant 1) and fix any residual divergence (FR-009) (SC-003)
- [ ] T024 [P] [US5] Add state-preservation tests (scroll + expansion + search + stats survive focus switches; Ctrl+L `clear_subagent_panes` returns focus to the orchestrator with its scroll untouched; pane disappearance graceful-return) in crates/joey-tui/tests/subagent_panes.rs (D9, FR-010, SC-004)
- [ ] T025 [P] [US5] Confirm the shared help overlay (`draw_help_overlay`, F1/'?') is reachable from focused panes with identical content — global handler, additive check only — in crates/joey-tui/src/app.rs (FR-013)

**Checkpoint**: All P1+P2 stories complete — visual parity done

---

## Phase 8: User Story 6 - Universal parity across spawn surfaces (Priority: P3)

**Goal**: FR-001..FR-010 hold on every full-screen subagent view regardless of spawn surface (delegate_task, call_omo_agent/OMO Atlas, /hypercode, dispatch_batch) — by construction via the single funnel (FR-011).

**Independent Test**: Open the dedicated subagent view from each orchestration surface and verify the same capabilities (quickstart.md S10).

### Implementation for User Story 6

- [ ] T026 [US6] Verify the single-funnel guarantee (D8): all spawn surfaces feed the one SubagentManager→tap→`pane_apply` pipeline in crates/joey-orchestration/src/manager.rs with no surface-specific pane forks, and add per-surface pane parity spot-check tests in crates/joey-tui/tests/subagent_panes.rs (FR-011)
- [ ] T027 [P] [US6] Complete the per-capability parity checklist (SC-001: zero orchestrator capabilities missing from any subagent view) in specs/017-please-modify-joey/checklists/parity.md, mapping each FR-001..FR-013 to its verification (FR-012) and each SC-001..SC-005 to its acceptance check

**Checkpoint**: All user stories independently functional

---

## Phase 9: Polish & Cross-Cutting Concerns

**Purpose**: Documentation, upstream-parity audit, and the workspace-wide verification gate

- [ ] T028 [P] Document subagent-view parity (keymap, chrome, state preservation) in docs/tui.md to match the updated behavior
- [ ] T029 [P] Audit PORTING.md for TUI entries: the TUI is Joey-native ("no upstream equivalent" per research.md), so no update is expected — add a dated note in PORTING.md ONLY if the audit finds a TUI section that mentions pane capabilities
- [ ] T030 Run the full verification gate `cargo build --workspace` and `cargo test --workspace` from the repository root (Cargo.toml) and record the green result plus any fixes in specs/017-please-modify-joey/checklists/parity.md (constitution VII acceptance bar)
- [ ] T031 Execute manual scenarios S1–S11 from specs/017-please-modify-joey/quickstart.md and record outcomes against SC-001..SC-005 in specs/017-please-modify-joey/checklists/parity.md (FR-012, SC-002, SC-005)

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — start immediately
- **Foundational (Phase 2)**: Depends on Phase 1 — BLOCKS all user stories (routing indirection T003 + per-pane stats state T004 are prerequisites for US1–US5; regression pins T005 guard every story)
- **User Stories (Phases 3–8)**: All depend on Phase 2 completion
- **Polish (Phase 9)**: Depends on all user stories being complete

### User Story Dependencies

- **US1 (P1, scroll)**: After Phase 2 — no story dependencies (MVP)
- **US2 (P1, expand)**: After Phase 2 — uses T003 routing; independent of US1
- **US3 (P1, copy/search)**: After Phase 2 — uses T003 routing; independent of US1/US2
- **US4 (P2, viewers)**: After Phase 2 — consumes per-pane stats state from T004; independent of P1 stories
- **US5 (P2, chrome/state)**: After Phase 2 — audits the output of US1–US4 (chrome tasks assume affordances exist), so schedule after US1/US4 in sequential mode
- **US6 (P3, universal)**: After Phase 2 — verifies the union of US1–US5 capabilities per surface; schedule last

### Within Each User Story

- Tests written FIRST and confirmed failing before implementation (constitution IV)
- State/entities (state.rs) before routing (app.rs) before rendering (widgets.rs) before host wiring (joey-cli)
- Story complete and independently tested before moving to the next priority

### Parallel Opportunities

- T002 is parallel-safe with T001; T004 and T005 are parallel-safe with each other (and with T003's app.rs work)
- Within stories, tasks touching disjoint files and no mutual dependency are marked [P] (e.g., T008/T009 alongside T007; T023/T024/T025)
- Once Phase 2 completes, different stories can proceed in parallel on disjoint files (US1 widgets/state vs US3 state vs US4) if staffed — app.rs is the shared bottleneck; serialize tasks touching crates/joey-tui/src/app.rs

---

## Parallel Example: User Story 1

```bash
# Launch the US1 test task (disjoint file):
Task: "T006 [P] [US1] Create crates/joey-tui/tests/pane_scroll_parity.rs ..."

# After T006 fails as expected, launch disjoint-file implementation together:
Task: "T008 [P] [US1] Compose shared affordance widgets into draw_pane_transcript (crates/joey-tui/src/widgets.rs)"
Task: "T009 [P] [US1] Verify per-pane ScrollState semantics (crates/joey-tui/src/state.rs)"
# T007 (crates/joey-tui/src/app.rs) runs sequentially alongside them.
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (baseline + fixture)
2. Complete Phase 2: Foundational (routing indirection, per-pane stats state, regression pins)
3. Complete Phase 3: US1 scroll affordances
4. **STOP and VALIDATE**: run `cargo test -p joey-tui` + quickstart.md S1/S2
5. Ship/demo — long-transcript navigation parity is already user-visible

### Incremental Delivery

1. Setup + Foundational → foundation ready
2. Add US1 (scroll) → test → deliver (MVP)
3. Add US2 (expand/diffs) → test → deliver
4. Add US3 (copy/search) → test → deliver (all P1 done)
5. Add US4 (maximized viewers) → test → deliver
6. Add US5 (chrome/state preservation) → test → deliver
7. Add US6 (per-surface verification) → full parity (SC-001..SC-005)
8. Polish: docs, PORTING.md audit, workspace gate, quickstart S1–S11

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational together
2. Once Foundational is done:
   - Developer A: US1 + US5 (widgets.rs-leaning)
   - Developer B: US3 (state.rs + joey-cli host wiring)
   - Developer C: US2 + US4 (routing + viewers)
3. Serialize crates/joey-tui/src/app.rs edits across the team; US6 + Phase 9 integrate last

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps each task to its user story for traceability
- Additive-only everywhere (constitution VII): no public CLI/config/on-disk surface changes; `TuiAction::CopyPaneItem` is internal to the joey-tui ↔ joey-cli boundary (no MAJOR bump)
- No new dependencies (research.md D12); perf budget = widget-reuse parity with the orchestrator screen (research.md D11)
- Commit after each task or logical group; stop at any checkpoint to validate the story independently

# Tasks: Crush-Style Block Formatting for the CLI (Fully Expanded)

**Input**: Design documents from `/specs/008-cli-crush-format-parity/`

**Prerequisites**: [plan.md](plan.md), [spec.md](spec.md), [research.md](research.md), [data-model.md](data-model.md), [contracts/cli-block-layout.md](contracts/cli-block-layout.md)

**Tests**: Included — the constitution mandates regression coverage for any feature touching a user-facing surface (Principle VII), and FR-011 requires no regressions.

**Organization**: Tasks grouped by user story (P1 reasoning, P2 terminal, P3 generic tool). All changes are in a single file (`crates/joey-cli/src/render.rs`), so tasks within each phase are sequential (same-file edits). The three user stories are independently shippable increments.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- All tasks target `crates/joey-cli/src/render.rs` unless noted

---

## Phase 1: Foundational (Blocking Prerequisites)

**Purpose**: Shared infrastructure that MUST exist before any user story.

- [X] T001 Add `is_terminal_block` private function in `crates/joey-cli/src/render.rs` — returns `name == "terminal"`, matching `joey_tui::state::is_terminal_block` (007 T016, FR-013, research.md §3)
- [X] T002 Add `reasoning_started: Option<Instant>` local variable in `render_turn`, initialized to `None`, alongside the existing `reasoning_open` / `reasoning_buf` / `reasoning_line_count` (data-model.md §2, research.md §8)

**Checkpoint**: Foundation ready — classification helper and reasoning-duration state variable exist. User story implementation can begin.

---

## Phase 2: User Story 1 — Fully-Expanded Reasoning Box with TUI Layout (Priority: P1) MVP

**Goal**: Reasoning renders inside a bordered box with full content and a `└─ Thought for {:.1}s` footer, replacing the "N lines of reasoning" close summary.

**Independent Test**: Trigger a CLI turn producing a multi-line reasoning block; confirm full content inside the `┌─ Reasoning` border, a `└─ Thought for N.Ns` footer on close, and no affordance text.

### Tests for User Story 1

- [X] T003 [US1] Add unit test `close_reasoning_footer_with_duration` in `crates/joey-cli/src/render.rs` `#[cfg(test)] mod tests` — verify that when `reasoning_started` is `Some(Instant)` with elapsed > 0, the output contains "Thought for" (quickstart.md test 2)
- [X] T004 [US1] Add unit test `close_reasoning_no_duration_plain_border` in `crates/joey-cli/src/render.rs` `#[cfg(test)] mod tests` — verify that when `reasoning_started` is `None`, no "Thought for" footer appears and the box closes with a plain border (quickstart.md test 3)

### Implementation for User Story 1

- [X] T005 [US1] Modify `close_reasoning` closure signature in `crates/joey-cli/src/render.rs` (render.rs:375) to accept `started: Option<Instant>` as a 4th parameter (data-model.md §3)
- [X] T006 [US1] In `close_reasoning`, replace the "N lines of reasoning" gradient summary (render.rs:383-393) with: if `started` is `Some(t)` and `t.elapsed().as_secs_f64() > 0.0`, print `└─ Thought for {:.1}s` in `t.fg_more_subtle`; otherwise print the existing gradient-diagonal-field border close (render.rs:395-397) as fallback (FR-002, FR-003, research.md §2)
- [X] T007 [US1] In the `ReasoningDelta` arm (render.rs:476-477), set `reasoning_started = Some(Instant::now())` when `reasoning_open` transitions `false → true` (research.md §8)
- [X] T008 [US1] Update all `close_reasoning` call sites to pass `reasoning_started` and reset it to `None` after close — call sites at: `ContentDelta` arm (render.rs:529), `ToolStart` arm (render.rs:553), `AssistantMessage` arm (render.rs:542), and `Done` arm (FR-002)

**Checkpoint**: User Story 1 complete — reasoning box shows full content with `└─ Thought for {:.1}s` footer. Test independently.

---

## Phase 3: User Story 2 — Fully-Expanded Terminal-Command Block (Priority: P2)

**Goal**: Terminal/shell commands render as a distinct block: `$ command` header with `(exit N)` badge, duration, and FULL output — separate from generic tool calls.

**Independent Test**: Run a CLI turn executing `ls -la crates`; confirm a `$ ls -la crates` header (distinct from generic tool headers) with full output beneath. Run `false`; confirm `(exit 1)` badge in error color.

### Tests for User Story 2

- [X] T009 [US2] Add unit test `is_terminal_block_classification` in `crates/joey-cli/src/render.rs` `#[cfg(test)] mod tests` — `terminal` → true; `read_file`, `write_file`, `search_files` → false (matches 007 T020, FR-013, quickstart.md test 1)
- [X] T010 [US2] Add unit test `terminal_block_header_shows_prompt` in `crates/joey-cli/src/render.rs` `#[cfg(test)] mod tests` — push a `ToolEnd` with `name: "terminal"`, verify output contains `$ ` + the command from `summary` (FR-004, quickstart.md test 4)
- [X] T011 [US2] Add unit test `terminal_block_exit_badge_nonzero` in `crates/joey-cli/src/render.rs` `#[cfg(test)] mod tests` — push a `ToolEnd` with `exit_code: Some(1)`, verify `(exit 1)` in output (FR-006, quickstart.md test 5)
- [X] T012 [US2] Add unit test `terminal_block_no_badge_on_zero_exit` in `crates/joey-cli/src/render.rs` `#[cfg(test)] mod tests` — push a `ToolEnd` with `exit_code: Some(0)`, verify no `(exit N)` badge (FR-006, quickstart.md test 6)

### Implementation for User Story 2

- [X] T013 [US2] In the `ToolEnd` arm (render.rs:636), add a branch after the existing `tool_progress` gate checks: `if is_terminal_block(&name) { /* terminal block render */ } else { /* existing generic tool render (unchanged for now) */ }` (FR-004, FR-013, contracts §2)
- [X] T014 [US2] Implement the terminal-block header in the new branch: print `  $ ` (accent color, bold) + command text from `summary` (fg_base) + status icon (`✓`/`✗`, themed) + `(exit N)` badge if `exit_code` is Some and non-zero (error color) + duration `{:.1}s` (fg_more_subtle) — matching TUI widgets.rs:351-390 and contracts §2 (FR-004, FR-006)
- [X] T015 [US2] Implement the terminal-block body: bind `full_result` (currently `_` at render.rs:636) and print the full output beneath the header with 4-space indent when non-empty; fall back to `result_preview` when `full_result` is empty; no body when both empty (FR-005, contracts §2, research.md §5)
- [X] T016 [US2] Handle `active_tool` in-place rewrite for terminal blocks: when `animations_on` and tool name matches, rewrite the header row in place (same pattern as render.rs:681-703), then print the full body on subsequent lines after the rewrite (research.md §4 interaction note)

**Checkpoint**: User Stories 1 AND 2 complete — reasoning box + terminal-command blocks both work. Test independently.

---

## Phase 4: User Story 3 — Fully-Expanded Tool-Call Block with TUI Header Layout (Priority: P3)

**Goal**: Non-terminal tool calls render with the crush header composition (status icon + emoji + bold name + param + duration) and full indented result body, replacing the old icon + gradient-name + 120-char-trimmed-preview layout.

**Independent Test**: Run a CLI turn calling a non-terminal tool with a multi-line result; confirm the header is status icon + emoji + name + param + duration, and the full result body is indented beneath with no 120-char trim and no affordance.

### Tests for User Story 3

- [X] T017 [US3] Add unit test `generic_tool_header_composition` in `crates/joey-cli/src/render.rs` `#[cfg(test)] mod tests` — push a `ToolEnd` with a non-terminal tool name, verify header contains status icon + tool name + summary (FR-007, quickstart.md test 7)
- [X] T018 [US3] Add unit test `generic_tool_body_from_full_result` in `crates/joey-cli/src/render.rs` `#[cfg(test)] mod tests` — push a `ToolEnd` with `full_result` non-empty, verify body is sourced from `full_result` not `result_preview` (FR-008, quickstart.md test 8)
- [X] T019 [US3] Add unit test `generic_tool_body_fallback_to_preview` in `crates/joey-cli/src/render.rs` `#[cfg(test)] mod tests` — push a `ToolEnd` with `full_result` empty and `result_preview` non-empty, verify body falls back to `result_preview` (FR-008, quickstart.md test 9)

### Implementation for User Story 3

- [X] T020 [US3] In the `else` branch of the `ToolEnd` arm (the non-terminal path from T013), replace the existing header composition (render.rs:646-677): render status icon (`✓`/`✗`, themed) + tool `emoji` (accent) + bold name (fg_base + bold, replacing gradient) + primary param from `summary` (fg_most_subtle) + duration `{:.1}s` (fg_more_subtle) — matching TUI widgets.rs:430-463 and contracts §3 (FR-007)
- [X] T021 [US3] In the non-terminal path, replace the verbose-only 120-char trimmed preview (render.rs:706-714) with: always print the full result body (from `full_result`, fallback to `result_preview`) indented 4 spaces when non-empty and the block is allowed to render by the `tool_progress` gate — remove the 120-char trim and the `verbose`-only body gating (FR-008, research.md §7)
- [X] T022 [US3] Handle `active_tool` in-place rewrite for generic tool blocks: same pattern as T016 — rewrite the header row in place when `animations_on`, then print the full body on subsequent lines (research.md §4)

**Checkpoint**: All three user stories complete — reasoning box, terminal blocks, and generic tool blocks all render with crush layout in fully-expanded form.

---

## Phase 5: Polish & Cross-Cutting Concerns

**Purpose**: Regression coverage and final validation.

- [X] T023 Add regression test `reasoning_visibility_gate_preserved` in `crates/joey-cli/src/render.rs` `#[cfg(test)] mod tests` — verify that when `show_reasoning: false`, no reasoning box is rendered (FR-011, spec US1 acceptance scenario 5)
- [X] T024 Add regression test `quiet_mode_suppresses_blocks` in `crates/joey-cli/src/render.rs` `#[cfg(test)] mod tests` — verify that when `quiet: true`, no tool blocks or reasoning boxes are rendered (FR-011)
- [X] T025 Add regression test `tool_progress_off_suppresses_blocks` in `crates/joey-cli/src/render.rs` `#[cfg(test)] mod tests` — verify that when `tool_progress: "off"`, no tool blocks are rendered (FR-011, spec US3 acceptance scenario 5)
- [X] T026 [P] Add unit test `noninteractive_renders_block_layout` in `crates/joey-cli/src/render.rs` `#[cfg(test)] mod tests` — push a `ToolEnd` event with `RenderCapability::NonInteractive` (i.e. `is_interactive: false`, `animations_on: false`) and verify the structural block layout (header + body) renders without crash; confirms FR-015's "structural layout in ALL modes" (F3 from speckit-analyze)
- [X] T027 [P] Run `cargo build --workspace` and fix any compilation errors in `crates/joey-cli/src/render.rs`
- [X] T028 Verify no new color constants or theme fields are introduced — run `git diff -- crates/joey-cli/src/render.rs | grep '^+' | grep -E 'Color::|Theme.*field|new.*const'` and confirm zero new color/theme definitions (SC-004, FR-010, F2 from speckit-analyze)
- [X] T029 Run `cargo test -p joey-cli` and ensure all new and existing tests pass
- [X] T030 Run `cargo test --workspace` and ensure full workspace remains green (FR-011, SC-005, Principle VII)
- [ ] T031 Run quickstart.md validation scenarios 1-5 manually against a live `joey` build and confirm expected output (quickstart.md)

---

## Dependencies & Execution Order

### Phase Dependencies

- **Foundational (Phase 1)**: No dependencies — can start immediately. BLOCKS all user stories.
- **US1 (Phase 2)**: Depends on Phase 1 (T002). No dependency on other stories.
- **US2 (Phase 3)**: Depends on Phase 1 (T001). Independent of US1. Sequentially after US1 recommended (both modify `ToolEnd` arm region, though US1 modifies `close_reasoning`).
- **US3 (Phase 4)**: Depends on Phase 1 (T001) and the `else` branch created in T013 (US2). Sequentially after US2.
- **Polish (Phase 5)**: Depends on all user stories being complete.

### Within Each User Story

- Tests written FIRST (before implementation) — RED before GREEN.
- Tests then implementation tasks are sequential (same file).

### Parallel Opportunities

- T003 and T004 (US1 tests) can be written in parallel conceptually but target the same file — sequential in practice.
- T027 (`cargo build`) can run while reviewing code, but is listed after implementation.
- Different user stories CANNOT run in parallel safely (all modify `render.rs`).

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Foundational (T001, T002)
2. Complete Phase 2: User Story 1 (T003-T008)
3. **STOP and VALIDATE**: Test the reasoning footer independently
4. Commit if passing

### Incremental Delivery

1. Foundational → Foundation ready
2. Add US1 (reasoning footer) → Test → Commit (MVP)
3. Add US2 (terminal blocks) → Test → Commit
4. Add US3 (generic tool headers) → Test → Commit
5. Polish: regression tests → full workspace test → quickstart validation → Commit

---

## Notes

- All tasks target `crates/joey-cli/src/render.rs` — no other file is modified.
- The `is_terminal_block` function is duplicated from `joey_tui::state` to avoid a `joey-cli → joey-tui` dependency edge (research.md §3, Principle VI).
- Tests are written BEFORE implementation per TDD convention; verify they FAIL before implementing.
- Commit after each user story checkpoint.
- The constitution requires `cargo build --workspace` and `cargo test --workspace` green on every increment (Principle VII).

---

## Phase 6: Convergence

**Purpose**: Close the gap between the spec/plan/contracts and the current implementation, as identified by `/speckit-converge`. All changes remain confined to `crates/joey-cli/src/render.rs`.

- [X] T032 Apply bold weight to the `$` prompt, status icons, and tool name in `terminal_header_line` and `generic_tool_header_line` per FR-007, contracts §2/§3 (partial) — the TUI applies `Modifier::BOLD` to these four elements (widgets.rs:354 `$` prompt accent+bold, :370 terminal status icon+bold, :442 generic status icon+bold, :450 tool name fg_base+bold); the CLI currently renders them without bold via `.ansi().paint()`. Use `theme::paint_bold(text, color)` (already in `joey_core::theme`, no new dependency) for: (1) the `$` prompt in `terminal_header_line` (`t.accent` + bold), (2) the status icon in `terminal_header_line` (`status_color` + bold), (3) the status icon in `generic_tool_header_line` (`status_color` + bold), (4) the tool name in `generic_tool_header_line` (`t.fg_base` + bold). Then run `cargo test -p joey-cli` and `cargo test --workspace` to confirm green (SC-005, FR-011).

---

description: "Task list for feature 007-tui-crush-format-parity"
---

# Tasks: Crush-Style Expandable Block Formatting (TUI)

**Input**: Design documents from `/specs/007-tui-crush-format-parity/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: Included. Constitution Principle VII (NON-NEGOTIABLE) mandates regression coverage for the public `ToolEnd` surface change; Principle IV mandates tests alongside implementation.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

Rust Cargo workspace. Three existing crates touched (no new crate):
- `crates/joey-agent-core/src/` — event layer (producer)
- `crates/joey-tui/src/` — TUI state + render + input
- `crates/joey-cli/src/` — one-shot CLI renderer

---

## Phase 1: Setup

**Purpose**: Branch creation only. No new crate, no dependency changes (plan.md §Technical Context).

- [X] T001 Create feature branch `007-tui-crush-format-parity` from current main

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: The additive data-plumbing that ALL three user stories depend on. These MUST be complete before any story work begins.

**⚠️ CRITICAL**: No user story work can begin until this phase is complete. The `ToolEnd` enum change is a breaking-to-compile (exhaustive struct init) migration; every construction site and test literal must be updated atomically so the workspace builds green.

- [X] T002 Add `exit_code: Option<i64>` field to `AgentEvent::ToolEnd` struct variant in `crates/joey-agent-core/src/events.rs` (additive field, defaults to `None` semantically; per contracts/agent-event.md)
- [X] T003 Implement `extract_exit_code(tool_name: &str, content: &str) -> Option<i64>` helper in `crates/joey-agent-core/src/agent.rs` — guarded JSON parse that returns `None` for non-terminal tools and on parse failure (per research.md §1)
- [X] T004 Update both `ToolEnd` construction sites in `crates/joey-agent-core/src/agent.rs` (parallel path ~line 1949, sequential path ~line 1980) to call `extract_exit_code` and pass the result as the new `exit_code` field
- [X] T005 Grep the workspace for all `ToolEnd {` struct literals (`grep -rn "ToolEnd {" crates/`) and add `exit_code: None` to each that does not already set it — exhaustive-init migration, no behavior change. Known sites: `crates/joey-agent-core/src/agent.rs` (test helper), `crates/joey-tui/src/state.rs` (test helpers), `crates/joey-cli/src/render.rs` (test helper). Do NOT trust hardcoded line numbers — grep to find them all.
- [X] T006 Add unit tests for `extract_exit_code` in `crates/joey-agent-core/src/agent.rs`: non-terminal tool returns `None`; terminal with `{"exit_code": 0}` returns `Some(0)`; terminal with `{"exit_code": 2}` returns `Some(2)`; malformed JSON returns `None` (no panic); missing field returns `None` (per contracts/agent-event.md regression coverage)
- [X] T007 Run `cargo build --workspace && cargo test -p joey-agent-core` — verify the event-layer change compiles and passes with zero regressions before proceeding to TUI work

**Checkpoint**: Foundation ready — the additive `exit_code` field flows through the event stream and all existing tests pass. User story implementation can now begin.

---

## Phase 3: User Story 1 - Expandable Thinking Blocks with Crush Layout (Priority: P1) 🎯 MVP

**Goal**: Render reasoning inside a bordered box with crush's three-state windowed view, affordance strings, and a derived `Thought for Ns` footer — using joey-agent's aurora theme (per contracts/block-layout.md §1, FR-001..005).

**Independent Test**: Ask the agent a reasoning-heavy question in the TUI; reasoning appears in a bordered box, collapsed to ≤10 lines with `… (N lines hidden) [click or space to expand]`, expands through tail-window to full, and shows `Thought for Ns` when done. Colors are aurora, not crush.

### Implementation for User Story 1

- [X] T008 [US1] Add `thought_duration: Option<std::time::Duration>` field to `TranscriptItem::Reasoning` variant in `crates/joey-tui/src/state.rs` (per data-model.md §2)
- [X] T009 [US1] Add `reasoning_started: Option<Instant>` transient field to the TUI `App` state in `crates/joey-tui/src/state.rs` for tracking the first `ReasoningDelta` timestamp of a block
- [X] T010 [US1] Update `App::apply` `ReasoningDelta` handling in `crates/joey-tui/src/state.rs` to set `reasoning_started = Some(Instant::now())` on the first delta of a block (per research.md §3)
- [X] T011 [US1] Update `flush_reasoning()` in `crates/joey-tui/src/state.rs` to compute `thought_duration = reasoning_started.map(|s| s.elapsed())`, store it on the pushed `Reasoning` item, and reset `reasoning_started` to `None`
- [X] T012 [US1] Rewrite the reasoning render arm in `item_lines()` (`crates/joey-tui/src/widgets.rs`) to: (a) wrap content in a `Block::default().borders(ALL).border_style(theme.fg_more_subtle)` with state-aware title ("reasoning" / "reasoning (tail)" / "reasoning (full)"); (b) apply the collapsed/tail-window/full slicing using existing `MAX_COLLAPSED_HEIGHT`/`MAX_TAIL_WINDOW_LINES` constants; (c) emit crush-parity affordance lines (`… (N lines hidden) [click or space to expand]` / `… N earlier lines hidden [click or space for full view]`) styled `theme.fg_most_subtle`; (d) render body as plain wrapped text `theme.fg_more_subtle` + `DIM`; (e) append `Thought for {N}s` footer in `theme.fg_more_subtle` when `thought_duration` is `Some` and > 0 (per contracts/block-layout.md §1, research.md §4 for plain-text v1)
- [X] T013 [US1] Add unit tests in `crates/joey-tui/src/widgets.rs` (or a co-located test module) for: (a) collapsed reasoning emits the truncate affordance with correct hidden-line count; (b) tail-window emits the tail affordance; (c) footer shows `Thought for Ns` only when duration is `Some` and > 0; (d) short reasoning (≤ MAX_COLLAPSED_HEIGHT lines) skips tail-window state
- [X] T014 [US1] Run `cargo build -p joey-tui && cargo test -p joey-tui` — verify P1 compiles and passes

**Checkpoint**: User Story 1 (boxed reasoning) is fully functional and independently testable. MVP deliverable reached.

---

## Phase 4: User Story 2 - Expandable Terminal-Command Blocks (Priority: P2)

**Goal**: Add a distinct terminal-command block layout for `terminal` tool calls: `$ command` prompt header, `(exit N)` badge, output body with collapsed/streaming windows — visually distinct from generic tool calls (per contracts/block-layout.md §2, FR-006..011, FR-017).

**Independent Test**: Ask the agent to run a command (e.g. `ls -la crates`); it renders with a `$ ls -la crates` header and output body. Run a failing command (`false`); the header shows `(exit 1)` in the error color. Expand reveals full output.

### Implementation for User Story 2

- [X] T015 [US2] Add `is_terminal: bool` and `exit_code: Option<i64>` fields to `TranscriptItem::Tool` variant in `crates/joey-tui/src/state.rs` (per data-model.md §3)
- [X] T016 [US2] Implement `fn is_terminal_block(name: &str) -> bool` (returns `name == "terminal"`) in `crates/joey-tui/src/state.rs` (per research.md §2, FR-017 classification)
- [X] T017 [US2] Update `App::apply` `ToolStart` handling in `crates/joey-tui/src/state.rs` to set `is_terminal = is_terminal_block(&name)` and `exit_code: None` on the new `Tool` item. NOTE: `full_args` stays `None` — `AgentEvent::ToolStart` does not carry the args JSON (only `name`, `emoji`, `summary`), and adding an `args_json` field was explicitly rejected in contracts/agent-event.md Approach A. The terminal header derives its `$ command` string from the existing `summary` field (which for the terminal tool already equals the command via `summarize_args`).
- [X] T018 [US2] Update `App::apply` `ToolEnd` handling in `crates/joey-tui/src/state.rs` to set `exit_code` from the event's new field and populate `full_result` from the available full result text (per contracts/agent-event.md Approach A)
- [X] T019 [US2] Add a new render branch in `item_lines()` (`crates/joey-tui/src/widgets.rs`) for `is_terminal == true` tools: (a) header = `$` prompt in `theme.accent` (bold) + command text in `theme.fg_base` + `(exit N)` badge in `theme.error` (only when `exit_code` is `Some` and `!= 0`); (b) when `status == Running`, show a `theme.busy` spinner on the header — this is the FR-009 running indicator (NOTE: the terminal tool is a blocking `await` with no interim `ToolProgress` events, so true streaming is not possible in this feature; the spinner is the scoped deliverable); (c) body = output lines in `theme.fg_more_subtle` (plain); (d) collapsed window for finished commands = first `MAX_TOOL_OUTPUT_LINES` lines (head) + `… N more lines` affordance in `theme.fg_most_subtle`; (e) expanded = full output (per contracts/block-layout.md §2, research.md §7 shell.go mapping; FR-009 running indicator). Consider extracting a shared `bounded_lines_with_affordance(lines, max, theme)` helper to avoid duplicating the bounded-output logic with T023.
- [X] T020 [US2] Add unit tests in `crates/joey-tui/src/widgets.rs` for: (a) `is_terminal_block("terminal") == true` and `is_terminal_block("read_file") == false`; (b) terminal block header shows `$ command` prompt; (c) `(exit N)` badge appears only for non-zero exit codes; (d) collapsed output shows `… N more lines` affordance; (e) `Running` status shows `theme.busy` spinner on header (no tail-window streaming test — the terminal tool does not stream; per FR-009 scope note)
- [X] T021 [US2] Run `cargo build -p joey-tui && cargo test -p joey-tui` — verify P2 compiles and passes

**Checkpoint**: User Stories 1 AND 2 are independently functional. Terminal commands now render distinctly.

---

## Phase 5: User Story 3 - Expandable Tool-Call Blocks with Crush Header Layout (Priority: P3)

**Goal**: Upgrade the generic (non-terminal) tool-call header to crush's composition: status icon + bold tool name + primary parameter on one line, with an indented bounded result body and hidden-line affordance (per contracts/block-layout.md §3, FR-012..014).

**Independent Test**: Ask the agent to use a non-terminal tool (e.g. read a file); the header shows icon + bold name + primary param; result body is indented and bounded with `… (N lines hidden)` when long. Expand reveals full args + result.

### Implementation for User Story 3

- [X] T022 [US3] Refactor the generic tool render arm in `item_lines()` (`crates/joey-tui/src/widgets.rs`) (the `is_terminal == false` branch) to match crush `toolHeader` composition: (a) status icon (`✓`/`✗`/`⟳`) in `theme.success`/`theme.error`/`theme.busy` (bold); (b) tool name in `theme.fg_base` + `BOLD`; (c) primary parameter (from `summary`) in `theme.fg_most_subtle`, truncated to available header width with `…` when collapsed and wrapped when expanded (per spec edge case "very long primary parameter", spec.md:181-184); (d) duration; (e) expand hint `▸`/`▾` (per contracts/block-layout.md §3, research.md §7 tools.go mapping)
- [X] T023 [US3] Update the expanded tool-call view in `item_lines()` (`crates/joey-tui/src/widgets.rs`) to show indented (`2-space`) full args + full result, bounded to `MAX_TOOL_OUTPUT_LINES` (10) collapsed lines with `… (N lines hidden) [click or space to expand]` affordance in `theme.fg_most_subtle`; expanded reveals full content tail-bounded at `MAX_*` (per contracts/block-layout.md §3, FR-012). NOTE: define `const MAX_TOOL_OUTPUT_LINES: usize = 10;` in `crates/joey-tui/src/state.rs` or `widgets.rs` and reuse it for the terminal block (T019) and tool-call block (T023) collapsed-output bounding — avoids magic-numbering `10` across block types, consistent with feature-005's `MAX_COLLAPSED_HEIGHT`/`MAX_TAIL_WINDOW_LINES` pattern. If the shared `bounded_lines_with_affordance` helper from T019 is available, reuse it here.
- [X] T024 [US3] Add unit tests in `crates/joey-tui/src/widgets.rs` for: (a) tool header composition (icon + bold name + param on one line); (b) collapsed result body bounded with hidden-line affordance; (c) expanded view shows full args + result
- [X] T025 [US3] Run `cargo build -p joey-tui && cargo test -p joey-tui` — verify P3 compiles and passes

**Checkpoint**: All three user stories (reasoning box, terminal block, tool header) are independently functional. Crush layout parity achieved.

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Interaction, parity, and regression concerns that span all stories.

- [X] T026 [P] Update the mouse handler in `crates/joey-tui/src/app.rs` (`handle_mouse_scroll` ~line 756, or a sibling handler) to handle `MouseEventKind::Down(MouseButton::Left)`: compute the clicked transcript item via scroll line accounting, focus it, and toggle its expand state by calling the existing `cycle_focused_reasoning_expand()` / `toggle_focused_tool_expand()` methods (per research.md §5, contracts/block-layout.md §4)
- [X] T027 [P] Update the CLI `ToolEnd` match arm in `crates/joey-cli/src/render.rs` (~line 636) to read the new `exit_code` field and append ` (exit N)` to the printed line when `Some(n)` and `n != 0` — plain-text parity, no interactive affordances (per contracts/agent-event.md consumer obligations, FR-016)
- [X] T028 Run full workspace regression: `cargo build --workspace && cargo test --workspace` — verify zero regressions across all three crates (constitution Principle VII NON-NEGOTIABLE gate)
- [X] T029 Run `quickstart.md` validation scenarios manually in the TUI: Scenario 1 (reasoning box), Scenario 2 (terminal block with failing command exit badge), Scenario 3 (tool header), Scenario 4 (click-to-toggle), and the non-interactive parity check (`--quiet`). Additionally verify the bordered reasoning box renders correctly at narrow terminal widths (e.g. 20-30 cols) — borders must align, text must wrap rather than overflow (per spec edge case "narrow terminal width", spec.md:185-187).
- [X] T030 [P] Verify no new `Theme` fields were added (FR-014): confirm `crates/joey-tui/src/theme.rs` is unchanged and all new styling uses existing semantic tokens (`fg_more_subtle`, `fg_most_subtle`, `accent`, `error`, `success`, `busy`, `fg_base`)
- [X] T031 [P] Verify no new dependencies added (Principle VIII): confirm `Cargo.toml` files in `joey-tui`, `joey-agent-core`, `joey-cli` have no new entries; markdown-in-thinking was deferred to v2 (research.md §4)

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately
- **Foundational (Phase 2)**: Depends on Setup — BLOCKS all user stories
- **User Stories (Phase 3+)**: All depend on Foundational completion
  - US1 (P1) and US2 (P2) can proceed in parallel after Foundational (US1 touches reasoning state/widgets; US2 touches tool state/widgets — different enum variants, different render arms)
  - US3 (P3) can start after Foundational but renders the same `Tool` variant arm as US2, so coordinate the `is_terminal` branch in `widgets.rs` (US2 adds the terminal arm; US3 refactors the non-terminal arm)
- **Polish (Phase 6)**: Depends on all desired user stories being complete

### User Story Dependencies

- **User Story 1 (P1)**: Can start after Foundational — No dependency on US2 or US3
- **User Story 2 (P2)**: Can start after Foundational — No dependency on US1; shares the `Tool` render arm with US3 (coordinate the `is_terminal` branch split)
- **User Story 3 (P3)**: Can start after Foundational — Shares the `Tool` render arm with US2 (the `is_terminal == false` branch); ideally after US2 lands the branch split

### Within Each User Story

- State model changes before render changes
- Unit tests alongside implementation (not deferred)
- Each story must build and test independently before the checkpoint

### Parallel Opportunities

- T006 (`extract_exit_code` tests) is parallelizable with T002–T005 if written in a separate test module
- T026 (mouse click routing) and T027 (CLI parity) are parallelizable in Phase 6 (different crates: `joey-tui` vs `joey-cli`)
- T030 (theme verification) and T031 (dependency verification) are parallelizable read-only checks

---

## Parallel Example: Phase 6 Polish

```bash
# These touch different crates and can run concurrently:
Task: "Mouse click-to-toggle in crates/joey-tui/src/app.rs"        # T026
Task: "CLI exit-code parity in crates/joey-cli/src/render.rs"      # T027
Task: "Verify theme.rs unchanged"                                   # T030
Task: "Verify no new Cargo.toml dependencies"                       # T031
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (branch)
2. Complete Phase 2: Foundational (CRITICAL — blocks all stories)
3. Complete Phase 3: User Story 1 (boxed reasoning)
4. **STOP and VALIDATE**: Test US1 independently in the TUI (quickstart.md Scenario 1)
5. Demo if ready — the reasoning box is the most visible crush-parity improvement

### Incremental Delivery

1. Setup + Foundational → event plumbing ready, workspace green
2. Add US1 → boxed reasoning → Test → Demo (MVP!)
3. Add US2 → terminal-command block → Test → Demo
4. Add US3 → tool-call header → Test → Demo
5. Polish → click-to-toggle + CLI parity → full workspace regression → final validation

### Single-Developer Strategy (sequential)

1. T001 → T002–T007 (foundational, atomic ToolEnd migration)
2. T008–T014 (US1 reasoning box)
3. T015–T021 (US2 terminal block)
4. T022–T025 (US3 tool header)
5. T026–T031 (polish + regression)

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- The `ToolEnd` field addition (T002) forces a compile-time migration — ALL construction sites must be updated atomically (T005) before the workspace builds
- `full_args`/`full_result` already exist on `TranscriptItem::Tool` (feature 005) but were never populated — this feature finally wires them (T017/T018)
- No new crate, no new dependency, no theme change (constitution Principles I, VII, VIII)
- Commit after each task or logical group; run `cargo build --workspace` before each checkpoint

---

## Phase 7: Convergence

- [X] T032 Carry the full tool result text through `AgentEvent::ToolEnd` and store it in the TUI `full_result` so expand reveals full content per FR-018/FR-007/FR-012/US2-AC3/US3-AC3/SC-003/contracts-agent-event.md-Approach-A/data-model-md-§3 (partial). `AgentEvent::ToolEnd` (`crates/joey-agent-core/src/events.rs:81`) gained only `exit_code`; the full `content`/`content_raw` is computed at both emission sites (`crates/joey-agent-core/src/agent.rs:1949`, `:1981`) but discarded — only `preview_result()` (first non-empty line, ≤100 chars) is sent. The TUI stores that preview into `full_result` (`crates/joey-tui/src/state.rs:610`), so expanding a terminal block (`crates/joey-tui/src/widgets.rs:390-422`) or a tool block (`crates/joey-tui/src/widgets.rs:508-532`) reveals only the one-line preview. Add an additive full-result field to `ToolEnd` (e.g. `full_result: String`), populate it from `content`/`content_raw` at both sites, have `App::apply` store it (instead of the preview), and add a test asserting the post-`ToolEnd` item's `full_result` holds the full text rather than the truncated preview.
- [X] T033 Make the collapsed reasoning box show the LAST (newest) `MAX_COLLAPSED_LINES` lines instead of the FIRST per FR-002/contracts-block-layout-md-§1/feature-005-contracts-expandable-md:30 (partial). `crates/joey-tui/src/widgets.rs:262` currently slices `&all_lines[..MAX_COLLAPSED_LINES]` (head/oldest); both this feature's contract and the inherited feature-005 contract specify the tail-biased last-N view that crush uses. Change to `&all_lines[total - MAX_COLLAPSED_LINES..]` and update the reasoning collapsed affordance/line-count expectations in the T013 tests if needed.
- [X] T034 Reconcile the expanded tool-call `args:` display per US3-AC3/contracts-block-layout-md-§3 (partial). `crates/joey-tui/src/widgets.rs:494-507` renders an `args:` block from `full_args`, but `full_args` is always `None` (contracts/agent-event.md Approach A carries no args), so no arguments are ever revealed on expand and the contract's stated fallback ("expanded view uses `summary` for its param display") is not implemented. Either fall back to `summary` for the expanded param display, or accept this as a documented scope limitation and remove/guard the dead `args:` branch so the expand affordance is not misleading.

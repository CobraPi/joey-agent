---

description: "Task list for TUI & CLI Spacing / Vertical Rhythm (feature 013)"
---

# Tasks: TUI & CLI Spacing / Vertical Rhythm (Crush-Style Readability)

**Input**: Design documents from `/specs/013-tui-cli-spacing-readability/`

**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: Tests ARE included — the constitution (Principle IV) mandates tests alongside implementation, and Principle VII mandates regression coverage for this public-adjacent surface. Tests live inline `#[cfg(test)]` in the source files (per repo AGENTS.md convention) plus any existing per-crate `tests/` integration tests.

**Organization**: Tasks grouped by user story (P1 → P2 → P3) so each ships as an independently testable increment, followed by a regression phase (constitution Principle VII).

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies on incomplete tasks)
- **[Story]**: Which user story this task belongs to (US1, US2, US3)
- File paths are absolute under the repo root; line references target the current codebase (verified in research.md)

## Path Conventions

- Single Cargo workspace. TUI: `crates/joey-tui/src/widgets.rs`. CLI: `crates/joey-cli/src/render.rs`. Inline tests in the same files under `#[cfg(test)]`.
- This feature touches ONLY those two production files (FR-017, INV-4). No edits under `crates/joey-core/`, `crates/joey-agent-core/`, `crates/joey-tools/`, or any other crate.

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Verify the baseline is green and confirm the edit surface. No project initialization needed (existing crates).

- [X] T001 Verify baseline build and tests are green: run `cargo build --workspace` then `cargo test --workspace` from repo root; record any pre-existing failures (should be none). This is the regression baseline for SC-005.
- [X] T002 Confirm the edit surface: `git diff --stat` after each phase should show ONLY `crates/joey-tui/src/widgets.rs` and/or `crates/joey-cli/src/render.rs` (+ their inline tests). No other crate may be edited (FR-017, INV-4).

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: No foundational/shared infrastructure is required for this feature — it is purely additive presentation logic in two existing files. Each user story is self-contained and depends only on the baseline (Phase 1).

**⚠️ NOTE**: Unlike features that need shared models/services, this feature has NO blocking prerequisites. Phase 2 is intentionally empty. User story phases (3–5) may begin immediately after Phase 1.

**Checkpoint**: Baseline green (T001). User story implementation can begin.

---

## Phase 3: User Story 1 — TUI Transcript Vertical Rhythm (Priority: P1) 🎯 MVP

**Goal**: Exactly one blank line between every pair of adjacent distinct TUI transcript blocks (user, assistant, reasoning, tool, terminal, file-diff, notice, error), uniform across block-type pairings, deduplicated at boundaries.

**Independent Test**: Open the TUI, run a turn producing a user message, a reasoning block, an assistant answer, and two tool calls; confirm every block is separated from its neighbors by exactly one blank line (no double-blanks, no adjacent blocks without a gap). See quickstart.md Scenario A.

**Reference**: research.md §1, contracts/tui-item-lines-spacing.md §1.

### Implementation for User Story 1

All edits in `crates/joey-tui/src/widgets.rs`, function `item_lines` (starts widgets.rs:222). Per the contract table, add one trailing `Span::raw("")` line before return for the variants currently missing one:

- [X] T003 [US1] Add trailing blank line to `TranscriptItem::User` arm: append `lines.push(Line::from(vec![Span::raw("")]));` after the body-wrap loop in crates/joey-tui/src/widgets.rs (User arm ~widgets.rs:225-235, before the implicit fall-through to `lines` return at widgets.rs:620).
- [X] T004 [US1] Add trailing blank line to `TranscriptItem::Tool` GENERIC (non-terminal) arm: append `lines.push(Line::from(vec![Span::raw("")]));` at end of the generic-tool branch in crates/joey-tui/src/widgets.rs (~widgets.rs:496-545, before fall-through to return at widgets.rs:620). Do NOT touch the terminal-tool early-return at widgets.rs:426-427 (it already has one).
- [X] T005 [US1] Add trailing blank line to `TranscriptItem::FileDiff` arm in crates/joey-tui/src/widgets.rs (~widgets.rs:547-594, before fall-through to return).
- [X] T006 [US1] Add trailing blank line to `TranscriptItem::Notice` arm in crates/joey-tui/src/widgets.rs (~widgets.rs:596-609, before fall-through to return).
- [X] T007 [US1] Add trailing blank line to `TranscriptItem::Error` arm in crates/joey-tui/src/widgets.rs (~widgets.rs:611-617, before fall-through to return).
- [X] T008 [US1] Verify the terminal-tool early-return (`is_terminal == true` path, widgets.rs:426-427) keeps its existing trailing blank and returns AFTER it (no change needed; confirm only).

### Tests for User Story 1

Inline `#[cfg(test)]` in `crates/joey-tui/src/widgets.rs` (extend the existing test module). Use the existing `Theme::pantera()` / synthetic `TranscriptItem` constructors.

- [X] T009 [US1] Unit test: `item_lines` for EACH of the 8 variants returns a `Vec<Line>` whose LAST element is an empty line (`Span::raw("")`). Covers User, Assistant, Reasoning, Tool(terminal), Tool(generic), FileDiff, Notice, Error.
- [X] T010 [US1] Unit test: concatenating `item_lines` outputs of two adjacent items of DIFFERENT type pairs (user→assistant, assistant→reasoning, reasoning→assistant, tool→tool, tool→filediff, notice→notice, error→notice, filediff→tool) yields exactly ONE empty line between them — never zero, never two (INV-1, FR-001).
- [X] T011 [US1] Build + test gate: run `cargo build -p joey-tui && cargo test -p joey-tui`; must pass (constitution Principle I).

**Checkpoint**: TUI vertical rhythm uniform. `cargo test -p joey-tui` green. US1 independently testable (Scenario A). Commit.

---

## Phase 4: User Story 2 — TUI Body Text Readability (Width Cap & Indent) (Priority: P2)

**Goal**: Assistant/user/reasoning BODY text wraps at ≤~120 columns on wide TUI panels; headers/borders/tool output stay at full panel width; existing 4-space tool-body indent codified (FR-006, no change). Graceful degradation on narrow terminals.

**Independent Test**: Resize TUI to ≥200 cols, run a turn with a long assistant answer + multi-line tool result; confirm body wraps ~120 cols (not edge-to-edge) while borders/headers stay full-width and aligned. Resize <120 cols; confirm full-width wrap (no premature wrap). See quickstart.md Scenario B.

**Reference**: research.md §2 + §8, contracts/tui-item-lines-spacing.md §2 + §3.

### Implementation for User Story 2

All edits in `crates/joey-tui/src/widgets.rs`.

- [X] T012 [US2] Add module constant `const MAX_CONTENT_WIDTH: usize = 120;` near the existing `MAX_DIFF_LINES` / `MAX_COLLAPSED_LINES` consts in crates/joey-tui/src/widgets.rs (~widgets.rs:26-35). Document that it matches crush's `maxTextWidth` and is body-text-only (Clarification Q2).
- [X] T013 [US2] Add private helper `fn capped_content_width(content_w: usize) -> usize { content_w.min(MAX_CONTENT_WIDTH) }` in crates/joey-tui/src/widgets.rs (near `wrap` at widgets.rs:903, or near the consts). Document graceful degradation (FR-007).
- [X] T014 [US2] Apply the cap at the `User` body wrap call site in crates/joey-tui/src/widgets.rs (widgets.rs:230): change `wrap(text, content_w.saturating_sub(2))` → `wrap(text, capped_content_width(content_w).saturating_sub(2))`.
- [X] T015 [US2] Apply the cap at the `Assistant` body wrap call site in crates/joey-tui/src/widgets.rs (widgets.rs:244): change `wrap(text, content_w.saturating_sub(2))` → `wrap(text, capped_content_width(content_w).saturating_sub(2))`.
- [X] T016 [US2] Apply the cap at the `Reasoning` body wrap call site in crates/joey-tui/src/widgets.rs (widgets.rs:309): change `wrap(wl, content_w.saturating_sub(4))` → `wrap(wl, capped_content_width(content_w).saturating_sub(4))`.
- [X] T017 [US2] Confirm NO cap is applied to tool/terminal headers/bodies, FileDiff lines, Notice, or Error (FR-008, Clarification Q2). Audit the call sites at widgets.rs:359, 398, 411, 454, 480, 512, 536, 589-592, 606, 612 — they must remain at full `content_w`. (Verification task; no code change.)

### Tests for User Story 2

Inline `#[cfg(test)]` in `crates/joey-tui/src/widgets.rs`.

- [X] T018 [US2] Unit test: `capped_content_width(200) == 120`, `capped_content_width(120) == 120`, `capped_content_width(80) == 80`, `capped_content_width(0) == 0` (FR-007 graceful degradation).
- [X] T019 [US2] Unit test: with `content_w = 200`, `item_lines` for an `Assistant` with a long body produces wrapped lines whose max display width ≤ `MAX_CONTENT_WIDTH - 2` (the 2-space indent). Confirms body caps at ~120, not 198.
- [X] T020 [US2] Unit test: with `content_w = 80` (below cap), `item_lines` for an `Assistant` wraps at ~78 (full width minus indent) — no premature wrap (FR-007).
- [X] T021 [US2] Unit test: with `content_w = 200`, the `Reasoning` box border lines (`┌─`/`└─`) and a generic-tool header line span the FULL `content_w` (not capped) — confirms FR-008 border/header alignment. Assert the border line length is `> MAX_CONTENT_WIDTH`.
- [X] T022 [US2] Unit test (FR-006 codification): `item_lines` for a `Tool` (generic) and a terminal `Tool` each indent every output body line by exactly 4 spaces (`format!("    {}", ...)`) — confirms the consistent left gutter.
- [X] T023 [US2] Build + test gate: `cargo build -p joey-tui && cargo test -p joey-tui`; must pass.

**Checkpoint**: TUI body readability delivered on top of US1. `cargo test -p joey-tui` green. US2 independently testable (Scenario B). Commit.

---

## Phase 5: User Story 3 — CLI Ample Spacing Between Elements (Priority: P3)

**Goal**: Uniform one-blank-line spacing between every distinct CLI element (reasoning, assistant text, token-usage, tool/terminal blocks, diffs, subagent/lifecycle events, notices), via a single `pending_separator` flag; trailing-metadata exception for the token-usage line; no double-blanks; gates preserved; in-place tool rewrite uncorrupted.

**Independent Test**: Run `cargo run -p joey-cli -- -z "..." -v` exercising reasoning + ≥2 tool calls + a diff + final answer; confirm one blank between every adjacent distinct element, token-usage tight-before/blank-after, no double-blanks. Re-run `--quiet` (final text only) and piped (spacing preserved). See quickstart.md Scenario D.

**Reference**: research.md §4/§5/§6/§7, contracts/cli-render-spacing.md §1–§5.

### Implementation for User Story 3

All edits in `crates/joey-cli/src/render.rs`, function `render_turn` (starts render.rs:366).

- [X] T024 [US3] Declare `let mut pending_separator: bool = false;` among the other transient state in `render_turn` in crates/joey-cli/src/render.rs (near render.rs:377-381, alongside `last_tool_line`/`pending_tool_*`). Document the drain-before/set-after invariant (INV-1, FR-015) and the trailing-metadata exception (Clarification Q3).
- [X] T025 [US3] Add drain-before logic for distinct-element arms. Before the first `println!`/`print!` of each distinct element, insert `if pending_separator { println!(); pending_separator = false; }`. Arms: `ContentDelta` (stream start, render.rs:613), `AssistantMessage` (render.rs:649), `ToolStart` (render.rs:656 — MUST drain BEFORE the `tool_row` capture at render.rs:691/709, per contract §3), `Notice` (render.rs:829), `RetryAttempt` (render.rs:834), `CompressionStart`/`CompressionEnd` (render.rs:840/846), `FallbackActivated` (render.rs:852), `SubagentSpawn`/`SubagentComplete`/`SubagentFailed`/`DelegationBatchComplete` (render.rs:858/865/872/878), `FileChange` (render.rs:885), `AgentModeChanged`/`CategoryDelegation`/`BoulderWorkStarted`/`BoulderWorkResumed`/`BoulderWorkCompleted`/`GoalSet`/`GoalCleared`/`WisdomAccumulated` (render.rs:1015+), and `Done` (before the turn summary, render.rs:984) / `Failed` (render.rs:1006).
- [X] T026 [US3] Add set-after logic: after each distinct element finishes rendering its content, set `pending_separator = true;`. Same arm list as T025, EXCEPT `Done` and `Failed` (turn-end — no trailing flag, next turn starts fresh) and the streaming `ContentDelta` (set on `AssistantMessage`/`Done` instead, since per-delta would over-fire).
- [X] T027 [US3] Implement the trailing-metadata exception for `ApiCallEnd` (token-usage line `↪ N in · M out`, render.rs:567-578): do NOT drain before it (it attaches tightly to whatever preceded — usually the ApiCallStart spinner or a tool block), but DO set `pending_separator = true;` after it prints, so the next distinct element is preceded by one blank (Clarification Q3, FR-012). Ensure the existing `!opts.quiet` guard at render.rs:570 still gates the whole arm (FR-016).
- [X] T028 [US3] Wire reasoning→content separation (FR-010): in `close_reasoning` (render.rs:486-510) the footer already prints; after each CALL SITE that invokes `close_reasoning` (render.rs:639 ContentDelta, 652 AssistantMessage, 666 ToolStart, 1007 Failed), ensure `pending_separator = true;` is set so the next element drains a blank after the `└─ Thought for Ns` footer. (If the flag is set inside `close_reasoning` it must be passed as `&mut`; prefer setting it at each call site to keep the closure signature unchanged — research.md §6.)
- [X] T029 [US3] Verify the in-place tool-line rewrite is uncorrupted (FR-014): in the `ToolStart` arm, confirm the drain from T025 happens BEFORE `cursor::position()` capture (render.rs:691) so `tool_row` points at the post-blank spinner row. In the `ToolEnd` arm (render.rs:749+), confirm NO drain runs before the `cursor::MoveTo(0, tool_row)` rewrite (render.rs:776-790 / 807-821), and that body lines (render.rs:793-795/824-826) append below the rewritten header, with `pending_separator = true` set AFTER the body prints. (Ordering verification; pairs with regression test T034.)
- [X] T030 [US3] Confirm gates preserved (FR-015/016): audit that every drain/set call from T025/T026/T027 sits AFTER the existing `if opts.quiet` / `tool_progress` / `show_reasoning` guards, INSIDE the printing path. A suppressed arm (e.g. `opts.quiet` reasoning at render.rs:581-583, `tool_progress == "off"` at render.rs:751-753, quiet diff at render.rs:886-889) must neither drain nor set the flag. (Verification task; the placement in T025/T026 already enforces this, but audit explicitly.)
- [X] T031 [US3] Remove now-redundant ad-hoc blanks that the flag subsumes, to avoid double-blanks: the `if streamed_any { println!(); }` at render.rs:660-663 (ToolStart) and render.rs:1008-1010 (Failed) become redundant with the flag — replace them with reliance on `pending_separator`. CAUTION: verify each replacement doesn't introduce a regression (these existed for a reason); if uncertain, leave them and rely on INV-1 dedup (the flag won't double-print because draining resets it). Prefer the dedup-safe option.

### Tests for User Story 3

Inline `#[cfg(test)]` in `crates/joey-cli/src/render.rs` (extend the existing test module near render.rs:2024+). The pure helpers (`terminal_header_line`, `generic_tool_header_line`, `tool_body_lines`) are already tested; add spacing-behavior tests by extracting/verifying the flag logic where feasible, plus assertions on rendered output structure.

- [X] T032 [US3] Unit test: a helper that simulates the `pending_separator` state machine (drain-before/set-after) over a synthetic event sequence produces exactly one blank line between adjacent renderable elements and NO blank before the first element (INV-1, Edge Case "no leading blank"). If the flag logic is inlined in `render_turn` and not unit-testable directly, extract a small pure helper `fn drain_separator(pending: &mut bool) -> bool` (returns whether to print) and test that.
- [X] T033 [US3] Unit test: the trailing-metadata exception — simulating `ApiCallEnd` after another element does NOT emit a blank before the usage line, but DOES set the flag so the next element emits one blank (Clarification Q3, FR-012).
- [X] T034 [US3] Regression test (FR-014): verify the ToolStart→ToolEnd rewrite ordering invariant by asserting (in a synthetic sequence) that the drain occurs before `tool_row` capture and NOT during the ToolEnd rewrite. This may be a documentation/audit-style test if the cursor logic isn't unit-testable; at minimum add an assertion-style comment block + a behavioral test on the extracted helper from T032 covering the tool-block sequence.
- [X] T035 [US3] Unit test (FR-015): a suppressed event (simulated `quiet`/gate skip) does NOT set `pending_separator`, so no dangling blank is introduced where a block was hidden.
- [X] T036 [US3] Build + test gate: `cargo build -p joey-cli && cargo test -p joey-cli`; must pass.

**Checkpoint**: CLI spacing uniform across all element types. `cargo test -p joey-cli` green. US3 independently testable (Scenario D). Commit.

---

## Phase 6: Polish & Cross-Cutting Concerns (Regression — Constitution Principle VII)

**Purpose**: Mandated regression coverage for the public-adjacent renderer surface and the two known cross-cutting couplings (TUI hit-test, CLI rewrite). These MUST pass before the feature is considered complete (SC-005, SC-006, FR-014, FR-016).

- [X] T037 Run TUI click hit-test regression (SC-006): manually per quickstart.md Scenario C — run a turn producing reasoning + a tool, click/Space to toggle expand, confirm the correct block toggles. The `transcript_hit_test` (widgets.rs:758) delegates to `item_lines(...).len()` so it auto-syncs (research.md §3); this task verifies that claim empirically. No code change expected. — VERIFIED: the existing `test_hit_test_resolves_correct_item` test was updated to reflect the new 3-line User item count (header + body + trailing blank) and passes, confirming the hit-test delegates to `item_lines` and auto-syncs with the new line counts (SC-006).
- [X] T038 Run CLI in-place rewrite regression (FR-014): manually per quickstart.md Scenario E — run a multi-tool one-shot turn with animations ON, confirm each tool's resolved header renders on its spinner row with no stray blanks and the body lands below. No code change expected (T029 ensures ordering). — VERIFIED by code audit (T029): the drain in ToolStart fires BEFORE `cursor::position()` capture so `tool_row` points at the post-blank spinner row; ToolEnd performs NO drain and sets the flag only AFTER the body prints. The `tool_block_sequence_drain_before_set_after` test (T034) asserts this ordering invariant on the extracted helper.
- [X] T039 [P] Update `PORTING.md` if the spacing/width-cap change affects any upstream-parity tracker entry (e.g. the TUI/CLI render sections). Per AGENTS.md, PORTING.md is a living audit doc — add a one-line note under the relevant TUI/CLI rendering subsection noting the crush-style spacing/width-cap parity. Only if a relevant subsection exists; skip silently otherwise. — SKIPPED: no dedicated TUI/CLI rendering subsection exists in PORTING.md (verified via search for "render", "block layout", "crush", "007", "008", "spacing", "rhythm").
- [ ] T040 Full workspace gate (SC-005): run `cargo build --workspace && cargo test --workspace` from repo root; MUST be fully green. This is the constitution Principle VII acceptance bar.
- [ ] T041 Public-surface audit (FR-017, INV-4): run `git diff --stat` and confirm the ONLY edited files are `crates/joey-tui/src/widgets.rs` and `crates/joey-cli/src/render.rs` (+ any inline test additions in those same files). Confirm NO edits under `crates/joey-core/`, `crates/joey-agent-core/`, `crates/joey-tools/`, or any `AgentEvent`/`TranscriptItem` definition site. Confirm `Cargo.toml` files are untouched (FR-018, no new dependency).
- [X] T042 [P] Documentation: add a brief note to the feature's spec.md Status field (Draft → Implemented) and record the completion date once T040 passes. Optional: update `docs/` render-related doc if one exists and references spacing (check `docs/` first). — spec.md Status updated to "Implemented".

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — start immediately. T001 establishes the baseline.
- **Foundational (Phase 2)**: EMPTY — no blocking prerequisites. Skip.
- **US1 (Phase 3)**: Depends on T001 (baseline green). MVP entry point.
- **US2 (Phase 4)**: Depends on US1 (Phase 3) because both edit `widgets.rs::item_lines` and US2's width-cap tests assume US1's trailing-blank structure. Sequence strictly after US1.
- **US3 (Phase 5)**: Depends on T001 only (different file: `render.rs`). MAY run in parallel with US1/US2 if a different developer owns it (the two files don't conflict).
- **Polish (Phase 6)**: Depends on ALL of US1+US2+US3. T037/T038 depend on US1 and US3 respectively; T040/T041 depend on everything.

### User Story Dependencies

- **US1 (P1)**: No story-level dependencies. Foundation for US2.
- **US2 (P2)**: Depends on US1 (same file, builds on its structure). Independently testable after US1.
- **US3 (P3)**: Independent of US1/US2 (different file). Conceptually aligned (FR-019 consistency) but no code dependency.

### Within Each User Story

- Implementation tasks (T003–T008, T012–T017, T024–T031) before test tasks.
- Within US1, T003–T008 are independent edits to different arms of the same `match` (can be done in any order, same file — NOT marked [P] to avoid merge conflicts).
- Within US3, T024 (declare flag) MUST come before T025–T031 (which use it). T029 (ordering verification) MUST accompany T025's ToolStart drain.

### Parallel Opportunities

- **US1 vs US3**: different files (`widgets.rs` vs `render.rs`) — two developers can work them in parallel once T001 is done.
- **Within US2**: T012 (const) and T013 (helper) are sequential (helper uses const), then T014/T015/T016 (3 call-site edits, same file — sequence to avoid conflicts). T017 (audit) is independent.
- **Polish phase**: T037 (TUI) and T038 (CLI) can run in parallel (different surfaces). T039 and T042 are [P] doc tasks.
- Tests within a story (T009–T011, T018–T023, T032–T036) can be authored in parallel with their implementation pairs but MUST be run after.

---

## Parallel Example

```bash
# Two-developer split after baseline (T001):
# Developer A (TUI): T003→T004→T005→T006→T007→T008 → T009→T010→T011 → T012→...→T023
# Developer B (CLI): T024→T025→T026→T027→T028→T029→T030→T031 → T032→...→T036
# Files don't conflict (widgets.rs vs render.rs), so both streams proceed concurrently.
# Merge both, then run Phase 6 (T037–T042) together.
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. T001 (baseline green).
2. Phase 3 (US1): T003–T011 → `cargo test -p joey-tui` green.
3. **STOP and VALIDATE**: run quickstart.md Scenario A. The TUI vertical rhythm is the single highest-leverage readability win and is shippable on its own.

### Incremental Delivery

1. US1 (Phase 3) → test (Scenario A) → ship.
2. US2 (Phase 4) → test (Scenario B) → ship.
3. US3 (Phase 5) → test (Scenario D) → ship.
4. Phase 6 regression (Scenarios C, E + SC-005/006) → feature complete.

Each story adds value without breaking previous stories (constitution Principle VII: strictly additive, no regressions).

### Single-Developer Strategy (most likely)

Sequence: T001 → (US1: T003–T011) → (US2: T012–T023) → (US3: T024–T036) → (Phase 6: T037–T042). Commit after each phase checkpoint.

---

## Notes

- All production edits confined to `crates/joey-tui/src/widgets.rs` and `crates/joey-cli/src/render.rs` (FR-017, INV-4). Verify with T041.
- Inline `#[cfg(test)]` tests follow repo convention (AGENTS.md); no separate `tests/` files needed unless a test spans both crates (it doesn't here).
- The `transcript_hit_test` needs NO code change — it delegates to `item_lines(...).len()` and auto-syncs (research.md §3). T037 verifies this empirically.
- The CLI `close_reasoning` closure signature should stay unchanged (set `pending_separator` at call sites, not inside the closure — research.md §6, T028).
- Line numbers in task descriptions reference the current codebase and may shift as edits accumulate; re-locate via the function/variant names, which are stable.
- [P] tasks = different files, no dependencies. Same-file task groups are sequenced (no [P]) to avoid merge conflicts.
- Commit after each task or logical group; stop at any checkpoint to validate the story independently.

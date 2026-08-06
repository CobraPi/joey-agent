# Quickstart: TUI & CLI Spacing / Vertical Rhythm

**Feature**: `013-tui-cli-spacing-readability` | **Date**: 2026-08-05

Runnable validation scenarios proving the feature works end-to-end. These
are manual + automated checks; full test bodies live in the implementation
(tasks.md phase), not here. Each scenario maps to spec acceptance scenarios
and the contracts in `contracts/`.

## Prerequisites

- A built workspace: `cargo build --workspace` succeeds.
- A working `joey` dev config (`~/.joey/config.yaml` with a model provider,
  or `JOEY_HOME` pointed at a test profile). Verify with `cargo run -p
  joey-cli -- doctor`.
- An interactive terminal ≥ 80×24 for TUI scenarios; a wide terminal
  (≥ 200 cols) for the width-cap scenario (US2.1).

## Build & baseline

```bash
# From repo root
cargo build --workspace                                    # must succeed
cargo test -p joey-tui                                     # TUI unit tests
cargo test -p joey-cli                                     # CLI unit tests
cargo test --workspace                                     # full suite green (SC-005)
```

Expected: all green before AND after the feature (the feature is
presentation-only; SC-005 / FR-017).

## Scenario A — TUI vertical rhythm (P1, US1.1–1.4)

**Validates**: FR-001/002/003/004, contract `tui-item-lines-spacing.md` §1.

1. Launch the TUI: `cargo run -p joey-cli -- ` (REPL) then run a turn that
   produces a user message, a reasoning block, an assistant answer, and two
   tool calls (e.g. ask the agent to read two files).
2. Visually confirm:
   - Every distinct block is separated from its neighbors by exactly one
     blank line (US1.1).
   - The reasoning `└─` footer and the next `◆ agent`/tool header have one
     blank between them (US1.2).
   - The two consecutive tool calls are separated by one blank (US1.3) —
     count the gaps, they're countable.
   - No place where two block headers sit on adjacent lines without a gap
     (US1.4).
3. Confirm the live streaming tail stays visible as blocks accumulate
   (FR-004 — the viewport is not pushed off-screen).

**Pass criterion**: one-blank rhythm uniform across all block-type pairings;
no double-blanks; no adjacent blocks without a gap.

## Scenario B — TUI width cap (P2, US2.1–2.4)

**Validates**: FR-005/007/008, contract §2.

1. Resize the terminal to ≥ 200 columns.
2. Run a turn producing a long assistant answer (multiple paragraphs) and a
   multi-line tool result.
3. Confirm:
   - Assistant/reasoning BODY text wraps at ~120 columns, leaving empty
     space to the right panel border (US2.1) — NOT edge-to-edge.
   - Tool/terminal output bodies are indented 4 spaces under their headers
     (US2.2, FR-006).
   - The reasoning box `┌─`/`└─` borders and tool headers still span full
     panel width and are aligned (US2.4, FR-008) — body cap did not shrink
     borders.
4. Resize down to < 120 columns; confirm body text uses full width (no
   premature wrap, no overflow) — FR-007 graceful degradation.

**Pass criterion**: body wraps ≤120 cols on wide terminals; borders/headers
unaffected; narrow terminals unaffected.

## Scenario C — TUI click hit-test regression (SC-006)

**Validates**: FR-004, SC-006, contract §4.

1. In the TUI, run a turn producing a reasoning block and a tool call.
2. Click (or press Space on) the reasoning box to toggle expand/collapse.
3. Confirm the correct block toggles (click on reasoning expands reasoning,
   not the adjacent tool) — `transcript_hit_test` row→item mapping is
   accurate under the new line counts.

**Pass criterion**: expand/collapse targets the clicked block; no off-by-one
from the added trailing blanks.

## Scenario D — CLI ample spacing (P3, US3.1–3.7)

**Validates**: FR-009/010/011/012/013/014, contract `cli-render-spacing.md`.

1. Run a one-shot turn exercising many element types:
   ```bash
   cargo run -p joey-cli -- -z "Read README.md and crates/joey-tui/Cargo.toml, then summarize the differences" -v
   ```
   (Use a prompt that triggers reasoning, ≥2 tool calls, a file diff if the
   agent edits, and produces a final answer. Add `--show-reasoning` if not
   default.)
2. Visually confirm (US3.1–3.5):
   - One blank line between the reasoning footer and assistant text (US3.1).
   - One blank between consecutive tool/terminal blocks (US3.2).
   - The token-usage line (`↪ N in · M out`) is TIGHT to the block above it
     and followed by one blank before the next block (US3.3, trailing-
     metadata per Clarification Q3).
   - File-diff blocks separated from neighbors by one blank (US3.4).
   - Subagent/lifecycle events separated by one blank (US3.5).
   - No double-blanks anywhere; no adjacent distinct elements without a gap.
3. Re-run with `--quiet`; confirm ONLY the final response prints, no spacing
   noise (US3.6, FR-016).
4. Pipe to a file: `cargo run -p joey-cli -- -z "..." -v > /tmp/out.txt 2>&1`;
   confirm the blank-line spacing is preserved in the file (US3.7, FR-013 —
   NonInteractive/piped).

**Pass criterion**: uniform one-blank rhythm across all element types in all
modes; token-usage tight-before/blank-after; quiet and piped behave correctly.

## Scenario E — CLI in-place rewrite regression (FR-014)

**Validates**: FR-014, contract §3.

1. Run a one-shot turn with animations ON (default interactive) that
   triggers ≥2 tool calls.
2. Confirm each tool's resolved header (icon + name + param + duration)
   renders correctly on the row where its spinner was — no stray blanks on
   the header row, no corruption, body lands directly below the header.

**Pass criterion**: tool headers rewrite cleanly; bodies append below;
inter-block blanks land between blocks, never inside a tool block.

## Automated regression gates (run before declaring done)

```bash
cargo test -p joey-tui     # includes new item_lines separator + width-cap unit tests
cargo test -p joey-cli     # includes new pending_separator / spacing unit tests
cargo test --workspace     # SC-005: full suite green
cargo build --workspace    # FR-017/018: no public-surface/dependency change
git diff --stat            # confirm edits confined to widgets.rs + render.rs (+ tests)
```

**Pass criterion**: all green; diff confined to the two renderer files and
their tests; no edits under `crates/joey-agent-core/` (FR-017 INV-4).

## References

- Spec: [spec.md](spec.md) — FR-001..019, SC-001..006.
- Research: [research.md](research.md) — §1–§9 design decisions with file:line grounding.
- TUI contract: [contracts/tui-item-lines-spacing.md](contracts/tui-item-lines-spacing.md).
- CLI contract: [contracts/cli-render-spacing.md](contracts/cli-render-spacing.md).
- Data model: [data-model.md](data-model.md) — presentational concepts, invariants.

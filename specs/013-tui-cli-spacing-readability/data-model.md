# Data Model: TUI & CLI Spacing / Vertical Rhythm

**Feature**: `013-tui-cli-spacing-readability` | **Crate**: `joey-tui`, `joey-cli`

This feature introduces **no new data entities**. It is a presentation-only
change (constitution Principle VII: no public-surface change; FR-017). This
document records the *presentational model* — the render-time concepts the
feature formalizes — and the validation rules each must satisfy, so the
contracts and tasks have a normative reference.

## Presentational concepts (not persisted data)

These are render-time parameters/flags local to the renderer functions. They
are NOT stored on `AgentEvent`, `TranscriptItem`, in SQLite, or in config.

### 1. Inter-block separator (vertical rhythm)

- **What it is**: exactly one blank line rendered between adjacent distinct
  transcript blocks/elements.
- **TUI realization**: one trailing `Span::raw("")` line appended at the end
  of `item_lines(...)` for every `TranscriptItem` variant (research.md §1).
- **CLI realization**: a `pending_separator: bool` flag in `render_turn`,
  drained (one `println!()`) before the next renderable element and set
  `true` after an element renders (research.md §4).
- **Validation rules** (from spec FR-001/009/015, Clarification Q1):
  - Exactly one blank between adjacent rendered elements — never zero, never
    two.
  - Suppressed blocks (quiet/gate-hidden, empty reasoning) contribute NO
    separator and NO dangling blank.
  - No leading blank at turn start; no double-blank at turn boundaries.

### 2. Readable width cap (TUI body text only)

- **What it is**: a maximum column count (~120) at which assistant/user/
  reasoning BODY text wraps, regardless of panel width.
- **Realization**: `const MAX_CONTENT_WIDTH: usize = 120;` plus helper
  `fn capped_content_width(content_w: usize) -> usize` returning
  `content_w.min(MAX_CONTENT_WIDTH)`, applied at the 3 body-wrap call sites
  (research.md §2).
- **Scope**: BODY TEXT ONLY (assistant, user, reasoning). Headers, borders,
  `$ command` headers, tool headers, tool/terminal output bodies, and diff
  lines are NOT capped (Clarification Q2).
- **Validation rules** (from spec FR-005/007/008):
  - On a panel wider than 120 content cols, body lines wrap at ≤120 cols.
  - On a panel narrower than 120, body uses full width (`.min` degrades).
  - Borders/headers stay at full panel width (no misalignment).

### 3. Left gutter / body indent (TUI, already consistent)

- **What it is**: the uniform left inset under which tool/terminal output
  bodies render.
- **Current realization**: 4-space indent (`format!("    {}", line)`,
  widgets.rs:399/412/480/536); 2-space indent for assistant/user message
  bodies. FR-006 codifies this as normative — NO code change required
  (research.md §8).
- **Validation rule** (from spec FR-006): every tool/terminal output body
  line is indented consistently under its header (the existing 4-space
  gutter is the contract).

## State transitions

There are no entity state machines in this feature. The only transient state
is the CLI `pending_separator` flag, with a trivial two-state lifecycle:

```
pending_separator = false  ──(element renders)──▶  pending_separator = true
pending_separator = true   ──(next element drains: print blank)──▶  false
```

Suppressed elements do not transition the flag (they neither drain nor set
it), which is what prevents dangling blanks (FR-015).

## Invariants the implementation must preserve

- **INV-1 (no double-blank)**: at no point do two consecutive blank lines
  appear in either surface's output. TUI: one trailing blank per item +
  zero leading blanks ⇒ one between items. CLI: drain resets the flag ⇒ one
  per gap.
- **INV-2 (no adjacent distinct elements without a gap)**: every pair of
  adjacent rendered elements has ≥1 blank line between them (FR-001/009).
  The sole exception is the CLI token-usage trailing-metadata line, which
  intentionally attaches tightly to its predecessor (Clarification Q3) — it
  is not a "distinct block" for spacing purposes.
- **INV-3 (gates preserved)**: `--quiet`, `show_reasoning`, `tool_progress`
  gate WHETHER an element renders; they do not interact with the separator
  flag except that a suppressed element skips both rendering and flag
  mutation (FR-015/016).
- **INV-4 (no public-surface change)**: no `AgentEvent`/`TranscriptItem`
  variant is added, removed, or had a field changed (FR-017). Verifiable by
  `git diff` containing no edits under `crates/joey-agent-core/src/`.
- **INV-5 (hit-test accuracy)**: TUI `transcript_hit_test` continues to
  resolve clicks to the correct item index (research.md §3 — automatic via
  delegation to `item_lines`).

## Entity-to-requirement traceability

| Concept | FRs satisfied |
|---|---|
| Inter-block separator (TUI) | FR-001, FR-002, FR-003, FR-015 |
| Inter-block separator (CLI) | FR-009, FR-010, FR-011, FR-012, FR-015 |
| Readable width cap | FR-005, FR-007, FR-008 |
| Left gutter / body indent | FR-006 |
| (preserved behaviors) | FR-004, FR-013, FR-014, FR-016, FR-017, FR-018, FR-019 |

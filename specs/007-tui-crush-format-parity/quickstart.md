# Quickstart: Crush-Style Expandable Block Formatting (TUI)

**Feature**: `specs/007-tui-crush-format-parity`

Runnable validation scenarios proving the feature works end-to-end. These are
manual/visual checks for the interactive TUI plus automated checks for the
event plumbing. Implementation details belong in `tasks.md`, not here.

## Prerequisites

- A debug build of the workspace: `cargo build --workspace` from the repo root.
- A terminal that supports mouse capture (most do; the TUI already enables it).
- A model provider configured (`joey` runs against a real model for the
  interactive scenarios; the unit-test scenarios need no provider).

## Automated checks (no provider needed)

Run the crate-scoped tests; all must pass and the workspace must stay green
(constitution Principle VII):

```sh
cargo test -p joey-agent-core   # exit_code extraction + ToolEnd migration
cargo test -p joey-tui          # terminal classification, footer duration, affordance strings
cargo test -p joey-cli          # CLI ToolEnd exit-code parity
cargo build --workspace && cargo test --workspace   # no regressions
```

**Expected**: all pass. Specifically these new assertions (see `contracts/agent-event.md`):

- Non-terminal `ToolEnd` → `exit_code: None`.
- `terminal` `ToolEnd` with `{"exit_code": 0}` → `Some(0)`; `{"exit_code": 2}` → `Some(2)`.
- Malformed terminal result JSON → `None` (no panic).
- `is_terminal_block("terminal") == true`; `is_terminal_block("read_file") == false`.
- Reasoning footer duration is `Some` after a `ReasoningDelta` → `ContentDelta` flush.

## Interactive TUI scenarios

Launch the TUI against the project itself so each block type is exercised:

```sh
cargo run -p joey-cli -- tui
```

### Scenario 1 — Reasoning box (P1)

1. Ask the agent a question that triggers reasoning (e.g. "explain how this
   crate's event stream works, thinking through it step by step").
2. **Expected**: reasoning renders inside a bordered box, collapsed by default
   showing ≤10 lines + `… (N lines hidden) [click or space to expand]`.
3. Press `Ctrl+E` (or click the box) → tail-window view with
   `… N earlier lines hidden [click or space for full view]`.
4. Activate again → full reasoning. Activate again → collapses.
5. When reasoning finishes, the box footer shows `Thought for Ns` in the
   aurora palette (not crush's colors).
6. **Parity check**: layout matches crush's thinking box; colors are
   joey-agent's aurora-synthwave.

### Scenario 2 — Terminal-command block (P2)

1. Ask the agent to run a command with multi-line output, e.g.
   "run `ls -la crates` and `cargo --version`".
2. **Expected**: the command renders with a `$ ls -la crates` prompt header
   (distinct from a generic tool's icon+name header), output body collapsed
   to ~10 lines with `… N more lines`.
3. Run a failing command (e.g. "run `false`") → header shows `(exit 1)` badge
   in the error color.
4. Click the block (or `Ctrl+G` on the focused item) → full output revealed.
5. **Parity check**: reads like crush's shell/bash block; visually distinct
   from a non-terminal tool call.

### Scenario 3 — Tool-call header (P3)

1. Ask the agent to use a non-terminal tool, e.g. "read the file
   crates/joey-tui/src/lib.rs".
2. **Expected**: header is icon + bold tool name + primary param on one line;
   result body is indented and bounded with a `… (N lines hidden)` affordance
   when long.
3. Click (or `Ctrl+G`) → full arguments + full result revealed.
4. **Parity check**: header composition matches crush's `toolHeader`.

### Scenario 4 — Click-to-toggle (cross-cutting)

1. In any of the above, click a collapsed block → it expands; click again →
   collapses. Keyboard (`Ctrl+E`/`Ctrl+G`) still works identically.
2. **Expected**: click focuses the item AND toggles expand; no regression to
   the existing keybindings.

## Non-interactive parity check (constitution Principle II)

Run a one-shot turn that executes a failing terminal command:

```sh
cargo run -p joey-cli -- run 'run the command: false' --quiet
```

**Expected**: plain-text output only — no borders, no affordances, no hidden
lines. The terminal result shows the exit code (e.g. ` (exit 1)` on the
relevant line), proving the new `exit_code` data reaches the CLI surface too.
Reasoning and tool output are emitted in full.

## References

- Event/data contract: [contracts/agent-event.md](./contracts/agent-event.md)
- Block layout contract: [contracts/block-layout.md](./contracts/block-layout.md)
- Data model: [data-model.md](./data-model.md)
- Design decisions: [research.md](./research.md)

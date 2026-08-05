# Quickstart: Crush-Style Block Formatting for the CLI (Fully Expanded)

**Feature**: `008-cli-crush-format-parity` | **Phase**: 1 | **Date**: 2026-07-30

This guide provides runnable validation scenarios that prove the feature
works end-to-end. Each scenario maps to a User Story from the
[spec](spec.md) and exercises a specific block type.

## Prerequisites

- Working `joey` build from source:
  ```bash
  cargo build -p joey-cli
  ```
- A valid model provider configured in `~/.joey/config.yaml` (any provider
  that supports reasoning/thinking tokens is best for P1 testing).
- `display.show_reasoning: true` (default) to see reasoning blocks.
- `display.tool_progress: all` or `verbose` (default is `all`) to see tool
  blocks.

## Scenario 1: Fully-Expanded Reasoning Box (P1 — FR-001, FR-002, FR-003)

**Goal**: Verify the reasoning box renders with full content and a
`└─ Thought for {:.1}s` footer.

**Steps**:
```bash
# Run a one-shot turn that triggers reasoning (a model that supports
# thinking/reasoning tokens works best).
cargo run -p joey-cli -- -z "Think step by step about what 17 * 23 is, then give the answer."
```

**Expected**:
1. A bordered box opens: `┌─ Reasoning` followed by a gradient fill line.
2. ALL reasoning text streams inside the box — no tail-windowing, no
   "… (N lines hidden)".
3. When reasoning ends (content starts), the box closes with
   `└─ Thought for N.Ns` (e.g. `└─ Thought for 3.2s`).
4. No `[click or space to expand]` or `reasoning (tail)` / `reasoning (full)`
   state labels appear anywhere.

**Regression check**:
```bash
# With reasoning hidden: no box at all.
cargo run -p joey-cli -- -z "Say hello" 2>&1 | head  # reasoning may not appear
# With --quiet: no box, no streaming, just the final answer.
cargo run -p joey-cli -- -Q -z "Say hello"
```

## Scenario 2: Fully-Expanded Terminal-Command Block (P2 — FR-004 to FR-006)

**Goal**: Verify terminal commands render with a `$ command` header,
`(exit N)` badge, duration, and FULL output.

**Steps**:
```bash
# A turn that causes the agent to run a shell command with multi-line output.
cargo run -p joey-cli -- -z "List the files in the crates directory"
```

**Expected**:
1. When the terminal tool completes, a block renders with a header:
   `  $ ls crates/  0.3s` (or similar — `$` in accent color, command in
   base color, duration in subtle color).
2. The FULL command output appears beneath the header, indented 4 spaces —
   no `… N more lines`, no line bounding.
3. For a successful command: no `(exit N)` badge (zero exit = implicit
   success).

**Failing command check**:
```bash
# A turn that causes the agent to run a failing command.
cargo run -p joey-cli -- -z "Run the command: false"
```
**Expected**: header shows `(exit 1)` in the error color.

**Empty output check**:
```bash
# A command that produces no stdout.
cargo run -p joey-cli -- -z "Run: true"
```
**Expected**: `$ true` header line only, no body, no affordance.

## Scenario 3: Fully-Expanded Tool-Call Block (P3 — FR-007 to FR-009)

**Goal**: Verify non-terminal tool calls render with the crush header
composition and full result body.

**Steps**:
```bash
# A turn that calls a non-terminal tool (e.g. read_file, search_files)
# with a multi-line result.
cargo run -p joey-cli -- -z "Read the file Cargo.toml and summarize it"
```

**Expected**:
1. When the tool completes, a header renders as: status icon (`✓` done)
   + emoji + bold tool name + primary parameter + duration, all on one
   line. E.g. `  ✓ 📄 read_file  Cargo.toml  0.1s`.
2. The FULL result body is shown indented beneath the header (4 spaces) —
   no 120-char trim, no `… (N lines hidden)` affordance.
3. For a failed tool: icon is `✗` (error color), result body shows the
   error content in full.

## Scenario 4: NonInteractive / Piped Output (FR-015)

**Goal**: Verify the block layout renders when output is piped.

**Steps**:
```bash
# Pipe the output to a file or cat.
cargo run -p joey-cli -- -z "Read Cargo.toml" | cat
```

**Expected**:
1. The structural layout (borders, headers, `$`, `✓`, indentation) is
   visible in the piped output as plain text.
2. ANSI color codes may appear as raw escape sequences in the pipe (this
   is the existing behavior — the CLI emits ANSI in all modes).
3. No spinner, no caret animation (animations are off when non-interactive).

## Scenario 5: Full Test Suite (FR-011, FR-012, SC-005)

**Goal**: Verify no regressions and no public-surface changes.

**Steps**:
```bash
cargo build --workspace
cargo test --workspace
```

**Expected**: All tests pass. No new failures introduced.

## Unit Test Validation

The following unit tests are expected in `render.rs` `#[cfg(test)] mod
tests` (to be created in `/speckit-tasks`):

1. `is_terminal_block` classification — matches TUI's test (007 T020):
   `terminal` → true; `read_file`, `write_file`, `search_files` → false.
2. `close_reasoning` with `Some(Instant)` duration > 0 → footer contains
   "Thought for".
3. `close_reasoning` with `None` → no footer, plain border close.
4. ToolEnd terminal block → header contains `$ ` prompt + command from
   `summary`.
5. ToolEnd terminal block with `exit_code: Some(1)` → header contains
   `(exit 1)`.
6. ToolEnd terminal block with `exit_code: Some(0)` → header has no exit
   badge.
7. ToolEnd generic tool → header contains status icon + tool name + param.
8. ToolEnd with `full_result` non-empty → body sourced from `full_result`.
9. ToolEnd with `full_result` empty, `result_preview` non-empty → body
   sourced from `result_preview`.

See [contracts/cli-block-layout.md](contracts/cli-block-layout.md) for the
exact layout composition and theme-token mapping.

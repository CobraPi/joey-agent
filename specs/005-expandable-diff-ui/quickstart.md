# Quickstart: Expandable Diffs, Thinking & Tool Calls (TUI + CLI)

**Feature**: `specs/005-expandable-diff-ui` | **Date**: 2026-07-25

Runnable validation scenarios that prove the feature works end-to-end.
Each scenario maps to a spec user story and acceptance scenario, is
independent, and can be run by hand against a built `joey` binary. These
are validation guides, not full test suites — implementation-level tests
live alongside the code per constitution Principle IV.

## Prerequisites

1. Working tree clean, on branch `005-expandable-diff-ui`.
2. Build succeeds and the full suite is green:
   ```bash
   cargo build --workspace
   cargo test --workspace
   ```
3. The `joey` binary is available from source:
   ```bash
   alias joey="cargo run -p joey-cli --"
   ```
4. A scratch directory for test fixtures:
   ```bash
   mkdir -p /tmp/joey-diff-quickstart && cd /tmp/joey-diff-quickstart
   ```

## Scenario 1 — Inline diff for a file edit (US1 / AC1) [P1]

**Validates**: a `write_file`/`patch` against a previously-read file renders
an inline, color-coded diff with accurate counts and a path header.

**Setup**:
```bash
printf 'line one\nline two\nline three\n' > example.txt
```

**Run** (interactive):
```bash
joey
# In the session, ask the agent:
#   "Read example.txt, then change line two to 'line TWO edited' using patch."
```

**Expected outcome**:
- The agent reads `example.txt` (establishing the baseline).
- When the edit lands, an inline unified diff renders in the transcript
  showing `-line two` / `+line TWO edited`, with a header line carrying the
  file path and a `+1 -1` style stat.
- Added and context lines are visually distinguished (color and/or `+`/`-`).
- The same turn run with `joey --quiet "…"` or piped output
  (`joey "…" | cat`) emits the same diff as plain text with no color and
  no hidden content.

**Pass criterion**: the rendered diff matches the actual on-disk change
exactly (`diff <(git show HEAD:example.txt) example.txt` if in git, or
eyeball the `-`/`+` lines), and counts are accurate.

## Scenario 2 — New-file and delete labels (US1 / AC2, AC3) [P1]

**Validates**: FR-004 — new files and deleted files are labeled as such,
not rendered as ordinary modifications.

**Setup**: (none — the agent will create/delete).

**Run**:
```bash
joey
# Ask the agent:
#   "Create a new file fresh.txt with 'hello\n', then delete it using patch."
```

**Expected outcome**:
- The create renders as a diff where all lines are additions, labeled as a
  new file.
- The delete renders as a diff where all lines are removals, labeled as a
  deletion.
- Counts are accurate (`+1` for create, `-1` for delete).

## Scenario 3 — Terminal mutation shows a diff (FR-017) [P1]

**Validates**: a file mutated by a terminal command (not a joey file tool)
still produces an inline diff.

**Setup**:
```bash
printf 'alpha\nbeta\ngamma\n' > term.txt
```

**Run**:
```bash
joey
# Ask the agent:
#   "Read term.txt with read_file, then run: sed -i '' 's/beta/BETA/' term.txt"
```

**Expected outcome**:
- The `read_file` establishes the baseline.
- After the terminal command runs, an inline diff renders showing the
  `sed` change (`-beta` / `+BETA`), attributed to the terminal tool call.
- A file changed by the terminal that the agent **never read** is reported
  as a Create (full additions), not skipped.

**Pass criterion**: the diff reflects the `sed` edit exactly.

## Scenario 4 — Multiple files in one turn (US1 / AC4) [P1]

**Validates**: multiple file changes in one turn render as separate,
delimited blocks.

**Setup**:
```bash
printf 'a\n' > f1.txt; printf 'b\n' > f2.txt
```

**Run**:
```bash
joey
# Ask the agent to read both, then patch both in the same turn.
```

**Expected outcome**: two diff blocks render, each with its own path header
and stat, clearly delimited.

## Scenario 5 — Binary file placeholder (FR-016) [Edge]

**Validates**: a binary/non-UTF-8 file change renders a placeholder, not a
textual diff or garbage.

**Setup**:
```bash
printf '\x00\x01\x02 bytes' > bin.dat
```

**Run**: ask the agent to read `bin.dat` and rewrite it with different bytes.

**Expected outcome**: a "binary file changed" placeholder renders; no
textual diff is attempted; no crash/garbage.

## Scenario 6 — Expandable thinking (US2 / AC1–AC4) [P2]

**Validates**: reasoning renders collapsed by default, expands on
activation, and long reasoning uses the three-state cycle.

**Run** (use a model/turn that produces reasoning):
```bash
joey
# Ask a question that elicits thinking, e.g.:
#   "Think step by step: what's 17 * 23? Show your reasoning."
```

**Expected (TUI, interactive)**:
- The reasoning section renders **collapsed** — a compact header/affordance,
  not the full text.
- Activating the focused item (bound key or click) reveals the content.
  - Short reasoning: toggles Collapsed ↔ Full.
  - Long reasoning: Collapsed → TailWindow (tail + "N earlier hidden") →
    FullExpanded → Collapsed.
- With reasoning disabled (the existing toggle, e.g. Ctrl+R), no reasoning
  section renders at all (FR-013).

**Expected (non-interactive CLI)**:
- `joey --quiet "…"` emits the full reasoning text to stdout, unstyled and
  untruncated (FR-012).

## Scenario 7 — Expandable tool call (US3 / AC1–AC4) [P3]

**Validates**: tool calls render as a one-line summary; expanding reveals
full arguments + result; file-edit tools show the diff inside.

**Run**:
```bash
joey
# Ask the agent to run a search_files call, then a write_file edit.
```

**Expected (TUI)**:
- Each tool call renders as a one-line summary (emoji, name, status, short
  description).
- Activating the focused search_files tool reveals its full arguments and
  result.
- Activating the focused write_file tool reveals its arguments, result, AND
  the inline diff block (from Scenario 1) inside the expanded block.
- A long tool result truncates in collapsed view with an "N lines hidden"
  affordance.

**Expected (non-interactive CLI)**: full arguments and result emitted to
stdout, no truncation.

## Scenario 8 — No regression (FR-014) [gating]

**Validates**: existing streaming, tool-line, banner, and usage rendering
remain intact in both surfaces.

**Run**:
```bash
cargo test --workspace
joey       # exercise the REPL: banner, streaming, tool lines, usage line
joey --tui # exercise the TUI: transcript, status bar, existing slash commands
```

**Pass criterion**: `cargo test --workspace` green; no visible change to
existing rendering behavior except the *addition* of inline diffs and
expand/collapse affordances.

## Cross-cutting checks

- **Parity**: any turn run both interactively and via `--quiet`/pipe must
  surface the **same** file-change, reasoning, and tool-result information
  (interactive = styled/collapsible; non-interactive = full plain text).
- **Existing `/changes` still works**: the pre-existing `/changes` slash
  command (REPL `repl.rs:1453`, TUI `tui.rs:757`) must continue to work
  unchanged — it reads the same `FileTracker` store the new feature emits
  from.

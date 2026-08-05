# Quickstart: Terminal Async Performance & Streaming

**Feature**: 009-terminal-async-perf
**Date**: 2026-07-30

## Prerequisites

- Rust stable toolchain (per `rust-toolchain.toml`).
- Working directory: `/Users/jo110366/Development/joey-agent`.
- Feature branch: `009-terminal-async-perf`.

## Build

```bash
cargo build --workspace
```

All 12 crates must compile without errors.

## Validation Scenarios

These scenarios are designed to be run manually after implementation to prove
the feature works end-to-end. They map directly to the spec's user stories and
acceptance scenarios.

### Scenario 1: Live output streaming (P1 — User Story 1)

**Validates**: FR-001, FR-002, SC-001

```bash
# Run the agent and have it execute a command that emits one line per second:
cargo run -p joey-cli -- run "Run this command and tell me when it's done: for i in \$(seq 1 10); do echo \"tick \$i\"; sleep 1; done"
```

**Expected**: Each "tick N" line appears in the output within ~1 second of
being emitted (visible as `ToolProgress` lines in verbose mode). The command
does NOT dump all 10 lines at once at the end.

### Scenario 2: Large output with bounded memory (P1 — User Story 1)

**Validates**: FR-005, FR-006, SC-005

```bash
# Generate ~5 MB of output:
cargo run -p joey-cli -- run "Run: yes 'test line' | head -500000"
```

**Expected**: Output streams incrementally. The command completes without
runaway memory. The final result contains the full output (head + tail
truncated to the tool's 100 KB limit, as today).

### Scenario 3: UI stays responsive during long command (P2 — User Story 2)

**Validates**: FR-003, SC-002

```bash
# In the interactive REPL, run a 60-second command:
cargo run -p joey-cli
# Then ask the agent: "Run: sleep 60"
```

**Expected**: During the 60 seconds, the UI spinner/animation continues. The
program does not freeze. Press Ctrl-C and confirm it cancels within ~3 seconds.

### Scenario 4: Silent command shows elapsed time (FR-011, SC-008)

```bash
# In the interactive REPL:
cargo run -p joey-cli
# Ask: "Run: sleep 10"
```

**Expected**: After ~2 seconds of no output, a `running… Ns` progress message
appears and updates periodically until the command finishes.

### Scenario 5: Background job notifies on completion (P3 — User Story 3)

**Validates**: FR-007, FR-008, SC-004

```bash
# In the interactive REPL:
cargo run -p joey-cli
# Ask: "Start this in the background with notify_on_complete: sleep 5 && echo done"
# Then immediately ask: "What is 2+2?"
```

**Expected**: The agent answers "4" immediately. ~5 seconds later, a
completion notification appears for the background job (exit code 0, "done"
in output) without you having to poll.

### Scenario 6: Backward compatibility — short commands unchanged

**Validates**: FR-009, FR-010, SC-006, SC-007

```bash
cargo run -p joey-cli -- run "Run: echo hello"
```

**Expected**: Result is instantaneous, identical to pre-feature behavior:
`{"output":"hello\n","exit_code":0,"error":null}`.

## Test Suite

```bash
cargo test --workspace
```

All existing ~520+ tests must pass (no regressions). New tests added:
- `joey-tools`: terminal streaming, temp-file round-trip, ring-buffer filling.
- `joey-agent-core`: ToolProgress emission, event ordering.

Run just the new tests:

```bash
cargo test -p joey-tools terminal_streaming
cargo test -p joey-tools process_reaper
cargo test -p joey-agent-core tool_progress
```

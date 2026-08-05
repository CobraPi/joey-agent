# Implementation Plan: Terminal Async Performance & Streaming

**Branch**: `009-terminal-async-perf` | **Date**: 2026-07-30 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `specs/009-terminal-async-perf/spec.md`

## Summary

The agent's terminal tool freezes the UI during heavy commands because it buffers
all output in memory via `spawn_blocking(read_to_end)` until process exit, emits
zero `ToolProgress` events mid-run, and has no mechanism for tools to push events.
Background jobs are worse: their stdout/stderr pipes are never read (the
`RingBuffer` stays empty), and `notify_on_complete` is set but never acted upon
(no reaper task exists).

The fix introduces a progress channel into `ToolContext`, replaces the blocking
read-to-end with async chunked reading that streams `ToolProgress` events and
writes to a temp-file capture, and adds a background reaper task that reads pipes
into the ring buffer and fires completion events. All existing result schemas,
exit codes, and public interfaces remain unchanged.

## Technical Context

**Language/Version**: Rust (stable, edition 2021 per `rust-toolchain.toml`)

**Primary Dependencies**:
- `tokio` (multi-threaded runtime, `tokio::process`, `tokio::time`, `tokio::sync::mpsc`)
- `os_pipe` (merged stdout/stderr pipe — already in use)
- `tempfile` (temp-file capture for large output — new dependency, see research.md)
- `uuid` (session IDs — already in use)

**Storage**: Temp files under `std::env::temp_dir()` for output capture (cleaned up after readback); existing SQLite session store unchanged.

**Testing**: `cargo test --workspace` (workspace ~520+ tests, must stay green); new tests in `joey-tools` (terminal streaming, ring-buffer filling, temp-file round-trip) and `joey-agent-core` (ToolProgress emission, event ordering).

**Target Platform**: Linux/macOS (the existing terminal tool already targets POSIX).

**Project Type**: CLI agent with async tool-dispatch runtime.

**Performance Goals**:
- Output delta from child stdout → `ToolProgress` event: ≤ 1s latency under normal load.
- Peak in-memory buffer per running foreground command: ≤ 64 KB (chunk size), NOT proportional to total output size.
- Fast (sub-second) commands: zero perceptible overhead (temp-file create/delete adds ~microseconds).
- Render loop maintains its existing tick cadence without stalls during any running command.

**Constraints**:
- Backward-compatible: terminal tool result schema (`{output, exit_code, error, exit_code_meaning}`), exit-code semantics, timeout policy (180s default, 600s max), and background session-handle format MUST NOT change (Constitution Principle VII).
- `Tool` trait signature and `ToolContext` public API MUST remain backward-compatible (additive only).
- `cargo build --workspace` and `cargo test --workspace` MUST stay green.
- No new runtime dependency without justification recorded in research.md (Constitution Principle VIII).

**Scale/Scope**: 3 crates touched (`joey-tools`, `joey-agent-core`, `joey-cli`); 25 tasks total (1 setup + 3 foundational + 7 US1 + 2 US2 + 8 US3 + 4 polish); no new crate.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Workspace-First Rust | ✅ Pass | All changes within existing crates (`joey-tools`, `joey-agent-core`, `joey-cli`). No new crate, no root-level code. |
| II. CLI/TUI Parity | ✅ Pass | `ToolProgress` is already consumed by BOTH `joey-cli` render (render.rs:744) and `joey-tui` state (state.rs:577). Streaming progress reaches both surfaces automatically via the shared `AgentEvent` enum. |
| III. Filesystem Source of Truth | ✅ N/A | No spec-kit artifacts or file-backed UI state involved. |
| IV. Test-First for New Crates | ✅ N/A | No new crate. New modules (`streaming`, `reaper`) add tests alongside implementation per existing convention. |
| V. Incremental, Reviewable Delivery | ✅ Pass | Three user stories (P1 streaming, P2 responsiveness, P3 background notify) are independently shippable. Each builds and tests green on its own. |
| VI. Modularity and Decoupling | ✅ Pass | Progress channel added to `ToolContext` behind a narrow, optional API (`with_progress_sender`). Terminal tool depends only on the `ToolContext` abstraction. Reaper task is self-contained. No cross-crate threading of new logic through shared paths. |
| VII. Backward Compatibility (NON-NEGOTIABLE) | ✅ Pass | `Tool` trait unchanged. `ToolContext` gains an additive optional field (builder method, default None — existing callers unaffected). Terminal result schema, exit codes, timeout policy, background session handles all preserved. Regression tests mandated (see tasks.md). |
| VIII. Performance Discipline | ✅ Pass | Temp-file capture (zero-copy disk write, bounded memory). Async chunked reads (no spawn_blocking stall). Chunk size capped at 64 KB. Performance budget recorded above. New dependency (`tempfile`) justified in research.md. |

**Gate result**: PASS — no violations. No Complexity Tracking entries needed.

## Project Structure

### Documentation (this feature)

```text
specs/009-terminal-async-perf/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
│   ├── progress-channel.md
│   └── terminal-streaming.md
└── tasks.md             # Phase 2 output (/speckit-tasks — NOT created by /speckit-plan)
```

### Source Code (repository root)

```text
crates/
├── joey-tools/
│   └── src/
│       ├── context.rs              # MODIFY: add optional progress sender to ToolContext
│       ├── tools/
│       │   ├── terminal_tool.rs    # MODIFY: replace run_bash with streaming version
│       │   └── process_tool.rs     # MODIFY: add reaper task launch, pipe-reader integration
│       └── tests/
│           ├── terminal_streaming.rs  # NEW: streaming + temp-file round-trip tests
│           └── process_reaper.rs      # NEW: ring-buffer filling + completion tests
├── joey-agent-core/
│   └── src/
│       ├── agent.rs                # MODIFY: wire progress sender into ToolContext; emit ToolProgress
│       └── events.rs               # NO CHANGE (ToolProgress already exists)
├── joey-cli/
│   └── src/
│       └── repl.rs                 # NO CHANGE (render_turn already handles ToolProgress)
└── joey-tui/
    └── src/
        └── state.rs                # NO CHANGE (already handles ToolProgress)
```

**Structure Decision**: Minimal, additive changes within 3 existing crates. The streaming
logic lives in `terminal_tool.rs` (where `run_bash` already is), the reaper lives in
`process_tool.rs` (where `ProcessSession` already is), and the progress-channel plumbing
threads through `context.rs` → `agent.rs`. No new crate, no new module file — changes are
localized to the files that already own this logic.

## Complexity Tracking

> No Constitution Check violations — this section is intentionally empty.

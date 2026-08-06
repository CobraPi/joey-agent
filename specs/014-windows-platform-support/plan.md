# Implementation Plan: Windows Platform Support

**Branch**: `014-windows-platform-support` | **Date**: 2026-08-05 | **Spec**: `specs/014-windows-platform-support/spec.md`

**Input**: Feature specification from `/specs/014-windows-platform-support/spec.md`

## Summary

**Primary requirement** (spec FR-001/FR-002): make `cargo build
--workspace` succeed on Windows (x86_64-pc-windows-msvc) without
regressing the Unix (Linux/macOS) build. The build currently fails with
7 hard errors (`E0433`), all confined to a single file:
`crates/joey-tools/src/tools/terminal_tool.rs`, whose foreground command
execution path uses Unix-only APIs (`std::os::unix::io::*`,
`tokio::io::unix::AsyncFd`, `libc::read`) without `#[cfg]` guards.

**Technical approach** (from research.md):
1. **Streaming layer (R1):** extract the Unix-only `AsyncFd`/`os_pipe`
   read path behind a cfg-forked boundary (`OutputChunkStream`). On Unix,
   behavior is byte-for-byte unchanged (feature 009's design preserved —
   Principle VII). On Windows, drop `os_pipe` and read `child.stdout` +
   `child.stderr` via tokio's native `AsyncRead` (same pattern the
   background reaper already uses cross-platform). The shared
   `stream_output` body (throttle, heartbeat, interrupt, temp-file spill)
   is written once.
2. **Shell discovery (R2):** generalize `find_bash()` into a `Shell` enum
   with a bash-first → PowerShell (`pwsh`→`powershell`) fallback on
   Windows, cached per-process (FR-011/FR-013). Add a PowerShell-dialect
   wrapper script that mirrors the bash path's exit-code + CWD-marker
   contract (FR-012).
3. **Test gating (R4):** gate the one Unix-assuming test
   (`grep zz /dev/null`); add cross-platform wrapper-string unit tests.

**Zero new dependencies.** The feature rearranges existing crates
(`tokio`, `which`, `tempfile`) behind cfg gates. See research.md R5.

## Technical Context

**Language/Version**: Rust, stable channel, edition 2021 (per
`rust-toolchain.toml`).

**Primary Dependencies**: tokio (async runtime, `process`, `io`),
`os_pipe` (Unix merged-pipe only), `which` (shell PATH resolution),
`tempfile` (output spill), `libc` (Unix syscalls). All already workspace
deps; **none added**.

**Storage**: N/A — no on-disk format changes. SQLite schema, cron
`jobs.json`, config.yaml all untouched.

**Testing**: `cargo test --workspace` (workspace has ~520+ tests, per
AGENTS.md). New: cross-platform wrapper-string unit tests; Unix-only
integration test gated. See research.md R4.

**Target Platform**: Windows 10/11 (x86_64-pc-windows-msvc) — primary
new target. Linux + macOS — regression gate (must stay green). GNU/MinGW
targets out of scope (spec Assumption).

**Project Type**: CLI agent (existing `joey` binary). This feature adds
no new binary or crate; it modifies one file in `joey-tools`.

**Performance Goals**: First output chunk → progress event ≤ 50ms on
both platforms (parity with feature 009 on Unix). Shell cold-start
≤ 50ms (PowerShell via `-NoProfile`).

**Constraints**: Unix behavior byte-for-byte preserved (Principle VII).
Memory bound per command ≤ 64KB chunk buffer + temp-file spill at 4KB
(unchanged). No new dependencies (Principle VIII). No public-surface
change (no new CLI flags, config keys, file formats, or traits).

**Scale/Scope**: ~150–250 lines of new/changed code in one crate
(`joey-tools`), plus ~1 test gate and ~3 new unit tests. No cross-crate
ripple. Single-PR-sized.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Constitution v1.1.0 (`/.specify/memory/constitution.md`). Every principle
evaluated honestly:

### I. Workspace-First Rust — ✅ PASS
No new crate. The change lives entirely in `crates/joey-tools/`, an
existing workspace member. `cargo build -p joey-tools` and `cargo test
-p joey-tools` remain the scoped build/test commands.

### II. CLI/TUI Parity — ✅ PASS (N/A)
The terminal tool is invoked identically from `joey-cli` and `joey-tui`.
This feature changes *internal* execution mechanics, not the tool's
surface (same name, same parameters, same JSON result shape). No new UI;
no parity divergence possible.

### III. Filesystem Is the Source of Truth (NON-NEGOTIABLE) — ✅ PASS (N/A)
This feature touches no spec-kit artifacts, no `.specify/` files as data,
no file-backed UI state. It is a pure code refactor.

### IV. Test-First for New Crates — ✅ PASS
No new crate. New logic (shell resolution, PowerShell wrapper generation)
gets unit tests alongside implementation per research.md R4:
- `resolve_shell()` returns `Bash` on Unix, does not panic on Windows.
- PowerShell wrapper script contains `$LASTEXITCODE` / `$PWD` / marker.
- `extract_cwd_marker` parses PowerShell-produced markers identically.

### V. Incremental, Reviewable Delivery — ✅ PASS
The feature decomposes into the prioritized user stories (P1 build → P2
foreground → P3 background → P4 tests). Each ships independently: P1
alone produces a compiling Windows binary (the user's literal ask). The
plan phases map 1:1 to the stories.

### VI. Modularity and Decoupling — ✅ PASS
The new boundary (`OutputChunkStream`, `Shell`, `WrapperScript`) is a
narrow, explicit internal interface inside `terminal_tool`. It depends
only on tokio primitives, not on sibling module internals. No new
cross-crate coupling; the change is localized to `joey-tools`. Existing
callers (`joey-agent-core`, `joey-cli`) see no API change.

### VII. Backward Compatibility and Non-Regression (NON-NEGOTIABLE) — ✅ PASS
- **No public-surface change.** The `Tool` trait impl for `Terminal`
  (name, parameters, description, result shape) is unchanged. No CLI
  flag, config key, on-disk format, or trait is altered.
- **Unix path is preserved byte-for-byte.** The `os_pipe` + `AsyncFd` +
  `libc::read` logic is extracted behind `#[cfg(unix)]` verbatim — same
  syscalls, same chunk size, same throttle. Feature 009's perf
  characteristics are untouched.
- **Regression coverage mandated.** research.md R4 specifies new unit
  tests asserting (a) Unix shell resolution unchanged, (b) PowerShell
  wrapper string contract, (c) CWD-marker parse parity. The quickstart
  requires `cargo build` + `cargo test` green on Unix as a gate.
- **Exit-code semantics:** `exit_code_from_status` is already correctly
  cfg-forked; no change. Windows uses `ExitStatus::code()` (no signal
  concept) — already the case.

### VIII. Performance Discipline and Lean Code — ✅ PASS
- **No new dependencies** (research.md R5). Zero binary-size or
  compile-time cost from new crate graphs.
- **Performance budgets recorded** (research.md "Performance-sensitive
  paths"): ≤50ms first-chunk on both platforms; ≤50ms PowerShell
  cold-start via `-NoProfile`; unchanged 64KB/4KB/50ms/2s/100ms
  streaming constants.
- **No speculative abstraction.** The `OutputChunkStream` boundary exists
  because two concrete platform impls are genuinely needed — it is not
  generalization for its own sake. `Shell` has exactly two variants
  because exactly two shell families are supported.

**Gate result: PASS on all eight principles. No violations. Complexity
Tracking section is empty (no justified deviations needed).**

## Project Structure

### Documentation (this feature)

```text
specs/014-windows-platform-support/
├── spec.md              # Feature spec (from /speckit-specify + /speckit-clarify)
├── plan.md              # This file (/speckit-plan output)
├── research.md          # Phase 0 — streaming/shell/dependency research
├── data-model.md        # Phase 1 — in-memory entities (Shell, OutputChunkStream)
├── quickstart.md        # Phase 1 — Win+Unix end-to-end validation
├── contracts/
│   ├── shell-discovery.md          # resolve_shell() contract (FR-011, FR-013)
│   └── streaming-and-execution.md  # OutputChunkStream + wrapper scripts (FR-003/004/005/012)
└── tasks.md             # Phase 2 (/speckit-tasks — NOT created by this command)
```

### Source Code (repository root)

```text
crates/
└── joey-tools/
    └── src/
        └── tools/
            └── terminal_tool.rs   # ← the ONE file modified by this feature
                ├── use std::os::unix::io::AsRawFd   # → moved inside #[cfg(unix)]
                ├── find_bash() -> String            # → generalized to resolve_shell() -> Shell
                ├── run_bash()                       # → cfg-forked: run_command_unix / run_command_windows
                ├── stream_output(async_fd, ...)     # → takes OutputChunkStream, shared body
                ├── OwnedFd / AsyncFd glue           # → #[cfg(unix)] only
                └── #[cfg(test)] mod tests           # → 1 test gated, 3 unit tests added
```

**Structure Decision**: Single-file modification in an existing crate.
No new modules, files, or crates — the change is small enough (~150–250
LOC) that splitting `terminal_tool.rs` further would be premature (the
file is already cohesive at 975 lines and this feature adds a parallel
code path, not a new responsibility). The cfg-forked functions live
adjacently in the same file, which is the established pattern in this
codebase (cf. `memory_tool.rs` FileLock, `process_tool.rs`
exit_code_from_status — both inline cfg forks).

If, during implementation, the file grows past ~1200 lines or the
Windows path proves to need substantial helper functions, a follow-up
refactor could extract a `terminal_tool/shell.rs` submodule — but that
is an implementation judgment, not a plan-level decision, and is not
required to meet the spec.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

No constitution violations identified. This section is intentionally empty.

The feature is strictly additive behind cfg gates: it adds a Windows
code path without altering any Unix code path's behavior, introduces no
new dependencies, changes no public surfaces, and ships with regression
coverage. All eight principles pass cleanly.

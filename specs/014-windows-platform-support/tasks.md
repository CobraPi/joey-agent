# Tasks: Windows Platform Support

**Input**: Design documents from `/specs/014-windows-platform-support/`
(plan.md, research.md, data-model.md, contracts/, quickstart.md)

**Prerequisites**: plan.md ✓, spec.md ✓, research.md ✓, data-model.md ✓, contracts/ ✓, quickstart.md ✓

**Tests**: Regression + contract tests are MANDATORY for this feature (Constitution Principle VII — the terminal tool is a public tool surface and the Unix code path must be preserved byte-for-byte).

**Organization**: Tasks grouped by user story. All tasks modify a SINGLE file (`crates/joey-tools/src/tools/terminal_tool.rs`) unless noted, so most are sequential within a phase. `[P]` is used only where two tasks genuinely touch different files or locations.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, or non-overlapping regions with no dependency)
- **[Story]**: User story for traceability (US1–US4)
- Exact file paths + line numbers included where stable

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: No project-structure work needed — this feature modifies one existing file in an existing crate. No new dependencies (research.md R5). Phase 1 is intentionally empty.

*(no tasks — skip to Phase 2)*

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Introduce the cross-platform abstractions (`Shell` enum, `OutputChunkStream` boundary, `WrapperScript`) that all user stories depend on. These are pure type/function definitions with NO behavior change yet — they unblock the cfg-forking in US1.

**⚠️ CRITICAL**: No user-story work can begin until this phase is complete. The abstractions defined here are referenced by every subsequent task.

All tasks in this phase touch `crates/joey-tools/src/tools/terminal_tool.rs` and are sequential (each builds on the prior).

- [ ] T001 [US1] Define `Shell` enum in `crates/joey-tools/src/tools/terminal_tool.rs` (near `find_bash` ~line 47). Two variants: `Bash(String)` and `PowerShell(String)`. Derive `Clone, Debug, PartialEq`. Cross-platform (no cfg gates on the enum itself). Per data-model.md "Shell" entity.

- [ ] T002 [US1] Implement `resolve_shell()` function in `crates/joey-tools/src/tools/terminal_tool.rs` (replaces the resolution logic currently inside `find_bash`, lines 47–62). Returns `Result<Shell, ShellResolutionError>`. Resolution order per contracts/shell-discovery.md: Unix → bash candidates → `Shell::Bash`; Windows → bash → pwsh → powershell → `Shell::*` or `Err`. Define `ShellResolutionError` struct with a `tried: Vec<&'static str>` field and a `Display` impl naming the shells. (depends on T001)

- [ ] T003 [US1] Add the per-process shell cache: `static RESOLVED_SHELL: Lazy<Mutex<Option<Shell>>>` in `crates/joey-tools/src/tools/terminal_tool.rs`. Wrap `resolve_shell()` so the first call resolves + stores, subsequent calls return the cached clone (FR-013). (depends on T002)

- [ ] T004 [US1] Define the `OutputChunkStream` boundary as a conceptual interface (see contracts/streaming-and-execution.md Part A). This is NOT a `dyn` trait — it is two concrete cfg-selected types sharing the same `async fn next_chunk(&mut self) -> Option<Vec<u8>>` shape. Add a doc comment documenting the contract (≤64KB chunks, `None` once at EOF, raw bytes). The concrete Unix and Windows impls land in US1 (T008) and US2 (T015). (No code yet — just the doc boundary that T008/T015 implement; this task is the placeholder ensuring the boundary is agreed before forking.)

- [ ] T005 [US1] Define `build_wrapper_script(shell: &Shell, command: &str) -> WrapperScript` signature + `WrapperScript` struct (`shell`, `argv0`, `args: Vec<String>`, `body: String`) in `crates/joey-tools/src/tools/terminal_tool.rs`. Implement ONLY the `Shell::Bash` arm for now (move the existing format! from `run_bash` lines 476–480 into it). The `PowerShell` arm is a `todo!()` filled in US2 (T014). (depends on T001)

**Checkpoint**: Abstractions in place. `cargo build -p joey-tools` still fails on the unguarded Unix code (expected — US1 fixes that), but the new types compile on Windows where they're cfg-gated out. Proceed to US1.

---

## Phase 3: User Story 1 — The Agent Builds on Windows (Priority: P1) 🎯 MVP

**Goal**: `cargo build --workspace` (and `cargo build -p joey-tools`) succeeds on Windows with exit 0, no regression on Unix. This is the literal blocker the user reported.

**Independent Test** (quickstart.md Phase 1): On Windows, `cargo build --workspace` → exit 0, `joey.exe --version` works. On Unix, `cargo build --workspace` → exit 0 (unchanged).

### Implementation for User Story 1

These tasks split the unguarded Unix streaming path behind cfg gates. All touch `crates/joey-tools/src/tools/terminal_tool.rs`. Sequential — each isolates one Unix-only region.

- [ ] T006 [US1] Move the module-level `use std::os::unix::io::AsRawFd as _;` (line 11) inside a `#[cfg(unix)]` block, OR delete it and import locally where used. This is the root cause of 1 of the 7 E0433 errors. Verify the Unix build still compiles after this move.

- [ ] T007 [US1] Gate the `OwnedFd` struct + its `impl AsRawFd`, `impl Drop`, `unsafe impl Send/Sync` (lines 585–604) behind `#[cfg(unix)]`. These reference `std::os::unix::io::RawFd` and `libc::close` — Windows-incompatible. No behavior change on Unix (the symbols are just now cfg-gated).

- [ ] T008 [US1] Implement the Unix concrete `OutputChunkStream` (`UnixFdReader`) behind `#[cfg(unix)]` in `crates/joey-tools/src/tools/terminal_tool.rs`. This is an EXTRACTION of the existing `AsyncFd<OwnedFd>` + `readable()` + `libc::read` logic from `run_bash`/`stream_output` (lines 510–538, 703–727) into the new type implementing `next_chunk()`. Byte-for-byte identical behavior on Unix (research.md R1). (depends on T004, T007)

- [ ] T009 [US1] Rewrite `stream_output` (line 639) to take the `OutputChunkStream` boundary instead of a raw `async_fd: AsyncFd<OwnedFd>` parameter. The function BODY (throttle/heartbeat/interrupt/temp-file spill/readback, lines 644–791) stays IDENTICAL — only the read site changes from `async_fd.readable()` + `libc::read` to `stream.next_chunk().await`. This is the shared cross-platform body. (depends on T008)

- [ ] T010 [US1] Gate the existing `run_bash` function (lines 468–581) behind `#[cfg(unix)]` and rename it `run_command_unix`. Update it to: (a) call `resolve_shell()` instead of `find_bash()`, (b) build the wrapper via `build_wrapper_script` (T005), (c) construct `UnixFdReader` and pass it to the rewritten `stream_output`. No behavioral change on Unix. (depends on T002, T005, T008, T009)

- [ ] T011 [US1] Add `run_command_windows` behind `#[cfg(not(unix))]` in `crates/joey-tools/src/tools/terminal_tool.rs`. For US1 this is a MINIMAL stub that allows compilation: it spawns via `Stdio::piped()`, takes `child.stdout`/`child.stderr`, reads them to completion via `tokio::io::AsyncReadExt` (blocking-style, no streaming yet), and returns `(output, exit_code, timed_out, false)`. Full streaming + PowerShell support lands in US2. The stub exists so the workspace COMPILES on Windows. (depends on T002, T005, T009)

- [ ] T012 [US1] Update the call site in `execute()` (line 282) to call the cfg-selected function: `run_command_unix` on Unix, `run_command_windows` on Windows, via a `#[cfg]` fork or a thin dispatcher. Delete the old `find_bash()` function (lines 47–62) — its logic now lives in `resolve_shell()`. (depends on T010, T011)

**Checkpoint**: `cargo build -p joey-tools` succeeds on Windows (US1 scenarios 1–2 from quickstart.md Phase 1 pass). `cargo build --workspace` on Unix still succeeds (scenario 3). `joey --version` works on Windows. The MVP is delivered — the agent builds. Commit here.

---

## Phase 4: User Story 2 — Terminal Tool Runs Commands on Windows (Priority: P2)

**Goal**: Foreground terminal commands execute correctly on Windows via BOTH the bash path (Git Bash present) and the PowerShell fallback (bash absent), with streaming output, correct exit codes, CWD tracking, timeout, and interrupt.

**Independent Test** (quickstart.md Phase 2): `echo hello` returns output + exit_code 0 on Windows; merged stderr works; CWD persists; PowerShell fallback works when bash is removed from PATH.

### Implementation for User Story 2

- [ ] T013 [US2] Implement the Windows concrete `OutputChunkStream` (`WindowsPipeReader`) behind `#[cfg(not(unix))]` in `crates/joey-tools/src/tools/terminal_tool.rs`. Holds `child.stdout` + `child.stderr`; `next_chunk()` uses `tokio::select!` over `AsyncReadExt::read_buf` on both (64KB buffer), returns `None` at double-EOF. Per contracts/streaming-and-execution.md Part A Windows impl. (depends on T004, T011)

- [ ] T014 [US2] Implement the `Shell::PowerShell` arm of `build_wrapper_script` (the `todo!()` from T005). Generate the PowerShell-dialect body per contracts/streaming-and-execution.md Part B: `$LASTEXITCODE`/`$?` exit-code normalization, `Write-Output "\`n{m}$PWD{m}"` CWD marker, `exit $code`. Args: `["-NoProfile", "-Command", body]`. (depends on T005)

- [ ] T015 [US2] Rewrite `run_command_windows` (from T011) to use `WindowsPipeReader` (T013) + the rewritten `stream_output` (T009) instead of the blocking stub. Spawn the child with the shell from `resolve_shell()` (bash → `-c`, PowerShell → `-NoProfile -Command`), `Stdio::piped()` for stdout+stderr, `stdin::null()`, sanitized env. Wire timeout (124 on expiry) and cooperative interrupt (poll `ctx.is_interrupted()`). The streaming body (throttle/heartbeat/spill) is shared via `stream_output`. (depends on T013, T014)

- [ ] T016 [US2] Verify `extract_cwd_marker` (line 438) parses PowerShell-produced markers identically to bash-produced ones. The parser only matches `CWD_MARKER` framing (shell-agnostic), so NO code change should be needed — but add an assertion in the test (T019) proving it. If a discrepancy is found (e.g. PowerShell `Write-Output` adds different whitespace), fix the PowerShell wrapper in T014, not the parser.

**Checkpoint**: Foreground commands work on Windows end-to-end via both shells. Quickstart.md Phase 2 scenarios pass (bash-first + PowerShell fallback + no-shell error). Commit here.

---

## Phase 5: User Story 3 — Background Processes Work on Windows (Priority: P3)

**Goal**: `background=true`, `process` tool (poll/kill), and ProcessRegistry lifecycle work on Windows.

**Independent Test** (quickstart.md Phase 3): start bg process → get session_id → poll → kill; start bg process → wait → poll shows exit code.

### Implementation for User Story 3

- [ ] T017 [US3] Verify `execute_background` (line 351) works on Windows without modification. It already uses `tokio::process::Command` with `Stdio::piped()` (portable) and `find_bash()` → now must call `resolve_shell()` instead. Update line 363 (`let bash = find_bash();`) to use the resolved shell. Audit the spawn path for any remaining `find_bash` references. (depends on T012 — `find_bash` is deleted there)

- [ ] T018 [US3] Audit `crates/joey-tools/src/tools/process_tool.rs` for any Unix-only code in the kill/poll/reaper paths. The exit-code extraction there is already cfg-gated (line 356). Confirm `spawn_reaper` uses only portable tokio APIs. Fix any gaps found. (Can run in parallel with T017 — different file.)

**Checkpoint**: Background process lifecycle works on Windows. Quickstart.md Phase 3 passes. Commit here.

---

## Phase 6: User Story 4 — Test Suite Green on Windows (Priority: P4)

**Goal**: `cargo test --workspace` passes on Windows; Unix-only tests are explicitly gated; regression coverage for Principle VII is in place.

**Independent Test** (quickstart.md Phase 4): `cargo test --workspace` on Windows completes without unexpected failures; on Unix, all prior tests still pass.

### Tests for User Story 4 (regression + contract — MANDATORY per Principle VII)

- [ ] T019 [P] [US4] Add a unit test in `crates/joey-tools/src/tools/terminal_tool.rs` `#[cfg(test)] mod tests`: assert `extract_cwd_marker` parses a PowerShell-produced marker string identically to a bash-produced one (same input bytes between markers → same output). Pure string test, runs on all platforms. (depends on T016)

- [ ] T020 [P] [US4] Add a unit test in `crates/joey-tools/src/tools/terminal_tool.rs` `#[cfg(test)] mod tests`: assert `build_wrapper_script(&Shell::PowerShell(_), "echo hi")` produces a body containing `$LASTEXITCODE`, `$PWD`, and `CWD_MARKER`. Pure string assertion, runs on all platforms. (depends on T014)

- [ ] T021 [P] [US4] Add a unit test in `crates/joey-tools/src/tools/terminal_tool.rs` `#[cfg(test)] mod tests`: assert `resolve_shell()` does not panic on Windows and returns `Shell::Bash(_)` on Unix (assert variant via `cfg!`). Verifies FR-011/FR-013 resolution + cache. (depends on T003)

- [ ] T022 [US4] Gate the `exit_code_meaning_table` test (line 919) — it runs `grep zz /dev/null` which assumes Unix. Wrap it `#[cfg(unix)]`. The pure-logic `interpret_exit_code` assertions within it can be extracted into a separate cross-platform test if desired, but the minimum is gating the subprocess-dependent part. (research.md R4)

**Checkpoint**: `cargo test --workspace` green on Windows (Unix-only tests gated, not failing). `cargo test --workspace` on Unix has zero regressions. Commit here.

---

## Phase 7: Polish & Cross-Cutting Concerns

**Purpose**: Final validation, grep audit, and documentation.

- [ ] T023 [US4] Run the SC-005 grep audit from quickstart.md: `grep -rn "std::os::unix\|tokio::io::unix" crates/joey-tools/src/tools/terminal_tool.rs` — confirm every hit is inside a `#[cfg(unix)]` block. Any hit in a Windows-compiled context is a bug to fix.

- [ ] T024 [US4] Update `PORTING.md` (repo root) with a note that Windows platform support is now Complete for the terminal tool, per the AGENTS.md convention that PORTING.md is a living audit document. Note the one deliberate limitation (PowerShell fallback, no PTY).

- [ ] T025 [US4] Run full quickstart.md validation on Windows (all 4 phases) and on Unix (regression). Document results. This is the final acceptance gate for SC-001 through SC-006. Also verify FR-010 (non-regression): confirm `reset_sigpipe()` in `crates/joey-cli/src/main.rs` still has its `#[cfg(unix)]`/`#[cfg(not(unix))]` gates intact (lines ~427–434) — it is already correct and needs no implementation, only preservation.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: Empty — no work. Skip.
- **Foundational (Phase 2)**: No deps. **BLOCKS all user stories.** T001→T002→T003 (sequential), T004 (standalone doc), T005 (depends T001).
- **User Story 1 (Phase 3)**: Depends on Foundational. T006→T007→T008→T009→T010, T011 (parallel with T010 after T009), T012 (after T010+T011). **This is the MVP — deliverable alone.**
- **User Story 2 (Phase 4)**: Depends on US1 (specifically T011's stub + T009's shared `stream_output`). T013, T014 (parallel-ish, different regions), T015 (after T013+T014), T016 (verification).
- **User Story 3 (Phase 5)**: Depends on US1 (T012 deletes `find_bash`). T017 + T018 can run in parallel (different files).
- **User Story 4 (Phase 6)**: Depends on US2 (wrapper script) + US1 (resolve_shell). T019/T020/T021 are `[P]` (independent test functions, same file but non-overlapping). T022 independent.
- **Polish (Phase 7)**: Depends on all stories. Sequential final validation.

### User Story Dependencies

- **US1 (P1)**: Starts after Foundational. No dependency on other stories. **Deliverable as MVP.**
- **US2 (P2)**: Starts after US1 (needs the Windows stub T011 + shared stream_output T009). Independently testable once US1 compiles.
- **US3 (P3)**: Starts after US1 (needs `find_bash` deletion from T012). The background path is mostly already-portable; US3 is verification + small wiring.
- **US4 (P4)**: Starts after US1+US2 (needs the implemented abstractions to test against). Regression + contract tests.

### Within Each User Story

- Abstractions/types before behavior (Foundational pattern)
- Unix preservation (cfg-gate existing code) before Windows implementation
- Stub that compiles before full implementation (T011 before T015)
- Tests after the code they test exists (US4 last, but tests are cross-platform string/unit assertions)

### Parallel Opportunities

- **Limited** because this is a single-file feature. Genuine parallelism:
  - T017 (terminal_tool.rs bg path) ∥ T018 (process_tool.rs) — different files
  - T019 ∥ T020 ∥ T021 — independent test functions added to the same `mod tests` (non-overlapping)
  - T022 — independent of T019–T021
- All other tasks are sequential within their phase (same file, overlapping or adjacent regions).

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 2: Foundational (T001–T005)
2. Complete Phase 3: User Story 1 (T006–T012)
3. **STOP and VALIDATE**: `cargo build --workspace` on Windows (exit 0) + Unix (exit 0, no regression). Run `joey --version`.
4. This alone resolves the user's reported blocker ("build is failing on windows machines").

### Incremental Delivery

1. Foundational → abstractions ready
2. US1 → **Windows build works** (MVP — the user's literal ask) ✅
3. US2 → terminal commands actually run on Windows (both shells)
4. US3 → background processes work
5. US4 → test suite green, regression coverage in place
6. Polish → audit + PORTING.md + final validation

---

## Notes

- Single-file feature (`crates/joey-tools/src/tools/terminal_tool.rs`), ~150–250 LOC changed. Minimal parallelism opportunity — most tasks are sequential.
- **Unix behavior is sacred** (Principle VII). Every Unix-code-gating task (T006–T010) must preserve byte-for-byte behavior. The extraction in T008/T009 is mechanical: move code, don't rewrite it.
- The Windows stub (T011) is intentionally minimal — it exists only to make `cargo build` succeed for the US1 checkpoint. Full streaming lands in US2 (T015). Do not gold-plate the stub.
- `find_bash()` is deleted in T012 — ensure ALL call sites (line 282 foreground, line 363 background) are migrated to `resolve_shell()` before deletion.
- Commit after each task or logical group (per template guidance). The US1 checkpoint is a natural commit boundary (MVP).

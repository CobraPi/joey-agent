# Feature Specification: Windows Platform Support

**Feature Branch**: `014-windows-platform-support`

**Created**: 2026-08-05

**Status**: Draft

**Input**: User description: "make this agent compatible with windows - right now the build is failing on windows machines"

## Context (Evidence-Grounded)

The agent is a Rust workspace that compiles cleanly on Unix (Linux/macOS)
but **fails to compile on Windows**. The failure was reproduced locally on
the target Windows 10 machine (`cargo build --workspace`, exit code 101).

The root cause is **not** a widespread Unix-only-API problem across the
codebase. An audit of all 12 crates shows the vast majority of
platform-specific code is **already correctly cfg-guarded** with
`#[cfg(unix)]` / `#[cfg(not(unix))]` branches:

- `joey-core` (config, constants, utils, auth_store, lib, logging) — guarded
- `joey-mcp` (config, lib kill path) — guarded
- `joey-cron` (jobs) — guarded
- `joey-cli` (doctor_cmd, main SIGPIPE, secret_prompt) — guarded
- `joey-tools` sibling files (`memory_tool.rs` flock, `process_tool.rs`
  exit status, `vcs.rs` perms) — guarded

The **sole compile blocker** is a single file:
`crates/joey-tools/src/tools/terminal_tool.rs`. Its foreground command
execution path (`run_bash` → `stream_output` → `OwnedFd`) uses Unix-only
APIs **without cfg guards**:

- `std::os::unix::io::AsRawFd` (module-level import, line 11)
- `std::os::unix::io::RawFd` (OwnedFd struct, line 585)
- `tokio::io::unix::AsyncFd` (lines 526, 640)
- `libc::dup` / `libc::close` / `libc::read` (lines 515, 530, 596, 712)

This produces **7 hard errors** (`E0433: cannot find 'unix' in 'os'/'io'`),
which halt compilation of `joey-tools` and therefore every crate that
depends on it (`joey-agent-core`, `joey-cli`, etc.).

Beyond compilation, there are **secondary runtime/test concerns** to
address for the agent to actually *work* on Windows, not just build.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - The Agent Builds on Windows (Priority: P1)

A developer clones the repository onto a Windows machine with the Rust
toolchain installed and runs `cargo build --workspace`. The build
completes successfully and produces the `joey` binary, with zero
compile errors related to Unix-only APIs.

**Why this priority**: This is the literal blocker the user reported.
Nothing else matters if the code does not compile. It is the MVP —
delivering this alone unblocks all downstream Windows work.

**Independent Test**: Run `cargo build --workspace` on Windows and
confirm exit code 0. Then run `cargo build -p joey-tools` in isolation
to confirm the previously-failing crate now builds. Deliverable: a
green Windows build.

**Acceptance Scenarios**:

1. **Given** a Windows 10/11 machine with stable Rust toolchain, **When**
   the developer runs `cargo build --workspace`, **Then** the build
   completes with exit code 0 and no `E0433`/`E0432`/`E0573` errors.
2. **Given** the same machine, **When** the developer runs
   `cargo build -p joey-tools`, **Then** it compiles in isolation.
3. **Given** a Unix (Linux/macOS) machine, **When** the developer runs
   `cargo build --workspace`, **Then** the build still succeeds
   (regression: Unix must not break).
4. **Given** the Windows binary, **When** the developer runs
   `joey --version` (or `joey --help`), **Then** it prints version/help
   and exits 0.

---

### User Story 2 - The Terminal Tool Runs Commands on Windows (Priority: P2)

A user runs the `joey` agent on Windows and invokes the `terminal` tool
to execute a shell command (e.g. `git status`, `cargo test`, `ls`).
The command executes, its combined stdout/stderr is captured, the exit
code is reported correctly, and the session's working directory is
tracked across calls.

**Why this priority**: The terminal tool is the agent's primary actuator.
Without it, a compiling binary is useless for real work. This is the
first scenario that makes the agent *functional* on Windows, not just
*buildable*.

**Independent Test**: On Windows, run `joey` and ask it to execute
`echo hello` via the terminal tool. Confirm the output contains "hello"
and exit_code is 0. Then execute `cargo --version` and confirm capture.
Deliverable: working foreground command execution.

**Acceptance Scenarios**:

1. **Given** the agent running on Windows, **When** the terminal tool
   executes `echo hello`, **Then** the result contains `"output"` with
   "hello" and `"exit_code": 0`.
2. **Given** the agent running on Windows, **When** the terminal tool
   executes a failing command (e.g. `false` or `exit 7`), **Then**
   the result reports the correct non-zero `"exit_code"`.
3. **Given** the agent running on Windows, **When** the terminal tool
   executes `cargo --version`, **Then** the captured output includes
   the cargo version string.
4. **Given** a command that prints to both stdout and stderr, **When**
   it runs, **Then** both streams are merged into the single `output`
   field (parity with Unix behavior).
5. **Given** the agent running on Windows, **When** the terminal tool
   runs `pwd` (or equivalent), **Then** the session cwd is recorded
   and a subsequent command without an explicit workdir inherits it.
6. **Given** a Windows machine with NO bash on PATH but PowerShell
   present, **When** the terminal tool executes `echo hello`, **Then**
   the PowerShell fallback is used, the output contains "hello",
   `exit_code` is 0, and the CWD marker is parsed correctly (FR-012).

---

### User Story 3 - Background Processes & Process Management Work on Windows (Priority: P3)

A user runs a long-lived command in the background via
`background=true`, then polls it with the `process` tool, and
optionally kills it. This works on Windows the same way it works on
Unix.

**Why this priority**: Background execution is essential for long-running
tasks (servers, builds, watchers). It depends on P1/P2 being in place
but adds the process-registry lifecycle on top.

**Independent Test**: On Windows, start a background `sleep`/`timeout`
command, poll it, confirm it is still running, then kill it. Confirm
the process registry reports correct status transitions.

**Acceptance Scenarios**:

1. **Given** the agent on Windows, **When** a command is started with
   `background=true`, **Then** a `session_id` is returned and the
   process is registered.
2. **Given** a registered background process, **When** the user calls
   `process(action="poll", session_id=...)`, **Then** current output
   and status are returned.
3. **Given** a registered background process, **When** the user calls
   `process(action="kill", session_id=...)`, **Then** the process is
   terminated and its status reflects that.
4. **Given** a background process that exits on its own, **When** it
   is polled afterward, **Then** the recorded exit code and final
   output are correct.

---

### User Story 4 - Workspace Test Suite is Green on Windows (Priority: P4)

A developer runs `cargo test --workspace` on Windows and the suite
passes (or, where a test is fundamentally Unix-specific, it is
explicitly `#[cfg(unix)]`-gated with a documented Windows equivalent
or skipped with rationale).

**Why this priority**: The constitution (Principle VII) mandates
`cargo test --workspace` stay green. Windows-specific test failures
would erode confidence and block CI. This hardens the porting work.

**Independent Test**: Run `cargo test --workspace` on Windows; confirm
either all-pass or that any skips/failures are explicitly gated and
documented, not silent.

**Acceptance Scenarios**:

1. **Given** the repository on Windows, **When** the developer runs
   `cargo test --workspace`, **Then** the run completes without
   panics/compile errors in test code.
2. **Given** a test that asserts Unix-only behavior (e.g. `libc::flock`,
   `std::os::unix::fs::symlink`), **When** compiled on Windows, **Then**
   it is excluded via `#[cfg(unix)]` rather than failing.
3. **Given** the repository on Unix, **When** the developer runs
   `cargo test --workspace`, **Then** all previously-passing tests
   still pass (no regression from added cfg gates).

---

### Edge Cases

- **No bash available on Windows**: If Git Bash is not on PATH, the
  tool falls back to `pwsh.exe`, then `powershell.exe` (FR-011). If
  none of the three is found, `find_bash()`/shell discovery returns a
  clear error naming the shells it looked for, and the terminal tool
  surfaces it as a spawn-failure message — never a panic.
- **Shell differs across the session**: Once a shell is chosen for a
  session it is cached (FR-013); the session does not flip between
  bash and PowerShell even if PATH changes.
- **Command timeout on Windows**: A foreground command exceeding the
  timeout must be killed and report `exit_code: 124` with
  `timed_out: true`, identical to Unix.
- **Large output on Windows**: Output capture (in-memory → temp-file
  spill) must continue to bound memory; the temp-file path uses the
  Windows temp dir (`%TEMP%`).
- **Paths with spaces / non-ASCII**: Windows paths frequently contain
  spaces (e.g. `Program Files`) and may be non-ASCII. Command spawning
  and cwd resolution must handle these without corruption.
- **Cooperative interrupt (Ctrl-C)**: The interrupt-polling during
  streaming must still function on Windows so long-running commands
  can be cancelled.
- **Line endings**: Captured command output on Windows may contain CRLF;
  downstream parsing/redaction that assumes LF must not break.
- **symlink fallback (`copy_dir_recursive`)**: `joey-core::utils` already
  handles `EXDEV`/`EBUSY` via a non-symlink copy fallback; confirm this
  path is exercised correctly on Windows where symlinks are restricted.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: `cargo build --workspace` MUST succeed on Windows
  (x86_64-pc-windows-msvc) with exit code 0.
- **FR-002**: `cargo build --workspace` MUST continue to succeed on
  Unix (Linux/macOS) — no regression.
- **FR-003**: The `terminal` tool's foreground execution path
  (`run_bash` and its streaming helper) MUST be platform-aware: on
  Unix it uses the existing `AsyncFd`/`os_pipe`/`libc::read` path;
  on Windows it uses a portable async read path that does not reference
  `std::os::unix` or `tokio::io::unix`.
- **FR-004**: The terminal tool MUST capture combined stdout+stderr,
  report the correct exit code, honor the timeout contract (default
  180s, hard max 600s foreground, `124` on timeout), and support
  cooperative interrupt — on both Windows and Unix.
- **FR-005**: The platform-specific streaming code MUST be isolated
  behind a clean internal boundary (e.g. a `#[cfg(unix)]`/`#[cfg(not(unix))]`
  function pair or a small trait) so that no Unix-only symbol is
  referenced in a context compiled on Windows.
- **FR-006**: Background process management (`background=true`, the
  `process` tool, the ProcessRegistry) MUST work on Windows, including
  spawn, poll, kill, and exit-code extraction.
- **FR-007**: Exit-code extraction (`exit_code_from_status`) MUST handle
  Windows `ExitStatus::code()` correctly (no signal concept) — the
  existing `#[cfg(not(unix))]` branch is the model.
- **FR-008**: `cargo test --workspace` MUST pass on Windows, or any
  Unix-only test MUST be explicitly `#[cfg(unix)]`-gated with a brief
  rationale comment.
- **FR-009**: Any test that hardcodes Unix-only assumptions (e.g.
  `/tmp/...` paths used as real filesystem paths, `std::os::unix::fs`
  calls outside a cfg gate) MUST be adjusted so it either runs portably
  or is gated.
- **FR-010**: `joey-cli` startup (SIGPIPE handling in `main.rs`) MUST
  not reference `libc::signal`/`SIGPIPE` on Windows; the existing
  `#[cfg(unix)]` gate is the model and MUST be preserved.
- **FR-011**: On Windows, the terminal tool MUST use a bash-first
  shell discovery with a PowerShell fallback: probe for `bash` (Git
  Bash on PATH) first; if absent, fall back to `pwsh.exe`, then
  `powershell.exe`. If no shell is found, the tool MUST return a clear
  spawn-failure error naming the shells it looked for (no panic).
  (Decision: PowerShell-only fallback, cmd.exe excluded — see
  Clarifications 2026-08-05.)
- **FR-012**: When the terminal tool runs a command via PowerShell
  (bash absent), it MUST produce behavior equivalent to the bash path:
  (a) execute the user command, (b) capture the exit code via
  `$LASTEXITCODE` (for native executables) or `$?` (for cmdlets),
  (c) print the session cwd surrounded by the existing `CWD_MARKER`
  framing via `$PWD`/`Get-Location`, and (d) exit with the captured
  code. The `extract_cwd_marker` parser MUST work identically across
  both shells.
- **FR-013**: The shell-selection logic MUST be cached per-session (or
  per-process): once a shell is discovered, subsequent terminal calls
  reuse it without re-probing, so a session does not flip between bash
  and PowerShell across calls.

### Key Entities *(include if feature involves data)*

- **OutputChunkStream**: The abstraction over the command-output reader
  (named `OutputChunkStream` in plan.md, data-model.md, and contracts/;
  concrete impls `UnixFdReader` on Unix and `WindowsPipeReader` on
  Windows). On Unix: a `tokio::io::unix::AsyncFd<OwnedFd>` wrapper
  supporting non-blocking `read`. On Windows: an async reader over the
  child's piped stdout/stderr (e.g. tokio's native `AsyncRead` over
  `ChildStdout`). This entity is the primary new abstraction introduced
  by this feature; its shape is an implementation concern (plan.md),
  but the spec requires that such a boundary exist.
- **ExitCode**: Cross-platform exit-code derivation. Unix retains
  signal-aware extraction; Windows uses `ExitStatus::code()`. Already
  partially modeled by `exit_code_from_status`; this feature ensures
  the model is consistently applied.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: `cargo build --workspace` exits 0 on Windows
  (x86_64-pc-windows-msvc), producing the `joey` binary. (Measured:
  zero compile errors; binary exists at `target/debug/joey.exe`.)
- **SC-002**: `cargo build --workspace` exits 0 on Linux/macOS
  (regression gate; measured: zero new errors vs. the pre-feature
  baseline).
- **SC-003**: The terminal tool successfully executes commands on
  Windows and returns correct output + exit code via BOTH the bash
  path and the PowerShell fallback path (measured by acceptance
  scenarios in User Story 2, including scenario 6).
- **SC-004**: `cargo test --workspace` on Windows completes with no
  unexpected failures (all Unix-only tests explicitly gated).
- **SC-005**: No Unix-only symbol (`std::os::unix`, `tokio::io::unix`,
  `libc::*` Unix syscalls) appears in a non-`cfg(unix)` code path
  reachable from a Windows build (verifiable by grep audit after
  implementation).
- **SC-006**: Background process lifecycle (start → poll → kill / exit)
  works end-to-end on Windows (measured by User Story 3 scenarios).

## Assumptions

- **Windows target is x86_64-pc-windows-msvc** (the MSVC toolchain, the
  default on Windows). GNU/MinGW targets are out of scope for v1.
- **Bash (Git Bash) is the preferred shell on Windows; PowerShell is
  the fallback.** The terminal tool probes for `bash` first and falls
  back to `pwsh.exe`/`powershell.exe` only if bash is absent (FR-011).
  The existing `find_bash()` Windows branch is the starting point; this
  feature extends it with a PowerShell fallback and a per-session cache
  of the chosen shell. The POSIX-shaped wrapper script (CWD markers via
  `printf "$PWD"`, `$?` capture) is reused when bash is present; a
  PowerShell-dialect wrapper (`$PWD`, `$LASTEXITCODE`/`$?`) is added
  for the fallback path (FR-012).
- **Unix (Linux/macOS) remains the primary development platform.** This
  feature must not regress it. The cfg-guard pattern already used
  throughout the codebase is the established convention and will be
  followed.
- **`portable-pty` remains unused.** It is declared as a workspace
  dependency but the PTY code path always returns "not supported in
  this build." This feature does not change PTY behavior; PTY support
  on Windows is out of scope.
- **Secondary runtime concerns (security-guard Unix path lists,
  `joey-providers::copilot` Unix-only gh path candidates,
  `joey-mcp` stdio command resolution) are noted but NOT in P1 scope.**
  They do not block compilation and only affect narrow code paths;
  they are candidates for P4+ hardening. P1–P3 focus on build +
  terminal + process functionality.
- **The constitution (v1.1.0) governs this feature.** Principle VII
  (Non-Regression) applies in full: the cfg gates are additive and
  must preserve all existing Unix behavior byte-for-byte. Principle I
  (each crate independently buildable) means the fix is localized to
  `joey-tools` (and test gating where needed), not a cross-crate
  refactor.

## Clarifications

### 2026-08-05

**Q**: On Windows, the terminal tool currently assumes bash (Git Bash) is
on PATH. Should v1 require Git Bash as a prerequisite, or add a native
Windows shell fallback so the agent works on any Windows box?

**A**: Add a native fallback — the agent must work on a Windows box with
NO Git Bash installed. Bash-first is still preferred (reuse the existing
POSIX-shaped wrapper), but a native shell path is required when bash is
absent. This is a deliberate scope expansion from the original "Git Bash
required" default.

**Formalized as**: FR-011 rewritten from a `[NEEDS CLARIFICATION]` into a
bash-first discovery with fallback. The Assumption about Git Bash being a
hard prerequisite was rewritten to "preferred, with fallback."

---

**Q**: When bash is absent, which native Windows shell should the fallback
use — PowerShell, cmd.exe, or both?

**A**: PowerShell only (prefer `pwsh.exe`, fall back to the built-in
`powershell.exe`). cmd.exe is excluded: it lacks a clean `$PWD`-equivalent
(needs `%CD%`), has weaker exit-code handling, and maintaining two
fallback dialects adds cost without value for a shell most users won't
prefer.

**Formalized as**: Two new requirements added to capture the fallback's
behavioral contract:
- **FR-012**: the PowerShell wrapper must match the bash path's contract
  (exit code via `$LASTEXITCODE`/`$?`, CWD marker via `$PWD`, identical
  `extract_cwd_marker` parsing).
- **FR-013**: the chosen shell is cached per-session so a session never
  flips between bash and PowerShell.

User Story 2 gained acceptance scenario 6 (PowerShell fallback end-to-end).
SC-003 now explicitly covers both the bash and PowerShell paths. The edge
case for "no bash available" was expanded to describe the fallback chain
and the no-shell-found error contract.

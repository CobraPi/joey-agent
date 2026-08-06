# Data Model: Windows Platform Support

**Feature**: 014-windows-platform-support
**Date**: 2026-08-05

This feature introduces **no on-disk data entities** and **no changes to
existing on-disk formats** (SQLite schema, cron jobs.json, config.yaml —
all untouched). It is purely an in-process refactoring of the terminal
tool's execution layer. The "entities" below are in-memory types only,
documented here because they are the primary new abstractions and the
contracts in `contracts/` reference them.

---

## In-memory entities

### `Shell` (enum) — resolved shell selection

Represents which shell the terminal tool will use to execute a command.
Resolved once per process and cached (FR-013).

```
enum Shell {
    /// POSIX shell (bash, Git Bash). Path to the executable.
    /// The only variant produced on Unix.
    Bash(String),

    /// PowerShell. Path to pwsh.exe or powershell.exe.
    /// Only produced on Windows when bash is absent.
    PowerShell(String),
}
```

**Fields**:
- `Bash(path)` / `PowerShell(path)`: absolute or PATH-resolved path to
  the shell executable, as returned by `which::which`.

**Relationships**:
- Produced by `resolve_shell()` (see `contracts/shell-discovery.md`).
- Consumed by `build_wrapper_script()` to select the dialect.
- Consumed by `spawn_child()` to pick `Command::new(bash)` vs
  `Command::new(pwsh)` and the right flags (`-c` vs `-NoProfile -Command`).
- Cached in `RESOLVED_SHELL: Lazy<Mutex<Option<Shell>>>`.

**Key invariant**: once resolved, a process never observes a different
variant (FR-013). The cache is populated on first terminal call and read
on every subsequent call.

**No platform-specific methods on the enum itself** — the variant
discriminant drives behavior via match arms in the cfg-gated execution
functions, not via methods on `Shell`. This keeps the type itself
cross-platform and testable everywhere.

---

### `OutputChunkStream` (streaming boundary) — abstract reader

The platform-abstraction boundary over which `stream_output` reads
chunks. Not a Rust `trait` in the public API sense (it is private to
`terminal_tool`); documented as a conceptual contract because two
distinct concrete types back it on different platforms.

```
// Conceptual shape — the concrete impl differs per platform.
async fn next_chunk(&mut self) -> Option<Vec<u8>>;  // None = EOF
```

**Concrete implementations**:
- **Unix (`#[cfg(unix)]`)**: `tokio::io::unix::AsyncFd<OwnedFd>` wrapping
  a `dup`'d `os_pipe` reader FD. `next_chunk` = `readable()` guard +
  `libc::read` into a 64KB buffer. Unchanged from feature 009.
- **Windows (`#[cfg(not(unix))]`)**: a struct holding
  `child.stdout: ChildStdout` and `child.stderr: ChildStderr`, merged via
  `tokio::select!`. `next_chunk` = read up to 64KB from whichever pipe
  is ready; `None` when both yield EOF.

**Shared invariants (both impls)**:
- Chunk size ≤ 64KB.
- `None` strictly once, after the underlying source(s) hit EOF.
- Does NOT decode UTF-8 (caller does lossy decode) — bytes in, bytes out.
- Does NOT apply throttling or temp-file spill (that's `stream_output`'s
  job, shared and cross-platform).

**Relationship to `stream_output`**: `stream_output` is rewritten to take
this boundary (via a `#[cfg]`-selected constructor) rather than an
`AsyncFd` directly. Everything *inside* `stream_output` — the 50ms
throttle, 2s heartbeat, 100ms interrupt poll, 4KB temp-file spill,
final readback — stays identical across platforms.

---

### `WrapperScript` (value, generated per-call)

The shell-dialect script that wraps the user's command to capture exit
code and emit the CWD marker. Not a persistent entity; generated from
`Shell` + user command, consumed immediately by `spawn_child`.

```
struct WrapperScript {
    shell: Shell,          // dialect that generated this
    argv0: String,         // shell executable path
    args: Vec<String>,     // ["-c", script] | ["-NoProfile", "-Command", script]
    body: String,          // the full script text incl. command + marker
}
```

See `contracts/streaming-and-execution.md` for the exact dialect bodies.

---

## What is NOT a new entity

- **`ExitStatus` → exit code**: already handled by
  `exit_code_from_status` (terminal_tool.rs:608–626), which is already
  `#[cfg(unix)]`/`#[cfg(not(unix))]`-forked. No change.
- **`ProcessSession` / `ProcessRegistry`** (background path): already
  cross-platform (uses `tokio::process::Command` with `Stdio::piped()`,
  which works on Windows). No change for P3 — the background path
  already works once P1 unblocks compilation.
- **`ToolContext`, `ToolProgress`, `ToolResult`**: untouched.
- **CWD marker parsing (`extract_cwd_marker`, `CWD_MARKER`)**: untouched
  — the marker framing is shell-agnostic by design.

---

## Persistence / serialization

**None.** No new config keys, no new file formats, no schema changes.
The shell cache is process-local and dies with the process. This is a
pure in-process refactor, which is why the constitution's Non-Regression
gate (VII) is satisfied trivially: there is no public-surface change to
config, CLI, file formats, or traits (see plan.md Constitution Check).

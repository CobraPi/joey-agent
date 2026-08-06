# Contract: Streaming Reader & Command Execution

**Feature**: 014-windows-platform-support
**Spec refs**: FR-003, FR-004, FR-005, FR-012

## Purpose

Define the cross-platform boundary that `stream_output` reads through,
and the per-shell command-execution contract (wrapper script + spawn).

---

## Part A — OutputChunkStream (streaming boundary, FR-005)

### Conceptual interface

```
// Private to terminal_tool. Concrete type selected by #[cfg].
impl OutputChunkStream {
    async fn next_chunk(&mut self) -> Option<Vec<u8>>;
}
```

- Returns `Some(bytes)` for each read chunk (≤ 64KB).
- Returns `None` exactly once at EOF.
- Does not decode UTF-8, throttle, or spill to disk (caller's job).

### Unix concrete impl (`#[cfg(unix)]`)

Type: `UnixFdReader` wrapping `tokio::io::unix::AsyncFd<OwnedFd>`.

- Source: `os_pipe` reader FD, `dup`'d so `AsyncFd` owns it.
- `next_chunk`: `async_fd.readable()` guard → `libc::read(buf, 64KB)`
  → on `EWOULDBLOCK`, retry; on 0 or error, return `None`.
- This is the **exact** logic from feature 009 (`terminal_tool.rs:703-727`),
  extracted verbatim behind the new boundary. Byte-for-byte identical
  behavior on Unix (Principle VII).

### Windows concrete impl (`#[cfg(not(unix))]`)

Type: `WindowsPipeReader` holding `child.stdout` + `child.stderr`.

- Source: two `tokio::process::ChildStd*` handles (impl `AsyncRead`).
- `next_chunk`: `tokio::select!` over `stdout.read_buf(64KB)` and
  `stderr.read_buf(64KB)`; whichever is ready, read and return. When
  both return 0 (EOF), return `None`.
- Merge order is not guaranteed (two pipes race) — but the spec
  requires only that both streams appear in `output`, not byte-order.

### Why a boundary, not a trait object

`stream_output` is generic or `#[cfg]`-forked at the call site; there is
no `dyn` dispatch. The boundary exists so the *body* of `stream_output`
(throttle / heartbeat / interrupt / spill — ~120 lines) is written once
and shared. The platform impls are thin (~30 lines each).

---

## Part B — Wrapper script generation (FR-012)

### Function

```
fn build_wrapper_script(shell: &Shell, command: &str) -> WrapperScript;
```

### Bash dialect (unchanged)

argv0: the bash path. args: `["-c", body]`.

```
body = format!(
    "{command}\n__JOEY_STATUS=$?\nprintf '\\n{m}%s{m}' \"$PWD\"\nexit $__JOEY_STATUS",
    command = command, m = CWD_MARKER
)
```

(This is the existing `run_bash` script, terminal_tool.rs:476–480.)

### PowerShell dialect (new)

argv0: the pwsh/powershell path. args: `["-NoProfile", "-Command", body]`.

```
body = format!(
    "{command}\n$code = $LASTEXITCODE\nif ($code -eq $null) {{ $code = if ($?) {{ 0 }} else {{ 1 }} }}\nWrite-Output \"`n{m}$PWD{m}\"\nexit $code",
    command = command, m = CWD_MARKER
)
```

**Semantics matched to bash path** (FR-012):
| Concern | Bash | PowerShell |
|---------|------|------------|
| User command runs | line 1 | line 1 |
| Exit code captured | `$?` → `__JOEY_STATUS` | `$LASTEXITCODE` (or `$?` fallback) |
| CWD emitted | `printf '\n{m}%s{m}' "$PWD"` | `Write-Output "`n{m}$PWD{m}"` |
| Process exits with code | `exit $__JOEY_STATUS` | `exit $code` |

`-NoProfile` is mandatory: skips the user's PowerShell profile load
(~300–800ms), keeping cold-start ≤ 50ms (perf budget, research.md R2).

`CWD_MARKER` (`__JOEY_CWD_MARKER__`) is identical across dialects.
`extract_cwd_marker` (terminal_tool.rs:438–453) is unchanged — it only
matches the marker framing, not the producing shell.

---

## Part C — Command execution (FR-003, FR-004)

### Function (per-platform, cfg-forked)

```
#[cfg(unix)]
async fn run_command_unix(
    shell: &Shell,
    script: &WrapperScript,
    cwd: &Path,
    timeout_secs: u64,
    ctx: &ToolContext,
) -> (String, i64, bool, bool);  // (output, exit_code, timed_out, interrupted)

#[cfg(not(unix))]
async fn run_command_windows(...) -> (String, i64, bool, bool);
```

### Shared contract (both impls MUST satisfy)

| Requirement | Source |
|-------------|--------|
| Spawn child with sanitized env (`sanitized_env()`) | existing |
| stdin = `Stdio::null()` | existing |
| Combined stdout+stderr into single `output` | FR-004 |
| Stream chunks via `OutputChunkStream` → `stream_output` | FR-003 |
| Honor timeout: on expiry, kill child, `exit_code=124`, `timed_out=true` | FR-004 |
| Honor cooperative interrupt: poll `ctx.is_interrupted()` every 100ms; on interrupt, kill, `interrupted=true`, `exit_code=124` | FR-004 |
| Memory bound: 64KB chunk buffer, temp-file spill at 4KB | research.md R1 |
| Throttle progress events: 50ms window | existing |
| Heartbeat: 2s for silent commands | existing |

### Spawn differences

| Aspect | Unix | Windows |
|--------|------|---------|
| Shell argv | `bash -c <body>` | `pwsh -NoProfile -Command <body>` |
| Stdout/stderr capture | merged via `os_pipe` (single pipe) | separate `Stdio::piped()`, merged in reader |
| `child.stdout` after spawn | `None` (os_pipe owns it) | `Some(ChildStdout)` |
| Reader type | `AsyncFd<OwnedFd>` | `WindowsPipeReader` (two `AsyncRead`s) |

### Exit-code extraction

Unchanged: `exit_code_from_status` (terminal_tool.rs:608–626) is already
`#[cfg(unix)]`/`#[cfg(not(unix))]`-forked. Unix extracts signal info;
Windows uses `ExitStatus::code()`. No modification needed.

---

## Error cases

| Case | Behavior |
|------|----------|
| Shell not found | `resolve_shell()` Err → terminal tool returns error JSON (no panic) |
| Spawn fails (e.g. bad path) | `(format!("Failed to spawn command: {}", e), -1, false, false)` — existing shape |
| `os_pipe::pipe()` fails (Unix) | same spawn-failure shape |
| Read returns error mid-stream | treat as EOF, flush, return partial output (existing Unix behavior; Windows matches) |
| Timeout | kill, `124`, `timed_out=true` |
| Interrupt | kill, `124`, `interrupted=true` |

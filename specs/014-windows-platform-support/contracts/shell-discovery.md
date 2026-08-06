# Contract: Shell Discovery & Selection

**Feature**: 014-windows-platform-support
**Spec refs**: FR-011, FR-013

## Purpose

Resolve which shell the terminal tool uses to execute commands, with a
per-process cache so a session never flips shells mid-stream.

## Function

```
fn resolve_shell() -> Result<Shell, ShellResolutionError>;
```

(Private to `joey_tools::tools::terminal_tool`. Not a public API.)

## Resolution order

### On Unix (`cfg(unix)`)

1. `which::which("bash")` → `Shell::Bash(path)`
2. Probe `/usr/bin/bash`, `/bin/bash` → `Shell::Bash(path)`
3. `$SHELL` env var, else `/bin/sh` → `Shell::Bash(...)`

(Identical to the existing `find_bash()` logic, lines 53–61. Only the
return type changes: `String` → `Shell::Bash(String)`.)

**Never returns `PowerShell` on Unix.**

### On Windows (`cfg(windows)` / `cfg(not(unix))`)

1. `which::which("bash")` → `Shell::Bash(path)` (Git Bash if installed)
2. `which::which("pwsh")` → `Shell::PowerShell(path)` (PowerShell 7+)
3. `which::which("powershell")` → `Shell::PowerShell(path)` (built-in)
4. None → `Err(ShellResolutionError { tried: ["bash","pwsh","powershell"] })`

## Caching (FR-013)

```
static RESOLVED_SHELL: Lazy<Mutex<Option<Shell>>> = Lazy::new(|| Mutex::new(None));
```

- First call: resolve, store `Some(shell)`, return clone.
- Subsequent calls: return the cached value without re-probing.
- Lock held only for the duration of the read/write (microseconds).

**Invariant**: within a single process lifetime, `resolve_shell()`
returns the same `Shell` variant on every call.

## Error contract

```
struct ShellResolutionError {
    tried: Vec<&'static str>,  // names probed, in order
}
```

Display: `"No usable shell found. Tried: bash, pwsh, powershell. Install Git Bash (recommended) or PowerShell."`

The terminal tool's `execute()` maps this to a `ToolResult::Text` with:
```json
{
  "output": "",
  "exit_code": -1,
  "error": "<the message above>",
  "status": "error"
}
```
**Never panics.**

## Inputs / outputs

| Input | Source |
|-------|--------|
| PATH env var | inherited from process |
| `$SHELL` (Unix only) | env var |

| Output | Meaning |
|--------|---------|
| `Ok(Shell::Bash(p))` | use `p -c <script>` |
| `Ok(Shell::PowerShell(p))` | use `p -NoProfile -Command <script>` |
| `Err(ShellResolutionError)` | no shell; surface to user |

## Non-goals

- Does NOT accept a user-supplied shell override (out of scope; could be
  a future config key under `terminal.shell` — deliberately deferred).
- Does NOT validate the shell actually works (e.g. `bash --version`).
  Spawn-time errors surface naturally via the existing spawn-failure path.
- Does NOT re-resolve if PATH changes mid-process (cache is sticky).
